#![forbid(unsafe_code)]

//! MIDI controller: wires a CoreMIDI input port (via midir) to application actions.
//!
//! Architecture:
//!   CoreMIDI callback thread (OS-managed)
//!       │  mpsc::UnboundedSender<Vec<u8>>
//!       ▼
//!   tokio dispatcher task
//!       │  writes to SharedState (page, device name)
//!       │  sends to SignalPathCommand channel (freq, volume, start/stop)
//!       │  sends to RecorderCommand channel (record start/stop)
//!       ▼
//!   Signal path / Recorder

use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use sdrapp_core::signal_path::{BookmarkCmd, DisplayCmd, ReceiverCmd, SharedState, SignalPathCommand};
use sdrapp_recorder::RecorderCommand;

use crate::action::MidiAction;
use crate::config::{MidiActionTag, MidiConfig, MidiKey, MidiKeyKind};

/// Parses a raw MIDI byte slice into a (MidiKey, value) pair.
fn parse_midi(data: &[u8]) -> Option<(MidiKey, u8)> {
    match data {
        [status, number, value] => {
            let channel = status & 0x0F;
            match status >> 4 {
                0x9 => Some((
                    MidiKey {
                        channel,
                        kind: MidiKeyKind::NoteOn,
                        number: *number,
                    },
                    *value,
                )),
                0xB => Some((
                    MidiKey {
                        channel,
                        kind: MidiKeyKind::ControlChange,
                        number: *number,
                    },
                    *value,
                )),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Dispatches MidiAction to application sinks (recorder, signal path, etc.)
pub struct MidiController {
    pub config: MidiConfig,
    pub recorder_cmd_tx: mpsc::Sender<RecorderCommand>,
}

impl MidiController {
    pub fn new(config: MidiConfig, recorder_cmd_tx: mpsc::Sender<RecorderCommand>) -> Self {
        Self {
            config,
            recorder_cmd_tx,
        }
    }

    /// Start listening on the configured MIDI port.
    ///
    /// Opens the midir input port by name, forwards callbacks to an unbounded channel,
    /// then dispatches actions from the tokio task.
    ///
    /// Returns:
    /// - A JoinHandle for the dispatcher task.
    /// - An UnboundedSender for injecting messages (tests / future UI learn mode).
    pub fn start(
        &self,
        shared: Arc<RwLock<SharedState>>,
        signal_cmd_tx: crossbeam_channel::Sender<SignalPathCommand>,
    ) -> (JoinHandle<()>, mpsc::UnboundedSender<Vec<u8>>) {
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let config = self.config.clone();
        let recorder_tx = self.recorder_cmd_tx.clone();

        // Try to open the physical MIDI port on a std::thread (midir is sync).
        // Forward callbacks to msg_tx so the tokio task can process them.
        let inject_tx = msg_tx.clone();
        let port_name = config.port_name.clone().unwrap_or_default();
        let shared_for_midi_thread = Arc::clone(&shared);

        std::thread::Builder::new()
            .name("sdrapp-midi".into())
            .spawn(move || {
                open_midi_port(&port_name, inject_tx, shared_for_midi_thread);
            })
            .expect("failed to spawn MIDI thread");

        let handle = tokio::spawn(async move {
            let mut current_page = config.current_page;

            while let Some(raw) = msg_rx.recv().await {
                if let Some((key, value)) = parse_midi(&raw) {
                    let action = resolve_action(&config, current_page, &key, value);

                    match &action {
                        MidiAction::PageNext => {
                            current_page = (current_page + 1) % config.page_count;
                            shared.write().midi_page = current_page;
                            tracing::debug!(page = current_page, "MIDI page advanced");
                        }

                        // ── Tuning ────────────────────────────────────────────
                        MidiAction::TuneCoarseUp => {
                            let freq = shared.read().center_freq_hz.saturating_add(1_000_000);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneCoarseDown => {
                            let freq = shared.read().center_freq_hz.saturating_sub(1_000_000).max(1);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneMediumUp => {
                            let freq = shared.read().center_freq_hz.saturating_add(100_000);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneMediumDown => {
                            let freq = shared.read().center_freq_hz.saturating_sub(100_000).max(1);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneFineUp => {
                            let freq = shared.read().center_freq_hz.saturating_add(10_000);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneFineDown => {
                            let freq = shared.read().center_freq_hz.saturating_sub(10_000).max(1);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneUltraFineUp => {
                            let freq = shared.read().center_freq_hz.saturating_add(1_000);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }
                        MidiAction::TuneUltraFineDown => {
                            let freq = shared.read().center_freq_hz.saturating_sub(1_000).max(1);
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                        }

                        // ── Demod & step size cycling ─────────────────────────
                        MidiAction::DemodModeCycle => {
                            use sdrapp_core::signal_path::DemodMode;
                            let next = match shared.read().demod.demod_mode {
                                DemodMode::Wbfm => DemodMode::Nfm,
                                DemodMode::Nfm => DemodMode::Am,
                                DemodMode::Am  => DemodMode::Usb,
                                DemodMode::Usb => DemodMode::Lsb,
                                DemodMode::Lsb => DemodMode::Dsb,
                                DemodMode::Dsb => DemodMode::Cw,
                                DemodMode::Cw  => DemodMode::Wbfm,
                            };
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetDemodMode(next).into());
                        }
                        MidiAction::StepSizeCycle => {
                            let next_step = match shared.read().demod.tune_step_hz {
                                100 => 1_000,
                                1_000 => 10_000,
                                10_000 => 100_000,
                                _ => 100,
                            };
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetTuneStep(next_step).into());
                        }

                        // ── Volume & squelch (absolute from fader 0–127) ──────
                        MidiAction::VolumeSet(_) => {
                            let linear = value as f32 / 127.0;
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetVolume(linear).into());
                        }
                        MidiAction::SquelchSet(_) => {
                            // 0–127 → -80.0 dBFS to 0.0 dBFS
                            let dbfs = -80.0 + (value as f32 / 127.0) * 80.0;
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetSquelchThreshold(dbfs).into());
                        }

                        // ── Display (zoom & waterfall) ────────────────────────
                        MidiAction::ZoomIn => {
                            let z = (shared.read().zoom_level / 1.5).max(0.005);
                            let _ = signal_cmd_tx.try_send(DisplayCmd::SetZoom(z).into());
                        }
                        MidiAction::ZoomOut => {
                            let z = (shared.read().zoom_level * 1.5).min(1.0);
                            let _ = signal_cmd_tx.try_send(DisplayCmd::SetZoom(z).into());
                        }
                        MidiAction::ZoomSet(_) => {
                            // 0–127 → 0.05 (zoomed in) to 1.0 (full bandwidth)
                            let z = 0.05 + (value as f32 / 127.0) * 0.95;
                            let _ = signal_cmd_tx.try_send(DisplayCmd::SetZoom(z).into());
                        }
                        MidiAction::WaterfallSpeedUp => {
                            let spd = (shared.read().waterfall_speed + 0.5).min(10.0);
                            let _ = signal_cmd_tx.try_send(DisplayCmd::SetWaterfallSpeed(spd).into());
                        }
                        MidiAction::WaterfallSpeedDown => {
                            let spd = (shared.read().waterfall_speed - 0.5).max(0.1);
                            let _ = signal_cmd_tx.try_send(DisplayCmd::SetWaterfallSpeed(spd).into());
                        }
                        MidiAction::WaterfallSpeedSet(_) => {
                            // 0–127 → 0.1 to 5.0
                            let s = 0.1 + (value as f32 / 127.0) * 4.9;
                            let _ = signal_cmd_tx.try_send(DisplayCmd::SetWaterfallSpeed(s).into());
                        }

                        // ── Bookmarks ─────────────────────────────────────────
                        MidiAction::BookmarkNext => {
                            let (freq, mode) = {
                                let mut s = shared.write();
                                let len = s.bookmarks.len();
                                if len > 0 {
                                    s.bookmark_cursor = (s.bookmark_cursor + 1) % len;
                                    let bm = &s.bookmarks[s.bookmark_cursor];
                                    (bm.freq_hz, bm.mode)
                                } else {
                                    continue;
                                }
                            };
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetDemodMode(mode).into());
                        }
                        MidiAction::BookmarkPrev => {
                            let (freq, mode) = {
                                let mut s = shared.write();
                                let len = s.bookmarks.len();
                                if len > 0 {
                                    s.bookmark_cursor = s.bookmark_cursor.checked_sub(1).unwrap_or(len - 1);
                                    let bm = &s.bookmarks[s.bookmark_cursor];
                                    (bm.freq_hz, bm.mode)
                                } else {
                                    continue;
                                }
                            };
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetFrequency(freq).into());
                            let _ = signal_cmd_tx.try_send(ReceiverCmd::SetDemodMode(mode).into());
                        }
                        MidiAction::BookmarkSave => {
                            let freq_label = {
                                let s = shared.read();
                                format!("{:.3} MHz", s.center_freq_hz as f64 / 1_000_000.0)
                            };
                            let _ = signal_cmd_tx.try_send(BookmarkCmd::Add(freq_label).into());
                        }

                        // ── Recording ─────────────────────────────────────────
                        MidiAction::RecordStart => {
                            let (freq_hz, iq_sr, mode) = {
                                let s = shared.read();
                                (s.center_freq_hz, s.sample_rate_sps, s.recording_mode)
                            };
                            let _ = recorder_tx.send(RecorderCommand::Start {
                                freq_hz, iq_sample_rate: iq_sr, mode,
                            }).await;
                            let _ = signal_cmd_tx.try_send(SignalPathCommand::StartRecording);
                        }
                        MidiAction::RecordStop => {
                            let _ = recorder_tx.send(RecorderCommand::Stop).await;
                            let _ = signal_cmd_tx.try_send(SignalPathCommand::StopRecording);
                        }
                        MidiAction::RecordingToggle => {
                            let is_recording = shared.read().is_recording;
                            if is_recording {
                                let _ = recorder_tx.send(RecorderCommand::Stop).await;
                                let _ = signal_cmd_tx.try_send(SignalPathCommand::StopRecording);
                            } else {
                                let (freq_hz, iq_sr, mode) = {
                                    let s = shared.read();
                                    (s.center_freq_hz, s.sample_rate_sps, s.recording_mode)
                                };
                                let _ = recorder_tx.send(RecorderCommand::Start {
                                    freq_hz, iq_sample_rate: iq_sr, mode,
                                }).await;
                                let _ = signal_cmd_tx.try_send(SignalPathCommand::StartRecording);
                            }
                        }

                        // ── Transport / system ────────────────────────────────
                        MidiAction::PlayToggle => {
                            // Future: toggle signal path start/stop
                        }
                        MidiAction::Stop => {
                            let _ = signal_cmd_tx.try_send(SignalPathCommand::Stop);
                        }
                        MidiAction::HelpPanelToggle => {
                            let mut s = shared.write();
                            s.help_panel_open = !s.help_panel_open;
                        }

                        MidiAction::Unmapped => {
                            tracing::trace!(?key, value, "unmapped MIDI input");
                        }
                    }
                }
            }
        });

        (handle, msg_tx)
    }
}

/// Open the named midir input port and forward messages to `tx`.
/// If the exact name isn't found, tries a case-insensitive prefix match,
/// then falls back to the first available port with a warning.
/// On successful connection, writes the port name to `shared.midi_device`.
fn open_midi_port(port_name: &str, tx: mpsc::UnboundedSender<Vec<u8>>, shared: Arc<RwLock<SharedState>>) {
    use midir::MidiInput;

    let midi_in = match MidiInput::new("sdrapp") {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("midir init error: {e}");
            return;
        }
    };

    let ports = midi_in.ports();
    if ports.is_empty() {
        tracing::warn!("no MIDI input ports available");
        return;
    }

    // Find the best matching port
    let port = if port_name.is_empty() {
        // No preference — use first available
        Some(&ports[0])
    } else {
        // Exact match first
        ports
            .iter()
            .find(|p| midi_in.port_name(p).ok().as_deref() == Some(port_name))
            // Then prefix match (nanoKontrol2 enumerates with a suffix like " 0")
            .or_else(|| {
                let lower = port_name.to_lowercase();
                ports.iter().find(|p| {
                    midi_in
                        .port_name(p)
                        .ok()
                        .map(|n| n.to_lowercase().contains(&lower))
                        .unwrap_or(false)
                })
            })
            // Fall back to first
            .or(Some(&ports[0]))
    };

    let port = match port {
        Some(p) => p,
        None => return,
    };

    let name = midi_in.port_name(port).unwrap_or_default();
    tracing::info!(port = %name, "MIDI port opened");

    // The connection must live for as long as we want to receive.
    // Blocking the thread keeps it alive.
    let _conn = midi_in.connect(
        port,
        "sdrapp-midi-in",
        move |_timestamp_us, data, _| {
            let _ = tx.send(data.to_vec());
        },
        (),
    );

    match _conn {
        Ok(_conn) => {
            tracing::info!("MIDI connection established");
            // Update shared state so the UI can show the connected device name.
            shared.write().midi_device = Some(name.clone());
            // Park the thread — connection stays alive until the process exits
            // or the tx is dropped (channel closed).
            loop {
                std::thread::park();
            }
        }
        Err(e) => {
            tracing::error!("failed to connect to MIDI port '{name}': {e}");
        }
    }
}

fn resolve_action(config: &MidiConfig, page: usize, key: &MidiKey, _value: u8) -> MidiAction {
    config
        .lookup(page, key)
        .map(tag_to_action)
        .unwrap_or(MidiAction::Unmapped)
}

fn tag_to_action(tag: &MidiActionTag) -> MidiAction {
    match tag {
        MidiActionTag::TuneCoarseUp => MidiAction::TuneCoarseUp,
        MidiActionTag::TuneCoarseDown => MidiAction::TuneCoarseDown,
        MidiActionTag::TuneMediumUp => MidiAction::TuneMediumUp,
        MidiActionTag::TuneMediumDown => MidiAction::TuneMediumDown,
        MidiActionTag::TuneFineUp => MidiAction::TuneFineUp,
        MidiActionTag::TuneFineDown => MidiAction::TuneFineDown,
        MidiActionTag::TuneUltraFineUp => MidiAction::TuneUltraFineUp,
        MidiActionTag::TuneUltraFineDown => MidiAction::TuneUltraFineDown,
        MidiActionTag::DemodModeCycle => MidiAction::DemodModeCycle,
        MidiActionTag::StepSizeCycle => MidiAction::StepSizeCycle,
        MidiActionTag::VolumeSet => MidiAction::VolumeSet(0.0),
        MidiActionTag::SquelchSet => MidiAction::SquelchSet(0.0),
        MidiActionTag::ZoomIn => MidiAction::ZoomIn,
        MidiActionTag::ZoomOut => MidiAction::ZoomOut,
        MidiActionTag::ZoomSet => MidiAction::ZoomSet(0.0),
        MidiActionTag::WaterfallSpeedUp => MidiAction::WaterfallSpeedUp,
        MidiActionTag::WaterfallSpeedDown => MidiAction::WaterfallSpeedDown,
        MidiActionTag::WaterfallSpeedSet => MidiAction::WaterfallSpeedSet(0.0),
        MidiActionTag::BookmarkNext => MidiAction::BookmarkNext,
        MidiActionTag::BookmarkPrev => MidiAction::BookmarkPrev,
        MidiActionTag::BookmarkSave => MidiAction::BookmarkSave,
        MidiActionTag::RecordStart => MidiAction::RecordStart,
        MidiActionTag::RecordStop => MidiAction::RecordStop,
        MidiActionTag::RecordingToggle => MidiAction::RecordingToggle,
        MidiActionTag::PlayToggle => MidiAction::PlayToggle,
        MidiActionTag::Stop => MidiAction::Stop,
        MidiActionTag::HelpPanelToggle => MidiAction::HelpPanelToggle,
        MidiActionTag::PageNext => MidiAction::PageNext,
        MidiActionTag::Unmapped => MidiAction::Unmapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::RwLock;
    use sdrapp_core::signal_path::SharedState;
    use std::sync::Arc;

    fn make_shared() -> Arc<RwLock<SharedState>> {
        let mut s = SharedState::new();
        s.center_freq_hz = 100_000_000;
        Arc::new(RwLock::new(s))
    }

    #[tokio::test]
    async fn page_cycles_through_all_pages() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let (rec_tx, _rec_rx) = mpsc::channel(8);
        let (cmd_tx, _cmd_rx) = crossbeam_channel::bounded(64);
        let shared = make_shared();
        let ctrl = MidiController::new(config, rec_tx);
        let (_handle, msg_tx) = ctrl.start(Arc::clone(&shared), cmd_tx);

        // CYCLE button: NoteOn ch=0 note=46 vel=127
        let cycle = vec![0x90, 46, 127];
        for _ in 0..3 {
            msg_tx.send(cycle.clone()).unwrap();
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;
        // Page wraps back to 0 after 3 presses (page_count=3)
        assert_eq!(shared.read().midi_page, 0);
    }

    #[tokio::test]
    async fn record_start_message_forwarded_to_recorder() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let (rec_tx, mut rec_rx) = mpsc::channel(8);
        let (cmd_tx, _cmd_rx) = crossbeam_channel::bounded(64);
        let shared = make_shared();
        let ctrl = MidiController::new(config, rec_tx);
        let (_handle, msg_tx) = ctrl.start(Arc::clone(&shared), cmd_tx);

        // Cycle to page 2 first (CYCLE = note 46)
        msg_tx.send(vec![0x90, 46, 127]).unwrap(); // page 1
        msg_tx.send(vec![0x90, 46, 127]).unwrap(); // page 2
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        // REC button on page 2: NoteOn ch=0 note=45 vel=127
        msg_tx.send(vec![0x90, 45, 127]).unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let cmd = rec_rx.try_recv();
        assert!(matches!(cmd, Ok(RecorderCommand::Start { .. })));
    }

    #[test]
    fn unmapped_key_returns_unmapped_action() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let key = MidiKey {
            channel: 0,
            kind: MidiKeyKind::ControlChange,
            number: 99,
        };
        let action = resolve_action(&config, 0, &key, 0);
        assert_eq!(action, MidiAction::Unmapped);
    }

    #[tokio::test]
    async fn tune_coarse_up_sends_frequency_command() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let (rec_tx, _) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded(64);
        let shared = make_shared();
        let ctrl = MidiController::new(config, rec_tx);
        let (_handle, msg_tx) = ctrl.start(Arc::clone(&shared), cmd_tx);

        // TuneCoarseUp is on page 0, S button 0 = Note 32
        msg_tx.send(vec![0x90, 32, 127]).unwrap(); // NoteOn ch=0 note=32 vel=127
        tokio::time::sleep(tokio::time::Duration::from_millis(30)).await;

        let cmd = cmd_rx.try_recv();
        assert!(matches!(cmd, Ok(SignalPathCommand::Receiver(ReceiverCmd::SetFrequency(_)))));
    }
}
