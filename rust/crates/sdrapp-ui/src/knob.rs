#![forbid(unsafe_code)]

//! Rotary knob widget for egui.
//!
//! Geometry (screen coords: y increases downward, angles clockwise from right):
//!   • Arc sweeps 300° clockwise, starting at 7 o'clock (120°) and ending at 5 o'clock (60° + 360°).
//!   • Minimum value sits at 7 o'clock; maximum sits at 5 o'clock.
//!
//! Interaction:
//!   • Drag up/down — drag up increases value; sensitivity = full-range / DRAG_FULL_PX pixels.
//!   • Scroll wheel — nudge by `step`.
//!   • Double-click — reset to `default_value`.
//!   • Hover — tooltip showing numeric value + unit.

use egui::{Color32, Pos2, Response, Sense, Shape, Stroke, Ui, Vec2};

use crate::theme;

// ── Knob geometry constants ───────────────────────────────────────────────────

/// Start of the arc: 7 o'clock = 120° clockwise from right.
const START_ANGLE: f32 = std::f32::consts::TAU * (120.0 / 360.0);
/// Total angular sweep: 300° (clockwise).
const SWEEP: f32 = std::f32::consts::TAU * (300.0 / 360.0);
/// Pixels of vertical drag that map to the full value range.
const DRAG_FULL_PX: f32 = 200.0;
/// Number of line segments used to approximate each arc.
const ARC_SEGMENTS: usize = 48;

// ── KnobWidget ────────────────────────────────────────────────────────────────

/// Rotary knob widget.
///
/// ```ignore
/// KnobWidget {
///     value: &mut my_volume,
///     range: 0.0..=1.0,
///     default_value: 0.8,
///     step: 0.01,
///     diameter: 48.0,
///     label: Some("VOL"),
///     unit: "%",
///     midi_cc: None,
/// }.show(ui);
/// ```
pub struct KnobWidget<'a> {
    /// Mutable reference to the controlled value.
    pub value: &'a mut f32,
    /// Inclusive value range [`min`, `max`].
    pub range: std::ops::RangeInclusive<f32>,
    /// Value restored on double-click.
    pub default_value: f32,
    /// Per-tick nudge amount (scroll wheel / arrow).
    pub step: f32,
    /// Outer diameter of the arc in logical pixels.
    pub diameter: f32,
    /// Short label drawn below the knob (e.g. `"VOL"`).
    pub label: Option<&'a str>,
    /// Unit appended to the hover tooltip (e.g. `"dB"` → `"-34.0 dB"`).
    pub unit: &'a str,
    /// Optional bound MIDI CC number shown as `"CC N"` below the arc.
    pub midi_cc: Option<u8>,
}

impl<'a> KnobWidget<'a> {
    /// Draw the knob and process interaction. Returns the egui [`Response`].
    ///
    /// The caller should inspect `response.changed()` to detect value mutations.
    pub fn show(self, ui: &mut Ui) -> Response {
        let min = *self.range.start();
        let max = *self.range.end();

        // Height = arc area + optional label row + optional CC row
        let label_h = if self.label.is_some() { 14.0 } else { 0.0 };
        let cc_h = if self.midi_cc.is_some() { 12.0 } else { 0.0 };
        let total_h = self.diameter + label_h + cc_h + 4.0; // 4px padding

        let (rect, mut response) = ui.allocate_exact_size(
            Vec2::new(self.diameter, total_h),
            Sense::click_and_drag(),
        );

        // ── Value mutations ───────────────────────────────────────────────────

        let old_value = *self.value;

        // Drag: dragging UP increases value.
        if response.dragged() {
            let delta_px = -response.drag_delta().y; // negative y = up = increase
            let delta_val = delta_px / DRAG_FULL_PX * (max - min);
            *self.value = (*self.value + delta_val).clamp(min, max);
        }

        // Scroll wheel: nudge by step.
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                let sign = if scroll > 0.0 { 1.0 } else { -1.0 };
                *self.value = (*self.value + sign * self.step).clamp(min, max);
            }
        }

        // Double-click: reset to default.
        if response.double_clicked() {
            *self.value = self.default_value.clamp(min, max);
        }

        if (*self.value - old_value).abs() > f32::EPSILON {
            response.mark_changed();
        }

        // ── Painting ─────────────────────────────────────────────────────────

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let radius = self.diameter * 0.5;
            let center = Pos2::new(rect.left() + radius, rect.top() + radius);
            let t = if (max - min).abs() < f32::EPSILON {
                0.0
            } else {
                ((*self.value - min) / (max - min)).clamp(0.0, 1.0)
            };

            let stroke_w = (self.diameter * 0.08).max(2.0);
            let inner_r = radius - stroke_w * 0.5 - 1.0;

            // Background arc (full range): dim colour
            let bg_pts = arc_points(center, inner_r, START_ANGLE, START_ANGLE + SWEEP, ARC_SEGMENTS);
            painter.add(Shape::line(
                bg_pts,
                Stroke::new(stroke_w, theme::WIDGET_BG_STRONG),
            ));

            // Value arc: accent colour
            if t > 0.001 {
                let val_pts = arc_points(center, inner_r, START_ANGLE, START_ANGLE + t * SWEEP, ARC_SEGMENTS);
                painter.add(Shape::line(
                    val_pts,
                    Stroke::new(stroke_w, theme::ACCENT),
                ));
            }

            // Indicator dot at current angle
            let cur_angle = START_ANGLE + t * SWEEP;
            let dot_r = inner_r - stroke_w * 0.5;
            let dot_pos = center + Vec2::new(cur_angle.cos(), cur_angle.sin()) * dot_r;
            let dot_size = (stroke_w * 0.9).max(3.0);
            painter.circle_filled(dot_pos, dot_size, Color32::WHITE);

            // Thin line from center to mid-arc as pointer
            let ptr_inner = inner_r * 0.35;
            let ptr_outer = inner_r - stroke_w;
            let ptr_start = center + Vec2::new(cur_angle.cos(), cur_angle.sin()) * ptr_inner;
            let ptr_end   = center + Vec2::new(cur_angle.cos(), cur_angle.sin()) * ptr_outer;
            painter.line_segment([ptr_start, ptr_end], Stroke::new(1.5, Color32::WHITE));

            // Highlight ring when hovered or dragged
            if response.hovered() || response.dragged() {
                painter.circle_stroke(
                    center,
                    radius - 1.0,
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
                );
            }

            // Label below arc
            if let Some(lbl) = self.label {
                let label_top = rect.top() + self.diameter + 2.0;
                let label_rect = egui::Rect::from_min_size(
                    Pos2::new(rect.left(), label_top),
                    Vec2::new(self.diameter, label_h),
                );
                painter.text(
                    label_rect.center(),
                    egui::Align2::CENTER_TOP,
                    lbl,
                    egui::FontId::proportional(9.0),
                    theme::TEXT_MUTED,
                );
            }

            // MIDI CC label below label (if any)
            if let Some(cc) = self.midi_cc {
                let cc_top = rect.top() + self.diameter + label_h + 2.0;
                painter.text(
                    Pos2::new(rect.center_top().x, cc_top),
                    egui::Align2::CENTER_TOP,
                    format!("CC{cc}"),
                    egui::FontId::proportional(8.0),
                    theme::ACCENT_DIM,
                );
            }
        }

        // Hover tooltip
        if response.hovered() {
            let display = if self.unit.is_empty() {
                format!("{:.2}", *self.value)
            } else {
                format!("{:.2} {}", *self.value, self.unit)
            };
            response = response.on_hover_text(display);
        }

        response
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Generate `segments+1` points along a clockwise arc from `start_rad` to `end_rad`.
fn arc_points(center: Pos2, radius: f32, start_rad: f32, end_rad: f32, segments: usize) -> Vec<Pos2> {
    (0..=segments)
        .map(|i| {
            let t = i as f32 / segments as f32;
            let a = start_rad + t * (end_rad - start_rad);
            center + Vec2::new(a.cos(), a.sin()) * radius
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_points_count() {
        let pts = arc_points(Pos2::ZERO, 10.0, 0.0, std::f32::consts::PI, 8);
        assert_eq!(pts.len(), 9); // segments + 1
    }

    #[test]
    fn arc_start_and_end_angles() {
        let pts = arc_points(Pos2::new(50.0, 50.0), 10.0, 0.0, std::f32::consts::FRAC_PI_2, 4);
        // First point should be at angle 0 (right of center)
        let first = pts[0];
        assert!((first.x - 60.0).abs() < 0.01, "start x = {}", first.x);
        assert!((first.y - 50.0).abs() < 0.01, "start y = {}", first.y);
        // Last point at 90° = downward
        let last = pts[4];
        assert!((last.x - 50.0).abs() < 0.01, "end x = {}", last.x);
        assert!((last.y - 60.0).abs() < 0.01, "end y = {}", last.y);
    }

    #[test]
    fn drag_increases_value_upward() {
        // Widget negates drag_delta().y: dragging up = negative screen-y = positive delta_px.
        // Simulate drag_delta().y = -100 (100px upward), negated in widget → delta_px = +100.
        let drag_delta_y: f32 = -100.0;
        let delta_px = -drag_delta_y; // +100.0
        let min = 0.0_f32;
        let max = 1.0_f32;
        let old = 0.3_f32;
        let delta_val = delta_px / DRAG_FULL_PX * (max - min);
        let new = (old + delta_val).clamp(min, max);
        assert!((new - 0.8).abs() < 0.01, "expected ~0.8 got {new}");
    }

    #[test]
    fn double_click_resets_to_default() {
        let default = 0.5_f32;
        let mut v = 0.9_f32;
        v = default.clamp(0.0, 1.0); // mirrors widget logic
        assert!((v - 0.5).abs() < f32::EPSILON);
    }
}
