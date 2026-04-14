// Note: unsafe code is INTENTIONALLY allowed in this file only.
// This is the FFI boundary crate — all unsafe is isolated here.
// All other crates in the workspace use #![forbid(unsafe_code)].

//! SDRplay RSPdx-R2 source implementation.
//!
//! Architecture:
//!   sdrplay_api callback thread (OS-managed)
//!       │  crossbeam_channel::Sender<Arc<[IqSample]>>
//!       ▼
//!   tokio streaming task
//!       │  broadcast::Sender<Arc<[IqSample]>>
//!       ▼
//!   Signal path consumers

use std::ffi::c_void;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use sdrapp_core::{block::Block, error::SourceError, sample::IqSample, source::Source};

use crate::config::{Antenna, RspdxConfig};

// Re-export the generated FFI bindings
use sdrapp_sdrplay_sys as sys;

/// Number of IQ samples per batch sent on the broadcast channel.
const BATCH_SIZE: usize = 1024;

/// Normalisation factor: convert int16 → f32 in [-1.0, 1.0]
const NORM: f32 = 1.0 / 32768.0;

/// Context passed as `void *` to the sdrplay callback.
/// Must be `Send` + `Sync` because the callback runs on the API's thread.
struct CallbackContext {
    tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    batch: Mutex<Vec<IqSample>>,
}

/// SDRplay RSPdx-R2 source.
pub struct RspdxSource {
    config: RspdxConfig,
    frequency_hz: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    tx: broadcast::Sender<Arc<[IqSample]>>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl RspdxSource {
    pub fn new(config: RspdxConfig) -> Self {
        let (tx, _) = broadcast::channel(64);
        let frequency_hz = Arc::new(AtomicU64::new(config.frequency_hz));
        Self {
            config,
            frequency_hz,
            running: Arc::new(AtomicBool::new(false)),
            tx,
            stop_tx: None,
        }
    }

    /// Returns a list of available SDRplay device hardware names.
    /// Returns a clone of the shared frequency atomic so callers can write new
    /// frequencies that the device thread will pick up within one poll interval.
    pub fn frequency_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.frequency_hz)
    }

    /// Returns `true` if the SDRplay API opens successfully **and** at least one
    /// device is enumerated. Used by `main.rs` as a pre-flight check before
    /// committing to the hardware source path.
    ///
    /// Opens and immediately closes the API, so it has no side effects on the
    /// subsequent `RspdxSource::start()` call.
    pub fn is_device_available() -> bool {
        // Open the SDRplay service.
        let err = unsafe { sys::sdrplay_api_Open() };
        if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
            tracing::debug!("sdrplay_api_Open failed ({err}) — no hardware available");
            return false;
        }

        // Enumerate connected devices.
        let mut devices = [sys::sdrplay_api_DeviceT::default(); 16];
        let mut num: u32 = 0;
        let enum_err =
            unsafe { sys::sdrplay_api_GetDevices(devices.as_mut_ptr(), &mut num, 16) };

        // Always close the API, regardless of enumeration result.
        unsafe { sys::sdrplay_api_Close() };

        let found = enum_err == sys::sdrplay_api_ErrT_sdrplay_api_Success && num > 0;
        if !found {
            tracing::info!("no SDRplay devices found — will start in demo mode");
        }
        found
    }

    pub fn available_devices() -> Vec<String> {
        let mut devices = [sys::sdrplay_api_DeviceT::default(); 16];
        let mut num: u32 = 0;
        let err = unsafe { sys::sdrplay_api_GetDevices(devices.as_mut_ptr(), &mut num, 16) };
        if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
            return vec![];
        }
        (0..num as usize)
            .map(|i| {
                let name = unsafe {
                    std::ffi::CStr::from_ptr(devices[i].SerNo.as_ptr())
                        .to_string_lossy()
                        .into_owned()
                };
                format!("RSP ({name})")
            })
            .collect()
    }
}

impl Block for RspdxSource {
    fn start(&mut self) -> JoinHandle<()> {
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        self.stop_tx = Some(stop_tx);

        let broadcast_tx = self.tx.clone();
        let config = self.config.clone();
        let running = Arc::clone(&self.running);
        let freq_atomic = Arc::clone(&self.frequency_hz);

        // Bridge channel: callback thread → tokio task
        let (iq_tx, iq_rx) = crossbeam_channel::bounded::<Arc<[IqSample]>>(128);

        // Spawn the blocking SDRplay driver thread
        let iq_tx_clone = iq_tx.clone();
        let freq_atomic_clone = Arc::clone(&self.frequency_hz);
        std::thread::Builder::new()
            .name("sdrapp-sdrplay".into())
            .spawn(move || {
                if let Err(e) = run_sdrplay_thread(config, iq_tx_clone, running, freq_atomic_clone) {
                    tracing::error!("SDRplay thread error: {e}");
                }
            })
            .expect("failed to spawn SDRplay thread");

        // Tokio task: forward from crossbeam channel to broadcast
        tokio::spawn(async move {
            tokio::select! {
                _ = stop_rx => {
                    tracing::info!("RSPdx-R2 source stopped via signal");
                }
                _ = tokio::task::spawn_blocking(move || {
                    while let Ok(batch) = iq_rx.recv() {
                        // Sync the atomic frequency from the latest batch
                        let _ = freq_atomic.load(Ordering::Relaxed); // just ensure it's live
                        let _ = broadcast_tx.send(batch);
                    }
                }) => {}
            }
        })
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        // stop_tx drop signals the tokio task to exit
    }
}

impl Source for RspdxSource {
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>> {
        self.tx.subscribe()
    }

    fn set_frequency(&self, hz: u64) -> Result<(), SourceError> {
        const MIN_HZ: u64 = 1_000;
        const MAX_HZ: u64 = 2_000_000_000;
        if !(MIN_HZ..=MAX_HZ).contains(&hz) {
            return Err(SourceError::FrequencyOutOfRange(hz));
        }
        self.frequency_hz.store(hz, Ordering::Relaxed);
        Ok(())
    }

    fn set_sample_rate(&self, sps: u32) -> Result<(), SourceError> {
        const SUPPORTED: &[u32] = &[
            200_000, 500_000, 1_000_000, 2_000_000, 6_000_000, 8_000_000, 10_000_000,
        ];
        if !SUPPORTED.contains(&sps) {
            return Err(SourceError::SampleRateNotSupported(sps));
        }
        Ok(())
    }

    fn frequency(&self) -> u64 {
        self.frequency_hz.load(Ordering::Relaxed)
    }

    fn sample_rate(&self) -> u32 {
        self.config.sample_rate_sps
    }
}

// ── SDRplay thread ────────────────────────────────────────────────────────────

fn run_sdrplay_thread(
    config: RspdxConfig,
    iq_tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    running: Arc<AtomicBool>,
    freq_atomic: Arc<AtomicU64>,
) -> anyhow::Result<()> {
    use anyhow::Context;

    // ── Open API ──────────────────────────────────────────────────────────────
    let err = unsafe { sys::sdrplay_api_Open() };
    anyhow::ensure!(
        err == sys::sdrplay_api_ErrT_sdrplay_api_Success,
        "sdrplay_api_Open failed: {err}"
    );

    struct ApiGuard;
    impl Drop for ApiGuard {
        fn drop(&mut self) {
            unsafe {
                sys::sdrplay_api_Close();
            }
        }
    }
    let _api_guard = ApiGuard;

    // ── Enumerate devices ─────────────────────────────────────────────────────
    let mut devices = [sys::sdrplay_api_DeviceT::default(); 16];
    let mut num_devices: u32 = 0;
    let err = unsafe { sys::sdrplay_api_GetDevices(devices.as_mut_ptr(), &mut num_devices, 16) };
    anyhow::ensure!(
        err == sys::sdrplay_api_ErrT_sdrplay_api_Success,
        "GetDevices failed: {err}"
    );
    anyhow::ensure!(num_devices > 0, "no SDRplay devices found");

    // Select first RSPdx-R2 (hwVer == 7) or fall back to first device
    let device_idx = (0..num_devices as usize)
        .find(|&i| devices[i].hwVer == sys::SDRPLAY_RSPdxR2_ID as u8)
        .or(if num_devices > 0 { Some(0) } else { None })
        .context("no compatible SDRplay device")?;

    tracing::info!(
        device = device_idx,
        hw_ver = devices[device_idx].hwVer,
        "SDRplay device selected"
    );

    // ── Select device ─────────────────────────────────────────────────────────
    let err = unsafe { sys::sdrplay_api_SelectDevice(&mut devices[device_idx]) };
    anyhow::ensure!(
        err == sys::sdrplay_api_ErrT_sdrplay_api_Success,
        "SelectDevice failed: {err}"
    );

    let dev_handle = devices[device_idx].dev;

    struct DeviceGuard(sys::sdrplay_api_DeviceT);
    impl Drop for DeviceGuard {
        fn drop(&mut self) {
            unsafe {
                sys::sdrplay_api_Uninit(self.0.dev);
                sys::sdrplay_api_ReleaseDevice(&mut self.0);
            }
        }
    }
    let _dev_guard = DeviceGuard(devices[device_idx]);

    // ── Get device parameters ─────────────────────────────────────────────────
    let mut params_ptr: *mut sys::sdrplay_api_DeviceParamsT = std::ptr::null_mut();
    let err = unsafe { sys::sdrplay_api_GetDeviceParams(dev_handle, &mut params_ptr) };
    anyhow::ensure!(
        err == sys::sdrplay_api_ErrT_sdrplay_api_Success && !params_ptr.is_null(),
        "GetDeviceParams failed"
    );

    let params = unsafe { &mut *params_ptr };

    // ── Configure sample rate ─────────────────────────────────────────────────
    unsafe {
        (*params.devParams).fsFreq.fsHz = config.sample_rate_sps as f64;
    }

    // ── Configure tuner parameters (rxChannelA) ────────────────────────────
    let ch = unsafe { &mut *params.rxChannelA };

    // Frequency
    ch.tunerParams.rfFreq.rfHz = config.frequency_hz as f64;

    // AGC
    if config.agc_enabled {
        ch.ctrlParams.agc.enable = sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_CTRL_EN;
        ch.ctrlParams.agc.setPoint_dBfs = -60;
    } else {
        ch.ctrlParams.agc.enable = sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_DISABLE;
        ch.tunerParams.gain.LNAstate = config.lna_state;
    }

    // IF mode
    ch.tunerParams.bwType = sys::sdrplay_api_Bw_MHzT_sdrplay_api_BW_1_536;

    // RSPdx-R2 antenna selection (rspDxParams is a direct field on DevParamsT)
    unsafe {
        let rsp_params = &mut (*params.devParams).rspDxParams;
        rsp_params.antennaSel = match config.antenna {
            Antenna::A => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_A,
            Antenna::B => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_B,
            Antenna::C => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_C,
        };
    }

    // ── Set up callback context ───────────────────────────────────────────────
    let ctx = Box::new(CallbackContext {
        tx: iq_tx,
        batch: Mutex::new(Vec::with_capacity(BATCH_SIZE * 2)),
    });
    let ctx_ptr = Box::into_raw(ctx) as *mut c_void;

    let mut callbacks = sys::sdrplay_api_CallbackFnsT {
        StreamACbFn: Some(stream_callback_a),
        StreamBCbFn: None,
        EventCbFn: Some(event_callback),
    };

    running.store(true, Ordering::Relaxed);

    // ── Initialize streaming ──────────────────────────────────────────────────
    let err = unsafe { sys::sdrplay_api_Init(dev_handle, &mut callbacks, ctx_ptr) };
    if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
        // Reclaim box to avoid leak
        let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };
        anyhow::bail!("sdrplay_api_Init failed: {err}");
    }

    tracing::info!(
        freq_hz = config.frequency_hz,
        sample_rate = config.sample_rate_sps,
        "RSPdx-R2 streaming started"
    );

    // ── Run until stop signal ─────────────────────────────────────────────────
    let mut last_freq = config.frequency_hz;
    while running.load(Ordering::Relaxed) {
        // Poll for frequency changes written by the signal path
        let new_freq = freq_atomic.load(Ordering::Relaxed);
        if new_freq != last_freq {
            unsafe {
                (*(*params_ptr).rxChannelA).tunerParams.rfFreq.rfHz = new_freq as f64;
                let err = sys::sdrplay_api_Update(
                    dev_handle,
                    sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
                    sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Frf,
                    sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None,
                );
                if err == sys::sdrplay_api_ErrT_sdrplay_api_Success {
                    tracing::info!(
                        freq_hz = new_freq,
                        freq_mhz = new_freq / 1_000_000,
                        "SDRplay frequency updated"
                    );
                } else {
                    tracing::warn!(err, "sdrplay_api_Update (Frf) failed");
                }
            }
            last_freq = new_freq;
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Cleanup: Uninit + ReleaseDevice happen in DeviceGuard drop.
    // Reclaim the callback context box.
    let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };

    tracing::info!("RSPdx-R2 streaming stopped");
    Ok(())
}

// ── Callbacks ─────────────────────────────────────────────────────────────────

extern "C" fn stream_callback_a(
    xi: *mut i16,
    xq: *mut i16,
    _params: *mut sys::sdrplay_api_StreamCbParamsT,
    num_samples: u32,
    _reset: u32,
    cb_context: *mut c_void,
) {
    // SAFETY: cb_context is a Box<CallbackContext> kept alive by run_sdrplay_thread.
    let ctx = unsafe { &*(cb_context as *const CallbackContext) };
    let n = num_samples as usize;

    let mut batch = ctx.batch.lock();
    for i in 0..n {
        let re = unsafe { *xi.add(i) } as f32 * NORM;
        let im = unsafe { *xq.add(i) } as f32 * NORM;
        batch.push(IqSample::new(re, im));
    }

    if batch.len() >= BATCH_SIZE {
        let frames: Arc<[IqSample]> = batch.drain(..).collect::<Vec<_>>().into();
        let _ = ctx.tx.try_send(frames);
    }
}

extern "C" fn event_callback(
    event_id: sys::sdrplay_api_EventT,
    tuner: sys::sdrplay_api_TunerSelectT,
    _params: *mut sys::sdrplay_api_EventParamsT,
    _cb_context: *mut c_void,
) {
    tracing::debug!(event = event_id, tuner = tuner, "SDRplay event");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_out_of_range_rejected() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_frequency(0).is_err());
        assert!(src.set_frequency(3_000_000_000).is_err());
    }

    #[test]
    fn valid_frequency_accepted() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_frequency(100_000_000).is_ok());
        assert_eq!(src.frequency(), 100_000_000);
    }

    #[test]
    fn unsupported_sample_rate_rejected() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_sample_rate(12345).is_err());
    }

    #[test]
    fn supported_sample_rates_accepted() {
        let src = RspdxSource::new(RspdxConfig::default());
        assert!(src.set_sample_rate(2_000_000).is_ok());
        assert!(src.set_sample_rate(10_000_000).is_ok());
    }

    #[test]
    fn iq_normalisation_is_correct() {
        // int16 max value (32767) should normalize close to 1.0
        let val = 32767_i16 as f32 * NORM;
        assert!(
            (val - 1.0).abs() < 0.0001,
            "32767 should normalize to ~1.0: {val}"
        );

        // int16 min value (-32768) should normalize to -1.0
        let val = -32768_i16 as f32 * NORM;
        assert!(
            (val - (-1.0)).abs() < 0.0001,
            "-32768 should normalize to ~-1.0: {val}"
        );
    }
}
