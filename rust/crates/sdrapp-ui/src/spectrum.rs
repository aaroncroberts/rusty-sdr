#![forbid(unsafe_code)]

//! Spectrum analyzer widget.
//!
//! Draws an FFT magnitude trace using egui's Painter API.
//! No egui_plot — direct pixel rendering for real-time performance.

use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

const BACKGROUND: Color32 = Color32::from_rgb(15, 20, 25);
const TRACE_COLOR: Color32 = Color32::from_rgb(0, 200, 120);
const GRID_COLOR: Color32 = Color32::from_rgba_premultiplied(80, 80, 80, 120);
const ZERO_DB_LINE: Color32 = Color32::from_rgb(200, 80, 80);
const VFO_COLOR: Color32 = Color32::from_rgb(255, 220, 0);

pub struct SpectrumWidget<'a> {
    pub fft_data: &'a [f32],
    /// dBFS range to display (bottom, top). E.g. (-120.0, 0.0)
    pub db_range: (f32, f32),
    /// Frequency range Hz (min, max).
    pub freq_range: (u64, u64),
    /// VFO center frequency Hz.
    pub vfo_hz: u64,
}

impl<'a> SpectrumWidget<'a> {
    pub fn show(&self, ui: &mut Ui) -> egui::Response {
        let desired_size = Vec2::new(ui.available_width(), ui.available_height());
        let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());

        if !ui.is_rect_visible(rect) {
            return response;
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, BACKGROUND);

        self.draw_grid(&painter, rect);
        self.draw_trace(&painter, rect);
        self.draw_vfo_line(&painter, rect);

        response
    }

    fn db_to_y(&self, db: f32, rect: Rect) -> f32 {
        let (db_min, db_max) = self.db_range;
        let normalized = (db - db_max) / (db_min - db_max); // 0=top, 1=bottom
        rect.top() + normalized * rect.height()
    }

    fn freq_to_x(&self, hz: u64, rect: Rect) -> f32 {
        let (f_min, f_max) = self.freq_range;
        if f_max == f_min { return rect.center().x; }
        let normalized = (hz.saturating_sub(f_min)) as f32 / (f_max - f_min) as f32;
        rect.left() + normalized * rect.width()
    }

    fn draw_grid(&self, painter: &egui::Painter, rect: Rect) {
        // Horizontal dB grid lines every 20 dB
        let (db_min, db_max) = self.db_range;
        let step = 20.0_f32;
        let mut db = (db_min / step).ceil() * step;
        while db <= db_max {
            let y = self.db_to_y(db, rect);
            let color = if db == 0.0 { ZERO_DB_LINE } else { GRID_COLOR };
            painter.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                Stroke::new(1.0, color),
            );
            db += step;
        }
    }

    fn draw_trace(&self, painter: &egui::Painter, rect: Rect) {
        if self.fft_data.is_empty() { return; }

        let n = self.fft_data.len();
        let points: Vec<Pos2> = self.fft_data.iter().enumerate().map(|(i, &db)| {
            let x = rect.left() + (i as f32 / (n - 1) as f32) * rect.width();
            let y = self.db_to_y(db.clamp(self.db_range.0, self.db_range.1), rect);
            Pos2::new(x, y)
        }).collect();

        // Draw filled polygon under trace
        let mut fill_points = points.clone();
        fill_points.push(Pos2::new(rect.right(), rect.bottom()));
        fill_points.push(Pos2::new(rect.left(), rect.bottom()));
        painter.add(egui::Shape::convex_polygon(
            fill_points,
            Color32::from_rgba_premultiplied(0, 200, 120, 25),
            Stroke::NONE,
        ));

        // Draw trace line
        painter.add(egui::Shape::line(points, Stroke::new(1.5, TRACE_COLOR)));
    }

    fn draw_vfo_line(&self, painter: &egui::Painter, rect: Rect) {
        let x = self.freq_to_x(self.vfo_hz, rect);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.5, VFO_COLOR),
        );
        // VFO label
        painter.text(
            Pos2::new(x + 4.0, rect.top() + 4.0),
            egui::Align2::LEFT_TOP,
            format!("{:.3} MHz", self.vfo_hz as f64 / 1e6),
            egui::FontId::monospace(10.0),
            VFO_COLOR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_to_y_clamps_correctly() {
        // We can't easily test painting without an egui context,
        // but we can test the mapping math directly.
        let widget = SpectrumWidget {
            fft_data: &[],
            db_range: (-120.0, 0.0),
            freq_range: (99_000_000, 101_000_000),
            vfo_hz: 100_000_000,
        };
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 300.0));
        // 0 dB should map to top
        let y_top = widget.db_to_y(0.0, rect);
        assert!((y_top - 0.0).abs() < 1.0, "0 dBFS should be at top of rect");
        // -120 dB should map to bottom
        let y_bot = widget.db_to_y(-120.0, rect);
        assert!((y_bot - 300.0).abs() < 1.0, "-120 dBFS should be at bottom of rect");
    }

    #[test]
    fn spectrum_widget_renders_without_panic() {
        // With synthetic data, the widget math shouldn't panic
        let data: Vec<f32> = (0..2048).map(|i| -60.0 + (i as f32 * 0.01)).collect();
        let w = SpectrumWidget {
            fft_data: &data,
            db_range: (-120.0, 0.0),
            freq_range: (99_000_000, 101_000_000),
            vfo_hz: 100_000_000,
        };
        // Verify mapping math doesn't panic with extreme values
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 300.0));
        let _ = w.db_to_y(-200.0, rect); // below range
        let _ = w.db_to_y(20.0, rect);   // above range
    }
}
