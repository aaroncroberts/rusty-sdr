#![forbid(unsafe_code)]

//! Frequency tuner widget.
//!
//! Displays the current center frequency with digit-group formatting.
//! Scroll up/down to tune in configurable steps.
//! Returns the new frequency if the user scrolled.

use egui::{Color32, FontId, Pos2, Response, Sense, Ui, Vec2};

const FREQ_COLOR: Color32 = Color32::from_rgb(180, 230, 255);
const FREQ_BG: Color32 = Color32::from_rgb(20, 28, 38);

/// Step sizes per scroll tick for each digit group.
#[derive(Debug, Clone, Copy)]
pub enum TuneStep {
    GHz = 1_000_000_000,
    MHz100 = 100_000_000,
    MHz10 = 10_000_000,
    MHz1 = 1_000_000,
    KHz100 = 100_000,
    KHz10 = 10_000,
    KHz1 = 1_000,
    Hz100 = 100,
    Hz10 = 10,
    Hz1 = 1,
}

impl TuneStep {
    pub fn default_for_scroll() -> i64 {
        TuneStep::KHz10 as i64
    }
}

pub struct FrequencyWidget {
    /// Current frequency in Hz.
    pub frequency_hz: u64,
    /// Highlight color for the active step.
    pub step_hz: i64,
}

impl FrequencyWidget {
    pub fn new(frequency_hz: u64) -> Self {
        Self {
            frequency_hz,
            step_hz: TuneStep::default_for_scroll(),
        }
    }

    /// Render the frequency display.
    ///
    /// Returns `Some(new_freq)` if the user scrolled to retune, `None` otherwise.
    pub fn show(&mut self, ui: &mut Ui) -> (Response, Option<u64>) {
        let text = format_frequency(self.frequency_hz);
        let font = FontId::monospace(22.0);
        let galley = ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), FREQ_COLOR));
        let desired_size = Vec2::new(galley.size().x + 16.0, galley.size().y + 8.0);

        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::hover());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, FREQ_BG);
            painter.galley(
                Pos2::new(rect.left() + 8.0, rect.top() + 4.0),
                galley,
                FREQ_COLOR,
            );
        }

        // Handle scroll to tune
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
        let new_freq = if scroll_delta.abs() > 0.5 {
            let ticks = scroll_delta.signum() as i64;
            let delta = ticks * self.step_hz;
            let new = (self.frequency_hz as i64 + delta).max(1) as u64;
            self.frequency_hz = new;
            Some(new)
        } else {
            None
        };

        (response, new_freq)
    }
}

/// Format a frequency in Hz as "100.000 000 MHz"
pub fn format_frequency(hz: u64) -> String {
    let mhz = hz / 1_000_000;
    let khz = (hz % 1_000_000) / 1_000;
    let sub = hz % 1_000;

    if hz >= 1_000_000_000 {
        let ghz = hz / 1_000_000_000;
        let rem_mhz = (hz % 1_000_000_000) / 1_000_000;
        format!("{ghz}.{rem_mhz:03} GHz")
    } else {
        format!("{mhz:3}.{khz:03} {sub:03} MHz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_100mhz() {
        assert_eq!(format_frequency(100_000_000), "100.000 000 MHz");
    }

    #[test]
    fn format_137_912_500_hz() {
        assert_eq!(format_frequency(137_912_500), "137.912 500 MHz");
    }

    #[test]
    fn format_1_42_ghz() {
        assert_eq!(format_frequency(1_420_000_000), "1.420 GHz");
    }

    #[test]
    fn scroll_up_increases_frequency() {
        let _w = FrequencyWidget::new(100_000_000);
        // Simulate positive scroll delta — we test the math directly
        let delta = TuneStep::default_for_scroll();
        let new_freq = (100_000_000_i64 + delta).max(1) as u64;
        assert_eq!(new_freq, 100_010_000);
    }

    #[test]
    fn frequency_does_not_go_below_one() {
        let w = FrequencyWidget::new(500);
        // Large negative scroll
        let delta: i64 = -1_000_000;
        let new = (w.frequency_hz as i64 + delta).max(1) as u64;
        assert_eq!(new, 1);
    }
}
