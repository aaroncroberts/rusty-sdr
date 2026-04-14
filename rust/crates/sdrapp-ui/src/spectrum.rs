#![forbid(unsafe_code)]

//! Spectrum display widget.
//!
//! Features:
//! - Filled trace in accent color with anti-aliased polygon
//! - Frequency axis (X) with labeled tick marks in MHz/kHz
//! - dBFS axis (Y) with horizontal gridlines at -20, -40, -60, -80, -100 dBFS
//! - VFO marker line with frequency label
//! - Click-to-tune is handled by the caller (we expose the rect via Response)

use egui::{Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2};

use crate::theme;

/// Spectrum display widget. Call `.show()` to render.
pub struct SpectrumWidget<'a> {
    /// Latest FFT magnitude data (dBFS), length = FFT size.
    pub fft_data: &'a [f32],
    /// (min_dbfs, max_dbfs) for Y-axis mapping. Typically (-120.0, 0.0).
    pub db_range: (f32, f32),
    /// (low_hz, high_hz) for X-axis frequency range.
    pub freq_range: (u64, u64),
    /// VFO center frequency in Hz (for the center marker line).
    pub vfo_hz: u64,
}

impl<'a> SpectrumWidget<'a> {
    pub fn show(&self, ui: &mut Ui) -> Response {
        let available = ui.available_size();
        let (rect, response) = ui.allocate_exact_size(available, Sense::click());

        if ui.is_rect_visible(rect) {
            self.paint(ui.painter(), rect);
        }

        response
    }

    fn paint(&self, painter: &Painter, rect: Rect) {
        let (db_min, db_max) = self.db_range;
        let (freq_lo, freq_hi) = (self.freq_range.0 as f64, self.freq_range.1 as f64);

        // ── Background ────────────────────────────────────────────────────────
        painter.rect_filled(rect, 0.0, theme::BG);

        // Leave margins for the Y-axis labels on the left
        let y_label_w = 38.0;
        let x_label_h = 18.0;
        let plot_rect = Rect::from_min_max(
            Pos2::new(rect.left() + y_label_w, rect.top()),
            Pos2::new(rect.right(), rect.bottom() - x_label_h),
        );

        // ── dBFS grid lines & Y-axis labels ──────────────────────────────────
        let db_steps = [-20.0_f32, -40.0, -60.0, -80.0, -100.0];
        for &db in &db_steps {
            if db < db_min || db > db_max {
                continue;
            }
            let y = db_to_y(db, db_min, db_max, plot_rect);

            // Grid line
            painter.line_segment(
                [Pos2::new(plot_rect.left(), y), Pos2::new(plot_rect.right(), y)],
                Stroke::new(1.0, theme::SPECTRUM_GRID),
            );

            // Y label
            painter.text(
                Pos2::new(rect.left() + y_label_w - 4.0, y - 1.0),
                egui::Align2::RIGHT_CENTER,
                format!("{db:.0}"),
                egui::FontId::proportional(9.0),
                theme::TEXT_MUTED,
            );
        }

        // ── Plot border ───────────────────────────────────────────────────────
        painter.rect_stroke(plot_rect, 0.0, Stroke::new(1.0, theme::SEPARATOR));

        // ── Spectrum trace ────────────────────────────────────────────────────
        let n = self.fft_data.len();
        if n > 1 {
            let mut points: Vec<Pos2> = Vec::with_capacity(n + 2);

            for (i, &db) in self.fft_data.iter().enumerate() {
                let x = plot_rect.left() + (i as f32 / (n - 1) as f32) * plot_rect.width();
                let y = db_to_y(db, db_min, db_max, plot_rect);
                points.push(Pos2::new(x, y));
            }

            // Close the polygon at the bottom to fill underneath
            let mut fill_polygon = points.clone();
            fill_polygon.push(Pos2::new(plot_rect.right(), plot_rect.bottom()));
            fill_polygon.push(Pos2::new(plot_rect.left(), plot_rect.bottom()));

            painter.add(egui::Shape::convex_polygon(
                fill_polygon,
                theme::SPECTRUM_FILL,
                Stroke::NONE,
            ));

            // Trace line on top
            painter.add(egui::Shape::line(
                points,
                Stroke::new(1.5, theme::SPECTRUM_TRACE),
            ));
        }

        // ── VFO line ─────────────────────────────────────────────────────────
        if freq_hi > freq_lo {
            let vfo_t = ((self.vfo_hz as f64 - freq_lo) / (freq_hi - freq_lo)) as f32;
            let vfo_x = plot_rect.left() + vfo_t.clamp(0.0, 1.0) * plot_rect.width();

            painter.line_segment(
                [Pos2::new(vfo_x, plot_rect.top()), Pos2::new(vfo_x, plot_rect.bottom())],
                Stroke::new(1.5, theme::VFO_LINE),
            );

            // VFO frequency label above the line
            let vfo_label = format_freq_short(self.vfo_hz);
            painter.text(
                Pos2::new(vfo_x + 3.0, plot_rect.top() + 3.0),
                egui::Align2::LEFT_TOP,
                vfo_label,
                egui::FontId::proportional(9.0),
                theme::ACCENT,
            );
        }

        // ── Frequency axis (X labels) ─────────────────────────────────────────
        if freq_hi > freq_lo {
            let tick_count = 8_usize;
            for i in 0..=tick_count {
                let t = i as f64 / tick_count as f64;
                let freq_hz = freq_lo + t * (freq_hi - freq_lo);
                let x = plot_rect.left() + t as f32 * plot_rect.width();

                // Tick mark
                painter.line_segment(
                    [
                        Pos2::new(x, plot_rect.bottom()),
                        Pos2::new(x, plot_rect.bottom() + 4.0),
                    ],
                    Stroke::new(1.0, theme::SEPARATOR),
                );

                // Label
                painter.text(
                    Pos2::new(x, rect.bottom() - 2.0),
                    egui::Align2::CENTER_BOTTOM,
                    format_freq_short(freq_hz as u64),
                    egui::FontId::proportional(9.0),
                    theme::TEXT_MUTED,
                );
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn db_to_y(db: f32, db_min: f32, db_max: f32, rect: Rect) -> f32 {
    let t = ((db - db_min) / (db_max - db_min)).clamp(0.0, 1.0);
    rect.bottom() - t * rect.height()
}

fn format_freq_short(hz: u64) -> String {
    if hz >= 1_000_000_000 {
        format!("{:.2}G", hz as f64 / 1_000_000_000.0)
    } else if hz >= 1_000_000 {
        format!("{:.2}M", hz as f64 / 1_000_000.0)
    } else if hz >= 1_000 {
        format!("{:.0}k", hz as f64 / 1_000.0)
    } else {
        format!("{hz}")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_to_y_clamps_correctly() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        // Max dBFS → top of rect (y = 0)
        let y_top = db_to_y(0.0, -120.0, 0.0, rect);
        assert!((y_top - 0.0).abs() < 1.0, "0 dBFS should map to top: {y_top}");

        // Min dBFS → bottom (y = 100)
        let y_bot = db_to_y(-120.0, -120.0, 0.0, rect);
        assert!((y_bot - 100.0).abs() < 1.0, "-120 dBFS should map to bottom: {y_bot}");

        // Out of range clamped
        let y_over = db_to_y(10.0, -120.0, 0.0, rect);
        assert!((y_over - 0.0).abs() < 1.0, "above 0 dBFS clamped to top: {y_over}");
    }

    #[test]
    fn spectrum_widget_renders_without_panic() {
        let data: Vec<f32> = (0..2048).map(|i| -60.0 + (i as f32 * 0.01).sin() * 20.0).collect();
        let ctx = egui::Context::default();
        ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                SpectrumWidget {
                    fft_data: &data,
                    db_range: (-120.0, 0.0),
                    freq_range: (95_000_000, 105_000_000),
                    vfo_hz: 100_000_000,
                }
                .show(ui);
            });
        });
    }

    #[test]
    fn format_freq_short_cases() {
        assert_eq!(format_freq_short(100_000_000), "100.00M");
        assert_eq!(format_freq_short(1_420_000_000), "1.42G");
        assert_eq!(format_freq_short(162_000), "162k");
        assert_eq!(format_freq_short(500), "500");
    }
}
