#![forbid(unsafe_code)]

//! Default binding profile for the Korg nanoKONTROL2.
//!
//! Hardware layout (all on MIDI channel 0):
//!
//!  8 channel strips, each with:
//!    Fader  → CC  0– 7
//!    Knob   → CC 16–23
//!    S btn  → Note 32–39
//!    M btn  → Note 48–55
//!    R btn  → Note 64–71
//!
//!  Transport buttons:
//!    Prev Track (58), Next Track (59), Cycle (46), Set (60),
//!    Marker◄ (61), Marker► (62),
//!    Rewind (43), Fast Forward (44), Stop (42), Play (41), Record (45)
//!
//! Three pages share the same physical controls but map to different actions:
//!
//!   Page 0 — TUNE    : frequency navigation, bookmarks, demod mode
//!   Page 1 — DISPLAY : zoom, waterfall speed, squelch, volume
//!   Page 2 — RECORD  : recording start/stop, help panel
//!
//! Transport buttons (Play, Stop, Cycle, Record) are consistent across all pages.

use crate::config::{MidiActionTag, MidiKey, MidiKeyKind};

fn cc(number: u8) -> MidiKey {
    MidiKey {
        channel: 0,
        kind: MidiKeyKind::ControlChange,
        number,
    }
}

fn note(number: u8) -> MidiKey {
    MidiKey {
        channel: 0,
        kind: MidiKeyKind::NoteOn,
        number,
    }
}

/// Returns the default nanoKONTROL2 bindings as a list of `((page, key), action)`.
///
/// Page 0 = Tune, Page 1 = Display, Page 2 = Record
pub fn default_bindings() -> Vec<((usize, MidiKey), MidiActionTag)> {
    let mut m = Vec::new();
    macro_rules! bind {
        ($page:expr, $key:expr, $action:expr) => {
            m.push((($page, $key), $action));
        };
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Transport — same on every page
    // ══════════════════════════════════════════════════════════════════════════
    for page in 0..3 {
        bind!(page, note(41), MidiActionTag::PlayToggle); // ▶ Play
        bind!(page, note(42), MidiActionTag::Stop); // ■ Stop
        bind!(page, note(45), MidiActionTag::RecordingToggle); // ● Record
        bind!(page, note(46), MidiActionTag::PageNext); // ↺ Cycle → advance page
        bind!(page, note(58), MidiActionTag::BookmarkPrev); // |◄ Prev Track
        bind!(page, note(59), MidiActionTag::BookmarkNext); // ►| Next Track
    }

    // ══════════════════════════════════════════════════════════════════════════
    // Page 0 — TUNE: frequency navigation, bookmarks, demod mode
    // ══════════════════════════════════════════════════════════════════════════

    // Fader 0 (CC 0): volume — always accessible on every page
    bind!(0, cc(0), MidiActionTag::VolumeSet);

    // S buttons (Note 32–39): tune UP in various step sizes
    bind!(0, note(32), MidiActionTag::TuneCoarseUp); // +1 MHz
    bind!(0, note(33), MidiActionTag::TuneMediumUp); // +100 kHz
    bind!(0, note(34), MidiActionTag::TuneFineUp); // +10 kHz
    bind!(0, note(35), MidiActionTag::TuneUltraFineUp); // +1 kHz
    bind!(0, note(36), MidiActionTag::DemodModeCycle); // cycle demod mode
    bind!(0, note(37), MidiActionTag::StepSizeCycle); // cycle step size
    bind!(0, note(38), MidiActionTag::HelpPanelToggle); // toggle help
    bind!(0, note(39), MidiActionTag::BookmarkSave); // save bookmark

    // M buttons (Note 48–55): tune DOWN in various step sizes
    bind!(0, note(48), MidiActionTag::TuneCoarseDown); // -1 MHz
    bind!(0, note(49), MidiActionTag::TuneMediumDown); // -100 kHz
    bind!(0, note(50), MidiActionTag::TuneFineDown); // -10 kHz
    bind!(0, note(51), MidiActionTag::TuneUltraFineDown); // -1 kHz
    bind!(0, note(52), MidiActionTag::BookmarkNext);
    bind!(0, note(53), MidiActionTag::BookmarkPrev);

    // R buttons (Note 64–71): bookmark navigation
    bind!(0, note(64), MidiActionTag::BookmarkNext);
    bind!(0, note(65), MidiActionTag::BookmarkPrev);
    bind!(0, note(66), MidiActionTag::BookmarkSave);
    bind!(0, note(67), MidiActionTag::DemodModeCycle);

    // Faders (CC 0–7): absolute controls on Page 0
    // Fader 0 already bound above (VolumeSet)
    bind!(0, cc(1), MidiActionTag::ZoomSet);         // Fader 1: zoom
    bind!(0, cc(2), MidiActionTag::WaterfallSpeedSet); // Fader 2: WF speed
    bind!(0, cc(3), MidiActionTag::SquelchSet);      // Fader 3: squelch

    // Knobs (CC 16–23): relative frequency tuning — each knob tunes at a different speed.
    // Turning right = tune up, turning left = tune down.
    // Speed is proportional to how fast/far you turn.
    bind!(0, cc(16), MidiActionTag::TuneKnobCoarse);     // Knob 0: 1 MHz / unit
    bind!(0, cc(17), MidiActionTag::TuneKnobMedium);     // Knob 1: 100 kHz / unit
    bind!(0, cc(18), MidiActionTag::TuneKnobFine);       // Knob 2: 10 kHz / unit
    bind!(0, cc(19), MidiActionTag::TuneKnobUltraFine);  // Knob 3: 1 kHz / unit
    bind!(0, cc(20), MidiActionTag::VolumeSet);          // Knob 4: volume
    bind!(0, cc(21), MidiActionTag::ZoomSet);            // Knob 5: zoom
    bind!(0, cc(22), MidiActionTag::WaterfallSpeedSet);  // Knob 6: WF speed
    bind!(0, cc(23), MidiActionTag::SquelchSet);         // Knob 7: squelch

    // ══════════════════════════════════════════════════════════════════════════
    // Page 1 — DISPLAY: spectrum zoom, waterfall speed, volume, squelch
    // ══════════════════════════════════════════════════════════════════════════

    // Faders (absolute controls)
    bind!(1, cc(0), MidiActionTag::VolumeSet); // Fader 0: volume
    bind!(1, cc(1), MidiActionTag::ZoomSet); // Fader 1: zoom
    bind!(1, cc(2), MidiActionTag::WaterfallSpeedSet); // Fader 2: waterfall speed
    bind!(1, cc(7), MidiActionTag::SquelchSet); // Fader 7: squelch threshold

    // Knobs (absolute controls — same mapping as faders for reach)
    bind!(1, cc(16), MidiActionTag::VolumeSet); // Knob 0: volume
    bind!(1, cc(17), MidiActionTag::ZoomSet); // Knob 1: zoom
    bind!(1, cc(18), MidiActionTag::WaterfallSpeedSet); // Knob 2: waterfall speed
    bind!(1, cc(23), MidiActionTag::SquelchSet); // Knob 7: squelch

    // S buttons: zoom and waterfall incremental control
    bind!(1, note(32), MidiActionTag::ZoomIn);
    bind!(1, note(33), MidiActionTag::ZoomOut);
    bind!(1, note(34), MidiActionTag::WaterfallSpeedUp);
    bind!(1, note(35), MidiActionTag::WaterfallSpeedDown);
    bind!(1, note(36), MidiActionTag::HelpPanelToggle);
    bind!(1, note(37), MidiActionTag::DemodModeCycle);
    bind!(1, note(38), MidiActionTag::StepSizeCycle);
    bind!(1, note(39), MidiActionTag::BookmarkSave);

    // M buttons
    bind!(1, note(48), MidiActionTag::TuneCoarseUp);
    bind!(1, note(49), MidiActionTag::TuneCoarseDown);
    bind!(1, note(50), MidiActionTag::TuneMediumUp);
    bind!(1, note(51), MidiActionTag::TuneMediumDown);

    // R buttons
    bind!(1, note(64), MidiActionTag::BookmarkNext);
    bind!(1, note(65), MidiActionTag::BookmarkPrev);
    bind!(1, note(66), MidiActionTag::BookmarkSave);
    bind!(1, note(67), MidiActionTag::ZoomIn);
    bind!(1, note(68), MidiActionTag::ZoomOut);

    // ══════════════════════════════════════════════════════════════════════════
    // Page 2 — RECORD: recording control, full access to core parameters
    // ══════════════════════════════════════════════════════════════════════════

    // Faders
    bind!(2, cc(0), MidiActionTag::VolumeSet);
    bind!(2, cc(1), MidiActionTag::ZoomSet);
    bind!(2, cc(2), MidiActionTag::WaterfallSpeedSet);
    bind!(2, cc(7), MidiActionTag::SquelchSet);

    // Knobs
    bind!(2, cc(16), MidiActionTag::VolumeSet);
    bind!(2, cc(23), MidiActionTag::SquelchSet);

    // S buttons: recording and tuning
    bind!(2, note(32), MidiActionTag::RecordStart);
    bind!(2, note(33), MidiActionTag::RecordStop);
    bind!(2, note(34), MidiActionTag::RecordingToggle);
    bind!(2, note(35), MidiActionTag::HelpPanelToggle);
    bind!(2, note(36), MidiActionTag::TuneCoarseUp);
    bind!(2, note(37), MidiActionTag::TuneCoarseDown);
    bind!(2, note(38), MidiActionTag::TuneMediumUp);
    bind!(2, note(39), MidiActionTag::TuneMediumDown);

    // M buttons
    bind!(2, note(48), MidiActionTag::DemodModeCycle);
    bind!(2, note(49), MidiActionTag::StepSizeCycle);
    bind!(2, note(50), MidiActionTag::ZoomIn);
    bind!(2, note(51), MidiActionTag::ZoomOut);
    bind!(2, note(52), MidiActionTag::WaterfallSpeedUp);
    bind!(2, note(53), MidiActionTag::WaterfallSpeedDown);

    // R buttons
    bind!(2, note(64), MidiActionTag::BookmarkNext);
    bind!(2, note(65), MidiActionTag::BookmarkPrev);
    bind!(2, note(66), MidiActionTag::BookmarkSave);
    bind!(2, note(67), MidiActionTag::TuneFineUp);
    bind!(2, note(68), MidiActionTag::TuneFineDown);
    bind!(2, note(69), MidiActionTag::TuneUltraFineUp);
    bind!(2, note(70), MidiActionTag::TuneUltraFineDown);

    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_action(
        bindings: &[((usize, MidiKey), MidiActionTag)],
        page: usize,
        key: MidiKey,
    ) -> Option<&MidiActionTag> {
        bindings
            .iter()
            .find(|((p, k), _)| *p == page && k == &key)
            .map(|(_, a)| a)
    }

    #[test]
    fn cycle_button_mapped_on_all_pages() {
        let bindings = default_bindings();
        for page in 0..3 {
            assert_eq!(
                find_action(&bindings, page, note(46)),
                Some(&MidiActionTag::PageNext),
                "CYCLE not mapped on page {page}"
            );
        }
    }

    #[test]
    fn record_toggle_on_transport_all_pages() {
        let bindings = default_bindings();
        for page in 0..3 {
            assert_eq!(
                find_action(&bindings, page, note(45)),
                Some(&MidiActionTag::RecordingToggle),
                "REC toggle not mapped on page {page}"
            );
        }
    }

    #[test]
    fn page0_has_full_tune_step_coverage() {
        let bindings = default_bindings();
        let required = [
            (note(32), MidiActionTag::TuneCoarseUp),
            (note(33), MidiActionTag::TuneMediumUp),
            (note(34), MidiActionTag::TuneFineUp),
            (note(35), MidiActionTag::TuneUltraFineUp),
            (note(48), MidiActionTag::TuneCoarseDown),
            (note(49), MidiActionTag::TuneMediumDown),
            (note(50), MidiActionTag::TuneFineDown),
            (note(51), MidiActionTag::TuneUltraFineDown),
        ];
        for (key, expected) in &required {
            assert_eq!(
                find_action(&bindings, 0, key.clone()),
                Some(expected),
                "Missing {:?} on page 0",
                expected
            );
        }
    }

    #[test]
    fn page1_faders_cover_display_controls() {
        let bindings = default_bindings();
        assert_eq!(
            find_action(&bindings, 1, cc(0)),
            Some(&MidiActionTag::VolumeSet)
        );
        assert_eq!(
            find_action(&bindings, 1, cc(1)),
            Some(&MidiActionTag::ZoomSet)
        );
        assert_eq!(
            find_action(&bindings, 1, cc(2)),
            Some(&MidiActionTag::WaterfallSpeedSet)
        );
        assert_eq!(
            find_action(&bindings, 1, cc(7)),
            Some(&MidiActionTag::SquelchSet)
        );
    }

    #[test]
    fn page2_has_record_start_and_stop() {
        let bindings = default_bindings();
        assert_eq!(
            find_action(&bindings, 2, note(32)),
            Some(&MidiActionTag::RecordStart)
        );
        assert_eq!(
            find_action(&bindings, 2, note(33)),
            Some(&MidiActionTag::RecordStop)
        );
    }

    #[test]
    fn no_duplicate_bindings_per_page() {
        let bindings = default_bindings();
        for page in 0..3 {
            let page_bindings: Vec<_> = bindings
                .iter()
                .filter(|((p, _), _)| *p == page)
                .map(|((_, k), _)| k)
                .collect();
            let mut seen = std::collections::HashSet::new();
            for key in &page_bindings {
                assert!(
                    seen.insert((key.kind.clone(), key.number)),
                    "Duplicate binding for {:?} on page {page}",
                    key
                );
            }
        }
    }
}
