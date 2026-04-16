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

use sdrapp_core::{
    block::Block,
    error::SourceError,
    sample::IqSample,
    signal_path::HardwareCommand,
    source::{Source, SourceCapabilities},
};

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
    /// Sender side of the hardware command channel; exposed to callers via
    /// `hardware_cmd_tx()` so they can send runtime parameter changes.
    hw_cmd_tx: crossbeam_channel::Sender<HardwareCommand>,
    /// Receiver side kept here until `start()` moves it into the device thread.
    hw_cmd_rx: Option<crossbeam_channel::Receiver<HardwareCommand>>,
    /// Handle to the blocking SDRplay driver thread.
    /// Stored so `Drop` can join it, ensuring `sdrplay_api_Uninit`/`ReleaseDevice`/`Close`
    /// are always called before the process exits — even on SIGTERM.
    device_thread: Option<std::thread::JoinHandle<()>>,
}

impl RspdxSource {
    pub fn new(config: RspdxConfig) -> Self {
        let (tx, _) = broadcast::channel(512);
        let frequency_hz = Arc::new(AtomicU64::new(config.frequency_hz));
        let (hw_cmd_tx, hw_cmd_rx) = crossbeam_channel::bounded::<HardwareCommand>(32);
        Self {
            config,
            frequency_hz,
            running: Arc::new(AtomicBool::new(false)),
            tx,
            stop_tx: None,
            hw_cmd_tx,
            hw_cmd_rx: Some(hw_cmd_rx),
            device_thread: None,
        }
    }

    /// Returns a clone of the hardware command sender.
    ///
    /// Pass this to [`SignalPath::start`] as the `hardware_cmd_tx` parameter
    /// so the signal path can forward hardware-control UI commands to the device thread.
    pub fn hardware_cmd_tx(&self) -> crossbeam_channel::Sender<HardwareCommand> {
        self.hw_cmd_tx.clone()
    }

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
        // GetDevices() acquires an internal device-list lock; we must release it
        // via UnlockDeviceApi() before Close(), otherwise the lock leaks into the
        // next Open() call made by the device thread and causes Init to fail.
        let mut devices = [sys::sdrplay_api_DeviceT::default(); 16];
        let mut num: u32 = 0;
        let enum_err = unsafe { sys::sdrplay_api_GetDevices(devices.as_mut_ptr(), &mut num, 16) };
        unsafe { sys::sdrplay_api_UnlockDeviceApi() };

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
        // Take the hardware command receiver out of self — it is consumed by the thread.
        let hw_cmd_rx = self
            .hw_cmd_rx
            .take()
            .expect("RspdxSource::start() called twice");

        // Bridge channel: callback thread → tokio task
        let (iq_tx, iq_rx) = crossbeam_channel::bounded::<Arc<[IqSample]>>(128);

        // Spawn the blocking SDRplay driver thread; store the handle so Drop can join it.
        let iq_tx_clone = iq_tx.clone();
        let freq_atomic_clone = Arc::clone(&self.frequency_hz);
        self.device_thread = Some(
            std::thread::Builder::new()
                .name("sdrapp-sdrplay".into())
                .spawn(move || {
                    if let Err(e) = run_sdrplay_thread(
                        config, iq_tx_clone, running, freq_atomic_clone, hw_cmd_rx,
                    ) {
                        tracing::error!("SDRplay thread error: {e}");
                    }
                })
                .expect("failed to spawn SDRplay thread"),
        );

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

    fn frequency_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.frequency_hz)
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            name: "SDRplay RSPdx-R2".into(),
            bias_t: true,
            direct_sampling: false,
            gain_range_db: (0.0, 59.0),
            has_hardware_cmd_tx: true,
        }
    }
}

// ── Clean shutdown ────────────────────────────────────────────────────────────

impl Drop for RspdxSource {
    /// Signal the device thread to stop and block until it finishes.
    ///
    /// This ensures `sdrplay_api_Uninit`, `sdrplay_api_ReleaseDevice`, and
    /// `sdrplay_api_Close` are always called before the process exits — whether
    /// the app closes normally, receives SIGTERM, or the source is hot-swapped.
    /// Without this join, `process::exit()` (called by eframe/winit on SIGTERM)
    /// kills threads before their RAII guards run, leaving the API service locked.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Drop stop_tx to signal the Tokio bridge task to exit.
        drop(self.stop_tx.take());
        // Join the device thread — blocks until sdrplay_api_Close() returns.
        if let Some(handle) = self.device_thread.take() {
            tracing::debug!("waiting for SDRplay device thread to exit...");
            let _ = handle.join();
            tracing::debug!("SDRplay device thread exited cleanly");
        }
    }
}

// ── SDRplay thread ────────────────────────────────────────────────────────────

/// One full attempt to open the SDRplay API, select the device, configure it,
/// initialize streaming, and run until `running` is cleared.
///
/// On return (success or failure) all RAII guards fire: `Uninit`, `ReleaseDevice`,
/// and `Close` are called in order, leaving the service in a clean state so the
/// caller can retry immediately.
fn try_run_sdrplay_session(
    config: &RspdxConfig,
    iq_tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    running: Arc<AtomicBool>,
    freq_atomic: Arc<AtomicU64>,
    hw_cmd_rx: crossbeam_channel::Receiver<HardwareCommand>,
) -> anyhow::Result<()> {
    use anyhow::Context;

    // ── Open API ──────────────────────────────────────────────────────────────
    let err = unsafe { sys::sdrplay_api_Open() };
    anyhow::ensure!(
        err == sys::sdrplay_api_ErrT_sdrplay_api_Success,
        "sdrplay_api_Open failed: {err}"
    );

    // RAII: Close the API connection when this function returns (success or error).
    struct ApiGuard;
    impl Drop for ApiGuard {
        fn drop(&mut self) {
            unsafe { sys::sdrplay_api_Close() };
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
    // tuner and rspDuoMode must be set before SelectDevice (required by the API).
    devices[device_idx].tuner = sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A;
    devices[device_idx].rspDuoMode =
        sys::sdrplay_api_RspDuoModeT_sdrplay_api_RspDuoMode_Single_Tuner;

    let err = unsafe { sys::sdrplay_api_SelectDevice(&mut devices[device_idx]) };
    anyhow::ensure!(
        err == sys::sdrplay_api_ErrT_sdrplay_api_Success,
        "SelectDevice failed: {err}"
    );

    // sdrplay_api_GetDevices() locks the device API — UnlockDeviceApi() MUST be
    // called after SelectDevice or sdrplay_api_Init will fail.
    unsafe { sys::sdrplay_api_UnlockDeviceApi() };

    let dev_handle = devices[device_idx].dev;

    // Enable verbose API logging so Init failures produce a detailed reason in stderr.
    unsafe {
        sys::sdrplay_api_DebugEnable(
            dev_handle,
            sys::sdrplay_api_DbgLvl_t_sdrplay_api_DbgLvl_Verbose,
        );
    }

    // RAII: Uninit + ReleaseDevice when this function returns.
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

    // ── Initialize streaming (NO pre-configuration, matching the upstream reference) ──
    // The upstream SDR++ app calls Init immediately after GetDeviceParams without
    // modifying any parameters first.  All configuration is applied via Update()
    // after a successful Init.  Pre-configuring params before Init was causing
    // sdrplay_api_Fail (err=1) on the RSPdx-R2.
    let init_err = unsafe { sys::sdrplay_api_Init(dev_handle, &mut callbacks, ctx_ptr) };
    if init_err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
        // Reclaim box to avoid leak before bailing (DeviceGuard + ApiGuard fire on return)
        let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };
        anyhow::bail!("sdrplay_api_Init failed: {init_err}");
    }

    // ── Configure device via Update() after successful Init ───────────────────
    // All parameter writes must happen through the params_ptr returned by
    // GetDeviceParams, followed by an sdrplay_api_Update call.
    unsafe {
        let params_ref = &mut *params_ptr;
        let ch = &mut *params_ref.rxChannelA;
        let rsp = &mut (*params_ref.devParams).rspDxParams;

        // Sample rate
        (*params_ref.devParams).fsFreq.fsHz = config.sample_rate_sps as f64;
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Dev_Fs,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);

        // Frequency
        ch.tunerParams.rfFreq.rfHz = config.frequency_hz as f64;
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Frf,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);

        // IF mode + bandwidth — both must be set together.
        // Zero-IF: IF centre = 0 Hz, wide bandwidth (1.536 MHz default).
        // Low-IF: IF centre shifts the signal away from the DC spike;
        //         narrower bandwidth reduces noise outside the channel.
        let (if_type, bw_type) = match config.if_mode {
            crate::config::IfMode::ZeroIf => (
                sys::sdrplay_api_If_kHzT_sdrplay_api_IF_Zero,
                sys::sdrplay_api_Bw_MHzT_sdrplay_api_BW_1_536,
            ),
            crate::config::IfMode::LowIf200kHz => (
                sys::sdrplay_api_If_kHzT_sdrplay_api_IF_0_450,
                sys::sdrplay_api_Bw_MHzT_sdrplay_api_BW_0_200,
            ),
            crate::config::IfMode::LowIf500kHz => (
                sys::sdrplay_api_If_kHzT_sdrplay_api_IF_0_450,
                sys::sdrplay_api_Bw_MHzT_sdrplay_api_BW_0_600,
            ),
            crate::config::IfMode::LowIf1MHz => (
                sys::sdrplay_api_If_kHzT_sdrplay_api_IF_1_620,
                sys::sdrplay_api_Bw_MHzT_sdrplay_api_BW_1_536,
            ),
            crate::config::IfMode::LowIf2MHz => (
                sys::sdrplay_api_If_kHzT_sdrplay_api_IF_2_048,
                sys::sdrplay_api_Bw_MHzT_sdrplay_api_BW_1_536,
            ),
        };
        ch.tunerParams.ifType = if_type;
        ch.tunerParams.bwType = bw_type;
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_IfType,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_BwType,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);

        // AGC / gain
        if config.agc_enabled {
            ch.ctrlParams.agc.enable = sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_CTRL_EN;
            ch.ctrlParams.agc.setPoint_dBfs = config.agc_setpoint_dbfs;
        } else {
            ch.ctrlParams.agc.enable = sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_DISABLE;
            ch.tunerParams.gain.LNAstate = config.lna_state;
            ch.tunerParams.gain.gRdB = (-config.if_gain_dbfs).clamp(0, 59);
        }
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Agc,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Gr,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);

        // RSPdx-R2 specific
        rsp.antennaSel = match config.antenna {
            Antenna::A => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_A,
            Antenna::B => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_B,
            Antenna::C => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_C,
        };
        rsp.biasTEnable = config.bias_t_enabled as u8;
        rsp.hdrEnable = config.hdr_mode as u8;
        rsp.rfNotchEnable = config.am_notch_enabled as u8;
        rsp.rfDabNotchEnable = config.fm_notch_enabled as u8;
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_AntennaControl);
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_BiasTControl);
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfNotchControl);
        sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfDabNotchControl);
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

        // Drain hardware commands (non-blocking)
        while let Ok(cmd) = hw_cmd_rx.try_recv() {
            unsafe {
                let ch = &mut *(*params_ptr).rxChannelA;
                let rsp = &mut (*(*params_ptr).devParams).rspDxParams;
                let (reason, reason_ext) = match cmd {
                    HardwareCommand::SetLnaState(n) => {
                        ch.tunerParams.gain.LNAstate = n;
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Gr,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetIfGain(g) => {
                        // Same sign convention: config/UI stores negative dBFS, API wants positive.
                        ch.tunerParams.gain.gRdB = (-g).clamp(0, 59);
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Gr,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetAgcEnabled(en) => {
                        if en {
                            ch.ctrlParams.agc.enable =
                                sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_CTRL_EN;
                        } else {
                            ch.ctrlParams.agc.enable =
                                sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_DISABLE;
                        }
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Agc,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetAgcSetpoint(sp) => {
                        ch.ctrlParams.agc.setPoint_dBfs = sp;
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Agc,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetBiasT(en) => {
                        rsp.biasTEnable = en as u8;
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_BiasTControl)
                    }
                    HardwareCommand::SetHdrMode(en) => {
                        rsp.hdrEnable = en as u8;
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_HdrEnable)
                    }
                    HardwareCommand::SetAmNotch(en) => {
                        rsp.rfNotchEnable = en as u8;
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfNotchControl)
                    }
                    HardwareCommand::SetFmNotch(en) => {
                        rsp.rfDabNotchEnable = en as u8;
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfDabNotchControl)
                    }
                    HardwareCommand::SetAntenna(port) => {
                        rsp.antennaSel = match port {
                            1 => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_B,
                            2 => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_C,
                            _ => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_A,
                        };
                        (sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_AntennaControl)
                    }
                };
                let err = sys::sdrplay_api_Update(
                    dev_handle,
                    sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
                    reason,
                    reason_ext,
                );
                if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
                    tracing::warn!(err, "sdrplay_api_Update (hardware cmd) failed");
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Reclaim the callback context box.
    // DeviceGuard + ApiGuard fire here (Uninit → ReleaseDevice → Close).
    let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };

    tracing::info!("RSPdx-R2 streaming stopped");
    Ok(())
}

/// Outer retry wrapper for `try_run_sdrplay_session`.
///
/// If the session fails (e.g. `sdrplay_api_Init` returns a generic error because a
/// previous process was killed before it could call `Uninit`/`ReleaseDevice`/`Close`),
/// the RAII guards in `try_run_sdrplay_session` fire on its return, fully closing
/// the API connection.  The outer loop then waits and retries — each attempt does a
/// fresh `Open → GetDevices → SelectDevice → UnlockDeviceApi → Init` cycle.
fn run_sdrplay_thread(
    config: RspdxConfig,
    iq_tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    running: Arc<AtomicBool>,
    freq_atomic: Arc<AtomicU64>,
    hw_cmd_rx: crossbeam_channel::Receiver<HardwareCommand>,
) -> anyhow::Result<()> {
    const MAX_ATTEMPTS: u32 = 4;
    const RETRY_DELAY_SECS: u64 = 2;

    let mut last_err = anyhow::anyhow!("no attempts made");
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            tracing::warn!(
                attempt,
                "SDRplay session failed, retrying in {RETRY_DELAY_SECS}s (service may still be \
                 releasing resources from previous session)"
            );
            std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));
        }

        match try_run_sdrplay_session(&config, iq_tx.clone(), Arc::clone(&running), Arc::clone(&freq_atomic), hw_cmd_rx.clone() /* crossbeam Receiver is Clone */) {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!(attempt, err = %e, "SDRplay session attempt failed");
                last_err = e;
                // If `running` was cleared externally (user hit Stop), don't retry.
                if !running.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
    Err(last_err)
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
