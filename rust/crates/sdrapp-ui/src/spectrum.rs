#![forbid(unsafe_code)]

//! Spectrum display widget.
//!
//! Features:
//! - Gradient-fill trace: bright at signal level, fading to dark at bottom (epaint::Mesh)
//! - Neon glow trace: 4 stacked line passes with decreasing width and increasing opacity
//! - Frequency axis (X) with labeled tick marks in MHz/kHz
//! - dBFS axis (Y) with horizontal gridlines at -20, -40, -60, -80, -100 dBFS
//! - VFO marker line with frequency label
//! - Peak-hold overlay: bright white line showing recent maximum per bin

use egui::epaint::{Mesh, Vertex};
use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui};

use crate::band_plan;
use crate::theme;

/// UV coordinate that samples the white texel from egui's font atlas.
/// Used for plain colored geometry rendered via Mesh.
const WHITE_UV: egui::Pos2 = egui::pos2(1.0, 1.0);

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
    /// Filter passband low edge in Hz (absolute). Drawn as a translucent overlay.
    /// Set to `vfo_hz` for symmetric demodulators; use asymmetric values for SSB.
    pub filter_lo_hz: u64,
    /// Filter passband high edge in Hz (absolute).
    pub filter_hi_hz: u64,
    /// Optional peak-hold buffer (same length as fft_data).
    pub peak_hold: Option<&'a [f32]>,
    /// Whether to draw the frequency band allocation overlay.
    pub show_band_plan: bool,
    /// Current mouse position in screen coordinates, if hovering over the widget.
    /// When Some, draws a crosshair + frequency/power readout at the cursor.
    pub hover_pos: Option<egui::Pos2>,
    /// Tuning step in Hz — draws small tick marks on the frequency axis so the
    /// operator can see the granularity of scroll-tuning at a glance.
    /// Pass 0 to suppress the step markers.
    pub tune_step_hz: u64,
    /// Frequencies (Hz) of detected signal peaks — pre-computed by center.rs.
    /// A small triangle marker is drawn above the spectrum trace at each position.
    /// Empty slice = no markers drawn.
    pub peak_marker_hz: &'a [u64],
}

impl<'a> SpectrumWidget<'a> {
    pub fn show(&self, ui: &mut Ui) -> Response {
        let available = ui.available_size();
        // Use hover-only so the parent (center.rs) owns all click/drag interactions.
        let (rect, response) = ui.allocate_exact_size(available, Sense::hover());

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
        // Draw a grid line every 10 dB; major lines (divisible by 20) are brighter.
        // Only draw lines that fall within the current display range.
        {
            let range_db = db_max - db_min;
            // Pick tick spacing: 5 dB for narrow ranges, 10 dB for normal, 20 dB for wide
            let tick_step = if range_db <= 40.0 {
                5.0_f32
            } else if range_db <= 100.0 {
                10.0
            } else {
                20.0
            };
            let first = (db_min / tick_step).ceil() as i32;
            let last = (db_max / tick_step).floor() as i32;
            for k in first..=last {
                let db = k as f32 * tick_step;
                let y = db_to_y(db, db_min, db_max, plot_rect);
                let is_major = (db as i32) % 20 == 0;
                let grid_color = if is_major {
                    Color32::from_rgba_unmultiplied(60, 80, 90, 200)
                } else {
                    Color32::from_rgba_unmultiplied(35, 50, 60, 130)
                };
                let line_w = if is_major { 1.0 } else { 0.5 };
                painter.line_segment(
                    [
                        Pos2::new(plot_rect.left(), y),
                        Pos2::new(plot_rect.right(), y),
                    ],
                    Stroke::new(line_w, grid_color),
                );
                painter.text(
                    Pos2::new(rect.left() + y_label_w - 4.0, y - 1.0),
                    egui::Align2::RIGHT_CENTER,
                    format!("{db:.0}"),
                    egui::FontId::proportional(9.0),
                    if is_major {
                        theme::TEXT_PRIMARY
                    } else {
                        theme::TEXT_MUTED
                    },
                );
            }
        }

        // ── Band allocation overlay ───────────────────────────────────────────
        // Drawn before the signal trace so bands appear behind the spectrum.
        if self.show_band_plan && freq_hi > freq_lo {
            let freq_span = freq_hi - freq_lo;
            for band in band_plan::USA {
                // Skip bands entirely outside the visible range.
                if band.end_hz as f64 <= freq_lo || band.start_hz as f64 >= freq_hi {
                    continue;
                }
                // Map band edges to pixel x coords, clamped to the plot rect.
                let x_lo = plot_rect.left()
                    + ((band.start_hz as f64 - freq_lo) / freq_span) as f32 * plot_rect.width();
                let x_hi = plot_rect.left()
                    + ((band.end_hz as f64 - freq_lo) / freq_span) as f32 * plot_rect.width();
                let x_lo = x_lo.max(plot_rect.left());
                let x_hi = x_hi.min(plot_rect.right());
                let visible_w = x_hi - x_lo;
                if visible_w < 1.5 {
                    continue; // too narrow to be visible
                }

                let band_rect = Rect::from_min_max(
                    Pos2::new(x_lo, plot_rect.top()),
                    Pos2::new(x_hi, plot_rect.bottom()),
                );

                // Semi-transparent fill
                painter.rect_filled(band_rect, 0.0, band.band_type.fill());

                // Top edge line for a cleaner look
                painter.line_segment(
                    [
                        Pos2::new(x_lo, plot_rect.top()),
                        Pos2::new(x_hi, plot_rect.top()),
                    ],
                    Stroke::new(1.0, band.band_type.accent()),
                );

                // Label — only if the visible slice is wide enough to hold text.
                if visible_w >= 28.0 {
                    let label_x = x_lo + visible_w * 0.5;
                    // Clip text so it doesn't bleed into adjacent bands.
                    let clip_rect = band_rect.intersect(plot_rect);
                    let clipped = painter.with_clip_rect(clip_rect);
                    clipped.text(
                        Pos2::new(label_x, plot_rect.top() + 3.0),
                        egui::Align2::CENTER_TOP,
                        band.name,
                        egui::FontId::proportional(8.5),
                        band.band_type.accent(),
                    );
                }
            }
        }

        // ── Plot border ───────────────────────────────────────────────────────
        painter.rect_stroke(plot_rect, 0.0, Stroke::new(1.0, theme::SEPARATOR));

        // ── Spectrum trace ────────────────────────────────────────────────────
        let n = self.fft_data.len();
        if n > 1 {
            // Build the trace points (pixel coordinates)
            let mut trace_pts: Vec<Pos2> = Vec::with_capacity(n);
            for (i, &db) in self.fft_data.iter().enumerate() {
                let x = plot_rect.left() + (i as f32 / (n - 1) as f32) * plot_rect.width();
                let y = db_to_y(db, db_min, db_max, plot_rect);
                trace_pts.push(Pos2::new(x, y));
            }

            // ── Gradient fill mesh ────────────────────────────────────────────
            // Top vertex at the signal level: bright teal, semi-transparent.
            // Bottom vertex: near-black, fully transparent.
            // Quads rendered as 2 triangles each.
            {
                let bottom_y = plot_rect.bottom();
                let fill_top = Color32::from_rgba_unmultiplied(15, 160, 120, 80);
                let fill_bot = Color32::TRANSPARENT;

                let mut mesh = Mesh::default();
                for (i, &pt) in trace_pts.iter().enumerate() {
                    mesh.vertices.push(Vertex {
                        pos: pt,
                        uv: WHITE_UV,
                        color: fill_top,
                    });
                    mesh.vertices.push(Vertex {
                        pos: Pos2::new(pt.x, bottom_y),
                        uv: WHITE_UV,
                        color: fill_bot,
                    });
                    if i > 0 {
                        let b = (i as u32) * 2;
                        // Triangle 1: prev_top, prev_bot, cur_top
                        // Triangle 2: prev_bot, cur_bot, cur_top
                        mesh.indices
                            .extend_from_slice(&[b - 2, b - 1, b, b - 1, b + 1, b]);
                    }
                }
                painter.add(egui::Shape::Mesh(mesh));
            }

            // ── Neon glow trace ───────────────────────────────────────────────
            // Four stacked passes: wide dim halo → narrow bright edge.
            let glow_layers: &[(f32, u8)] = &[
                (4.0, 12),  // wide halo
                (2.0, 40),  // inner glow
                (1.2, 130), // bright edge
                (0.6, 255), // sharp trace
            ];
            for &(width, alpha) in glow_layers {
                painter.add(egui::Shape::line(
                    trace_pts.clone(),
                    Stroke::new(width, Color32::from_rgba_unmultiplied(30, 215, 170, alpha)),
                ));
            }

            // ── Peak-hold line ────────────────────────────────────────────────
            if let Some(peak) = self.peak_hold {
                if peak.len() == n {
                    let peak_pts: Vec<Pos2> = peak
                        .iter()
                        .enumerate()
                        .map(|(i, &db)| {
                            let x =
                                plot_rect.left() + (i as f32 / (n - 1) as f32) * plot_rect.width();
                            Pos2::new(x, db_to_y(db, db_min, db_max, plot_rect))
                        })
                        .collect();
                    painter.add(egui::Shape::line(
                        peak_pts,
                        Stroke::new(0.75, Color32::from_rgba_unmultiplied(200, 255, 230, 100)),
                    ));
                }
            }
        }

        // ── Peak signal markers ───────────────────────────────────────────────
        // Small upward-pointing triangles above the trace at detected peak freqs.
        // Drawn before the filter overlay so the overlay tint doesn't occlude them.
        if !self.peak_marker_hz.is_empty() && freq_hi > freq_lo && n > 1 {
            let freq_span = freq_hi - freq_lo;
            let tri_h = 5.0_f32; // triangle height in pixels
            let tri_w = 6.0_f32; // triangle base half-width
            let marker_color = Color32::from_rgba_unmultiplied(80, 200, 255, 160);
            for &peak_hz in self.peak_marker_hz {
                if (peak_hz as f64) < freq_lo || (peak_hz as f64) > freq_hi {
                    continue;
                }
                let t = ((peak_hz as f64 - freq_lo) / freq_span) as f32;
                let cx = plot_rect.left() + t * plot_rect.width();

                // Look up trace Y at this position to seat the marker on the signal.
                let bin = (t * (n - 1) as f32).round() as usize;
                let bin = bin.min(n - 1);
                let trace_y = db_to_y(self.fft_data[bin], db_min, db_max, plot_rect);
                let tip_y = (trace_y - 3.0).max(plot_rect.top() + 1.0);

                // Upward triangle: tip at top, base below
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        Pos2::new(cx, tip_y),
                        Pos2::new(cx - tri_w * 0.5, tip_y + tri_h),
                        Pos2::new(cx + tri_w * 0.5, tip_y + tri_h),
                    ],
                    marker_color,
                    Stroke::NONE,
                ));
            }
        }

        // ── Filter passband overlay ───────────────────────────────────────────
        // Very subtle tint showing the demodulator's receive bandwidth.
        if freq_hi > freq_lo && self.filter_hi_hz > self.filter_lo_hz {
            let freq_span = freq_hi - freq_lo;
            let lo_t = ((self.filter_lo_hz as f64 - freq_lo) / freq_span).clamp(0.0, 1.0) as f32;
            let hi_t = ((self.filter_hi_hz as f64 - freq_lo) / freq_span).clamp(0.0, 1.0) as f32;
            let lo_x = plot_rect.left() + lo_t * plot_rect.width();
            let hi_x = plot_rect.left() + hi_t * plot_rect.width();
            if hi_x - lo_x >= 1.0 {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(lo_x, plot_rect.top()),
                        Pos2::new(hi_x, plot_rect.bottom()),
                    ),
                    0.0,
                    Color32::from_rgba_unmultiplied(0, 140, 220, 18),
                );
            }
        }

        // ── VFO line ─────────────────────────────────────────────────────────
        if freq_hi > freq_lo {
            let vfo_t = ((self.vfo_hz as f64 - freq_lo) / (freq_hi - freq_lo)) as f32;
            let vfo_x = plot_rect.left() + vfo_t.clamp(0.0, 1.0) * plot_rect.width();

            painter.line_segment(
                [
                    Pos2::new(vfo_x, plot_rect.top()),
                    Pos2::new(vfo_x, plot_rect.bottom()),
                ],
                Stroke::new(1.0, theme::VFO_LINE),
            );

            // VFO frequency label — rendered below the band-plan name row
            // (band names sit at top+3 with ~8.5px font; we start at top+14 to avoid overlap)
            let vfo_label = format_freq_short(self.vfo_hz);
            painter.text(
                Pos2::new(vfo_x + 4.0, plot_rect.top() + 14.0),
                egui::Align2::LEFT_TOP,
                vfo_label,
                egui::FontId::proportional(9.0),
                theme::ACCENT,
            );
        }

        // ── Tune-step markers ─────────────────────────────────────────────────
        // Small diamond ticks on the bottom of plot_rect at each tune_step_hz
        // interval from the VFO, showing scroll-tune granularity at a glance.
        if self.tune_step_hz > 0 && freq_hi > freq_lo {
            let step = self.tune_step_hz as f64;
            let freq_span = freq_hi - freq_lo;
            // First step position at or just below freq_lo
            let first_n = (freq_lo / step).floor() as i64;
            let last_n = (freq_hi / step).ceil() as i64;
            for n in first_n..=last_n {
                let step_freq = n as f64 * step;
                if step_freq < freq_lo || step_freq > freq_hi {
                    continue;
                }
                let x = plot_rect.left() + ((step_freq - freq_lo) / freq_span) as f32 * plot_rect.width();
                // Skip if too close to the VFO line (avoid clutter at center)
                let is_vfo = (step_freq - self.vfo_hz as f64).abs() < step * 0.1;
                let color = if is_vfo {
                    Color32::from_rgba_unmultiplied(100, 200, 255, 120)
                } else {
                    Color32::from_rgba_unmultiplied(150, 150, 180, 80)
                };
                // Small tick below the plot area
                painter.line_segment(
                    [
                        Pos2::new(x, plot_rect.bottom()),
                        Pos2::new(x, plot_rect.bottom() + 5.0),
                    ],
                    Stroke::new(if is_vfo { 1.5 } else { 1.0 }, color),
                );
            }
        }

        // ── Frequency grid lines + axis labels ───────────────────────────────
        // Lines are at absolute frequency intervals (not proportional screen positions)
        // so they slide past as you tune — giving a real "dial" feel.
        // The step size adapts to the visible bandwidth to maintain ~6-10 lines.
        if freq_hi > freq_lo {
            let span_hz = freq_hi - freq_lo;

            // Pick a round step that gives roughly 6-10 divisions.
            let raw_step = span_hz / 8.0;
            let magnitude = 10_f64.powf(raw_step.log10().floor());
            let nice_step = if raw_step / magnitude >= 5.0 {
                5.0 * magnitude
            } else if raw_step / magnitude >= 2.0 {
                2.0 * magnitude
            } else {
                magnitude
            };
            let step = nice_step.max(1.0);

            // First grid line at or just below freq_lo
            let first_n = (freq_lo / step).floor() as i64;
            let last_n  = (freq_hi / step).ceil()  as i64;

            for n in first_n..=last_n {
                let freq_hz = n as f64 * step;
                if freq_hz < freq_lo || freq_hz > freq_hi {
                    continue;
                }
                let t = ((freq_hz - freq_lo) / span_hz) as f32;
                let x = plot_rect.left() + t * plot_rect.width();

                // Major lines at every 5th step (round multiples of 5×step)
                let is_major = (n % 5) == 0;
                let grid_alpha: u8 = if is_major { 35 } else { 18 };
                let tick_alpha: u8 = if is_major { 120 } else { 60 };

                // Full-height vertical grid line
                painter.line_segment(
                    [
                        Pos2::new(x, plot_rect.top()),
                        Pos2::new(x, plot_rect.bottom()),
                    ],
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 160, 200, grid_alpha)),
                );

                // Short tick mark at the bottom edge
                painter.line_segment(
                    [
                        Pos2::new(x, plot_rect.bottom()),
                        Pos2::new(x, plot_rect.bottom() + 4.0),
                    ],
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(180, 200, 220, tick_alpha)),
                );

                // Frequency label — only on major lines (or first/last) to avoid crowding
                if is_major || n == first_n || n == last_n {
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

        // ── Hover crosshair + readout ─────────────────────────────────────────
        // Shows exact frequency and signal level at the cursor position.
        if let Some(hover) = self.hover_pos {
            if plot_rect.contains(hover) && freq_hi > freq_lo && n > 1 {
                let t = ((hover.x - plot_rect.left()) / plot_rect.width()).clamp(0.0, 1.0);

                // Dim vertical crosshair
                painter.line_segment(
                    [
                        Pos2::new(hover.x, plot_rect.top()),
                        Pos2::new(hover.x, plot_rect.bottom()),
                    ],
                    Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 50)),
                );

                // Frequency at cursor
                let cursor_hz = (freq_lo + t as f64 * (freq_hi - freq_lo)) as u64;

                // Signal level: look up nearest FFT bin
                let bin = (t * (n - 1) as f32).round() as usize;
                let db_val = self.fft_data[bin.min(n - 1)];

                let label = format!("{}   {:.0} dBFS", format_freq_cursor(cursor_hz), db_val);

                // Position readout: top-right of plot if cursor is in left half, else top-left.
                let (anchor, label_x) = if t < 0.5 {
                    (egui::Align2::LEFT_TOP, hover.x + 6.0)
                } else {
                    (egui::Align2::RIGHT_TOP, hover.x - 6.0)
                };

                // Dark background pill for readability
                let font = egui::FontId::monospace(10.0);
                let galley = painter.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE);
                let label_pos = match anchor {
                    egui::Align2::LEFT_TOP => Pos2::new(label_x, plot_rect.top() + 4.0),
                    _ => Pos2::new(label_x - galley.size().x, plot_rect.top() + 4.0),
                };
                let bg_rect = Rect::from_min_size(
                    Pos2::new(label_pos.x - 3.0, label_pos.y - 1.0),
                    galley.size() + egui::Vec2::new(6.0, 2.0),
                );
                painter.rect_filled(
                    bg_rect,
                    2.0,
                    Color32::from_rgba_unmultiplied(10, 13, 20, 210),
                );
                painter.text(
                    label_pos,
                    egui::Align2::LEFT_TOP,
                    label,
                    font,
                    Color32::WHITE,
                );

                // Horizontal dBFS dot on Y-axis
                let dot_y = db_to_y(db_val, db_min, db_max, plot_rect);
                painter.circle_filled(
                    Pos2::new(plot_rect.left() - 3.0, dot_y),
                    3.0,
                    Color32::from_rgba_unmultiplied(0, 210, 255, 200),
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

/// High-precision frequency label for the hover cursor readout.
fn format_freq_cursor(hz: u64) -> String {
    if hz >= 1_000_000_000 {
        format!("{:.4} GHz", hz as f64 / 1_000_000_000.0)
    } else if hz >= 1_000_000 {
        format!("{:.3} MHz", hz as f64 / 1_000_000.0)
    } else if hz >= 1_000 {
        format!("{:.1} kHz", hz as f64 / 1_000.0)
    } else {
        format!("{hz} Hz")
    }
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
    use egui::Vec2;

    #[test]
    fn db_to_y_clamps_correctly() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        let y_top = db_to_y(0.0, -120.0, 0.0, rect);
        assert!(
            (y_top - 0.0).abs() < 1.0,
            "0 dBFS should map to top: {y_top}"
        );

        let y_bot = db_to_y(-120.0, -120.0, 0.0, rect);
        assert!(
            (y_bot - 100.0).abs() < 1.0,
            "-120 dBFS should map to bottom: {y_bot}"
        );

        let y_over = db_to_y(10.0, -120.0, 0.0, rect);
        assert!(
            (y_over - 0.0).abs() < 1.0,
            "above 0 dBFS clamped to top: {y_over}"
        );
    }

    #[test]
    fn spectrum_widget_renders_without_panic() {
        let data: Vec<f32> = (0..2048)
            .map(|i| -60.0 + (i as f32 * 0.01).sin() * 20.0)
            .collect();
        let peak: Vec<f32> = (0..2048)
            .map(|i| -50.0 + (i as f32 * 0.01).sin() * 15.0)
            .collect();
        let ctx = egui::Context::default();
        ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                SpectrumWidget {
                    fft_data: &data,
                    db_range: (-120.0, 0.0),
                    freq_range: (95_000_000, 105_000_000),
                    vfo_hz: 100_000_000,
                    filter_lo_hz: 99_900_000,
                    filter_hi_hz: 100_100_000,
                    peak_hold: Some(&peak),
                    show_band_plan: true,
                    hover_pos: None,
                    tune_step_hz: 100_000,
                    peak_marker_hz: &[],
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
