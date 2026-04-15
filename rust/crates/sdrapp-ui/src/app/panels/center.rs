#![allow(clippy::too_many_lines)]

use egui::{RichText, Ui, Vec2};

use sdrapp_core::signal_path::{DemodMode, DisplayCmd, ReceiverCmd};

use super::super::SdrApp;
use crate::{frequency::FrequencyWidget, knob::KnobWidget, spectrum::SpectrumWidget, theme};

impl SdrApp {
    pub(in crate::app) fn center_panel(&mut self, ui: &mut Ui) {
        let (fft_data, band_plan_enabled, snr_db, demod_mode, nfm_bw_hz, is_running) = {
            let s = self.shared.read();
            (
                s.fft.fft_magnitudes.clone(),
                s.fft.band_plan_enabled,
                s.fft.snr_db,
                s.demod.demod_mode,
                s.demod.nfm_bandwidth_hz,
                s.is_running,
            )
        };

        let freq = self.config.ui.frequency_hz;
        let (span, zoom_level, waterfall_speed) = {
            let s = self.shared.read();
            let sr_half = s.sample_rate_sps as u64 / 2;
            // Apply zoom: zoom_level 1.0 = full hardware bandwidth, 0.05 = tightest zoom.
            // Prefer SharedState zoom when sample rate is known; fall back to config span_hz.
            let effective_span = if sr_half > 0 {
                let z = s.zoom_level.clamp(0.005, 1.0);
                (sr_half as f64 * z as f64) as u64
            } else {
                self.config.ui.span_hz
            };
            (effective_span, s.zoom_level, s.waterfall_speed)
        };

        // Update peak-hold: expand/shrink buffer with FFT size, then take max
        // per bin with a slow decay (≈ -0.5 dB/frame at 30fps = ~15 dB/s)
        let n = fft_data.len();
        if self.peak_hold.len() != n {
            self.peak_hold = vec![-120.0_f32; n];
        }
        let signal_active = is_running && fft_data.iter().any(|&v| v > -119.0);
        if signal_active {
            for (ph, &v) in self.peak_hold.iter_mut().zip(fft_data.iter()) {
                if v > *ph {
                    *ph = v;
                } else {
                    *ph -= 0.5; // decay per frame
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
            let db_floor = self.ref_level - self.dyn_range;
            let db_ceil = self.ref_level;

            // Waterfall range: floor is shifted down by wf_gain to reveal weaker signals;
            // ceiling has 30 dB of fixed headroom above the spectrum ceiling so strong
            // signals don't saturate to white.
            self.waterfall
                .set_db_range((db_floor - self.wf_gain, db_ceil + 30.0));
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
        // Spectrum gets a fixed portion; waterfall fills the rest
        let spectrum_height = (available_h * 0.36).clamp(120.0, 380.0);

        // ── Spectrum ──────────────────────────────────────────────────────────
        let (spectrum_rect, spectrum_resp) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), spectrum_height),
            egui::Sense::click(),
        );

        // Click-to-tune: map click X to frequency
        if let Some(click_pos) = spectrum_resp.interact_pointer_pos() {
            if spectrum_resp.clicked() {
                let t =
                    ((click_pos.x - spectrum_rect.left()) / spectrum_rect.width()).clamp(0.0, 1.0);
                let low = freq.saturating_sub(span) as f64;
                let high = freq as f64 + span as f64;
                let new_freq = (low + t as f64 * (high - low)).round() as u64;
                let _ = self
                    .cmd_tx
                    .try_send(ReceiverCmd::SetFrequency(new_freq).into());
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
                let factor = if scroll_delta > 0.0 {
                    0.8_f32
                } else {
                    1.25_f32
                };
                let new_zoom = (zoom_level * factor).clamp(0.005, 1.0);
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
                let _ = self
                    .cmd_tx
                    .try_send(ReceiverCmd::SetFrequency(new_freq).into());
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
        let peak_ref: Option<&[f32]> = if self.peak_hold.len() == fft_data.len() {
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
        }
        .show(&mut spectrum_ui);

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
            if ui.small_button(RichText::new(if self.auto_ref { "A✓" } else { "A" }).color(auto_color))
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
            let mut z = zoom_level;
            let zoom_resp = KnobWidget {
                value: &mut z,
                range: 0.005_f32..=1.0_f32,
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
                    let new_z = (zoom_level / 1.5).clamp(0.005, 1.0);
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
                    let new_z = (zoom_level * 1.5).clamp(0.005, 1.0);
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
        });

        // ── Waterfall ─────────────────────────────────────────────────────────
        let waterfall_resp = self.waterfall.show(ui, &ctx);

        // Click-to-tune on waterfall (same x→Hz conversion as spectrum)
        if let Some(click_pos) = waterfall_resp.interact_pointer_pos() {
            if waterfall_resp.clicked() {
                let rect = waterfall_resp.rect;
                let t = ((click_pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let low = freq.saturating_sub(span) as f64;
                let high = freq as f64 + span as f64;
                let new_freq = (low + t as f64 * (high - low)).round() as u64;
                let _ = self
                    .cmd_tx
                    .try_send(ReceiverCmd::SetFrequency(new_freq).into());
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
