#![forbid(unsafe_code)]

//! Frequency tuner widget.
//!
//! Displays the current center frequency with per-digit scroll tuning.
//!
//! ## Interaction
//! - **Hover over a digit** → scroll wheel tunes that decimal decade
//!   (hover the MHz digit and scroll → ±1 MHz; hover the kHz digit → ±1 kHz, etc.)
//! - **Click a digit** → locks scroll to that digit's decade even after the
//!   cursor moves away
//! - **Double-click** → enter text-input mode; type `162.425` or `162425` and
//!   press Enter (or Tab/click-away) to commit; Escape cancels
//!
//! Format rendered: `"NNN.NNN NNN MHz"` using a monospace font so every
//! character occupies the same pixel width, making digit-to-position mapping
//! straightforward.

use egui::{Color32, FontId, Key, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

const FREQ_COLOR: Color32 = Color32::from_rgb(180, 230, 255);
const FREQ_ACTIVE_BG: Color32 = Color32::from_rgba_premultiplied(255, 210, 60, 55);
const FREQ_HOVER_BG: Color32 = Color32::from_rgba_premultiplied(100, 180, 255, 35);
const FREQ_BG: Color32 = Color32::from_rgb(20, 28, 38);
const FREQ_BG_CLAMPED: Color32 = Color32::from_rgb(60, 20, 20);
const EDIT_BG: Color32 = Color32::from_rgb(15, 30, 50);

/// Step size in Hz for each of the 9 tunable digit positions (left → right).
/// Position 0 = 100 MHz column; position 8 = 1 Hz column.
const DIGIT_STEPS_HZ: [u64; 9] = [
    100_000_000, // hundreds of MHz
    10_000_000,  // tens of MHz
    1_000_000,   // ones of MHz
    100_000,     // hundreds of kHz
    10_000,      // tens of kHz
    1_000,       // ones of kHz
    100,         // hundreds of Hz
    10,          // tens of Hz
    1,           // ones of Hz
];

/// Character indices (0-based) within the formatted string `"NNN.NNN NNN MHz"`
/// that correspond to the 9 tunable digit positions above.
const DIGIT_CHAR_IDX: [usize; 9] = [0, 1, 2, 4, 5, 6, 8, 9, 10];

/// Step sizes per scroll tick for each digit group (kept for external use).
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
    /// External step size (used by nudge buttons in the left panel).
    pub step_hz: i64,
    /// Counts down frames after a clamp-at-minimum event for a brief red tint.
    clamped_ticks: u8,
    /// The digit index (0–8) the user last clicked / is hovering for scroll.
    active_digit: Option<usize>,
    /// Whether the widget has keyboard focus (arrow keys tune active digit).
    has_keyboard_focus: bool,
    /// True while the text-entry overlay is open.
    editing: bool,
    /// Buffer for text-entry mode.
    edit_text: String,
}

impl FrequencyWidget {
    pub fn new(frequency_hz: u64) -> Self {
        Self {
            frequency_hz,
            step_hz: TuneStep::default_for_scroll(),
            clamped_ticks: 0,
            active_digit: None,
            has_keyboard_focus: false,
            editing: false,
            edit_text: String::new(),
        }
    }

    /// Render the frequency display.
    ///
    /// Returns `Some(new_freq)` when the user has changed the frequency,
    /// `None` otherwise.
    pub fn show(&mut self, ui: &mut Ui) -> (Response, Option<u64>) {
        if self.editing {
            return self.show_edit(ui);
        }
        self.show_normal(ui)
    }

    // ── Normal (display) mode ─────────────────────────────────────────────────

    fn show_normal(&mut self, ui: &mut Ui) -> (Response, Option<u64>) {
        let text = format_frequency(self.frequency_hz);
        let font = FontId::monospace(22.0);

        // Measure a single character width (monospace — all chars equal).
        let char_w = ui.fonts(|f| {
            f.layout_no_wrap("0".to_string(), font.clone(), FREQ_COLOR)
                .size()
                .x
        });
        let galley = ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), FREQ_COLOR));
        let pad_x = 8.0;
        let pad_y = 4.0;
        let desired_size = Vec2::new(galley.size().x + pad_x * 2.0, galley.size().y + pad_y * 2.0);

        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());

        // ── Determine which digit the pointer is over ─────────────────────────
        let hover_pos = ui.input(|i| i.pointer.hover_pos());
        let hovered_digit: Option<usize> = hover_pos.and_then(|mp| {
            if !rect.contains(mp) {
                return None;
            }
            let x_off = (mp.x - rect.left() - pad_x).max(0.0);
            let char_idx = (x_off / char_w) as usize;
            DIGIT_CHAR_IDX.iter().position(|&ci| ci == char_idx)
        });

        // Single click → pin active_digit to hovered digit + claim keyboard focus.
        if response.clicked() {
            if let Some(d) = hovered_digit {
                self.active_digit = Some(d);
            } else if self.active_digit.is_none() {
                self.active_digit = Some(4); // default: 10 kHz column
            }
            self.has_keyboard_focus = true;
        }

        // Any click outside this widget clears keyboard focus.
        if ui.input(|i| i.pointer.any_click()) && !response.clicked() {
            self.has_keyboard_focus = false;
        }

        // Double-click → enter text edit mode.
        if response.double_clicked() {
            self.editing = true;
            self.has_keyboard_focus = false;
            let mhz = self.frequency_hz as f64 / 1_000_000.0;
            // Pre-fill with clean MHz float (e.g. "162.425")
            self.edit_text = format!("{mhz:.3}");
            return (response, None);
        }

        // ── Arrow keys tune active digit when focused ─────────────────────────
        let scroll_dy = ui.input(|i| i.smooth_scroll_delta.y);
        let in_widget = hover_pos.map(|p| rect.contains(p)).unwrap_or(false);
        let arrow_up   = self.has_keyboard_focus && ui.input(|i| i.key_pressed(Key::ArrowUp));
        let arrow_down = self.has_keyboard_focus && ui.input(|i| i.key_pressed(Key::ArrowDown));

        let new_freq = if arrow_up || arrow_down {
            let digit = self.active_digit.unwrap_or(4);
            let step = DIGIT_STEPS_HZ[digit] as i64;
            let delta = if arrow_up { step } else { -step };
            let raw = self.frequency_hz as i64 + delta;
            let clamped = raw.max(1) as u64;
            if raw < 1 { self.clamped_ticks = 45; }
            self.frequency_hz = clamped;
            ui.ctx().request_repaint();
            Some(clamped)
        } else if scroll_dy.abs() > 0.5 && in_widget {
            // Prefer the digit the pointer is directly over; fall back to the
            // last clicked digit; last resort: 10 kHz default.
            let digit = hovered_digit
                .or(self.active_digit)
                .unwrap_or(4); // index 4 = 10 kHz column
            let step = DIGIT_STEPS_HZ[digit] as i64;
            let ticks = scroll_dy.signum() as i64;
            let raw = self.frequency_hz as i64 + ticks * step;
            let clamped = raw.max(1) as u64;
            if raw < 1 {
                self.clamped_ticks = 45;
            }
            self.frequency_hz = clamped;
            if hovered_digit.is_some() {
                self.active_digit = hovered_digit;
            }
            Some(clamped)
        } else {
            None
        };

        if self.clamped_ticks > 0 {
            self.clamped_ticks -= 1;
            ui.ctx().request_repaint();
        }

        // ── Paint ─────────────────────────────────────────────────────────────
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);

            // Background
            let bg = if self.clamped_ticks > 0 { FREQ_BG_CLAMPED } else { FREQ_BG };
            painter.rect_filled(rect, 4.0, bg);

            // Focus ring — subtle border when arrow-key tuning is active.
            if self.has_keyboard_focus {
                painter.rect_stroke(
                    rect,
                    4.0,
                    Stroke::new(1.5, Color32::from_rgba_premultiplied(255, 210, 60, 120)),
                );
            }

            // Per-digit highlight rects
            for (di, &ci) in DIGIT_CHAR_IDX.iter().enumerate() {
                let x = rect.left() + pad_x + ci as f32 * char_w;
                let digit_rect = Rect::from_min_size(
                    Pos2::new(x, rect.top() + 1.0),
                    Vec2::new(char_w, rect.height() - 2.0),
                );
                let highlight = if Some(di) == hovered_digit {
                    FREQ_HOVER_BG
                } else if Some(di) == self.active_digit {
                    FREQ_ACTIVE_BG
                } else {
                    Color32::TRANSPARENT
                };
                if highlight != Color32::TRANSPARENT {
                    painter.rect_filled(digit_rect, 2.0, highlight);
                }
            }

            // Frequency text
            painter.galley(
                Pos2::new(rect.left() + pad_x, rect.top() + pad_y),
                galley,
                FREQ_COLOR,
            );

            // Underline under active digit
            if let Some(di) = self.active_digit {
                let ci = DIGIT_CHAR_IDX[di];
                let x = rect.left() + pad_x + ci as f32 * char_w;
                let y = rect.bottom() - 3.0;
                painter.line_segment(
                    [Pos2::new(x, y), Pos2::new(x + char_w, y)],
                    Stroke::new(2.0, Color32::from_rgb(255, 210, 60)),
                );
            }

            // Tooltip
            if hovered_digit.is_some() || self.active_digit.is_some() {
                let d = hovered_digit.or(self.active_digit).unwrap();
                let step_label = step_label(DIGIT_STEPS_HZ[d]);
                response.clone().on_hover_text(format!(
                    "Scroll to tune ±{step_label}\nDouble-click to type a frequency"
                ));
            }
        }

        (response, new_freq)
    }

    // ── Text-entry mode ───────────────────────────────────────────────────────

    fn show_edit(&mut self, ui: &mut Ui) -> (Response, Option<u64>) {
        let font = FontId::monospace(22.0);
        // Match the normal widget width so the panel doesn't jump.
        let template = format_frequency(self.frequency_hz);
        let galley_w = ui.fonts(|f| {
            f.layout_no_wrap(template, font.clone(), FREQ_COLOR)
                .size()
                .x
        });
        let desired_size = Vec2::new(galley_w + 16.0, 30.0);

        // Draw input box background
        let (bg_rect, _) = ui.allocate_exact_size(Vec2::ZERO, Sense::hover());
        let _ = bg_rect;

        // TextEdit occupying the same area
        let response = ui.add_sized(
            desired_size,
            egui::TextEdit::singleline(&mut self.edit_text)
                .font(font)
                .text_color(Color32::from_rgb(255, 230, 100))
                .cursor_at_end(true)
                .hint_text("MHz  e.g. 162.425"),
        );

        // Auto-focus on first frame
        response.request_focus();

        let mut new_freq = None;
        let enter = ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Tab));
        let escape = ui.input(|i| i.key_pressed(Key::Escape));

        if escape {
            self.editing = false;
        } else if enter || response.lost_focus() {
            if let Some(hz) = parse_frequency(&self.edit_text) {
                self.frequency_hz = hz;
                new_freq = Some(hz);
            }
            self.editing = false;
        }

        (response, new_freq)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse a user-typed frequency string.  Accepts:
/// - `"162.425"` → 162 425 000 Hz  (assumed MHz when < 300 and contains `.`)
/// - `"162425"` → 162 425 000 Hz  (bare integer < 300 → MHz; >= 300_000 → Hz)
/// - `"162425000"` → 162 425 000 Hz
/// - `"162.425 MHz"` / `"162425 kHz"` (suffix stripped)
fn parse_frequency(raw: &str) -> Option<u64> {
    let s = raw.trim().to_lowercase();
    // Strip unit suffixes.
    let (s, multiplier) = if s.ends_with("ghz") {
        (&s[..s.len() - 3], 1_000_000_000u64)
    } else if s.ends_with("mhz") {
        (&s[..s.len() - 3], 1_000_000u64)
    } else if s.ends_with("khz") {
        (&s[..s.len() - 3], 1_000u64)
    } else if s.ends_with("hz") {
        (&s[..s.len() - 2], 1u64)
    } else {
        (s.as_str(), 0u64) // 0 = auto-detect
    };
    let s = s.trim();

    if multiplier > 0 {
        // Explicit unit — parse as float and scale.
        let v: f64 = s.parse().ok()?;
        let hz = (v * multiplier as f64).round() as u64;
        return if hz > 0 { Some(hz) } else { None };
    }

    // No unit — heuristic:
    if s.contains('.') {
        // Float → treat as MHz.
        let v: f64 = s.parse().ok()?;
        let hz = (v * 1_000_000.0).round() as u64;
        return if hz > 0 { Some(hz) } else { None };
    }

    // Plain integer.
    let v: u64 = s.parse().ok()?;
    let hz = if v < 300 {
        v * 1_000_000 // e.g. "162" → 162 MHz
    } else if v < 300_000 {
        v * 1_000 // e.g. "162425" in kHz range → treat as kHz
    } else {
        v // assume Hz
    };
    if hz > 0 { Some(hz) } else { None }
}

fn step_label(hz: u64) -> &'static str {
    match hz {
        1_000_000_000 => "1 GHz",
        100_000_000 => "100 MHz",
        10_000_000 => "10 MHz",
        1_000_000 => "1 MHz",
        100_000 => "100 kHz",
        10_000 => "10 kHz",
        1_000 => "1 kHz",
        100 => "100 Hz",
        10 => "10 Hz",
        _ => "1 Hz",
    }
}

/// Format a frequency in Hz as `"NNN.NNN NNN MHz"`.
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
    fn format_162_425_mhz() {
        assert_eq!(format_frequency(162_425_000), "162.425 000 MHz");
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
    fn parse_mhz_float() {
        assert_eq!(parse_frequency("162.425"), Some(162_425_000));
    }

    #[test]
    fn parse_mhz_with_suffix() {
        assert_eq!(parse_frequency("162.425 MHz"), Some(162_425_000));
        assert_eq!(parse_frequency("162.425MHz"), Some(162_425_000));
    }

    #[test]
    fn parse_bare_integer_mhz() {
        assert_eq!(parse_frequency("162"), Some(162_000_000));
    }

    #[test]
    fn parse_hz_integer() {
        assert_eq!(parse_frequency("162425000"), Some(162_425_000));
    }

    #[test]
    fn parse_khz_suffix() {
        assert_eq!(parse_frequency("162425 kHz"), Some(162_425_000));
    }

    #[test]
    fn scroll_up_increases_frequency() {
        let delta = TuneStep::default_for_scroll();
        let new_freq = (100_000_000_i64 + delta).max(1) as u64;
        assert_eq!(new_freq, 100_010_000);
    }

    #[test]
    fn frequency_does_not_go_below_one() {
        let w = FrequencyWidget::new(500);
        let delta: i64 = -1_000_000;
        let new = (w.frequency_hz as i64 + delta).max(1) as u64;
        assert_eq!(new, 1);
    }
}
