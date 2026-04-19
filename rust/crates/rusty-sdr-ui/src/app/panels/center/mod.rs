#![allow(clippy::too_many_lines)]

mod controls;

use egui::{RichText, Ui, Vec2};

use rusty_sdr_core::signal_path::{DemodMode, DisplayCmd, ReceiverCmd};

use super::super::SdrApp;
use crate::{
    frequency::FrequencyWidget,
    hints::{self, HintAction, HintCtx},

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
                let z = s.zoom_level.clamp(s.demod.demod_mode.min_zoom(), 1.0);
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
                // Snap floor/ceil to the nearest 5 dB step so grid lines only move
                // when the optimal range shifts by a full tick-step.  Without snapping,
                // the 1-dB hysteresis below still lets the scale shift every ~30 frames,
                // causing all horizontal grid lines to visibly bounce up/down together.
                const SNAP: f32 = 5.0;
                let new_floor = ((self.noise_floor_ema - 5.0) / SNAP).floor() * SNAP;
                let new_floor = new_floor.clamp(-160.0, -10.0);
                let new_ceil = ((self.signal_ceil_ema + 5.0) / SNAP).ceil() * SNAP;
                let new_ceil = new_ceil.clamp(new_floor + 10.0, 0.0);
                // Only commit when the snapped value differs by ≥ SNAP from the current.
                if (new_floor - self.fft_floor).abs() >= SNAP {
                    self.fft_floor = new_floor;
                }
                if (new_ceil - self.fft_ceil).abs() >= SNAP {
                    self.fft_ceil = new_ceil;
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
                // zoom_level is passed so push_row renders the same bin window
                // as the spectrum widget (centre ± sr/2 * zoom_level).
                self.waterfall.push_row(&fft_data, zoom_level);
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
        // Always render one fixed-height row for the hint strip so the spectrum
        // below never shifts when a hint appears or disappears.
        // We always emit a label (invisible when no hint) to pin the row height
        // to the font metric regardless of whether a hint is active.
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let (msg, color) = match (&hint, strip_color) {
                (Some(h), Some(c)) => (h.message, c),
                _ => ("", egui::Color32::TRANSPARENT),
            };
            ui.label(RichText::new(msg).color(color).small());
            if let (Some(hint), Some(_)) = (hint, strip_color) {
                if let Some((label, action)) = hint.action {
                    if ui.small_button(label).clicked() {
                        match action {
                            HintAction::ZoomOut => {
                                let min_zoom = demod_mode.min_zoom();
                                let new_z = (zoom_level * 1.8).clamp(min_zoom, 1.0);
                                let _ = self.cmd_tx.try_send(
                                    rusty_sdr_core::signal_path::DisplayCmd::SetZoom(new_z).into(),
                                );
                                self.config.ui.zoom_level = new_z;
                                self.config_dirty = true;
                            }
                            HintAction::SetVolume(v) => {
                                self.config.ui.volume = v;
                                let _ = self.cmd_tx.try_send(
                                    rusty_sdr_core::signal_path::ReceiverCmd::SetVolume(v).into(),
                                );
                                self.config_dirty = true;
                            }
                            HintAction::SetDemodMode(mode) => {
                                let _ = self.cmd_tx.try_send(
                                    rusty_sdr_core::signal_path::ReceiverCmd::SetDemodMode(mode).into(),
                                );
                            }
                            HintAction::DisableFmNotch => {
                                self.config.source.fm_notch_enabled = false;
                                self.config_dirty = true;
                                let _ = self.cmd_tx.try_send(
                                    rusty_sdr_core::signal_path::HardwareCommand::SetFmNotch(false).into(),
                                );
                            }
                            HintAction::MaxAttenuation => {
                                self.config.source.agc_setpoint_dbfs = -60;
                                self.config_dirty = true;
                                let _ = self.cmd_tx.try_send(
                                    rusty_sdr_core::signal_path::HardwareCommand::SetLnaState(9).into(),
                                );
                                let _ = self.cmd_tx.try_send(
                                    rusty_sdr_core::signal_path::HardwareCommand::SetAgcSetpoint(-60).into(),
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
                self.apply_tune(new_freq);
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
                self.apply_tune(new_freq);
            }
        }

        // Scroll on spectrum: tune frequency (step); Ctrl+scroll: zoom in/out
        let (scroll_delta, ctrl_held) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
        if spectrum_resp.hovered() && scroll_delta.abs() > 0.5 {
            if ctrl_held {
                // Ctrl+scroll → zoom
                let factor = if scroll_delta > 0.0 { 0.8_f32 } else { 1.25_f32 };
                let new_zoom = (zoom_level * factor).clamp(demod_mode.min_zoom(), 1.0);
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
                self.apply_tune(new_freq);
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
            // Noise floor: 20th-percentile estimated from a 64-element stride sample.
            // Sampling every (n/64)th bin gives a representative estimate without
            // cloning or sorting the full 8192-bin FFT buffer every frame (~32 KB).
            let noise_floor = {
                let step = (n / 64).max(1);
                let mut sample: Vec<f32> =
                    fft_data.iter().step_by(step).copied().collect();
                let target = sample.len() / 5;
                *sample
                    .select_nth_unstable_by(target, |a, b| {
                        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .1
            };
            // 18 dB above noise, and must be a real signal (> -90 dBFS absolute).
            // High threshold prevents noise peaks from flickering with labels.
            let threshold = (noise_floor + 18.0).max(-90.0);
            let min_spacing = (n / 20).max(4);

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

        // ── Scanner lock tracking ─────────────────────────────────────────────
        // Read the locked freq from shared state; when a new lock appears, record
        // the egui time so the 3-second banner can be auto-dismissed.
        let now = ui.ctx().input(|i| i.time);
        {
            let locked = self.shared.read().scanner.last_locked_freq_hz;
            if locked.is_some() && locked != self.scan_last_locked_freq {
                self.scan_last_locked_freq = locked;
                self.scan_lock_time = Some(now);
                // Sync UI to the locked frequency — the per-frame sync (lines 40-43)
                // only runs while scanner_running == true, which is already false on
                // this frame, so we must update here.
                if let Some(locked_hz) = locked {
                    self.config.ui.frequency_hz = locked_hz;
                    self.frequency_widget = FrequencyWidget::new(locked_hz);
                    self.config_dirty = true;
                }
                // Re-arm repaint so the fade runs at display rate.
                ui.ctx().request_repaint();
            }
        }

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
            locked_freq_hz: self.scan_last_locked_freq,
        }
        .show(&mut spectrum_ui);

        // ── Scanner lock banner ───────────────────────────────────────────────
        // Prominent "LOCKED: 104.700 MHz" overlay for 3 s; fades out in last 0.5 s.
        if let (Some(locked_hz), Some(lock_t)) = (self.scan_last_locked_freq, self.scan_lock_time) {
            let elapsed = now - lock_t;
            if elapsed < 3.0 {
                // Alpha: full opacity for first 2.5 s, then fade to 0 over 0.5 s
                let alpha = if elapsed < 2.5 {
                    1.0_f32
                } else {
                    (1.0 - ((elapsed - 2.5) / 0.5)) as f32
                };
                let alpha_u8 = (alpha * 255.0) as u8;

                let mhz = locked_hz as f64 / 1_000_000.0;
                let banner_text = format!("LOCKED  {mhz:.3} MHz");
                let banner_color = egui::Color32::from_rgba_unmultiplied(255, 200, 50, alpha_u8);
                let bg_color = egui::Color32::from_rgba_unmultiplied(10, 10, 10, (alpha * 180.0) as u8);
                let border_color = egui::Color32::from_rgba_unmultiplied(255, 200, 50, (alpha * 120.0) as u8);

                let painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("scan_lock_banner"),
                ));
                let text_pos = egui::Pos2::new(
                    spectrum_rect.center().x,
                    spectrum_rect.top() + 18.0,
                );
                // Background box
                let galley = ui.ctx().fonts(|f| {
                    f.layout_no_wrap(
                        banner_text.clone(),
                        egui::FontId::proportional(14.0),
                        banner_color,
                    )
                });
                let text_size = galley.size();
                let pad = egui::Vec2::new(8.0, 4.0);
                let box_rect = egui::Rect::from_center_size(
                    text_pos,
                    text_size + pad * 2.0,
                );
                painter.rect(box_rect, 3.0, bg_color, egui::Stroke::new(1.0, border_color));
                painter.text(
                    text_pos,
                    egui::Align2::CENTER_CENTER,
                    banner_text,
                    egui::FontId::proportional(14.0),
                    banner_color,
                );
                // Keep repainting during the fade
                ui.ctx().request_repaint();
            }
        }

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
                                                rusty_sdr_core::signal_path::ReceiverCmd::SetDemodMode(expected).into(),
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
        self.show_display_controls(
            ui,
            span,
            zoom_level,
            waterfall_speed,
            demod_mode,
            nfm_bw_hz,
            band_plan_enabled,
            peak_hold_enabled,
            peak_hold_decay_db,
            snr_db,
            signal_level_dbfs,
            is_running,
        );

        // ── Demod / FM scan / Bookmark scanner strip ─────────────────────────
        // Must be rendered BEFORE the waterfall so it gets space first.
        // The waterfall uses available_height() which would consume all remaining
        // space, making the strip invisible if it came after.
        self.center_bottom_strip(ui);

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
        // The spectrum widget reserves SpectrumWidget::Y_LABEL_W pixels on the left
        // for its dBFS axis labels.  The waterfall texture must cover only the plot
        // area (not that label strip) so that equal frequencies land on the same
        // horizontal pixel in both widgets.
        let wf_left_offset = SpectrumWidget::Y_LABEL_W;
        let plot_width = (spectrum_rect.width() - wf_left_offset).max(8.0);
        self.waterfall.set_width(plot_width as usize);
        let wf_overlay = Some(WaterfallOverlay {
            vfo_hz: freq,
            freq_range: (freq.saturating_sub(span), freq + span),
            filter_lo_hz: show_filter_lo,
            filter_hi_hz: show_filter_hi,
        });
        let waterfall_resp = self.waterfall.show(ui, &ctx, wf_overlay, wf_left_offset);

        // ── Synchronized crosshair: spectrum ↔ waterfall ─────────────────────
        // Both widgets share the same plot origin: rect.left() + wf_left_offset.
        // All t values are normalised within the plot area, not the raw rect.
        {
            let wf_rect = waterfall_resp.rect;
            let low = freq.saturating_sub(span) as f64;
            let high = freq as f64 + span as f64;

            // Plot-area left edge and width (same offset in both widgets).
            let sp_plot_left = spectrum_rect.left() + wf_left_offset;
            let sp_plot_width = spectrum_rect.width() - wf_left_offset;
            let wf_plot_left = wf_rect.left() + wf_left_offset;
            let wf_plot_width = wf_rect.width() - wf_left_offset;

            // Determine normalized [0,1] x within the plot area.
            let hover_t: Option<f32> = ctx.pointer_hover_pos().and_then(|p| {
                if wf_rect.contains(p) && wf_plot_width > 0.0 {
                    Some(((p.x - wf_plot_left) / wf_plot_width).clamp(0.0, 1.0))
                } else if spectrum_rect.contains(p) && sp_plot_width > 0.0 {
                    Some(((p.x - sp_plot_left) / sp_plot_width).clamp(0.0, 1.0))
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

                // Always draw the crosshair in the waterfall (plot area only)
                let wf_x = wf_plot_left + t * wf_plot_width;
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
                    let sp_x = sp_plot_left + t * sp_plot_width;
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

                self.apply_tune(new_freq);
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
                self.apply_tune(new_freq);
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
                self.apply_tune(new_freq);
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

    fn center_bottom_strip(&mut self, ui: &mut Ui) {
        // ── Mode-specific sub-controls ─────────────────────────────────────────
        // The mode selector itself lives in the toolbar row (show_display_controls).
        // This strip only shows controls that are specific to the active mode.
        let current_mode = self.shared.read().demod.demod_mode;

        match current_mode {
            DemodMode::Nfm => {
                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);
                let nfm_bw = self.shared.read().demod.nfm_bandwidth_hz;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("BW").color(theme::TEXT_MUTED).small());
                    for (bw, label) in [(12_500u32, "12.5k"), (25_000u32, "25k")] {
                        let sel = nfm_bw == bw;
                        let txt = if sel {
                            RichText::new(label).color(theme::ACCENT).strong().small()
                        } else {
                            RichText::new(label).color(theme::TEXT_MUTED).small()
                        };
                        if ui
                            .add_sized([44.0, 22.0], egui::SelectableLabel::new(sel, txt))
                            .on_hover_text("NFM channel bandwidth")
                            .clicked()
                            && !sel
                        {
                            let _ = self.cmd_tx.try_send(ReceiverCmd::SetNfmBandwidth(bw).into());
                            self.config.ui.nfm_bandwidth_hz = bw;
                            self.config_dirty = true;
                        }
                    }

                    ui.separator();

                    let (ctcss_enabled, ctcss_detected) = {
                        let s = self.shared.read();
                        (s.demod.ctcss_squelch_enabled, s.demod.ctcss_tone_detected)
                    };
                    let ctcss_label = if ctcss_enabled && ctcss_detected {
                        "CTCSS: tone"
                    } else if ctcss_enabled {
                        "CTCSS: no tone"
                    } else {
                        "CTCSS: off"
                    };
                    let ctcss_color = if ctcss_enabled { theme::ACCENT } else { theme::TEXT_MUTED };
                    if ui
                        .add_sized(
                            [88.0, 22.0],
                            egui::Button::new(
                                RichText::new(ctcss_label).color(ctcss_color).small(),
                            ),
                        )
                        .on_hover_text("CTCSS tone squelch: mutes audio when no sub-audible tone detected")
                        .clicked()
                    {
                        let new_en = !ctcss_enabled;
                        let _ = self.cmd_tx.try_send(ReceiverCmd::SetCtcssEnabled(new_en).into());
                        self.config.ui.ctcss_enabled = new_en;
                        self.config_dirty = true;
                    }
                });

                ui.add_space(4.0);

                // Squelch slider
                let mut sq_threshold = self.shared.read().demod.squelch_threshold;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SQ").color(theme::TEXT_MUTED).small());
                    if ui
                        .add(
                            egui::Slider::new(&mut sq_threshold, -120.0_f32..=0.0_f32)
                                .suffix(" dB")
                                .show_value(true),
                        )
                        .changed()
                    {
                        let _ = self
                            .cmd_tx
                            .try_send(ReceiverCmd::SetSquelchThreshold(sq_threshold).into());
                        self.config.ui.squelch_threshold_dbfs = sq_threshold;
                        self.config_dirty = true;
                    }
                });

                ui.add_space(4.0);

                // NFM signal level bar with squelch threshold marker
                let (sig_level, sq_thr) = {
                    let s = self.shared.read();
                    (s.demod.nfm_signal_level_dbfs, s.demod.squelch_threshold)
                };
                let bar_color = if sig_level >= sq_thr { theme::STATUS_OK } else { theme::TEXT_MUTED };
                let fill = ((sig_level + 120.0) / 120.0).clamp(0.0, 1.0);
                let thresh_frac = ((sq_thr + 120.0) / 120.0).clamp(0.0, 1.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SIG").color(theme::TEXT_MUTED).small());
                    let (rect, _) = ui.allocate_exact_size(
                        egui::Vec2::new(ui.available_width() - 64.0, 10.0),
                        egui::Sense::hover(),
                    );
                    if ui.is_rect_visible(rect) {
                        let p = ui.painter_at(rect);
                        p.rect_filled(rect, 2.0, theme::WIDGET_BG);
                        p.rect_filled(
                            egui::Rect::from_min_max(
                                rect.left_top(),
                                egui::pos2(rect.left() + rect.width() * fill, rect.bottom()),
                            ),
                            2.0,
                            bar_color,
                        );
                        let tx = rect.left() + rect.width() * thresh_frac;
                        p.line_segment(
                            [egui::pos2(tx, rect.top()), egui::pos2(tx, rect.bottom())],
                            egui::Stroke::new(1.5, theme::DANGER),
                        );
                    }
                    ui.label(
                        RichText::new(format!("{sig_level:.0} dBFS"))
                            .color(bar_color)
                            .small()
                            .monospace(),
                    );
                });
            }
            DemodMode::Wbfm => {
                ui.label(
                    RichText::new("88-108 MHz broadcast FM  |  200 kHz bandwidth")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            }
            DemodMode::Am => {
                ui.label(
                    RichText::new("AM — shortwave / MW broadcast / aviation voice  |  10 kHz bandwidth")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            }
            DemodMode::Usb => {
                ui.label(
                    RichText::new("USB — upper sideband  |  HF amateur, maritime, aeronautical")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            }
            DemodMode::Lsb => {
                ui.label(
                    RichText::new("LSB — lower sideband  |  HF amateur below 10 MHz")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            }
            DemodMode::Dsb => {
                ui.label(
                    RichText::new("DSB — double sideband, suppressed carrier")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            }
            DemodMode::Cw => {
                ui.label(
                    RichText::new("CW — Morse code  |  narrow 400-900 Hz bandpass")
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            }
        }

        ui.add_space(4.0);
    }
}
