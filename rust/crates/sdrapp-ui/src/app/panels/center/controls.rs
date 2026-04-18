//! Display controls toolbar for the center spectrum panel.
//!
//! Extracted from `center/mod.rs` to keep `center_panel()` focused on
//! layout and event routing.

use egui::{RichText, Ui};

use sdrapp_core::signal_path::{DemodMode, DisplayCmd};

use crate::app::SdrApp;
use crate::knob::KnobWidget;
use crate::theme;

impl SdrApp {
    /// Render the display-controls toolbar row beneath the spectrum.
    ///
    /// Parameters come from the calling `center_panel` to avoid a redundant
    /// `shared.read()` inside this method.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn show_display_controls(
        &mut self,
        ui: &mut Ui,
        span: u64,
        zoom_level: f32,
        waterfall_speed: f32,
        demod_mode: DemodMode,
        nfm_bw_hz: u32,
        band_plan_enabled: bool,
        peak_hold_enabled: bool,
        peak_hold_decay_db: f32,
        snr_db: Option<f32>,
        signal_level_dbfs: f32,
        is_running: bool,
    ) {
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
            let min_zoom = demod_mode.min_zoom();
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
    }
}
