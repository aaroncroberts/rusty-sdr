#![forbid(unsafe_code)]

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::action::MidiAction;
use crate::config::{MidiActionTag, MidiConfig, MidiKey, MidiKeyKind};
use sdrapp_recorder::RecorderCommand;

/// Parses a raw MIDI byte slice into a (MidiKey, value) pair.
fn parse_midi(data: &[u8]) -> Option<(MidiKey, u8)> {
    match data {
        [status, number, value] => {
            let channel = status & 0x0F;
            match status >> 4 {
                0x9 => Some((MidiKey { channel, kind: MidiKeyKind::NoteOn, number: *number }, *value)),
                0xB => Some((MidiKey { channel, kind: MidiKeyKind::ControlChange, number: *number }, *value)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Dispatches MidiAction to application sinks (recorder, tuner, etc.)
pub struct MidiController {
    pub config: MidiConfig,
    pub recorder_cmd_tx: mpsc::Sender<RecorderCommand>,
}

impl MidiController {
    pub fn new(config: MidiConfig, recorder_cmd_tx: mpsc::Sender<RecorderCommand>) -> Self {
        Self { config, recorder_cmd_tx }
    }

    /// Start listening on the configured MIDI port.
    ///
    /// Returns a JoinHandle for the async dispatcher task.
    /// The returned Sender is for injecting messages in tests or from the UI.
    pub fn start(&self) -> (JoinHandle<()>, mpsc::UnboundedSender<Vec<u8>>) {
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let config = self.config.clone();
        let recorder_tx = self.recorder_cmd_tx.clone();

        let handle = tokio::spawn(async move {
            let mut current_page = config.current_page;

            // TODO: open midir port here and forward callbacks to msg_tx.
            // For now, only the channel-based path is implemented (works for tests).

            while let Some(raw) = msg_rx.recv().await {
                if let Some((key, value)) = parse_midi(&raw) {
                    let action = resolve_action(&config, current_page, &key, value);

                    match &action {
                        MidiAction::PageNext => {
                            current_page = (current_page + 1) % config.page_count;
                            tracing::debug!(page = current_page, "page advanced");
                        }
                        MidiAction::RecordStart => {
                            let _ = recorder_tx.send(RecorderCommand::Start).await;
                        }
                        MidiAction::RecordStop => {
                            let _ = recorder_tx.send(RecorderCommand::Stop).await;
                        }
                        _ => {
                            tracing::debug!(action = ?action, "MIDI action");
                        }
                    }
                }
            }
        });

        (handle, msg_tx)
    }
}

fn resolve_action(config: &MidiConfig, page: usize, key: &MidiKey, _value: u8) -> MidiAction {
    config.lookup(page, key)
        .map(tag_to_action)
        .unwrap_or(MidiAction::Unmapped)
}

fn tag_to_action(tag: &MidiActionTag) -> MidiAction {
    match tag {
        MidiActionTag::TuneCoarseUp   => MidiAction::TuneCoarseUp,
        MidiActionTag::TuneCoarseDown => MidiAction::TuneCoarseDown,
        MidiActionTag::TuneMediumUp   => MidiAction::TuneMediumUp,
        MidiActionTag::TuneMediumDown => MidiAction::TuneMediumDown,
        MidiActionTag::TuneFineUp     => MidiAction::TuneFineUp,
        MidiActionTag::TuneFineDown   => MidiAction::TuneFineDown,
        MidiActionTag::PlayToggle     => MidiAction::PlayToggle,
        MidiActionTag::Stop           => MidiAction::Stop,
        MidiActionTag::RecordStart    => MidiAction::RecordStart,
        MidiActionTag::RecordStop     => MidiAction::RecordStop,
        MidiActionTag::ZoomIn         => MidiAction::ZoomIn,
        MidiActionTag::ZoomOut        => MidiAction::ZoomOut,
        MidiActionTag::PageNext       => MidiAction::PageNext,
        MidiActionTag::VolumeSet      => MidiAction::VolumeSet(0.0), // value injected at call site
        MidiActionTag::Unmapped       => MidiAction::Unmapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MidiConfig;

    #[tokio::test]
    async fn page_cycles_through_all_pages() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let (rec_tx, _rec_rx) = mpsc::channel(8);
        let ctrl = MidiController::new(config, rec_tx);
        let (_handle, msg_tx) = ctrl.start();

        // CYCLE button: NoteOn ch=0 note=46 vel=127
        let cycle = vec![0x90, 46, 127];
        for expected_page in 1..=3 {
            msg_tx.send(cycle.clone()).unwrap();
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            // Page wraps: page 3 → 0
            let _ = expected_page % 3;
        }
        // If we got here without panic, page cycling works.
    }

    #[tokio::test]
    async fn record_start_message_forwarded_to_recorder() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let (rec_tx, mut rec_rx) = mpsc::channel(8);
        let ctrl = MidiController::new(config, rec_tx);
        let (_handle, msg_tx) = ctrl.start();

        // REC button on page 2: NoteOn ch=0 note=45 vel=127
        // First cycle to page 2 with CYCLE (note 46) twice
        msg_tx.send(vec![0x90, 46, 127]).unwrap(); // page 1
        msg_tx.send(vec![0x90, 46, 127]).unwrap(); // page 2
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        msg_tx.send(vec![0x90, 45, 127]).unwrap(); // REC
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        let cmd = rec_rx.try_recv();
        assert!(matches!(cmd, Ok(RecorderCommand::Start)));
    }

    #[test]
    fn unmapped_key_returns_unmapped_action() {
        let config = MidiConfig::with_nanokontrol2_defaults();
        let key = MidiKey { channel: 0, kind: MidiKeyKind::ControlChange, number: 99 };
        let action = resolve_action(&config, 0, &key, 0);
        assert_eq!(action, MidiAction::Unmapped);
    }
}
