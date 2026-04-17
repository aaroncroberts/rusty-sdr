#![allow(clippy::too_many_lines)]

use egui::{RichText, Ui, Vec2};

use sdrapp_core::signal_path::{min_zoom_for_mode, DemodMode, DisplayCmd, ReceiverCmd};

use super::super::SdrApp;
use crate::{
    frequency::FrequencyWidget,
    hints::{self, HintAction, HintCtx},
    knob::KnobWidget,
    spectrum::SpectrumWidget,
    theme,
    waterfall::WaterfallOverlay,
};

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

        // Clear the waterfall when the center frequency shifts by more than 10% of
        // the hardware bandwidth.  This prevents stale rows from a different channel
        // being mixed with new rows at the current channel, which causes persistent
        // "ghost" vertical lines that never move even after retuning.
        {
            let bw = self.shared.read().sample_rate_sps as u64;
            let threshold = (bw / 10).max(50_000);
            let delta = freq.abs_diff(self.last_waterfall_freq);
            if delta > threshold {
                self.waterfall.clear();
                self.last_waterfall_freq = freq;
            }
        }

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

            // Auto-ref: track fft_floor/fft_ceil from live FFT data.
            // floor = noise_floor_ema - 5 dB margin (noise sits just above the dark end)
            // ceil  = signal_ceil_ema  + 5 dB margin (signals sit just below the bright end)
            if self.auto_ref && n >= 10 {
                let mut sorted = fft_data.to_vec();
                sorted
                    .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let floor_sample = sorted[n / 10];
                let ceil_sample = sorted[(n * 99 / 100).min(n - 1)];
                // ALPHA=0.98 → τ ≈ 1.6 s at 30 Hz FFT writes — slow enough that
                // noise floor flutter doesn't bounce the grid on every frame.
                const ALPHA: f32 = 0.98;
                if self.noise_floor_ema <= -84.9 {
                    // Snap on the very first real-data frame.
                    self.noise_floor_ema = floor_sample;
                    self.signal_ceil_ema = ceil_sample;
                } else {
                    self.noise_floor_ema =
                        ALPHA * self.noise_floor_ema + (1.0 - ALPHA) * floor_sample;
                    self.signal_ceil_ema =
                        ALPHA * self.signal_ceil_ema + (1.0 - ALPHA) * ceil_sample;
                }
                // Move display range when it differs by ≥ 1 dB from the EMA target.
                let new_floor = (self.noise_floor_ema - 5.0).clamp(-160.0, -10.0);
                let new_ceil = (self.signal_ceil_ema + 5.0).clamp(new_floor + 10.0, 0.0);
                if (new_floor - self.fft_floor).abs() >= 1.0 {
                    self.fft_floor = new_floor;
                    self.config.ui.fft_floor = new_floor;
                    self.config_dirty = true;
                }
                if (new_ceil - self.fft_ceil).abs() >= 1.0 {
                    self.fft_ceil = new_ceil;
                    self.config.ui.fft_ceil = new_ceil;
                    self.config_dirty = true;
                }
            }

            // Re-arm auto-ref after 10 s of no manual adjustment.
            let now = ui.ctx().input(|i| i.time);
            if !self.wf_auto_armed && (now - self.wf_last_manual_drag) > 10.0 {
                self.wf_auto_armed = true;
                self.auto_ref = true;
            }

            // Spectrum and waterfall share the same fft_floor/fft_ceil range.
            self.waterfall.set_db_range((self.fft_floor, self.fft_ceil));
            // Fractional accumulator: push_row fires once per integer crossed.
            // Speed 1.0 = 1 row/frame, 2.0 = 2 rows/frame, 0.5 = every other frame.
            self.waterfall_row_frac += waterfall_speed.clamp(0.1, 10.0);
            while self.waterfall_row_frac >= 1.0 {
                self.waterfall.push_row(&fft_data);
                self.waterfall_row_frac -= 1.0;
            }
        }

        let db_floor = self.fft_floor;
        let db_ceil = self.fft_ceil;
        let db_range = (db_floor, db_ceil);

        // ── Hint strip ───────────────────────────────────────────────────────
        // Always reserve exactly one text row so the spectrum + waterfall below
        // never shift when a hint appears or disappears.
        let hint_ctx = HintCtx {
            is_running,
            demod_mode,
            span_hz: span,
            snr_db,
            audio_level: self.vu_peak * self.config.ui.volume,
            volume: self.config.ui.volume,
            fft_clipping,
            fm_notch_enabled: self.config.source.fm_notch_enabled,
            frequency_hz: self.config.ui.frequency_hz,
            scanner_running,
        };
        let hint = hints::evaluate(&hint_ctx);
        let strip_color = hint.as_ref().map(|h| {
            if h.priority == 0 {
                egui::Color32::from_rgb(220, 50, 50)
            } else if h.priority <= 2 {
                egui::Color32::from_rgb(240, 165, 0)
            } else {
                theme::TEXT_MUTED
            }
        });
        // ui.horizontal always allocates one row; empty when no hint is active.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if let (Some(hint), Some(color)) = (hint, strip_color) {
                ui.label(RichText::new(hint.message).color(color).small());
                if let Some((label, action)) = hint.action {
                    if ui.small_button(label).clicked() {
                        match action {
                            HintAction::ZoomOut => {
                                let min_zoom = sdrapp_core::signal_path::min_zoom_for_mode(demod_mode);
                                let new_z = (zoom_level * 1.8).clamp(min_zoom, 1.0);
                                let _ = self.cmd_tx.try_send(
                                    sdrapp_core::signal_path::DisplayCmd::SetZoom(new_z).into(),
                                );
                                self.config.ui.zoom_level = new_z;
                                self.config_dirty = true;
                            }
                            HintAction::SetVolume(v) => {
                                self.config.ui.volume = v;
                                let _ = self.cmd_tx.try_send(
                                    sdrapp_core::signal_path::ReceiverCmd::SetVolume(v).into(),
                                );
                                self.config_dirty = true;
                            }
                            HintAction::SetDemodMode(mode) => {
                                let _ = self.cmd_tx.try_send(
                                    sdrapp_core::signal_path::ReceiverCmd::SetDemodMode(mode).into(),
                                );
                            }
                            HintAction::DisableFmNotch => {
                                self.config.source.fm_notch_enabled = false;
                                self.config_dirty = true;
                                let _ = self.cmd_tx.try_send(
                                    sdrapp_core::signal_path::HardwareCommand::SetFmNotch(false).into(),
                                );
                            }
                            HintAction::MaxAttenuation => {
                                self.config.source.agc_setpoint_dbfs = -60;
                                self.config_dirty = true;
                                let _ = self.cmd_tx.try_send(
                                    sdrapp_core::signal_path::HardwareCommand::SetLnaState(9).into(),
                                );
                                let _ = self.cmd_tx.try_send(
                                    sdrapp_core::signal_path::HardwareCommand::SetAgcSetpoint(-60).into(),
                                );
                            }
                        }
                    }
                }
            }
        });
        ui.add_space(1.0);

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

        // ── Frequency context band label ──────────────────────────────────────
        // Show the band name in the top-left corner of the spectrum when tuned
        // into a known band.
        if let Some(band) = crate::bands::band_for_freq(freq) {
            let label_painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("band_label"),
            ));
            label_painter.text(
                egui::Pos2::new(spectrum_rect.left() + 6.0, spectrum_rect.top() + 6.0),
                egui::Align2::LEFT_TOP,
                band.name,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgba_premultiplied(180, 180, 180, 100),
            );
        }

        // ── Demod mode auto-suggest ───────────────────────────────────────────
        // When tuned to a known band with the wrong demod mode, suggest switching.
        // Dismissed per 500-kHz bucket; clears when moving >500 kHz.
        {
            let bucket = freq / 500_000;
            if bucket != self.last_freq_bucket {
                // Moved significantly — clear old dismissals
                self.demod_suggest_dismissed.clear();
                self.last_freq_bucket = bucket;
            }
            if let Some(band) = crate::bands::band_for_freq(freq) {
                if let Some(expected) = band.expected_demod {
                    if expected != demod_mode
                        && !self.demod_suggest_dismissed.contains(&bucket)
                        && is_running
                    {
                        let suggest_id = egui::Id::new("demod_suggest_open");
                        let is_open = ui.ctx().data(|d| d.get_temp::<bool>(suggest_id).unwrap_or(true));
                        if is_open {
                            let pos = egui::Pos2::new(
                                spectrum_rect.center_top().x - 130.0,
                                spectrum_rect.top() + 22.0,
                            );
                            egui::Window::new("Switch demod mode?")
                                .id(egui::Id::new("demod_suggest_window"))
                                .fixed_pos(pos)
                                .resizable(false)
                                .collapsible(false)
                                .show(ui.ctx(), |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "This looks like {} — switch to {:?}?",
                                            band.name, expected
                                        ))
                                        .small(),
                                    );
                                    ui.horizontal(|ui| {
                                        if ui.button("Switch").clicked() {
                                            let _ = self.cmd_tx.try_send(
                                                sdrapp_core::signal_path::ReceiverCmd::SetDemodMode(expected).into(),
                                            );
                                            self.demod_suggest_dismissed.insert(bucket);
                                            ui.ctx().data_mut(|d| d.insert_temp(suggest_id, false));
                                        }
                                        if ui.button("Dismiss").clicked() {
                                            self.demod_suggest_dismissed.insert(bucket);
                                            ui.ctx().data_mut(|d| d.insert_temp(suggest_id, false));
                                        }
                                    });
                                });
                        }
                    }
                }
            }
        }

        // ── ADC saturation badge ──────────────────────────────────────────────
        // Track the last time clipping was detected and hold the badge for 2 s.
        let now = ui.ctx().input(|i| i.time);
        if fft_clipping {
            self.last_clipping_time = Some(now);
        }
        if let Some(t) = self.last_clipping_time {
            if now - t < 2.0 {
                // Flash the badge: alternate bright/dark red every 0.4 s while actively clipping.
                let flash_on = fft_clipping && ((now / 0.4) as i32) % 2 == 0;
                let badge_color = if flash_on {
                    egui::Color32::from_rgb(230, 55, 55)
                } else {
                    egui::Color32::from_rgb(170, 25, 25)
                };

                let badge_pos = egui::Pos2::new(
                    spectrum_rect.right() - 70.0,
                    spectrum_rect.top() + 4.0,
                );
                let popover_id = egui::Id::new("adc_sat_popover_open");
                let mut popover_open =
                    ui.ctx().data(|d| d.get_temp::<bool>(popover_id).unwrap_or(false));

                // Render badge as an interactive Area so it receives clicks.
                let badge_area = egui::Area::new(egui::Id::new("adc_sat_badge"))
                    .fixed_pos(badge_pos)
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        let btn = egui::Button::new(
                            RichText::new("⚡ ADC SAT")
                                .color(egui::Color32::WHITE)
                                .small(),
                        )
                        .fill(badge_color)
                        .stroke(egui::Stroke::NONE)
                        .rounding(2.0);
                        ui.add(btn)
                            .on_hover_text("Input overloaded — click for fix")
                    });
                if badge_area.inner.clicked() {
                    popover_open = !popover_open;
                    ui.ctx().data_mut(|d| d.insert_temp(popover_id, popover_open));
                }

                if popover_open {
                    egui::Window::new("ADC Saturation")
                        .id(egui::Id::new("adc_sat_help_window"))
                        .fixed_pos(egui::Pos2::new(badge_pos.x - 180.0, badge_pos.y + 20.0))
                        .resizable(false)
                        .collapsible(false)
                        .show(ui.ctx(), |ui| {
                            ui.label(
                                RichText::new("⚡ Input Overloaded")
                                    .color(egui::Color32::from_rgb(230, 55, 55))
                                    .strong(),
                            );
                            ui.separator();
                            ui.label("The ADC is clipping — the signal is too strong.");
                            ui.add_space(4.0);
                            ui.label("To fix:");
                            ui.indent("adc_sat_tips", |ui| {
                                ui.label("• Raise LNA state (e.g. LNA = 9)");
                                ui.label("• Lower AGC Level setpoint (e.g. −60 dBFS)");
                                ui.label("• Use an external attenuator");
                            });
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new("→ Adjust in Device Settings (left panel)")
                                    .color(theme::TEXT_MUTED)
                                    .small(),
                            );
                            ui.add_space(4.0);
                            if ui.button("Close").clicked() {
                                popover_open = false;
                                ui.ctx().data_mut(|d| d.insert_temp(popover_id, popover_open));
                            }
                        });
                }

                ui.ctx().request_repaint();
            } else {
                self.last_clipping_time = None;
                // Auto-close popover when badge expires.
                let popover_id = egui::Id::new("adc_sat_popover_open");
                ui.ctx().data_mut(|d| d.insert_temp(popover_id, false));
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

            // MAX / MIN dBFS display-range controls.
            // These directly set the top and bottom of the spectrum Y-axis and the
            // waterfall colour range — the same pair drives both displays so what
            // you see on the spectrum matches what you see on the waterfall.
            ui.vertical(|ui| {
                ui.label(RichText::new("MAX").small().color(theme::TEXT_MUTED));
                let mut ceil_val = self.fft_ceil;
                let ceil_drag = egui::DragValue::new(&mut ceil_val)
                    .range((self.fft_floor + 10.0)..=0.0_f32)
                    .speed(1.0)
                    .suffix(" dB");
                if ui.add(ceil_drag).on_hover_text("Spectrum/waterfall ceiling (dBFS). Drag down to zoom in on weaker signals.").changed() {
                    self.fft_ceil = ceil_val.clamp(self.fft_floor + 10.0, 0.0);
                    self.config.ui.fft_ceil = self.fft_ceil;
                    self.config_dirty = true;
                    self.auto_ref = false;
                    self.wf_auto_armed = false;
                    self.wf_last_manual_drag = ui.ctx().input(|i| i.time);
                }
                ui.label(RichText::new("MIN").small().color(theme::TEXT_MUTED));
                let mut floor_val = self.fft_floor;
                let floor_drag = egui::DragValue::new(&mut floor_val)
                    .range(-160.0_f32..=(self.fft_ceil - 10.0))
                    .speed(1.0)
                    .suffix(" dB");
                if ui.add(floor_drag).on_hover_text("Spectrum/waterfall floor (dBFS). Drag down to reveal weaker signals.").changed() {
                    self.fft_floor = floor_val.clamp(-160.0, self.fft_ceil - 10.0);
                    self.config.ui.fft_floor = self.fft_floor;
                    self.config_dirty = true;
                    self.auto_ref = false;
                    self.wf_auto_armed = false;
                    self.wf_last_manual_drag = ui.ctx().input(|i| i.time);
                }
            });

            // Auto-range button: immediately set floor/ceil from live FFT data.
            if ui.small_button(RichText::new("Auto").color(if self.auto_ref { theme::ACCENT } else { theme::TEXT_MUTED }))
                .on_hover_text("Auto: continuously track signal level. Click once to snap to current signal, click again to keep tracking.")
                .clicked()
            {
                if !self.auto_ref {
                    // Snap immediately to current EMA values.
                    if self.noise_floor_ema > -120.0 {
                        self.fft_floor = (self.noise_floor_ema - 5.0).clamp(-160.0, -10.0);
                        self.fft_ceil = (self.signal_ceil_ema + 5.0).clamp(self.fft_floor + 10.0, 0.0);
                        self.config.ui.fft_floor = self.fft_floor;
                        self.config.ui.fft_ceil = self.fft_ceil;
                        self.config_dirty = true;
                    }
                }
                self.auto_ref = !self.auto_ref;
                self.wf_auto_armed = self.auto_ref;
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

            // WF Level knob removed — waterfall is now driven by the unified
            // MIN/MAX dBFS controls above, not a separate wf_level offset.

            // ── Audio health VU bar ──────────────────────────────────────────
            // Compact segmented bar showing post-demod audio output level.
            // Color: gray=silent/stopped, green=healthy, amber=loud, red=clipping.
            ui.add_space(6.0);
            let audio_level = self.vu_peak * self.config.ui.volume;
            let (vu_rect, vu_resp) = ui.allocate_exact_size(
                egui::Vec2::new(8.0, 40.0),
                egui::Sense::hover(),
            );
            let vu_painter = ui.painter_at(vu_rect);
            let seg_count = 8_u8;
            let seg_h = 3.0_f32;
            let seg_gap = 1.5_f32;
            // Convert linear peak [0,1] to segment count
            let segs_lit = if !is_running || audio_level < 0.01 {
                0
            } else {
                ((audio_level.sqrt() * seg_count as f32).ceil() as u8).min(seg_count)
            };
            for i in 0..seg_count {
                let y = vu_rect.bottom() - (i as f32 + 1.0) * (seg_h + seg_gap) + seg_gap;
                let seg_rect = egui::Rect::from_min_size(
                    egui::Pos2::new(vu_rect.left(), y),
                    egui::Vec2::new(vu_rect.width(), seg_h),
                );
                let color = if i >= segs_lit {
                    egui::Color32::from_gray(40)
                } else if i >= 6 {
                    egui::Color32::from_rgb(200, 50, 50) // top 2 = red
                } else if i >= 5 {
                    egui::Color32::from_rgb(240, 165, 0) // seg 6 = amber
                } else {
                    theme::STATUS_OK // lower segments = green
                };
                vu_painter.rect_filled(seg_rect, 1.0, color);
            }
            let dbfs_label = if audio_level < 0.001 {
                "−∞ dBFS".into()
            } else {
                format!("{:.0} dBFS", 20.0 * audio_level.log10())
            };
            vu_resp.on_hover_text(format!("Audio output level: {dbfs_label}"));
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

        // Row 2: FFT size, window, averaging, band plan, SNR, waterfall palette
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

            // ── Signal quality bar ─────────────────────────────────────────
            // 5-segment meter + POOR/FAIR/GOOD/EXCELLENT label.
            // Thresholds are mode-aware: WBFM needs more SNR than NFM/AM.
            ui.add_space(4.0);
            {
                // Mode-dependent thresholds (SNR in dB)
                let (th_fair, th_good, th_excellent) = match demod_mode {
                    DemodMode::Wbfm => (8.0_f32, 15.0, 22.0),
                    DemodMode::Nfm => (5.0, 10.0, 16.0),
                    DemodMode::Am => (6.0, 12.0, 18.0),
                    _ => (5.0, 10.0, 15.0), // SSB / CW
                };
                let snr = snr_db.unwrap_or(0.0);
                let segments_lit = if !is_running || snr_db.is_none() {
                    0
                } else if snr >= th_excellent {
                    5
                } else if snr >= th_good {
                    4
                } else if snr >= th_fair {
                    3
                } else if snr >= 2.0 {
                    2
                } else {
                    1
                };
                let bar_color = if !is_running || snr_db.is_none() {
                    theme::TEXT_MUTED
                } else if segments_lit >= 4 {
                    theme::STATUS_OK
                } else if segments_lit >= 3 {
                    theme::AMBER
                } else {
                    egui::Color32::from_rgb(200, 50, 50)
                };
                let quality_label = if !is_running || snr_db.is_none() {
                    ""
                } else if segments_lit >= 5 {
                    "EXCELLENT"
                } else if segments_lit >= 4 {
                    "GOOD"
                } else if segments_lit >= 3 {
                    "FAIR"
                } else {
                    "POOR"
                };

                // Draw 5 small vertical segments manually via painter
                let (bar_rect, bar_resp) = ui.allocate_exact_size(
                    egui::Vec2::new(22.0, 14.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(bar_rect);
                let seg_w = 3.0_f32;
                let seg_gap = 1.0_f32;
                for i in 0..5_u8 {
                    let seg_h = 4.0 + i as f32 * 2.0; // each segment taller than last
                    let x = bar_rect.left() + i as f32 * (seg_w + seg_gap);
                    let seg_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(x, bar_rect.bottom() - seg_h),
                        egui::Vec2::new(seg_w, seg_h),
                    );
                    let color = if i < segments_lit {
                        bar_color
                    } else {
                        egui::Color32::from_gray(50)
                    };
                    painter.rect_filled(seg_rect, 0.5, color);
                }

                // Always allocate a fixed-width slot for the quality label so the
                // Palette ComboBox (to the right) never shifts as the text changes.
                ui.add_sized(
                    [60.0, 14.0],
                    egui::Label::new(RichText::new(quality_label).color(bar_color).small()),
                );
                let hover_text = if let Some(snr) = snr_db {
                    format!("Signal: {:.0} dBFS  SNR: {:.1} dB", signal_level_dbfs, snr)
                } else {
                    "Signal quality (SNR not yet measured)".into()
                };
                bar_resp.on_hover_text(hover_text);
            }

            // Palette picker (merged from old Row 4)
            ui.add_space(6.0);
            ui.label(RichText::new("Palette").color(theme::TEXT_MUTED).small());
            let colormap_label = self.config.ui.waterfall_colormap.clone();
            egui::ComboBox::from_id_salt("wf_colormap")
                .selected_text(RichText::new(colormap_label.as_str()).small())
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
        let wf_overlay = Some(WaterfallOverlay {
            vfo_hz: freq,
            freq_range: (freq.saturating_sub(span), freq + span),
            filter_lo_hz: show_filter_lo,
            filter_hi_hz: show_filter_hi,
        });
        let waterfall_resp = self.waterfall.show(ui, &ctx, wf_overlay);

        // ── Synchronized crosshair: spectrum ↔ waterfall ─────────────────────
        // Whichever view the cursor is in, project the same frequency line into both.
        {
            let wf_rect = waterfall_resp.rect;
            let low = freq.saturating_sub(span) as f64;
            let high = freq as f64 + span as f64;

            // Determine normalized [0,1] x from whichever rect the pointer is in.
            let hover_t: Option<f32> = ctx.pointer_hover_pos().and_then(|p| {
                if wf_rect.contains(p) {
                    Some(((p.x - wf_rect.left()) / wf_rect.width()).clamp(0.0, 1.0))
                } else if spectrum_rect.contains(p) {
                    Some(((p.x - spectrum_rect.left()) / spectrum_rect.width()).clamp(0.0, 1.0))
                } else {
                    None
                }
            });

            if let Some(t) = hover_t {
                let hover_hz = (low + t as f64 * (high - low)).round() as u64;
                let mhz = hover_hz as f64 / 1_000_000.0;
                let label = format!("{:.4} MHz", mhz);
                let font = egui::FontId::proportional(11.0);
                let line_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70);
                let text_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200);

                // Always draw the crosshair in the waterfall
                let wf_x = wf_rect.left() + t * wf_rect.width();
                let wf_painter = ui.painter_at(wf_rect);
                wf_painter.line_segment(
                    [egui::pos2(wf_x, wf_rect.top()), egui::pos2(wf_x, wf_rect.bottom())],
                    egui::Stroke::new(1.0, line_color),
                );
                // Frequency label in top corner of waterfall
                let label_x = if t > 0.75 { wf_x - 4.0 } else { wf_x + 4.0 };
                let label_align = if t > 0.75 { egui::Align2::RIGHT_TOP } else { egui::Align2::LEFT_TOP };
                wf_painter.text(
                    egui::pos2(label_x, wf_rect.top() + 4.0),
                    label_align,
                    &label,
                    font,
                    text_color,
                );

                // When hovering over the waterfall, also draw a plain line in the spectrum.
                // (When hovering the spectrum, the SpectrumWidget already draws its own
                // full-featured crosshair via hover_pos.)
                let cursor_in_spectrum = ctx
                    .pointer_hover_pos()
                    .map(|p| spectrum_rect.contains(p))
                    .unwrap_or(false);
                if !cursor_in_spectrum {
                    let sp_x = spectrum_rect.left() + t * spectrum_rect.width();
                    let sp_painter = ui.painter_at(spectrum_rect);
                    sp_painter.line_segment(
                        [
                            egui::pos2(sp_x, spectrum_rect.top()),
                            egui::pos2(sp_x, spectrum_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, line_color),
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

        // ── First-run onboarding overlay ──────────────────────────────────────
        if self.show_onboarding {
            let overlay_painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("onboarding_overlay"),
            ));
            // Semi-transparent tint over the combined spectrum+waterfall area
            let combined = spectrum_rect.union(waterfall_resp.rect);
            overlay_painter.rect_filled(
                combined,
                0.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 160),
            );

            // Callout helper: box with arrow pointing to a target point
            let callout = |painter: &egui::Painter,
                           tip: egui::Pos2,
                           text: &str,
                           offset: egui::Vec2| {
                let box_pos = tip + offset;
                let galley = painter.layout_no_wrap(
                    text.into(),
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
                let box_rect = egui::Rect::from_center_size(box_pos, galley.size() + egui::Vec2::splat(10.0));
                painter.rect_filled(box_rect, 4.0, egui::Color32::from_rgba_premultiplied(30, 80, 160, 220));
                painter.rect_stroke(box_rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 140, 220)));
                painter.line_segment([tip, box_rect.center()], egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 140, 220)));
                painter.galley(box_rect.center() - galley.size() / 2.0, galley, egui::Color32::WHITE);
            };

            // Callout 1: spectrum area — click to tune
            callout(
                &overlay_painter,
                egui::Pos2::new(combined.center().x, combined.top() + 20.0),
                "Click spectrum to jump to a frequency",
                egui::Vec2::new(60.0, 40.0),
            );
            // Callout 2: waterfall
            callout(
                &overlay_painter,
                egui::Pos2::new(combined.center().x - 80.0, combined.bottom() - 30.0),
                "Waterfall shows signal history over time",
                egui::Vec2::new(80.0, -35.0),
            );
            // Callout 3: hint strip area (top of center panel)
            callout(
                &overlay_painter,
                egui::Pos2::new(combined.left() + 40.0, combined.top() - 12.0),
                "Hint bar: guidance appears here when something needs attention",
                egui::Vec2::new(140.0, -5.0),
            );

            // Dismiss instruction
            overlay_painter.text(
                combined.center_bottom() - egui::Vec2::new(0.0, 16.0),
                egui::Align2::CENTER_CENTER,
                "Click anywhere to dismiss",
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 180),
            );

            // Detect any click or key to dismiss
            let dismissed = ui.ctx().input(|i| {
                i.pointer.any_click() || i.keys_down.iter().any(|_| true)
            });
            if dismissed {
                self.show_onboarding = false;
                self.config.ui.seen_onboarding = true;
                self.config_dirty = true;
            }
            ui.ctx().request_repaint();
        }

        // ── Keyboard shortcut overlay (?) ─────────────────────────────────────
        let pressed_question = ui.ctx().input(|i| i.key_pressed(egui::Key::Questionmark));
        if pressed_question {
            self.show_shortcut_overlay = !self.show_shortcut_overlay;
        }
        if self.show_shortcut_overlay {
            let ctx = ui.ctx().clone();
            egui::Window::new("Keyboard Shortcuts")
                .id(egui::Id::new("shortcut_overlay"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .resizable(false)
                .collapsible(false)
                .show(&ctx, |ui| {
                    ui.columns(3, |cols| {
                        cols[0].label(RichText::new("Keyboard").strong().small());
                        cols[0].separator();
                        for (key, action) in [
                            ("Space", "Start / Stop"),
                            ("↑ / ↓", "Tune up / down"),
                            ("Ctrl+↑/↓", "Tune by 10×"),
                            ("PgUp/PgDn", "Tune coarse"),
                            ("F1–F6", "Demod mode"),
                            ("?", "This overlay"),
                            ("Ctrl+,", "Settings"),
                            ("Ctrl+Z", "Zoom in"),
                            ("Ctrl+X", "Zoom out"),
                        ] {
                            cols[0].horizontal(|ui| {
                                ui.label(RichText::new(key).monospace().small().color(theme::ACCENT));
                                ui.label(RichText::new(action).small());
                            });
                        }

                        cols[1].label(RichText::new("Mouse").strong().small());
                        cols[1].separator();
                        for (gesture, action) in [
                            ("Click spectrum", "Jump to frequency"),
                            ("Scroll", "Tune step"),
                            ("Ctrl+Scroll", "Zoom"),
                            ("Drag spectrum", "Pan frequency"),
                            ("Drag waterfall", "Pan frequency"),
                            ("Drag divider", "Resize panels"),
                            ("Right-click knob", "MIDI learn"),
                        ] {
                            cols[1].horizontal(|ui| {
                                ui.label(RichText::new(gesture).monospace().small().color(theme::ACCENT));
                                ui.label(RichText::new(action).small());
                            });
                        }

                        cols[2].label(RichText::new("MIDI (nanoKontrol2)").strong().small());
                        cols[2].separator();
                        for (ctrl, action) in [
                            ("CYCLE btn", "Next page"),
                            ("▶ Play", "Play toggle"),
                            ("■ Stop", "Stop"),
                            ("● Rec", "Record toggle"),
                            ("Page 0 knobs", "Tune (4 speeds)"),
                            ("Page 1 faders", "Display controls"),
                            ("Page 2 S-btns", "Record start/stop"),
                            ("|◄  ►|", "Bookmark prev/next"),
                        ] {
                            cols[2].horizontal(|ui| {
                                ui.label(RichText::new(ctrl).monospace().small().color(theme::ACCENT));
                                ui.label(RichText::new(action).small());
                            });
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Press ? or Escape to close").small().color(theme::TEXT_MUTED));
                        if ui.button("Close").clicked() {
                            self.show_shortcut_overlay = false;
                        }
                    });
                });
            let escape_pressed = ui.ctx().input(|i| i.key_pressed(egui::Key::Escape));
            if escape_pressed {
                self.show_shortcut_overlay = false;
            }
        }
    }
}
