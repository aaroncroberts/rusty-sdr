#![allow(clippy::too_many_lines)]

use egui::{RichText, Ui, Vec2};

use sdrapp_core::signal_path::{min_zoom_for_mode, DemodMode, DisplayCmd, ReceiverCmd};

use super::super::SdrApp;
use crate::{frequency::FrequencyWidget, knob::KnobWidget, spectrum::SpectrumWidget, theme};

impl SdrApp {
    pub(in crate::app) fn center_panel(&mut self, ui: &mut Ui) {
        let (fft_data, band_plan_enabled, snr_db, demod_mode, nfm_bw_hz, is_running, fft_clipping, peak_hold_enabled, peak_hold_decay_db, hw_center_freq, scanner_running, tune_step_hz, signal_level_dbfs) = {
            let s = self.shared.read();
            (
                s.fft.fft_magnitudes.clone(),
                s.fft.band_plan_enabled,
                s.fft.snr_db,
                s.demod.demod_mode,
                s.demod.nfm_bandwidth_hz,
                s.is_running,
                s.fft.fft_clipping_detected,
                s.fft.peak_hold_enabled,
                s.fft.peak_hold_decay_db,
                s.center_freq_hz,
                s.scanner.scan_running,
                s.demod.tune_step_hz,
                s.fft.signal_level_dbfs,
            )
        };

        // When the scanner is running it retunes hardware directly — sync the UI
        // display so the spectrum axis and frequency widget reflect the actual channel.
        if scanner_running && hw_center_freq != 0 && hw_center_freq != self.config.ui.frequency_hz {
            self.config.ui.frequency_hz = hw_center_freq;
            self.frequency_widget = FrequencyWidget::new(hw_center_freq);
        }

        let freq = self.config.ui.frequency_hz;
        let (span, zoom_level, waterfall_speed) = {
            let s = self.shared.read();
            let sr_half = s.sample_rate_sps as u64 / 2;
            // Apply zoom: zoom_level 1.0 = full hardware bandwidth, min per mode.
            // Prefer SharedState zoom when sample rate is known; fall back to config span_hz.
            let effective_span = if sr_half > 0 {
                let z = s.zoom_level.clamp(min_zoom_for_mode(s.demod.demod_mode), 1.0);
                (sr_half as f64 * z as f64) as u64
            } else {
                self.config.ui.span_hz
            };
            (effective_span, s.zoom_level, s.waterfall_speed)
        };

        // Update peak-hold buffer (size-tracks fft_magnitudes).
        // Only updates when peak_hold_enabled; uses decay rate from SharedState.
        let n = fft_data.len();
        if self.peak_hold.len() != n {
            self.peak_hold = vec![-120.0_f32; n];
        }
        let signal_active = is_running && fft_data.iter().any(|&v| v > -119.0);
        if signal_active {
            if peak_hold_enabled {
                for (ph, &v) in self.peak_hold.iter_mut().zip(fft_data.iter()) {
                    if v > *ph {
                        *ph = v;
                    } else {
                        *ph -= peak_hold_decay_db;
                    }
                }
            } else {
                // When disabled, keep buffer in sync so it starts fresh if re-enabled.
                for (ph, &v) in self.peak_hold.iter_mut().zip(fft_data.iter()) {
                    *ph = v;
                }
            }

            // Auto-ref: anchor ref_level to the noise floor so the full signal
            // range stays visible regardless of signal strength.
            // ref_level = noise_floor + dyn_range * 0.9  means the noise floor
            // sits at ~10% from the bottom of the display, and signals up to
            // 90% of dyn_range above the floor remain on-screen.
            if self.auto_ref && n >= 10 {
                let mut sorted = fft_data.to_vec();
                sorted
                    .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let floor_sample = sorted[n / 10];
                let ceil_sample = sorted[(n * 99 / 100).min(n - 1)];
                const ALPHA: f32 = 0.95;
                // On the very first frame of real data, snap immediately instead
                // of waiting ~2 seconds for the EMA to converge from the initial guess.
                if self.noise_floor_ema <= -84.9 {
                    self.noise_floor_ema = floor_sample;
                    self.signal_ceil_ema = ceil_sample;
                } else {
                    self.noise_floor_ema =
                        ALPHA * self.noise_floor_ema + (1.0 - ALPHA) * floor_sample;
                    self.signal_ceil_ema =
                        ALPHA * self.signal_ceil_ema + (1.0 - ALPHA) * ceil_sample;
                }
                // Place ref_level so the noise floor is ~10% up from the bottom.
                self.ref_level = (self.noise_floor_ema + self.dyn_range * 0.9).clamp(-120.0, 20.0);
            }
            let _db_floor = self.ref_level - self.dyn_range;
            let _db_ceil = self.ref_level;

            // Waterfall range: wf_level is the absolute dBFS floor (darkest colour);
            // wf_gain shifts that floor down to reveal weaker signals.
            // The ceiling is wf_level + 80 dB, giving a consistent 80 dB window.
            let wf_floor = self.wf_level - self.wf_gain;
            self.waterfall.set_db_range((wf_floor, self.wf_level + 80.0));
            // Fractional accumulator: push_row fires once per integer crossed.
            // Speed 1.0 = 1 row/frame, 2.0 = 2 rows/frame, 0.5 = every other frame.
            self.waterfall_row_frac += waterfall_speed.clamp(0.1, 10.0);
            while self.waterfall_row_frac >= 1.0 {
                self.waterfall.push_row(&fft_data);
                self.waterfall_row_frac -= 1.0;
            }
        }

        let db_floor = self.ref_level - self.dyn_range;
        let db_ceil = self.ref_level;
        let db_range = (db_floor, db_ceil);

        let available_h = ui.available_height();
        // Spectrum height driven by the user-adjustable split ratio.
        // Clamped so both spectrum (≥ 100 px) and waterfall (≥ 80 px) always have room.
        let spectrum_height = (available_h * self.config.ui.spectrum_split)
            .clamp(100.0, available_h - 80.0);

        // ── Spectrum ──────────────────────────────────────────────────────────
        let (spectrum_rect, spectrum_resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), spectrum_height),
            egui::Sense::click_and_drag(),
        );

        // Helper: map an X pixel position within spectrum_rect to a frequency.
        let x_to_freq = |x: f32| -> u64 {
            let t = ((x - spectrum_rect.left()) / spectrum_rect.width()).clamp(0.0, 1.0);
            let low = freq.saturating_sub(span) as f64;
            let high = freq as f64 + span as f64;
            (low + t as f64 * (high - low)).round() as u64
        };

        // Click-to-tune: point click sets VFO to clicked frequency.
        // Snap-to-peak: if a local FFT maximum is within ±5 bins of the click
        // AND is >3 dB stronger than the raw click bin, snap to the carrier.
        if spectrum_resp.clicked() {
            if let Some(click_pos) = spectrum_resp.interact_pointer_pos() {
                let raw_freq = x_to_freq(click_pos.x);
                let new_freq = if !fft_data.is_empty() && spectrum_rect.width() > 0.0 {
                    let n = fft_data.len();
                    let t = ((click_pos.x - spectrum_rect.left()) / spectrum_rect.width())
                        .clamp(0.0, 1.0);
                    let raw_bin = (t * (n - 1) as f32).round() as usize;
                    let lo = raw_bin.saturating_sub(5);
                    let hi = (raw_bin + 5).min(n - 1);
                    // Find the strongest bin in the search window
                    let (peak_bin, peak_db) = (lo..=hi).fold(
                        (raw_bin, fft_data[raw_bin]),
                        |(best_b, best_v), b| {
                            if fft_data[b] > best_v { (b, fft_data[b]) } else { (best_b, best_v) }
                        },
                    );
                    if peak_db > fft_data[raw_bin] + 3.0 {
                        // Snap to peak bin's center frequency
                        let peak_t = peak_bin as f64 / (n - 1) as f64;
                        let freq_lo = freq.saturating_sub(span) as f64;
                        let freq_hi = (freq + span) as f64;
                        (freq_lo + peak_t * (freq_hi - freq_lo)) as u64
                    } else {
                        raw_freq
                    }
                } else {
                    raw_freq
                };
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        // Drag-to-pan: dragging left/right shifts center frequency.
        // Hz per pixel = (2 * span) / width.  Negate: drag-right → lower freq displayed.
        if spectrum_resp.dragged() {
            let drag_dx = spectrum_resp.drag_delta().x;
            if drag_dx.abs() > 0.1 && spectrum_rect.width() > 0.0 {
                let hz_per_px = (2.0 * span as f64) / spectrum_rect.width() as f64;
                let delta_hz = (drag_dx as f64 * hz_per_px) as i64;
                let new_freq = if delta_hz < 0 {
                    freq.saturating_add((-delta_hz) as u64)
                } else {
                    freq.saturating_sub(delta_hz as u64).max(1)
                };
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        // Scroll on spectrum: tune frequency (step); Ctrl+scroll: zoom in/out
        let (scroll_delta, ctrl_held) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
        if spectrum_resp.hovered() && scroll_delta.abs() > 0.5 {
            if ctrl_held {
                // Ctrl+scroll → zoom
                let factor = if scroll_delta > 0.0 { 0.8_f32 } else { 1.25_f32 };
                let new_zoom = (zoom_level * factor).clamp(min_zoom_for_mode(demod_mode), 1.0);
                let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(new_zoom).into());
                self.config.ui.zoom_level = new_zoom;
                self.config_dirty = true;
            } else {
                // Plain scroll → step-tune frequency
                let step = self.shared.read().demod.tune_step_hz;
                let new_freq = if scroll_delta > 0.0 {
                    freq.saturating_add(step)
                } else {
                    freq.saturating_sub(step).max(1)
                };
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        let ctx = ui.ctx().clone();
        // Capture hover position over the spectrum rect for the crosshair readout.
        let spectrum_hover = ctx
            .pointer_hover_pos()
            .filter(|p| spectrum_rect.contains(*p));
        let mut spectrum_ui = ui.new_child(egui::UiBuilder::new().max_rect(spectrum_rect));
        let peak_ref: Option<&[f32]> =
            if peak_hold_enabled && self.peak_hold.len() == fft_data.len() {
                Some(&self.peak_hold)
            } else {
                None
            };
        // Compute filter passband bounds — only shown when live signal is active.
        // For symmetric modes, the filter spans ±bw/2 around vfo_hz.
        // For SSB, only one sideband is used.
        let (filter_lo_hz, filter_hi_hz) = {
            use DemodMode::*;
            let half = |hz: u64| (freq.saturating_sub(hz / 2), freq + hz / 2);
            match demod_mode {
                Wbfm => half(200_000),
                Nfm => half(nfm_bw_hz as u64),
                Am => half(10_000),
                Dsb => half(6_000),
                Usb => (freq, freq + 3_000),
                Lsb => (freq.saturating_sub(3_000), freq),
                Cw => (freq.saturating_sub(400), freq + 400),
            }
        };
        // Only show filter overlay when streaming (suppress when paused/stopped).
        let (show_filter_lo, show_filter_hi) = if is_running {
            (filter_lo_hz, filter_hi_hz)
        } else {
            (freq, freq) // zero-width → hidden
        };

        // ── Peak detection (once per frame) ──────────────────────────────────
        // Finds local maxima above the noise floor and provides them to both the
        // SpectrumWidget (for triangle markers) and the click-to-tune snap logic.
        //
        // Algorithm:
        //  1. Noise floor estimate = 20th-percentile bin (robust against signals)
        //  2. A bin is a peak if: value > both neighbours AND value > floor + 8 dB
        //  3. Minimum inter-peak spacing = n/32 bins (prevents marking every ripple)
        let freq_lo_hz = freq.saturating_sub(span) as f64;
        let freq_hi_hz = (freq + span) as f64;
        let peak_freqs_hz: Vec<u64> = if is_running && fft_data.len() > 2 {
            let n = fft_data.len();
            // Noise floor: 20th-percentile of bins
            let mut sorted = fft_data.clone();
            sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let noise_floor = sorted[n / 5];
            let threshold = noise_floor + 8.0;
            let min_spacing = (n / 32).max(2);

            let mut peaks: Vec<u64> = Vec::new();
            let mut last_peak_bin: Option<usize> = None;
            for i in 1..n - 1 {
                if fft_data[i] >= threshold
                    && fft_data[i] > fft_data[i - 1]
                    && fft_data[i] >= fft_data[i + 1]
                {
                    // Enforce minimum spacing
                    if last_peak_bin.is_none_or(|lb| i - lb >= min_spacing) {
                        let t = i as f64 / (n - 1) as f64;
                        let hz = (freq_lo_hz + t * (freq_hi_hz - freq_lo_hz)) as u64;
                        peaks.push(hz);
                        last_peak_bin = Some(i);
                    }
                }
            }
            peaks
        } else {
            vec![]
        };

        SpectrumWidget {
            fft_data: &fft_data,
            db_range,
            freq_range: (freq.saturating_sub(span), freq + span),
            vfo_hz: freq,
            filter_lo_hz: show_filter_lo,
            filter_hi_hz: show_filter_hi,
            peak_hold: peak_ref,
            show_band_plan: band_plan_enabled,
            hover_pos: spectrum_hover,
            tune_step_hz,
            peak_marker_hz: &peak_freqs_hz,
        }
        .show(&mut spectrum_ui);

        // ── ADC saturation badge ──────────────────────────────────────────────
        // Track the last time clipping was detected and hold the badge for 2 s.
        let now = ui.ctx().input(|i| i.time);
        if fft_clipping {
            self.last_clipping_time = Some(now);
        }
        if let Some(t) = self.last_clipping_time {
            if now - t < 2.0 {
                // Overlay "ADC SAT" in the top-right corner of the spectrum rect.
                let painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("adc_sat_badge"),
                ));
                let badge_w = 62.0_f32;
                let badge_h = 16.0_f32;
                let badge_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(
                        spectrum_rect.right() - badge_w - 4.0,
                        spectrum_rect.top() + 4.0,
                    ),
                    egui::Vec2::new(badge_w, badge_h),
                );
                painter.rect_filled(
                    badge_rect,
                    2.0,
                    egui::Color32::from_rgb(200, 40, 40),
                );
                painter.text(
                    badge_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "ADC SAT",
                    egui::FontId::proportional(10.0),
                    egui::Color32::WHITE,
                );
                ui.ctx().request_repaint();
            } else {
                self.last_clipping_time = None;
            }
        }

        // ── S-meter: signal strength bar ─────────────────────────────────────
        // Shows peak dBFS within the current filter passband. Only rendered
        // when the signal path is running so it doesn't mislead on idle startup.
        if is_running {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("SIG")
                        .color(theme::TEXT_MUTED)
                        .small()
                        .monospace(),
                );
                // Bar occupies the remaining width minus the dBFS label on the right.
                let dbfs_label = format!("{:+.0} dBFS", signal_level_dbfs.max(-120.0));
                let label_w = 58.0_f32;
                let bar_w = (ui.available_width() - label_w - 6.0).max(20.0);
                let bar_h = 8.0_f32;
                let (bar_rect, _) = ui.allocate_exact_size(
                    egui::Vec2::new(bar_w, bar_h),
                    egui::Sense::hover(),
                );
                if ui.is_rect_visible(bar_rect) {
                    let painter = ui.painter();
                    // Dark background track
                    painter.rect_filled(bar_rect, 2.0, egui::Color32::from_rgb(18, 24, 30));
                    // Filled portion: proportion = (level - floor) / range
                    // We map -120 dBFS → 0% width, 0 dBFS → 100% width.
                    let fill = ((signal_level_dbfs + 120.0) / 120.0).clamp(0.0, 1.0);
                    let fill_w = fill * bar_rect.width();
                    if fill_w >= 1.0 {
                        let fill_color = if signal_level_dbfs >= -60.0 {
                            egui::Color32::from_rgb(30, 200, 80)   // green — strong signal
                        } else if signal_level_dbfs >= -80.0 {
                            egui::Color32::from_rgb(210, 170, 0)   // yellow — marginal
                        } else {
                            egui::Color32::from_rgb(70, 85, 100)   // gray — noise floor
                        };
                        let fill_rect = egui::Rect::from_min_size(
                            bar_rect.min,
                            egui::Vec2::new(fill_w, bar_rect.height()),
                        );
                        painter.rect_filled(fill_rect, 2.0, fill_color);
                    }
                    // -60 dBFS threshold marker (green/yellow boundary)
                    let marker_t = 60.0 / 120.0;
                    let mx = bar_rect.left() + marker_t * bar_rect.width();
                    painter.line_segment(
                        [
                            egui::Pos2::new(mx, bar_rect.top()),
                            egui::Pos2::new(mx, bar_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
                    );
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(dbfs_label)
                        .color(if signal_level_dbfs >= -60.0 {
                            egui::Color32::from_rgb(30, 200, 80)
                        } else if signal_level_dbfs >= -80.0 {
                            egui::Color32::from_rgb(210, 170, 0)
                        } else {
                            theme::TEXT_MUTED
                        })
                        .small()
                        .monospace(),
                );
            });
        }

        // ── Display controls toolbar ──────────────────────────────────────────
        ui.add_space(3.0);

        // Bandwidth label derived from current zoom
        let bw_hz = (span * 2) as f64;
        let bw_label = if bw_hz >= 1_000_000.0 {
            format!("{:.2} MHz", bw_hz / 1_000_000.0)
        } else {
            format!("{:.0} kHz", bw_hz / 1_000.0)
        };

        // Knob row: REF · RANGE · WF GAIN · ZOOM · WF SPD
        // MIDI Learn state (read before closure to avoid split-borrow issues)
        let (zoom_learn, zoom_cc) = {
            let s = self.shared.read();
            (
                s.midi_learn_target.as_deref() == Some("zoom"),
                s.midi_cc_to_knob
                    .iter()
                    .find(|(_, v)| v.as_str() == "zoom")
                    .map(|(&c, _)| c),
            )
        };
        let (wfspd_learn, wfspd_cc) = {
            let s = self.shared.read();
            (
                s.midi_learn_target.as_deref() == Some("wf_speed"),
                s.midi_cc_to_knob
                    .iter()
                    .find(|(_, v)| v.as_str() == "wf_speed")
                    .map(|(&c, _)| c),
            )
        };
        // Action flags: set by context_menu closures, acted on after ui.horizontal returns
        let mut zoom_learn_req = false;
        let mut zoom_learn_cancel = false;
        let mut zoom_clear: Option<u8> = None;
        let mut wfspd_learn_req = false;
        let mut wfspd_learn_cancel = false;
        let mut wfspd_clear: Option<u8> = None;

        // Auto toggle + Zoom step buttons flank the knob row.
        ui.horizontal(|ui| {
            // Auto ref toggle (compact)
            let auto_color = if self.auto_ref { theme::ACCENT } else { theme::TEXT_MUTED };
            if ui.small_button(RichText::new(if self.auto_ref { "A*" } else { "A" }).color(auto_color))
                .on_hover_text("Auto ref: tracks signal ceiling automatically. Click to toggle manual override.")
                .clicked()
            {
                self.auto_ref = !self.auto_ref;
            }

            // Ref Level knob
            let mut rl = self.ref_level;
            let ref_resp = KnobWidget {
                value: &mut rl,
                range: -120.0_f32..=20.0_f32,
                default_value: -30.0,
                step: 2.0,
                diameter: 40.0,
                label: Some("REF"),
                unit: "dB",
                midi_cc: None,
                learn_active: false,
            }.show(ui);
            if ref_resp.changed() {
                self.ref_level = rl;
                self.auto_ref = false;
            }

            // Dynamic Range knob
            let mut dr = self.dyn_range;
            let range_resp = KnobWidget {
                value: &mut dr,
                range: 20.0_f32..=160.0_f32,
                default_value: 60.0,
                step: 5.0,
                diameter: 40.0,
                label: Some("RANGE"),
                unit: "dB",
                midi_cc: None,
                learn_active: false,
            }.show(ui);
            if range_resp.changed() {
                self.dyn_range = dr;
            }

            // WF Gain knob
            let mut wg = self.wf_gain;
            let wfg_resp = KnobWidget {
                value: &mut wg,
                range: -40.0_f32..=40.0_f32,
                default_value: 0.0,
                step: 2.0,
                diameter: 40.0,
                label: Some("WF GAIN"),
                unit: "dB",
                midi_cc: None,
                learn_active: false,
            }.show(ui);
            if wfg_resp.changed() {
                self.wf_gain = wg;
            }

            // Zoom knob + In/Out/Full step buttons
            let min_zoom = min_zoom_for_mode(demod_mode);
            let mut z = zoom_level;
            let zoom_resp = KnobWidget {
                value: &mut z,
                range: min_zoom..=1.0_f32,
                default_value: 1.0,
                step: 0.05,
                diameter: 40.0,
                label: Some("ZOOM"),
                unit: "",
                midi_cc: zoom_cc,
                learn_active: zoom_learn,
            }.show(ui);
            zoom_resp.context_menu(|ui| {
                if zoom_learn {
                    if ui.button("Cancel MIDI Learn").clicked() {
                        zoom_learn_cancel = true;
                        ui.close_menu();
                    }
                } else if ui.button("Assign MIDI CC").clicked() {
                    zoom_learn_req = true;
                    ui.close_menu();
                }
                if let Some(cc) = zoom_cc {
                    if ui.button(format!("Clear CC {cc} binding")).clicked() {
                        zoom_clear = Some(cc);
                        ui.close_menu();
                    }
                }
            });
            if zoom_resp.changed() {
                let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(z).into());
                self.config.ui.zoom_level = z;
                self.config_dirty = true;
            }
            ui.vertical(|ui| {
                if ui.small_button(RichText::new("In").color(theme::TEXT_MUTED))
                    .on_hover_text("Zoom in (Ctrl+scroll up)")
                    .clicked()
                {
                    let new_z = (zoom_level / 1.5).clamp(min_zoom, 1.0);
                    let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(new_z).into());
                    self.config.ui.zoom_level = new_z;
                    self.config_dirty = true;
                }
                if ui.small_button(RichText::new("Full").color(theme::ACCENT))
                    .on_hover_text("Show full hardware bandwidth")
                    .clicked()
                {
                    let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(1.0).into());
                    self.config.ui.zoom_level = 1.0;
                    self.config_dirty = true;
                }
                if ui.small_button(RichText::new("Out").color(theme::TEXT_MUTED))
                    .on_hover_text("Zoom out (Ctrl+scroll down)")
                    .clicked()
                {
                    let new_z = (zoom_level * 1.5).clamp(min_zoom, 1.0);
                    let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(new_z).into());
                    self.config.ui.zoom_level = new_z;
                    self.config_dirty = true;
                }
            });
            ui.label(RichText::new(&bw_label).color(theme::ACCENT).small())
                .on_hover_text("Displayed bandwidth (zoom × hardware bandwidth)");

            // Waterfall Speed knob
            let mut ws = waterfall_speed;
            let wfspd_resp = KnobWidget {
                value: &mut ws,
                range: 0.1_f32..=8.0_f32,
                default_value: 1.0,
                step: 0.2,
                diameter: 40.0,
                label: Some("WF SPD"),
                unit: "×",
                midi_cc: wfspd_cc,
                learn_active: wfspd_learn,
            }.show(ui);
            wfspd_resp.context_menu(|ui| {
                if wfspd_learn {
                    if ui.button("Cancel MIDI Learn").clicked() {
                        wfspd_learn_cancel = true;
                        ui.close_menu();
                    }
                } else if ui.button("Assign MIDI CC").clicked() {
                    wfspd_learn_req = true;
                    ui.close_menu();
                }
                if let Some(cc) = wfspd_cc {
                    if ui.button(format!("Clear CC {cc} binding")).clicked() {
                        wfspd_clear = Some(cc);
                        ui.close_menu();
                    }
                }
            });
            if wfspd_resp.changed() {
                let _ = self.cmd_tx.try_send(DisplayCmd::SetWaterfallSpeed(ws).into());
                self.config.ui.waterfall_speed = ws;
                self.config_dirty = true;
            }
        });
        // Handle MIDI Learn actions deferred from the horizontal closure
        if zoom_learn_req {
            self.shared.write().midi_learn_target = Some("zoom".into());
        }
        if zoom_learn_cancel {
            self.shared.write().midi_learn_target = None;
        }
        if let Some(cc) = zoom_clear {
            self.shared.write().midi_cc_to_knob.remove(&cc);
            self.config_dirty = true;
        }
        if wfspd_learn_req {
            self.shared.write().midi_learn_target = Some("wf_speed".into());
        }
        if wfspd_learn_cancel {
            self.shared.write().midi_learn_target = None;
        }
        if let Some(cc) = wfspd_clear {
            self.shared.write().midi_cc_to_knob.remove(&cc);
            self.config_dirty = true;
        }

        // Row 3: FFT size, window, averaging, band plan, SNR
        ui.add_space(1.0);
        ui.horizontal(|ui| {
            let (cur_fft_size, cur_fft_window, cur_fft_avg) = {
                let s = self.shared.read();
                (s.fft.fft_size, s.fft.fft_window, s.fft.fft_averaging)
            };

            // FFT size dropdown
            ui.label(RichText::new("FFT").color(theme::TEXT_MUTED).small());
            egui::ComboBox::from_id_salt("fft_size")
                .selected_text(RichText::new(cur_fft_size.to_string()).small())
                .width(52.0)
                .show_ui(ui, |ui| {
                    for sz in [512_usize, 1024, 2048, 4096, 8192] {
                        let sel = cur_fft_size == sz;
                        if ui.selectable_label(sel, sz.to_string()).clicked() && !sel {
                            let _ = self.cmd_tx.try_send(DisplayCmd::SetFftSize(sz).into());
                            self.config.ui.fft_size = sz;
                            self.config_dirty = true;
                        }
                    }
                });

            // Window function dropdown
            ui.label(RichText::new("Win").color(theme::TEXT_MUTED).small());
            egui::ComboBox::from_id_salt("fft_window")
                .selected_text(RichText::new(cur_fft_window.label()).small())
                .width(68.0)
                .show_ui(ui, |ui| {
                    use sdrapp_core::dsp::FftWindow;
                    for wf in [
                        FftWindow::Rectangular,
                        FftWindow::Hann,
                        FftWindow::Hamming,
                        FftWindow::BlackmanHarris,
                    ] {
                        let sel = cur_fft_window == wf;
                        if ui.selectable_label(sel, wf.label()).clicked() && !sel {
                            let _ = self.cmd_tx.try_send(DisplayCmd::SetFftWindow(wf).into());
                            self.config.ui.fft_window = format!("{wf:?}");
                            self.config_dirty = true;
                        }
                    }
                });

            // Averaging slider (1–16)
            ui.label(RichText::new("Avg").color(theme::TEXT_MUTED).small());
            let mut avg = cur_fft_avg as i32;
            if ui
                .add(egui::Slider::new(&mut avg, 1..=16).show_value(true))
                .on_hover_text(
                    "FFT averaging: frames blended via exponential moving average. 1 = off.",
                )
                .changed()
            {
                let _ = self
                    .cmd_tx
                    .try_send(DisplayCmd::SetFftAveraging(avg as u8).into());
                self.config.ui.fft_averaging = avg as u8;
                self.config_dirty = true;
            }

            // Band plan toggle
            let bp_color = if band_plan_enabled {
                theme::ACCENT
            } else {
                theme::TEXT_MUTED
            };
            if ui
                .small_button(RichText::new("BP").color(bp_color))
                .on_hover_text("Toggle band plan overlay on spectrum")
                .clicked()
            {
                let new_val = !band_plan_enabled;
                let _ = self
                    .cmd_tx
                    .try_send(DisplayCmd::SetBandPlanEnabled(new_val).into());
                self.config.ui.band_plan_enabled = new_val;
                self.config_dirty = true;
            }

            // Peak-hold toggle + decay slider
            let ph_color = if peak_hold_enabled {
                theme::ACCENT
            } else {
                theme::TEXT_MUTED
            };
            if ui
                .small_button(RichText::new("PH").color(ph_color))
                .on_hover_text("Toggle spectrum peak-hold line")
                .clicked()
            {
                let new_val = !peak_hold_enabled;
                let _ = self
                    .cmd_tx
                    .try_send(DisplayCmd::SetPeakHoldEnabled(new_val).into());
                self.config_dirty = true;
            }
            if peak_hold_enabled {
                let mut decay = peak_hold_decay_db;
                let resp = ui
                    .add(
                        egui::Slider::new(&mut decay, 0.1_f32..=2.0_f32)
                            .step_by(0.1)
                            .text(RichText::new("dB/fr").small())
                            .clamping(egui::SliderClamping::Always),
                    )
                    .on_hover_text("Peak-hold decay rate in dB per display frame");
                if resp.changed() {
                    let _ = self
                        .cmd_tx
                        .try_send(DisplayCmd::SetPeakHoldDecay(decay).into());
                }
            }

            // SNR display
            if let Some(snr) = snr_db {
                let snr_color = if snr > 20.0 {
                    theme::STATUS_OK
                } else if snr > 10.0 {
                    theme::AMBER
                } else {
                    theme::TEXT_MUTED
                };
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("SNR {:.0} dB", snr))
                        .color(snr_color)
                        .small(),
                )
                .on_hover_text("Estimated signal-to-noise ratio in the active demod channel");
            }

            // Waterfall visibility controls
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Colormap picker
                let colormap_label = self.config.ui.waterfall_colormap.as_str();
                egui::ComboBox::from_id_salt("wf_colormap")
                    .selected_text(RichText::new(colormap_label).small())
                    .width(72.0)
                    .show_ui(ui, |ui| {
                        for name in ["Thermal", "Inferno", "Grayscale", "Classic"] {
                            let sel = self.config.ui.waterfall_colormap == name;
                            if ui.selectable_label(sel, name).clicked() && !sel {
                                let cm: crate::theme::WaterfallColormap = match name {
                                    "Grayscale" => crate::theme::WaterfallColormap::Grayscale,
                                    "Inferno" => crate::theme::WaterfallColormap::Inferno,
                                    "Classic" => crate::theme::WaterfallColormap::Classic,
                                    _ => crate::theme::WaterfallColormap::Thermal,
                                };
                                self.waterfall.set_colormap(cm.build());
                                self.config.ui.waterfall_colormap = name.into();
                                self.config_dirty = true;
                            }
                        }
                    });
                ui.label(RichText::new("Palette").color(theme::TEXT_MUTED).small());

                ui.add_space(8.0);

                // WF Level slider
                let mut wf_lv = self.wf_level;
                let wf_resp = ui
                    .add(
                        egui::Slider::new(&mut wf_lv, -120.0_f32..=-40.0_f32)
                            .step_by(1.0)
                            .text(RichText::new("WF Level").small())
                            .clamping(egui::SliderClamping::Always),
                    )
                    .on_hover_text("Waterfall floor level in dBFS — shift down to reveal weaker signals");
                if wf_resp.changed() {
                    self.wf_level = wf_lv;
                    self.config.ui.wf_level = wf_lv;
                    self.config_dirty = true;
                }
            });
        });

        // ── Resizable split divider ───────────────────────────────────────────
        let divider_w = ui.available_width();
        let (div_rect, div_resp) = ui.allocate_exact_size(
            Vec2::new(divider_w, 6.0),
            egui::Sense::drag(),
        );
        if div_resp.hovered() || div_resp.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if div_resp.dragged() {
            let dy = div_resp.drag_delta().y;
            if available_h > 0.0 {
                let new_split = self.config.ui.spectrum_split + dy / available_h;
                self.config.ui.spectrum_split = new_split.clamp(0.15, 0.80);
                self.config_dirty = true;
            }
        }
        if ui.is_rect_visible(div_rect) {
            let mid_y = div_rect.center().y;
            let painter = ui.painter();
            let line_color = if div_resp.hovered() || div_resp.dragged() {
                theme::ACCENT
            } else {
                theme::TEXT_DISABLED
            };
            painter.line_segment(
                [
                    egui::pos2(div_rect.left(), mid_y),
                    egui::pos2(div_rect.right(), mid_y),
                ],
                egui::Stroke::new(1.0, line_color),
            );
            // Grip dots at centre
            for i in [-12.0_f32, -6.0, 0.0, 6.0, 12.0] {
                painter.circle_filled(
                    egui::pos2(div_rect.center().x + i, mid_y),
                    1.5,
                    theme::TEXT_MUTED,
                );
            }
        }

        // ── Waterfall ─────────────────────────────────────────────────────────
        let waterfall_resp = self.waterfall.show(ui, &ctx);

        // ── Waterfall hover crosshair + frequency tooltip ─────────────────────
        {
            let wf_rect = waterfall_resp.rect;
            if let Some(hover_pos) = ctx.pointer_hover_pos() {
                if wf_rect.contains(hover_pos) {
                    let t = ((hover_pos.x - wf_rect.left()) / wf_rect.width()).clamp(0.0, 1.0);
                    let low = freq.saturating_sub(span) as f64;
                    let high = freq as f64 + span as f64;
                    let hover_hz = (low + t as f64 * (high - low)).round() as u64;
                    let painter = ui.painter_at(wf_rect);
                    // Vertical crosshair line
                    painter.line_segment(
                        [
                            egui::pos2(hover_pos.x, wf_rect.top()),
                            egui::pos2(hover_pos.x, wf_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 80)),
                    );
                    // Frequency label just above the cursor
                    let mhz = hover_hz as f64 / 1_000_000.0;
                    let label = format!("{:.4} MHz", mhz);
                    let font = egui::FontId::proportional(11.0);
                    let label_pos = egui::pos2(hover_pos.x + 4.0, wf_rect.top() + 4.0);
                    painter.text(
                        label_pos,
                        egui::Align2::LEFT_TOP,
                        &label,
                        font,
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200),
                    );
                }
            }
        }

        // Click-to-tune on waterfall with snap-to-peak (same logic as spectrum)
        if waterfall_resp.clicked() {
            if let Some(click_pos) = waterfall_resp.interact_pointer_pos() {
                let wf_rect = waterfall_resp.rect;
                let t = ((click_pos.x - wf_rect.left()) / wf_rect.width()).clamp(0.0, 1.0);
                let low = freq.saturating_sub(span) as f64;
                let high = freq as f64 + span as f64;

                // Snap-to-peak: search ±5 bins around the click for a local FFT maximum
                let new_freq = if !fft_data.is_empty() && wf_rect.width() > 0.0 {
                    let n = fft_data.len();
                    let raw_bin = (t * (n - 1) as f32).round() as usize;
                    let lo = raw_bin.saturating_sub(5);
                    let hi = (raw_bin + 5).min(n - 1);
                    let (peak_bin, peak_db) = (lo..=hi).fold(
                        (raw_bin, fft_data[raw_bin]),
                        |(best_b, best_v), b| {
                            if fft_data[b] > best_v {
                                (b, fft_data[b])
                            } else {
                                (best_b, best_v)
                            }
                        },
                    );
                    let snapped = peak_db > fft_data[raw_bin] + 3.0;
                    let bin = if snapped { peak_bin } else { raw_bin };
                    let bin_t = bin as f64 / (n - 1) as f64;
                    (low + bin_t * (high - low)).round() as u64
                } else {
                    (low + t as f64 * (high - low)).round() as u64
                };

                let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        // Drag-to-pan on waterfall
        if waterfall_resp.dragged() {
            let drag_dx = waterfall_resp.drag_delta().x;
            if drag_dx.abs() > 0.1 {
                let rect = waterfall_resp.rect;
                let hz_per_px = (2.0 * span as f64) / rect.width() as f64;
                let delta_hz = (drag_dx as f64 * hz_per_px) as i64;
                let new_freq = if delta_hz < 0 {
                    freq.saturating_add((-delta_hz) as u64)
                } else {
                    freq.saturating_sub(delta_hz as u64).max(1)
                };
                let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(new_freq).into());
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }

        // Scroll on waterfall: tune (plain) or zoom (Ctrl)
        let wf_scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if waterfall_resp.hovered() && wf_scroll.abs() > 0.5 {
            let (wf_scroll_delta, wf_ctrl) =
                ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
            if wf_ctrl {
                let factor = if wf_scroll_delta > 0.0 {
                    0.8_f32
                } else {
                    1.25_f32
                };
                let new_z = (zoom_level * factor).clamp(0.005, 1.0);
                let _ = self.cmd_tx.try_send(DisplayCmd::SetZoom(new_z).into());
                self.config.ui.zoom_level = new_z;
                self.config_dirty = true;
            } else {
                let step = self.shared.read().demod.tune_step_hz;
                let new_freq = if wf_scroll_delta > 0.0 {
                    freq.saturating_add(step)
                } else {
                    freq.saturating_sub(step).max(1)
                };
                let _ = self
                    .cmd_tx
                    .try_send(ReceiverCmd::SetFrequency(new_freq).into());
                self.config.ui.frequency_hz = new_freq;
                self.frequency_widget = FrequencyWidget::new(new_freq);
                self.config_dirty = true;
            }
        }
    }
}
