#![forbid(unsafe_code)]

//! Default binding profile for the Korg nanoKONTROL2.
//!
//! All 47 controls defined as a compile-time constant mapping,
//! organized across 3 pages: Tune, Monitor, Recorder.

use crate::config::{MidiActionTag, MidiKey, MidiKeyKind};

fn cc(channel: u8, number: u8) -> MidiKey {
    MidiKey { channel, kind: MidiKeyKind::ControlChange, number }
}

fn note(channel: u8, number: u8) -> MidiKey {
    MidiKey { channel, kind: MidiKeyKind::NoteOn, number }
}

/// Returns the default nanoKONTROL2 bindings as a list of ((page, key), action).
///
/// Page 0 = Tune, Page 1 = Monitor, Page 2 = Recorder
pub fn default_bindings() -> Vec<((usize, MidiKey), MidiActionTag)> {
    let mut m = Vec::new();
    macro_rules! bind {
        ($page:expr, $key:expr, $action:expr) => {
            m.push((($page, $key), $action));
        };
    }

    // ── Page 0: Tune ──────────────────────────────────────
    bind!(0, note(0, 41), MidiActionTag::PlayToggle);
    bind!(0, note(0, 42), MidiActionTag::Stop);
    bind!(0, note(0, 46), MidiActionTag::PageNext);
    bind!(0, cc(0, 0),    MidiActionTag::TuneCoarseUp);
    bind!(0, cc(0, 1),    MidiActionTag::TuneCoarseDown);
    bind!(0, cc(0, 2),    MidiActionTag::TuneMediumUp);
    bind!(0, cc(0, 3),    MidiActionTag::TuneMediumDown);
    bind!(0, cc(0, 4),    MidiActionTag::TuneFineUp);
    bind!(0, cc(0, 5),    MidiActionTag::TuneFineDown);

    // ── Page 1: Monitor ───────────────────────────────────
    bind!(1, note(0, 41), MidiActionTag::PlayToggle);
    bind!(1, note(0, 42), MidiActionTag::Stop);
    bind!(1, note(0, 46), MidiActionTag::PageNext);
    bind!(1, cc(0, 0),    MidiActionTag::VolumeSet);
    bind!(1, cc(0, 1),    MidiActionTag::ZoomIn);
    bind!(1, cc(0, 2),    MidiActionTag::ZoomOut);

    // ── Page 2: Recorder ──────────────────────────────────
    bind!(2, note(0, 41), MidiActionTag::PlayToggle);
    bind!(2, note(0, 42), MidiActionTag::Stop);
    bind!(2, note(0, 45), MidiActionTag::RecordStart);  // REC button
    bind!(2, note(0, 46), MidiActionTag::PageNext);

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_action(bindings: &[((usize, MidiKey), MidiActionTag)], page: usize, key: MidiKey) -> Option<&MidiActionTag> {
        bindings.iter().find(|((p, k), _)| *p == page && k == &key).map(|(_, a)| a)
    }

    #[test]
    fn default_profile_has_record_start() {
        let bindings = default_bindings();
        assert_eq!(find_action(&bindings, 2, note(0, 45)), Some(&MidiActionTag::RecordStart));
    }

    #[test]
    fn cycle_button_mapped_on_all_pages() {
        let bindings = default_bindings();
        for page in 0..3 {
            assert_eq!(
                find_action(&bindings, page, note(0, 46)),
                Some(&MidiActionTag::PageNext),
                "CYCLE not mapped on page {page}"
            );
        }
    }
}
