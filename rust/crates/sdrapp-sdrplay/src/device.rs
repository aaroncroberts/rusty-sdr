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

use parking_lot::{Mutex, RwLock};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use sdrapp_core::{
    block::Block,
    error::SourceError,
    sample::IqSample,
    signal_path::{HardwareCommand, SharedState},
    source::{Source, SourceCapabilities},
};

use crate::config::{Antenna, RspdxConfig};

// Re-export the generated FFI bindings
use sdrapp_sdrplay_sys as sys;

/// Number of IQ samples per batch sent on the broadcast channel.
const BATCH_SIZE: usize = 1024;

/// Normalisation factor: convert int16 → f32 in [-1.0, 1.0]
const NORM: f32 = 1.0 / 32768.0;

// ── Device lifecycle status ───────────────────────────────────────────────────

/// Lifecycle status of the RSPdx-R2 hardware device.
///
/// Published via a `tokio::sync::watch` channel so any subscriber always has
/// the latest status without blocking. Read by `main.rs` to update
/// `SharedState.source_name` and by the diagnostics panel.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceStatus {
    /// Device thread is starting (initial open or after restart/reconnect).
    Connecting,
    /// Device is open and streaming IQ samples.
    Running { serial: String, hw_ver: u8 },
    /// A session failed; the thread will retry after a brief delay.
    Reconnecting { attempt: u32, reason: String },
    /// Device removed (hot-unplug) or retries exhausted; no longer streaming.
    Disconnected,
}

// ── Internal callback context ─────────────────────────────────────────────────

/// Context passed as `void *` to the sdrplay callback.
/// Must be `Send` + `Sync` because the callback runs on the API's thread.
struct CallbackContext {
    tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    batch: Mutex<Vec<IqSample>>,
    /// Set by `event_callback` when `sdrplay_api_DeviceRemoved` fires.
    /// The main device loop polls this flag and exits cleanly on hot-unplug.
    disconnected: Arc<AtomicBool>,
    /// Set by `event_callback` when `sdrplay_api_PowerOverloadChange` fires.
    /// The main device loop polls this and sends `OverloadMsgAck` to the API,
    /// which allows the AGC subsystem to reduce gain and correct the overload.
    overload_ack_needed: Arc<AtomicBool>,
}

// ── RspdxSource ───────────────────────────────────────────────────────────────

/// SDRplay RSPdx-R2 source.
pub struct RspdxSource {
    config: RspdxConfig,
    frequency_hz: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    tx: broadcast::Sender<Arc<[IqSample]>>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Sender side of the hardware command channel; exposed via `hardware_cmd_tx()`.
    hw_cmd_tx: crossbeam_channel::Sender<HardwareCommand>,
    /// Receiver side kept here until `start()` moves it into the device thread.
    hw_cmd_rx: Option<crossbeam_channel::Receiver<HardwareCommand>>,
    /// Device lifecycle status — subscribers always see the latest value.
    status_tx: tokio::sync::watch::Sender<DeviceStatus>,
    /// Shared application state; the device thread writes diagnostics into it.
    /// Set via `with_shared()` before calling `start()`.
    shared: Option<Arc<RwLock<SharedState>>>,
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
        let (status_tx, _) = tokio::sync::watch::channel(DeviceStatus::Connecting);
        Self {
            config,
            frequency_hz,
            running: Arc::new(AtomicBool::new(false)),
            tx,
            stop_tx: None,
            hw_cmd_tx,
            hw_cmd_rx: Some(hw_cmd_rx),
            status_tx,
            shared: None,
            device_thread: None,
        }
    }

    /// Attach shared application state so the device thread can write diagnostics.
    ///
    /// Must be called before `start()`.  Returns `self` for builder-style use:
    /// ```rust,ignore
    /// let src = RspdxSource::new(cfg).with_shared(Arc::clone(&shared));
    /// ```
    pub fn with_shared(mut self, shared: Arc<RwLock<SharedState>>) -> Self {
        self.shared = Some(shared);
        self
    }

    /// Returns a clone of the hardware command sender.
    pub fn hardware_cmd_tx(&self) -> crossbeam_channel::Sender<HardwareCommand> {
        self.hw_cmd_tx.clone()
    }

    /// Returns a clone of the shared frequency atomic.
    pub fn frequency_atomic(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.frequency_hz)
    }

    /// Subscribe to device lifecycle status changes.
    ///
    /// The returned `Receiver` always holds the last published `DeviceStatus`
    /// and wakes when a new status is published.
    pub fn status_rx(&self) -> tokio::sync::watch::Receiver<DeviceStatus> {
        self.status_tx.subscribe()
    }

    /// Returns `true` if the SDRplay API opens successfully **and** at least one
    /// device is enumerated.
    ///
    /// Opens and immediately closes the API — no side effects on a subsequent `start()`.
    pub fn is_device_available() -> bool {
        let err = unsafe { sys::sdrplay_api_Open() };
        if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
            tracing::debug!(err, "sdrplay_api_Open failed — no hardware available");
            return false;
        }

        let mut devices = [sys::sdrplay_api_DeviceT::default(); 16];
        let mut num: u32 = 0;
        let enum_err = unsafe { sys::sdrplay_api_GetDevices(devices.as_mut_ptr(), &mut num, 16) };
        unsafe { sys::sdrplay_api_UnlockDeviceApi() };
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
        let hw_cmd_rx = self
            .hw_cmd_rx
            .take()
            .expect("RspdxSource::start() called twice");
        let status_tx = self.status_tx.clone();
        let shared = self.shared.clone();

        // Bridge channel: callback thread → tokio task
        let (iq_tx, iq_rx) = crossbeam_channel::bounded::<Arc<[IqSample]>>(128);

        let iq_tx_clone = iq_tx.clone();
        let freq_atomic_clone = Arc::clone(&self.frequency_hz);
        self.device_thread = Some(
            std::thread::Builder::new()
                .name("sdrapp-sdrplay".into())
                .spawn(move || {
                    run_sdrplay_thread(
                        config,
                        iq_tx_clone,
                        running,
                        freq_atomic_clone,
                        hw_cmd_rx,
                        status_tx,
                        shared,
                    );
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
                        let _ = freq_atomic.load(Ordering::Relaxed);
                        let _ = broadcast_tx.send(batch);
                    }
                }) => {}
            }
        })
    }

    fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl Source for RspdxSource {
    fn subscribe(&self) -> broadcast::Receiver<Arc<[IqSample]>> {
        self.tx.subscribe()
    }

    fn iq_sender(&self) -> broadcast::Sender<Arc<[IqSample]>> {
        self.tx.clone()
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
    /// Ensures `sdrplay_api_Uninit`, `sdrplay_api_ReleaseDevice`, and
    /// `sdrplay_api_Close` are always called before the process exits.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        drop(self.stop_tx.take());
        if let Some(handle) = self.device_thread.take() {
            tracing::debug!("waiting for SDRplay device thread to exit...");
            let _ = handle.join();
            tracing::debug!("SDRplay device thread exited cleanly");
        }
    }
}

// ── SDRplay thread ────────────────────────────────────────────────────────────

/// Outcome of a single SDRplay streaming session.
enum SessionOutcome {
    /// Session ran until `running` was cleared by the user.  Do not retry.
    Stopped,
    /// A `HardwareCommand::RestartDevice` was received.  Restart immediately, no delay.
    Restart,
    /// The session failed (API error, hot-unplug, etc.).  Retry after backoff.
    Error(anyhow::Error),
}

/// Run one complete SDRplay session: Open → GetDevices → SelectDevice → Init
/// → configure → poll until stop/restart/error → Uninit → ReleaseDevice → Close.
///
/// RAII guards ensure the cleanup sequence always runs even when bailing via `?`.
#[allow(clippy::too_many_arguments)]
fn try_run_sdrplay_session(
    config: &RspdxConfig,
    iq_tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    running: Arc<AtomicBool>,
    freq_atomic: Arc<AtomicU64>,
    hw_cmd_rx: crossbeam_channel::Receiver<HardwareCommand>,
    status_tx: &tokio::sync::watch::Sender<DeviceStatus>,
    shared: Option<&Arc<RwLock<SharedState>>>,
    restart_requested: &Arc<AtomicBool>,
) -> SessionOutcome {
    let _session_span = tracing::info_span!("sdrplay_session").entered();

    // ── Open API ──────────────────────────────────────────────────────────────
    let err = unsafe { sys::sdrplay_api_Open() };
    if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
        let msg = format!("sdrplay_api_Open failed: {err}");
        tracing::error!(error_code = err, "sdrplay_api_Open failed");
        if let Some(s) = shared {
            s.write().device_diagnostics.push_error(&msg);
        }
        return SessionOutcome::Error(anyhow::anyhow!("{msg}"));
    }

    // RAII: Close the API connection when this function returns.
    struct ApiGuard;
    impl Drop for ApiGuard {
        fn drop(&mut self) {
            tracing::debug!("sdrplay_api_Close");
            unsafe { sys::sdrplay_api_Close() };
        }
    }
    let _api_guard = ApiGuard;

    // Read API version for diagnostics.
    let api_version = {
        let mut ver: f32 = 0.0;
        unsafe { sys::sdrplay_api_ApiVersion(&mut ver) };
        format!("{ver:.2}")
    };
    tracing::info!(api_version = %api_version, "SDRplay API opened");

    // ── Enumerate devices ─────────────────────────────────────────────────────
    let mut devices = [sys::sdrplay_api_DeviceT::default(); 16];
    let mut num_devices: u32 = 0;
    let err = unsafe { sys::sdrplay_api_GetDevices(devices.as_mut_ptr(), &mut num_devices, 16) };
    if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
        let msg = format!("GetDevices failed: {err}");
        tracing::error!(error_code = err, "sdrplay_api_GetDevices failed");
        if let Some(s) = shared {
            s.write().device_diagnostics.push_error(&msg);
        }
        unsafe { sys::sdrplay_api_UnlockDeviceApi() };
        return SessionOutcome::Error(anyhow::anyhow!("{msg}"));
    }
    if num_devices == 0 {
        unsafe { sys::sdrplay_api_UnlockDeviceApi() };
        return SessionOutcome::Error(anyhow::anyhow!("no SDRplay devices found"));
    }

    // Select first RSPdx-R2 (hwVer == 7) or fall back to first device.
    let device_idx = (0..num_devices as usize)
        .find(|&i| devices[i].hwVer == sys::SDRPLAY_RSPdxR2_ID as u8)
        .unwrap_or(0);

    let serial = unsafe {
        std::ffi::CStr::from_ptr(devices[device_idx].SerNo.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    let hw_ver = devices[device_idx].hwVer;
    tracing::info!(
        device_idx,
        hw_ver,
        serial = %serial,
        "SDRplay device selected"
    );

    // ── Select device ─────────────────────────────────────────────────────────
    devices[device_idx].tuner = sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A;
    devices[device_idx].rspDuoMode =
        sys::sdrplay_api_RspDuoModeT_sdrplay_api_RspDuoMode_Single_Tuner;

    let err = unsafe { sys::sdrplay_api_SelectDevice(&mut devices[device_idx]) };
    if err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
        let msg = format!("SelectDevice failed: {err}");
        tracing::error!(error_code = err, "sdrplay_api_SelectDevice failed");
        if let Some(s) = shared {
            s.write().device_diagnostics.push_error(&msg);
        }
        unsafe { sys::sdrplay_api_UnlockDeviceApi() };
        return SessionOutcome::Error(anyhow::anyhow!("{msg}"));
    }
    // UnlockDeviceApi MUST be called after SelectDevice or Init will fail.
    unsafe { sys::sdrplay_api_UnlockDeviceApi() };

    let dev_handle = devices[device_idx].dev;

    // Enable verbose API logging so Init failures produce detailed diagnostics.
    unsafe {
        sys::sdrplay_api_DebugEnable(
            dev_handle,
            sys::sdrplay_api_DbgLvl_t_sdrplay_api_DbgLvl_Verbose,
        );
    }

    // RAII: Uninit + ReleaseDevice when this function returns.
    //
    // `uninit_done` tracks whether Uninit has already been called so we never
    // call it twice (explicit early call before freeing the callback context,
    // then the Drop falls through to ReleaseDevice only).
    struct DeviceGuard {
        device: sys::sdrplay_api_DeviceT,
        uninit_done: bool,
    }
    impl DeviceGuard {
        /// Call Uninit now, marking it done so Drop won't call it again.
        /// MUST be called before freeing the callback context to ensure no
        /// stream callbacks fire on freed memory.
        fn uninit(&mut self) {
            if !self.uninit_done {
                tracing::debug!("sdrplay_api_Uninit (explicit — stopping callbacks)");
                unsafe { sys::sdrplay_api_Uninit(self.device.dev) };
                self.uninit_done = true;
            }
        }
    }
    impl Drop for DeviceGuard {
        fn drop(&mut self) {
            // Uninit should already have been called explicitly; call it here
            // only as a safety net (e.g. early-return via `?`).
            if !self.uninit_done {
                tracing::debug!("sdrplay_api_Uninit (fallback in drop)");
                unsafe { sys::sdrplay_api_Uninit(self.device.dev) };
            }
            tracing::debug!("sdrplay_api_ReleaseDevice");
            unsafe { sys::sdrplay_api_ReleaseDevice(&mut self.device) };
        }
    }
    let mut dev_guard = DeviceGuard { device: devices[device_idx], uninit_done: false };

    // ── Get device parameters ─────────────────────────────────────────────────
    let mut params_ptr: *mut sys::sdrplay_api_DeviceParamsT = std::ptr::null_mut();
    let err = unsafe { sys::sdrplay_api_GetDeviceParams(dev_handle, &mut params_ptr) };
    if err != sys::sdrplay_api_ErrT_sdrplay_api_Success || params_ptr.is_null() {
        let msg = format!("GetDeviceParams failed: {err}");
        tracing::error!(error_code = err, "sdrplay_api_GetDeviceParams failed");
        if let Some(s) = shared {
            s.write().device_diagnostics.push_error(&msg);
        }
        return SessionOutcome::Error(anyhow::anyhow!("{msg}"));
    }

    // ── Set up callback context ───────────────────────────────────────────────
    let disconnected = Arc::new(AtomicBool::new(false));
    let overload_ack_needed = Arc::new(AtomicBool::new(false));
    let ctx = Box::new(CallbackContext {
        tx: iq_tx,
        batch: Mutex::new(Vec::with_capacity(BATCH_SIZE * 2)),
        disconnected: Arc::clone(&disconnected),
        overload_ack_needed: Arc::clone(&overload_ack_needed),
    });
    let ctx_ptr = Box::into_raw(ctx) as *mut c_void;

    let mut callbacks = sys::sdrplay_api_CallbackFnsT {
        StreamACbFn: Some(stream_callback_a),
        StreamBCbFn: None,
        EventCbFn: Some(event_callback),
    };

    running.store(true, Ordering::Relaxed);

    // ── Initialize streaming ──────────────────────────────────────────────────
    // Init immediately after GetDeviceParams (no pre-configuration) to match the
    // upstream SDR++ reference.  All parameters are applied via Update() after Init.
    let init_err = unsafe { sys::sdrplay_api_Init(dev_handle, &mut callbacks, ctx_ptr) };
    if init_err != sys::sdrplay_api_ErrT_sdrplay_api_Success {
        let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };
        let msg = format!("sdrplay_api_Init failed: {init_err}");
        tracing::error!(error_code = init_err, "sdrplay_api_Init failed");
        if let Some(s) = shared {
            s.write().device_diagnostics.push_error(&msg);
        }
        return SessionOutcome::Error(anyhow::anyhow!("{msg}"));
    }

    // ── Configure device via Update() after successful Init ───────────────────
    unsafe {
        let params_ref = &mut *params_ptr;
        let ch = &mut *params_ref.rxChannelA;
        let rsp = &mut (*params_ref.devParams).rspDxParams;

        // Sample rate
        (*params_ref.devParams).fsFreq.fsHz = config.sample_rate_sps as f64;
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Dev_Fs,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        tracing::debug!(error_code = e, reason = "Dev_Fs", "sdrplay_api_Update");

        // Frequency
        ch.tunerParams.rfFreq.rfHz = config.frequency_hz as f64;
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Frf,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        tracing::debug!(error_code = e, reason = "Tuner_Frf", "sdrplay_api_Update");

        // IF mode + bandwidth
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
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_IfType,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        tracing::debug!(error_code = e, reason = "Tuner_IfType", "sdrplay_api_Update");
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_BwType,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        tracing::debug!(error_code = e, reason = "Tuner_BwType", "sdrplay_api_Update");

        // AGC / gain
        if config.agc_enabled {
            ch.ctrlParams.agc.enable = sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_CTRL_EN;
            ch.ctrlParams.agc.setPoint_dBfs = config.agc_setpoint_dbfs;
        } else {
            ch.ctrlParams.agc.enable = sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_DISABLE;
            ch.tunerParams.gain.LNAstate = config.lna_state;
            ch.tunerParams.gain.gRdB = (-config.if_gain_dbfs).clamp(0, 59);
        }
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Agc,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        tracing::debug!(error_code = e, reason = "Ctrl_Agc", "sdrplay_api_Update");
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Gr,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
        tracing::debug!(error_code = e, reason = "Tuner_Gr", "sdrplay_api_Update");

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
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_AntennaControl);
        tracing::debug!(error_code = e, reason = "RspDx_AntennaControl", "sdrplay_api_Update");
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_BiasTControl);
        tracing::debug!(error_code = e, reason = "RspDx_BiasTControl", "sdrplay_api_Update");
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfNotchControl);
        tracing::debug!(error_code = e, reason = "RspDx_RfNotchControl", "sdrplay_api_Update");
        let e = sys::sdrplay_api_Update(dev_handle,
            sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
            sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
            sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfDabNotchControl);
        tracing::debug!(error_code = e, reason = "RspDx_RfDabNotchControl", "sdrplay_api_Update");

        // Hardware decimation — reduces the USB/IQ stream rate from the hardware
        // sample rate down to sample_rate / decimation_factor.
        // At decimation_factor=4 and sample_rate=2 MHz the API streams 500 kHz,
        // cutting IQ pipeline load (FFT, FIR, demod) by 4×.
        // wideBandSignal=1 is required for RSPdx wideband (Zero-IF) mode.
        if config.decimation_factor > 1 {
            ch.ctrlParams.decimation.enable = 1;
            ch.ctrlParams.decimation.decimationFactor = config.decimation_factor as u8;
            ch.ctrlParams.decimation.wideBandSignal = 1;
            let e = sys::sdrplay_api_Update(dev_handle,
                sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
                sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Decimation,
                sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None);
            tracing::info!(
                decimation_factor = config.decimation_factor,
                effective_rate_hz = config.sample_rate_sps / config.decimation_factor,
                error_code = e,
                "Hardware decimation enabled"
            );
        }
    }

    // Compute the effective sample rate seen by the signal path after decimation.
    let effective_sample_rate = if config.decimation_factor > 1 {
        config.sample_rate_sps / config.decimation_factor
    } else {
        config.sample_rate_sps
    };

    // ── Publish Running status + update diagnostics ───────────────────────────
    let _ = status_tx.send(DeviceStatus::Running {
        serial: serial.clone(),
        hw_ver,
    });
    if let Some(s) = shared {
        let mut diag = s.write();
        diag.device_diagnostics.serial = serial.clone();
        diag.device_diagnostics.hw_ver = hw_ver;
        diag.device_diagnostics.api_version = api_version;
        diag.device_diagnostics.status = "Running".into();
        // Update the signal path's view of the sample rate to the effective
        // post-decimation rate so FFT bins and demod decimation factors are correct.
        diag.sample_rate_sps = effective_sample_rate;
    }

    tracing::info!(
        freq_hz = config.frequency_hz,
        hw_sample_rate = config.sample_rate_sps,
        decimation_factor = config.decimation_factor,
        effective_sample_rate,
        serial = %serial,
        "RSPdx-R2 streaming started"
    );

    // ── Run until stop / restart / hot-unplug ────────────────────────────────
    let mut last_freq = config.frequency_hz;

    loop {
        // Hot-unplug: event_callback set the disconnected flag.
        if disconnected.load(Ordering::Relaxed) {
            tracing::warn!("hot-unplug detected — exiting session for reconnect");
            let _ = status_tx.send(DeviceStatus::Disconnected);
            if let Some(s) = shared {
                let mut diag = s.write();
                diag.device_diagnostics.status = "Disconnected (hot-unplug)".into();
                diag.device_diagnostics.push_error("Device removed — hot-unplug detected");
            }
            // Stop callbacks before freeing the context (prevents use-after-free).
            dev_guard.uninit();
            let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };
            return SessionOutcome::Error(anyhow::anyhow!("device removed (hot-unplug)"));
        }

        // Stop signal from user or Drop.
        if !running.load(Ordering::Relaxed) {
            break;
        }

        // Acknowledge ADC overload — allows AGC to correct the gain level.
        if overload_ack_needed.swap(false, Ordering::Relaxed) {
            unsafe {
                sys::sdrplay_api_Update(
                    dev_handle,
                    sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
                    sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_OverloadMsgAck,
                    sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None,
                );
            }
            tracing::debug!("Sent OverloadMsgAck — AGC will reduce gain");
        }

        // Poll for frequency changes written by the signal path.
        let new_freq = freq_atomic.load(Ordering::Relaxed);
        if new_freq != last_freq {
            unsafe {
                (*(*params_ptr).rxChannelA).tunerParams.rfFreq.rfHz = new_freq as f64;
                let e = sys::sdrplay_api_Update(
                    dev_handle,
                    sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
                    sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Frf,
                    sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None,
                );
                if e == sys::sdrplay_api_ErrT_sdrplay_api_Success {
                    tracing::info!(
                        freq_hz = new_freq,
                        freq_mhz = new_freq / 1_000_000,
                        "SDRplay frequency updated"
                    );
                } else {
                    tracing::error!(
                        error_code = e,
                        freq_hz = new_freq,
                        "sdrplay_api_Update (Tuner_Frf) failed"
                    );
                    if let Some(s) = shared {
                        s.write().device_diagnostics.push_error(
                            format!("Frequency update failed (err={e}, freq={new_freq})")
                        );
                    }
                }
            }
            last_freq = new_freq;
        }

        // Drain hardware commands (non-blocking).
        while let Ok(cmd) = hw_cmd_rx.try_recv() {
            if let HardwareCommand::RestartDevice = cmd {
                tracing::info!("RestartDevice command received — scheduling clean restart");
                restart_requested.store(true, Ordering::Relaxed);
                running.store(false, Ordering::Relaxed);
                break;
            }

            unsafe {
                let ch = &mut *(*params_ptr).rxChannelA;
                let rsp = &mut (*(*params_ptr).devParams).rspDxParams;
                let (cmd_name, reason, reason_ext) = match cmd {
                    HardwareCommand::SetLnaState(n) => {
                        ch.tunerParams.gain.LNAstate = n;
                        ("SetLnaState",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Gr,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetIfGain(g) => {
                        // gRdB is gain *reduction* in dB: 0=max gain, 59=max attenuation.
                        // if_gain_dbfs is signed (0 = no attenuation).
                        ch.tunerParams.gain.gRdB = (-g).clamp(0, 59);
                        ("SetIfGain",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Tuner_Gr,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetAgcEnabled(en) => {
                        ch.ctrlParams.agc.enable = if en {
                            sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_CTRL_EN
                        } else {
                            sys::sdrplay_api_AgcControlT_sdrplay_api_AGC_DISABLE
                        };
                        ("SetAgcEnabled",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Agc,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetAgcSetpoint(sp) => {
                        ch.ctrlParams.agc.setPoint_dBfs = sp;
                        ("SetAgcSetpoint",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Agc,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::SetBiasT(en) => {
                        rsp.biasTEnable = en as u8;
                        ("SetBiasT",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_BiasTControl)
                    }
                    HardwareCommand::SetHdrMode(en) => {
                        if en && last_freq > 2_000_000 {
                            tracing::warn!(
                                freq_hz = last_freq,
                                "HDR mode rejected — only valid below 2 MHz on RSPdx-R2"
                            );
                            continue;
                        }
                        rsp.hdrEnable = en as u8;
                        ("SetHdrMode",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_HdrEnable)
                    }
                    HardwareCommand::SetAmNotch(en) => {
                        rsp.rfNotchEnable = en as u8;
                        ("SetAmNotch",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfNotchControl)
                    }
                    HardwareCommand::SetFmNotch(en) => {
                        rsp.rfDabNotchEnable = en as u8;
                        ("SetFmNotch",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_RfDabNotchControl)
                    }
                    HardwareCommand::SetAntenna(port) => {
                        rsp.antennaSel = match port {
                            1 => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_B,
                            2 => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_C,
                            _ => sys::sdrplay_api_RspDx_AntennaSelectT_sdrplay_api_RspDx_ANTENNA_A,
                        };
                        ("SetAntenna",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_None,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_RspDx_AntennaControl)
                    }
                    HardwareCommand::SetDecimationFactor(n) => {
                        // Live decimation change — no device restart required.
                        // sdrplay_api_Update_Ctrl_Decimation applies the new value
                        // to the running stream immediately.
                        let n = n.clamp(1, 32);
                        ch.ctrlParams.decimation.enable = if n > 1 { 1 } else { 0 };
                        ch.ctrlParams.decimation.decimationFactor = n as u8;
                        ch.ctrlParams.decimation.wideBandSignal = 1;
                        // Update the effective sample rate visible to the signal path.
                        let effective = config.sample_rate_sps / n;
                        if let Some(s) = shared {
                            s.write().sample_rate_sps = effective;
                        }
                        tracing::info!(
                            decimation_factor = n,
                            effective_rate_hz = effective,
                            "SetDecimationFactor applied live"
                        );
                        ("SetDecimationFactor",
                         sys::sdrplay_api_ReasonForUpdateT_sdrplay_api_Update_Ctrl_Decimation,
                         sys::sdrplay_api_ReasonForUpdateExtension1T_sdrplay_api_Update_Ext1_None)
                    }
                    HardwareCommand::RestartDevice => unreachable!(),
                };
                let e = sys::sdrplay_api_Update(
                    dev_handle,
                    sys::sdrplay_api_TunerSelectT_sdrplay_api_Tuner_A,
                    reason,
                    reason_ext,
                );
                if e != sys::sdrplay_api_ErrT_sdrplay_api_Success {
                    tracing::error!(
                        cmd = cmd_name,
                        error_code = e,
                        "sdrplay_api_Update failed"
                    );
                    if let Some(s) = shared {
                        s.write().device_diagnostics.push_error(
                            format!("{cmd_name} Update failed (err={e})")
                        );
                    }
                } else {
                    tracing::debug!(cmd = cmd_name, "sdrplay_api_Update ok");
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Stop SDRplay callbacks BEFORE freeing the callback context.
    // The stream callback thread fires asynchronously; Uninit() blocks until
    // it is fully stopped. Only then is it safe to drop the context memory.
    dev_guard.uninit();
    let _ = unsafe { Box::from_raw(ctx_ptr as *mut CallbackContext) };
    tracing::info!("RSPdx-R2 streaming stopped");

    if restart_requested.load(Ordering::Relaxed) {
        SessionOutcome::Restart
    } else {
        SessionOutcome::Stopped
    }
}

/// Outer loop: runs sessions, handles restart/reconnect, sends status updates.
///
/// - On `SessionOutcome::Stopped`: user requested stop — exit.
/// - On `SessionOutcome::Restart`: clean restart requested — retry immediately.
/// - On `SessionOutcome::Error`: hardware failure — retry with exponential backoff
///   (2 s, 4 s, 8 s, … capped at 30 s) until `running` is cleared.
fn run_sdrplay_thread(
    config: RspdxConfig,
    iq_tx: crossbeam_channel::Sender<Arc<[IqSample]>>,
    running: Arc<AtomicBool>,
    freq_atomic: Arc<AtomicU64>,
    hw_cmd_rx: crossbeam_channel::Receiver<HardwareCommand>,
    status_tx: tokio::sync::watch::Sender<DeviceStatus>,
    shared: Option<Arc<RwLock<SharedState>>>,
) {
    let restart_requested = Arc::new(AtomicBool::new(false));
    let mut error_attempt: u32 = 0;

    loop {
        restart_requested.store(false, Ordering::Relaxed);
        let _ = status_tx.send(DeviceStatus::Connecting);
        if let Some(s) = &shared {
            s.write().device_diagnostics.status = if error_attempt == 0 {
                "Connecting".into()
            } else {
                format!("Reconnecting (attempt {error_attempt})")
            };
        }

        let outcome = try_run_sdrplay_session(
            &config,
            iq_tx.clone(),
            Arc::clone(&running),
            Arc::clone(&freq_atomic),
            hw_cmd_rx.clone(),
            &status_tx,
            shared.as_ref(),
            &restart_requested,
        );

        match outcome {
            SessionOutcome::Stopped => {
                tracing::info!("SDRplay session stopped cleanly");
                let _ = status_tx.send(DeviceStatus::Disconnected);
                if let Some(s) = &shared {
                    s.write().device_diagnostics.status = "Stopped".into();
                }
                break;
            }
            SessionOutcome::Restart => {
                tracing::info!("SDRplay session restarting immediately (restart requested)");
                error_attempt = 0;
                // Reset running so the new session can set it to true.
                running.store(true, Ordering::Relaxed);
                // Brief pause to let the API service release resources.
                std::thread::sleep(std::time::Duration::from_millis(500));
                continue;
            }
            SessionOutcome::Error(e) => {
                // If `running` was cleared by the user while in error recovery, stop.
                if !running.load(Ordering::Relaxed) {
                    tracing::info!("SDRplay session ended (stop requested during error)");
                    let _ = status_tx.send(DeviceStatus::Disconnected);
                    break;
                }

                error_attempt += 1;
                // Exponential backoff: 2, 4, 8, … capped at 30 seconds.
                let delay_secs = (2u64 << error_attempt.saturating_sub(1).min(4)).min(30);
                tracing::warn!(
                    attempt = error_attempt,
                    delay_secs,
                    error = %e,
                    "SDRplay session failed — retrying"
                );
                let reason = e.to_string();
                let _ = status_tx.send(DeviceStatus::Reconnecting {
                    attempt: error_attempt,
                    reason: reason.clone(),
                });
                if let Some(s) = &shared {
                    let mut diag = s.write();
                    diag.device_diagnostics.status =
                        format!("Reconnecting (attempt {error_attempt})");
                    diag.device_diagnostics.push_error(format!(
                        "Session failed (attempt {error_attempt}): {reason}"
                    ));
                }
                std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            }
        }
    }
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
    // SAFETY: cb_context is a Box<CallbackContext> kept alive by try_run_sdrplay_session.
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

/// SDRplay event callback — handles device lifecycle and gain events.
///
/// Event types from the SDRplay API spec:
/// - 0: GainChange — AGC adjusted gain; expected and informational.
/// - 1: PowerOverloadChange — ADC input overload; log a warning.
/// - 2: DeviceRemoved — hot-unplug; set `disconnected` flag for main loop.
/// - 3: RspDuoModeChange — dual-tuner mode change (unused on RSPdx-R2).
/// - 4: DeviceFailure — internal API failure; treat as disconnect.
extern "C" fn event_callback(
    event_id: sys::sdrplay_api_EventT,
    tuner: sys::sdrplay_api_TunerSelectT,
    _params: *mut sys::sdrplay_api_EventParamsT,
    cb_context: *mut c_void,
) {
    // Guard against Rust panics crossing the C FFI boundary.
    // A panic propagating into libsdrplay_api would be undefined behaviour
    // and would cause SIGABRT (the runtime catches it at the extern "C" boundary
    // and calls abort() rather than letting C++ frames unwind).
    let result = std::panic::catch_unwind(|| {
        // SAFETY: cb_context is a Box<CallbackContext> kept alive by try_run_sdrplay_session.
        let ctx = unsafe { &*(cb_context as *const CallbackContext) };

        match event_id {
            // GainChange (0): AGC adjusted — routine, debug level only.
            sys::sdrplay_api_EventT_sdrplay_api_GainChange => {
                tracing::debug!(tuner, "SDRplay GainChange event (AGC)");
            }
            // PowerOverloadChange (1): ADC input overload — flag for OverloadMsgAck.
            // The main loop will call sdrplay_api_Update(OverloadMsgAck) which lets
            // the AGC subsystem reduce gain and correct the overload condition.
            sys::sdrplay_api_EventT_sdrplay_api_PowerOverloadChange => {
                tracing::warn!(tuner, "SDRplay ADC power overload — flagging for AGC correction");
                ctx.overload_ack_needed.store(true, Ordering::Relaxed);
            }
            // DeviceRemoved (2): physical hot-unplug — signal the main loop.
            sys::sdrplay_api_EventT_sdrplay_api_DeviceRemoved => {
                tracing::warn!("SDRplay DeviceRemoved event — hot-unplug detected");
                ctx.disconnected.store(true, Ordering::Relaxed);
            }
            // DeviceFailure (4): internal API failure — treat as removal.
            sys::sdrplay_api_EventT_sdrplay_api_DeviceFailure => {
                tracing::error!("SDRplay DeviceFailure event — treating as disconnect");
                ctx.disconnected.store(true, Ordering::Relaxed);
            }
            _ => {
                tracing::debug!(event = event_id, tuner, "SDRplay event");
            }
        }
    });

    if result.is_err() {
        tracing::error!("panic in SDRplay event_callback — suppressed to avoid FFI abort");
    }
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

    #[test]
    fn status_channel_starts_connecting() {
        let src = RspdxSource::new(RspdxConfig::default());
        let rx = src.status_rx();
        assert_eq!(*rx.borrow(), DeviceStatus::Connecting);
    }

    #[test]
    fn device_diagnostics_error_log_caps_at_20() {
        use sdrapp_core::signal_path::DeviceDiagnostics;
        let mut diag = DeviceDiagnostics::default();
        for i in 0..25u32 {
            diag.push_error(format!("error {i}"));
        }
        assert_eq!(diag.error_count, 25);
        assert_eq!(diag.error_log.len(), 20, "ring buffer must cap at 20");
        // Oldest entries should have been dropped; newest should be last.
        assert!(diag.error_log.back().unwrap().message.contains("24"));
    }

    #[test]
    fn restart_device_command_exists() {
        // Ensure the variant compiles and matches.
        let cmd = HardwareCommand::RestartDevice;
        assert!(matches!(cmd, HardwareCommand::RestartDevice));
    }
}
