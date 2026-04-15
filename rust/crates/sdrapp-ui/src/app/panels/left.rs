#![allow(clippy::too_many_lines)]

use egui::{RichText, Stroke, Ui, Vec2};

use sdrapp_core::{
    config::BookmarkConfig,
    signal_path::{DemodMode, RecordingMode, SharedState, SignalPathCommand},
};

use crate::{
    frequency::FrequencyWidget,
    theme,
};
use super::super::SdrApp;
use super::{category_color, format_frequency};

impl SdrApp {
    pub(in crate::app) fn left_panel(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // App title with version
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            ui.label(
                RichText::new("◉  SDR App")
                    .color(theme::ACCENT)
                    .size(15.0)
                    .strong(),
            );
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Source section ────────────────────────────────────────────────────
        ui.label(RichText::new("SOURCE").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let source_name = self.shared.read().source_name.clone();
        let is_demo = source_name
            .as_deref()
            .map(|n| n.contains("Demo"))
            .unwrap_or(false);

        let display_name = source_name
            .as_deref()
            .or_else(|| {
                self.registry
                    .sources
                    .first()
                    .map(|s| s.display_name)
            })
            .unwrap_or("No device");

        ui.horizontal(|ui| {
            let (icon, color) = if is_demo {
                ("⚠", theme::DANGER)
            } else {
                ("◈", theme::ACCENT_DIM)
            };
            ui.label(RichText::new(icon).color(color));
            ui.label(RichText::new(display_name).color(theme::TEXT_PRIMARY));
        });

        if is_demo {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    RichText::new("no hardware connected")
                        .color(theme::DANGER)
                        .small(),
                );
            });
        }

        ui.add_space(6.0);

        // Start / Stop button
        let is_running = self.shared.read().is_running;
        let (btn_text, btn_color) = if is_running {
            ("■  Stop", theme::DANGER)
        } else {
            ("▶  Start", theme::STATUS_OK)
        };

        let btn = egui::Button::new(RichText::new(btn_text).color(btn_color).strong())
            .fill(theme::WIDGET_BG)
            .stroke(Stroke::new(1.0, btn_color));

        if ui
            .add_sized(Vec2::new(ui.available_width(), 28.0), btn)
            .clicked()
            && is_running
        {
            let _ = self.cmd_tx.try_send(SignalPathCommand::Stop);
            // Start is handled by main.rs wiring the signal path; button is a placeholder
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Frequency section ─────────────────────────────────────────────────
        ui.label(RichText::new("FREQUENCY").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let (_, new_freq) = self.frequency_widget.show(ui);
        if let Some(freq) = new_freq {
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(freq));
            self.config.ui.frequency_hz = freq;
            self.config_dirty = true;
        }

        // ── Tuning step controls ──────────────────────────────────────────────
        let step_hz = self.shared.read().tune_step_hz;
        ui.add_space(4.0);

        // Step size selector row
        ui.label(RichText::new("STEP").color(theme::TEXT_MUTED).small());
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            for (hz, label) in [
                (100_u64,       "100 Hz"),
                (1_000,         "1 kHz"),
                (10_000,        "10 kHz"),
                (100_000,       "100 kHz"),
                (1_000_000,     "1 MHz"),
                (10_000_000,    "10 MHz"),
            ] {
                let selected = step_hz == hz;
                let text = RichText::new(label).small();
                let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                if ui.selectable_label(selected, text).clicked() {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetTuneStep(hz));
                }
            }
        });

        // Nudge buttons: ◄◄ ◄ ► ►► (×10 / ×1 step)
        ui.add_space(4.0);
        let freq = self.config.ui.frequency_hz;
        ui.horizontal(|ui| {
            let btn_w = (ui.available_width() - 16.0) / 4.0;
            for (label, delta, tip) in [
                ("◄◄", -(step_hz as i64 * 10), "−10 × step"),
                ("◄",  -(step_hz as i64),       "−1 × step  (or ↓ / ↑ arrow keys)"),
                ("►",   step_hz as i64,          "+1 × step  (or ↑ arrow key)"),
                ("►►",  step_hz as i64 * 10,    "+10 × step"),
            ] {
                if ui.add_sized(
                    Vec2::new(btn_w, 22.0),
                    egui::Button::new(RichText::new(label).color(theme::TEXT_PRIMARY))
                        .fill(theme::WIDGET_BG),
                ).on_hover_text(tip).clicked() {
                    let new_freq = (freq as i64 + delta).max(1) as u64;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(new_freq));
                    self.config.ui.frequency_hz = new_freq;
                    self.frequency_widget = FrequencyWidget::new(new_freq);
                    self.config_dirty = true;
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Demod mode ────────────────────────────────────────────────────────
        ui.label(RichText::new("DEMOD MODE").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let current_mode = self.shared.read().demod_mode;
        // Row 1: FM and AM modes
        ui.horizontal(|ui| {
            for (mode, label, tooltip) in [
                (DemodMode::Wbfm, "WBFM", "Wideband FM — FM broadcast stations (88–108 MHz). 75 kHz deviation, stereo, RDS."),
                (DemodMode::Nfm, "NFM", "Narrow FM — voice comms (aviation, marine, amateur, PMR). 12.5–25 kHz channels. Enable squelch."),
                (DemodMode::Am, "AM", "Amplitude Modulation — AM broadcast (530 kHz–1.7 MHz), shortwave, aviation voice."),
            ] {
                let selected = current_mode == mode;
                let text = RichText::new(label).small();
                let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                if ui.selectable_label(selected, text).on_hover_text(tooltip).clicked() && !selected {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetDemodMode(mode));
                }
            }
        });
        // Row 2: SSB and CW modes
        ui.horizontal(|ui| {
            for (mode, label, tooltip) in [
                (DemodMode::Usb, "USB", "Upper Sideband SSB — HF amateur and maritime voice. Tune above the suppressed carrier."),
                (DemodMode::Lsb, "LSB", "Lower Sideband SSB — HF amateur voice below 10 MHz (160m–40m). Tune below the carrier."),
                (DemodMode::Dsb, "DSB", "Double Sideband — both sidebands, suppressed carrier. Rare; used in some utility stations."),
                (DemodMode::Cw, "CW", "CW / Morse code — narrow 400–900 Hz bandpass centred on the 700 Hz sidetone."),
            ] {
                let selected = current_mode == mode;
                let text = RichText::new(label).small();
                let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                if ui.selectable_label(selected, text).on_hover_text(tooltip).clicked() && !selected {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetDemodMode(mode));
                }
            }
        });

        // ── NFM squelch & settings ────────────────────────────────────────────
        if current_mode == DemodMode::Nfm {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.label(RichText::new("NFM SETTINGS").color(theme::TEXT_MUTED).small());
            ui.add_space(4.0);

            // Channel bandwidth selector (12.5 / 25 kHz)
            let nfm_bw = self.shared.read().nfm_bandwidth_hz;
            ui.horizontal(|ui| {
                ui.label(RichText::new("BW").color(theme::TEXT_MUTED).small());
                for (bw, label) in [(12_500u32, "12.5k"), (25_000u32, "25k")] {
                    let selected = nfm_bw == bw;
                    let text = RichText::new(label).small();
                    let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                    if ui.selectable_label(selected, text)
                        .on_hover_text("NFM channel bandwidth. 12.5 kHz for modern PMR/amateur, 25 kHz for legacy systems.")
                        .clicked() && !selected
                    {
                        let _ = self.cmd_tx.try_send(SignalPathCommand::SetNfmBandwidth(bw));
                        self.config.ui.nfm_bandwidth_hz = bw;
                        self.config_dirty = true;
                    }
                }
            });

            ui.add_space(4.0);

            // Squelch threshold
            ui.label(RichText::new("SQUELCH").color(theme::TEXT_MUTED).small());
            ui.add_space(2.0);

            let mut sq_threshold = self.shared.read().squelch_threshold;
            let sq_label = format!("{:.0} dBFS", sq_threshold);
            ui.label(RichText::new(&sq_label).color(theme::TEXT_PRIMARY).small());
            let sq_slider = egui::Slider::new(&mut sq_threshold, -120.0_f32..=0.0_f32)
                .show_value(false)
                .trailing_fill(true);
            if ui
                .add(sq_slider)
                .on_hover_text(
                    "Squelch gates audio below this signal level (dBFS).\n\
                     Typical NFM voice: -70 to -40 dBFS.\n\
                     Set lower to hear weaker signals; higher to cut noise.",
                )
                .changed()
            {
                let _ = self
                    .cmd_tx
                    .try_send(SignalPathCommand::SetSquelchThreshold(sq_threshold));
            }

            ui.add_space(4.0);

            // CTCSS tone squelch toggle
            let (ctcss_enabled, ctcss_detected) = {
                let s = self.shared.read();
                (s.ctcss_squelch_enabled, s.ctcss_tone_detected)
            };
            ui.horizontal(|ui| {
                let label_color = if ctcss_enabled { theme::ACCENT } else { theme::TEXT_MUTED };
                let ctcss_label = if ctcss_enabled && ctcss_detected {
                    "CTCSS ✓"
                } else if ctcss_enabled {
                    "CTCSS (no tone)"
                } else {
                    "CTCSS off"
                };
                if ui
                    .small_button(RichText::new(ctcss_label).color(label_color))
                    .on_hover_text(
                        "CTCSS tone squelch: mutes audio when no sub-audible tone (67–254 Hz) is detected.\n\
                         Common on repeaters to prevent opening on distant interference.",
                    )
                    .clicked()
                {
                    let new_enabled = !ctcss_enabled;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetCtcssEnabled(new_enabled));
                    self.config.ui.ctcss_enabled = new_enabled;
                    self.config_dirty = true;
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Bookmarks ─────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            ui.label(RichText::new("BOOKMARKS").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Sort toggle
                let sort_color = if self.bookmark_sort_by_freq { theme::ACCENT } else { theme::TEXT_MUTED };
                if ui.small_button(RichText::new("↕f").color(sort_color))
                    .on_hover_text("Sort by frequency")
                    .clicked()
                {
                    self.bookmark_sort_by_freq = !self.bookmark_sort_by_freq;
                }
            });
        });
        ui.add_space(2.0);

        // Category filter chips
        let all_cats: Vec<String> = {
            let s = self.shared.read();
            let mut cats: Vec<String> = s.bookmarks.iter()
                .map(|b| b.category.clone())
                .filter(|c| !c.is_empty())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            cats.insert(0, String::new()); // "All" slot
            cats
        };
        if all_cats.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for cat in &all_cats {
                    let label = if cat.is_empty() { "All" } else { cat.as_str() };
                    let selected = self.bookmark_cat_filter == *cat;
                    let text = RichText::new(label).small();
                    let text = if selected { text.color(theme::ACCENT).strong() } else { text.color(theme::TEXT_MUTED) };
                    if ui.selectable_label(selected, text).clicked() {
                        self.bookmark_cat_filter = cat.clone();
                    }
                }
            });
            ui.add_space(2.0);
        }

        // Collect bookmark data
        let (mut bookmarks_snapshot, cursor) = {
            let s = self.shared.read();
            (s.bookmarks.clone(), s.bookmark_cursor)
        };
        // Sort if requested
        if self.bookmark_sort_by_freq {
            bookmarks_snapshot.sort_by_key(|b| b.freq_hz);
        }
        // Filter by category
        let filtered: Vec<(usize, _)> = bookmarks_snapshot.iter()
            .enumerate()
            .filter(|(_, b)| self.bookmark_cat_filter.is_empty() || b.category == self.bookmark_cat_filter)
            .map(|(i, b)| (i, b.clone()))
            .collect();

        let mut remove_idx: Option<usize> = None;
        let mut recall_idx: Option<usize> = None;
        let mut edit_start_idx: Option<usize> = None;
        let mut edit_commit_idx: Option<usize> = None;
        let mut edit_cancel = false;

        for (i, bm) in &filtered {
            let i = *i;
            let is_active = i == cursor;
            let is_editing = self.bookmark_edit_idx == Some(i);

            if is_editing {
                // Inline edit form
                ui.group(|ui| {
                    ui.label(RichText::new("Edit bookmark").color(theme::ACCENT).small());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Name").color(theme::TEXT_MUTED).small());
                        ui.text_edit_singleline(&mut self.bookmark_edit_buf.0);
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Freq (Hz)").color(theme::TEXT_MUTED).small());
                        ui.text_edit_singleline(&mut self.bookmark_edit_buf.1);
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
                        ui.text_edit_singleline(&mut self.bookmark_edit_buf.3);
                    });
                    // Mode selector
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Mode").color(theme::TEXT_MUTED).small());
                        for (mode, label) in [
                            (DemodMode::Wbfm, "WBFM"), (DemodMode::Nfm, "NFM"),
                            (DemodMode::Am, "AM"), (DemodMode::Usb, "USB"),
                            (DemodMode::Lsb, "LSB"), (DemodMode::Dsb, "DSB"),
                            (DemodMode::Cw, "CW"),
                        ] {
                            let sel = self.bookmark_edit_buf.2 == mode;
                            let txt = RichText::new(label).small();
                            let txt = if sel { txt.color(theme::ACCENT).strong() } else { txt.color(theme::TEXT_MUTED) };
                            if ui.selectable_label(sel, txt).clicked() {
                                self.bookmark_edit_buf.2 = mode;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.small_button(RichText::new("✓ Save").color(theme::STATUS_OK)).clicked() {
                            edit_commit_idx = Some(i);
                        }
                        if ui.small_button(RichText::new("✕ Cancel").color(theme::TEXT_MUTED)).clicked() {
                            edit_cancel = true;
                        }
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    // Recall button (star for active, circle for inactive)
                    let icon = if is_active { "★" } else { "☆" };
                    let icon_color = if is_active { theme::ACCENT } else { theme::TEXT_MUTED };
                    if ui.small_button(RichText::new(icon).color(icon_color)).clicked() {
                        recall_idx = Some(i);
                    }
                    // Category dot
                    if !bm.category.is_empty() {
                        let cat_color = category_color(&bm.category);
                        ui.label(RichText::new("●").color(cat_color).small());
                    }
                    // Bookmark name (click recalls)
                    let freq_label = format!("{:.3} MHz", bm.freq_hz as f64 / 1_000_000.0);
                    let text = format!("{} — {}", bm.name, freq_label);
                    if ui
                        .selectable_label(is_active, RichText::new(&text).color(theme::TEXT_PRIMARY).small())
                        .on_hover_text(format!("Mode: {:?}  Category: {}", bm.mode, if bm.category.is_empty() { "—" } else { &bm.category }))
                        .clicked()
                    {
                        recall_idx = Some(i);
                    }
                    // Edit button
                    if ui.small_button(RichText::new("✎").color(theme::TEXT_MUTED)).on_hover_text("Edit bookmark").clicked() {
                        edit_start_idx = Some(i);
                    }
                    // Delete button
                    if ui.small_button(RichText::new("×").color(theme::TEXT_MUTED)).clicked() {
                        remove_idx = Some(i);
                    }
                });
            }
        }

        // Apply bookmark actions
        if let Some(i) = recall_idx {
            let (bm_freq, bm_mode) = {
                let mut s = self.shared.write();
                s.bookmark_cursor = i;
                let bm = &s.bookmarks[i];
                (bm.freq_hz, bm.mode)
            };
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetFrequency(bm_freq));
            let _ = self.cmd_tx.try_send(SignalPathCommand::SetDemodMode(bm_mode));
            self.config.ui.frequency_hz = bm_freq;
            self.frequency_widget = FrequencyWidget::new(bm_freq);
            self.config_dirty = true;
        }
        if let Some(i) = remove_idx {
            self.bookmark_edit_idx = None;
            let _ = self.cmd_tx.try_send(SignalPathCommand::RemoveBookmark(i));
            if i < self.config.bookmarks.len() {
                self.config.bookmarks.remove(i);
                self.config_dirty = true;
            }
        }
        if let Some(i) = edit_start_idx {
            let s = self.shared.read();
            if i < s.bookmarks.len() {
                let bm = &s.bookmarks[i];
                self.bookmark_edit_buf = (
                    bm.name.clone(),
                    bm.freq_hz.to_string(),
                    bm.mode,
                    bm.category.clone(),
                );
                self.bookmark_edit_idx = Some(i);
            }
        }
        if let Some(i) = edit_commit_idx {
            let freq: u64 = self.bookmark_edit_buf.1.trim().parse().unwrap_or(0);
            if freq > 0 {
                let name = self.bookmark_edit_buf.0.clone();
                let mode = self.bookmark_edit_buf.2;
                let cat = self.bookmark_edit_buf.3.clone();
                let _ = self.cmd_tx.try_send(SignalPathCommand::EditBookmark(i, name.clone(), freq, mode, cat.clone()));
                if i < self.config.bookmarks.len() {
                    let mode_str = match mode {
                        DemodMode::Nfm => "Nfm", DemodMode::Am => "Am",
                        DemodMode::Usb => "Usb", DemodMode::Lsb => "Lsb",
                        DemodMode::Dsb => "Dsb", DemodMode::Cw => "Cw",
                        _ => "Wbfm",
                    };
                    self.config.bookmarks[i] = BookmarkConfig {
                        name, freq_hz: freq, mode: mode_str.into(), category: cat,
                    };
                    self.config_dirty = true;
                }
            }
            self.bookmark_edit_idx = None;
        }
        if edit_cancel {
            self.bookmark_edit_idx = None;
        }

        // Bottom row: Save + Export + Import
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if ui.small_button(RichText::new("+ Save").color(theme::ACCENT_DIM)).clicked() {
                let (freq, mode) = {
                    let s = self.shared.read();
                    (s.center_freq_hz, s.demod_mode)
                };
                let name = format!("{:.3} MHz", freq as f64 / 1_000_000.0);
                let _ = self.cmd_tx.try_send(SignalPathCommand::AddBookmark(name.clone()));
                let mode_str = match mode {
                    DemodMode::Nfm => "Nfm", DemodMode::Am => "Am",
                    DemodMode::Usb => "Usb", DemodMode::Lsb => "Lsb",
                    DemodMode::Dsb => "Dsb", DemodMode::Cw => "Cw",
                    _ => "Wbfm",
                };
                self.config.bookmarks.push(BookmarkConfig::new(name, freq, mode_str));
                self.config_dirty = true;
            }

            // Export CSV
            if ui.small_button(RichText::new("Export").color(theme::TEXT_MUTED))
                .on_hover_text("Export bookmarks as CSV to ~/bookmarks.csv")
                .clicked()
            {
                let csv = self.config.bookmarks.iter()
                    .map(|b| format!("{},{},{},{}", b.name.replace(',', " "), b.freq_hz, b.mode, b.category))
                    .collect::<Vec<_>>()
                    .join("\n");
                let path = dirs::home_dir().unwrap_or_default().join("bookmarks.csv");
                let header = "name,freq_hz,mode,category\n";
                let _ = std::fs::write(&path, format!("{header}{csv}"));
                tracing::info!(path = %path.display(), "bookmarks exported");
            }

            // Import CSV
            if ui.small_button(RichText::new("Import").color(theme::TEXT_MUTED))
                .on_hover_text("Import bookmarks from ~/bookmarks.csv")
                .clicked()
            {
                let path = dirs::home_dir().unwrap_or_default().join("bookmarks.csv");
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let mut imported: Vec<BookmarkConfig> = Vec::new();
                    for line in content.lines().skip(1) { // skip header
                        let parts: Vec<&str> = line.splitn(4, ',').collect();
                        if parts.len() >= 3 {
                            let freq: u64 = parts[1].trim().parse().unwrap_or(0);
                            if freq > 0 {
                                let mut bc = BookmarkConfig::new(parts[0].trim(), freq, parts[2].trim());
                                if parts.len() >= 4 { bc.category = parts[3].trim().into(); }
                                imported.push(bc);
                            }
                        }
                    }
                    if !imported.is_empty() {
                        // Replace all config bookmarks and rebuild SharedState
                        self.config.bookmarks = imported.clone();
                        self.config_dirty = true;
                        let new_bms: Vec<sdrapp_core::signal_path::Bookmark> = imported.iter().map(|b| {
                            use sdrapp_core::signal_path::{Bookmark, DemodMode};
                            let mode = match b.mode.as_str() {
                                "Nfm" => DemodMode::Nfm, "Am" => DemodMode::Am,
                                "Usb" => DemodMode::Usb, "Lsb" => DemodMode::Lsb,
                                "Dsb" => DemodMode::Dsb, "Cw" => DemodMode::Cw,
                                _ => DemodMode::Wbfm,
                            };
                            Bookmark::new(&b.name, b.freq_hz, mode).with_category(&b.category)
                        }).collect();
                        self.shared.write().bookmarks = new_bms;
                        tracing::info!(count = self.config.bookmarks.len(), "bookmarks imported");
                    }
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Scanner ───────────────────────────────────────────────────────────
        ui.label(RichText::new("SCANNER").color(theme::TEXT_MUTED).small());
        ui.add_space(4.0);

        let scan_running = self.shared.read().scan_running;

        // Category filter
        ui.horizontal(|ui| {
            ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
            ui.text_edit_singleline(&mut self.scan_cat_ui).on_hover_text("Scan only this category (empty = all bookmarks)");
        });

        // Dwell time
        ui.horizontal(|ui| {
            ui.label(RichText::new("Dwell").color(theme::TEXT_MUTED).small());
            if ui.add(egui::Slider::new(&mut self.scan_dwell_ui, 0.5_f32..=15.0_f32)
                .suffix(" s").show_value(true))
                .changed()
            {
                let _ = self.cmd_tx.try_send(SignalPathCommand::SetScanDwell(self.scan_dwell_ui));
            }
        });

        // Start / Stop / Next
        ui.horizontal(|ui| {
            if scan_running {
                let stop_btn = egui::Button::new(RichText::new("■  Stop").color(theme::DANGER).strong())
                    .fill(theme::WIDGET_BG);
                if ui.add_sized(Vec2::new(70.0, 22.0), stop_btn).clicked() {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::StopScan);
                }
                if ui.small_button(RichText::new("▶▶ Next").color(theme::TEXT_MUTED)).clicked() {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::ScanNext);
                }
                ui.label(RichText::new("SCAN").color(theme::STATUS_OK).small().strong());
            } else {
                let start_btn = egui::Button::new(RichText::new("▶  Scan").color(theme::STATUS_OK).strong())
                    .fill(theme::WIDGET_BG);
                if ui.add_sized(Vec2::new(70.0, 22.0), start_btn).clicked() {
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetScanDwell(self.scan_dwell_ui));
                    let _ = self.cmd_tx.try_send(SignalPathCommand::StartScan(self.scan_cat_ui.clone()));
                }
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(6.0);

        // ── Device settings ────────────────────────────────────────────────────
        ui.label(
            RichText::new("DEVICE SETTINGS")
                .color(theme::TEXT_MUTED)
                .small(),
        );
        ui.add_space(4.0);

        // Antenna selector
        ui.horizontal(|ui| {
            ui.label(RichText::new("Antenna").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for port in ["A", "B", "C"] {
                    let selected = self.config.source.antenna == port;
                    let label = RichText::new(port).small();
                    let label = if selected {
                        label.color(theme::ACCENT).strong()
                    } else {
                        label.color(theme::TEXT_MUTED)
                    };
                    if ui.selectable_label(selected, label).clicked() && !selected {
                        self.config.source.antenna = port.into();
                        self.config_dirty = true;
                        let port_num: u8 = match port { "B" => 1, "C" => 2, _ => 0 };
                        let _ = self.cmd_tx.try_send(SignalPathCommand::SetAntenna(port_num));
                    }
                }
            });
        });

        // Sample rate dropdown
        ui.horizontal(|ui| {
            ui.label(RichText::new("Rate").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let rates: &[(u32, &str)] = &[
                    (200_000, "200k"),
                    (500_000, "500k"),
                    (1_000_000, "1M"),
                    (2_000_000, "2M"),
                    (6_000_000, "6M"),
                    (8_000_000, "8M"),
                    (10_000_000, "10M"),
                ];
                let current = rates
                    .iter()
                    .find(|&&(r, _)| r == self.config.source.sample_rate_sps)
                    .map(|&(_, label)| label)
                    .unwrap_or("?");

                egui::ComboBox::from_id_salt("sample_rate")
                    .selected_text(RichText::new(current).small())
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        for &(rate, label) in rates {
                            let selected = rate == self.config.source.sample_rate_sps;
                            if ui.selectable_label(selected, label).clicked() {
                                self.config.source.sample_rate_sps = rate;
                                self.config_dirty = true;
                            }
                        }
                    });
            });
        });

        // AGC toggle
        ui.horizontal(|ui| {
            ui.label(RichText::new("AGC").color(theme::TEXT_MUTED).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let agc = &mut self.config.source.agc_enabled;
                let label = if *agc {
                    RichText::new("ON").color(theme::STATUS_OK).small().strong()
                } else {
                    RichText::new("OFF").color(theme::TEXT_MUTED).small()
                };
                if ui.selectable_label(*agc, label).clicked() {
                    *agc = !*agc;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetAgcEnabled(*agc));
                }
            });
        });

        // LNA state (only when AGC is off)
        if !self.config.source.agc_enabled {
            ui.horizontal(|ui| {
                ui.label(RichText::new("LNA").color(theme::TEXT_MUTED).small());
                let mut lna = self.config.source.lna_state as i32;
                if ui
                    .add(egui::Slider::new(&mut lna, 0..=9).show_value(true))
                    .changed()
                {
                    self.config.source.lna_state = lna as u8;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetLnaState(lna as u8));
                }
            });

            // IF gain slider (−59 to 0 dBFS)
            ui.horizontal(|ui| {
                ui.label(RichText::new("IF Gain").color(theme::TEXT_MUTED).small());
                let mut gain = self.config.source.if_gain_dbfs;
                if ui
                    .add(egui::Slider::new(&mut gain, -59..=0).suffix(" dBFS").show_value(true))
                    .changed()
                {
                    self.config.source.if_gain_dbfs = gain;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetIfGain(gain));
                }
            });
        }

        // AGC setpoint (only when AGC is on)
        if self.config.source.agc_enabled {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Setpoint").color(theme::TEXT_MUTED).small());
                let mut sp = self.config.source.agc_setpoint_dbfs;
                if ui
                    .add(egui::Slider::new(&mut sp, -60..=0).suffix(" dBFS").show_value(true))
                    .changed()
                {
                    self.config.source.agc_setpoint_dbfs = sp;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetAgcSetpoint(sp));
                }
            });
        }

        // Hardware-only advanced controls (not shown in demo mode)
        if !is_demo {
            ui.add_space(4.0);

            // Bias-T, HDR mode
            ui.horizontal(|ui| {
                // Bias-T
                let bias_t = self.config.source.bias_t_enabled;
                let label = RichText::new("Bias-T").small();
                let label = if bias_t { label.color(theme::ACCENT).strong() } else { label.color(theme::TEXT_MUTED) };
                if ui.selectable_label(bias_t, label)
                    .on_hover_text("Enable Bias-T 4.7 V supply on coax connector for active antennas.")
                    .clicked()
                {
                    self.config.source.bias_t_enabled = !bias_t;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetBiasT(!bias_t));
                }

                ui.add_space(6.0);

                // HDR mode
                let hdr = self.config.source.hdr_mode;
                let label = RichText::new("HDR").small();
                let label = if hdr { label.color(theme::ACCENT).strong() } else { label.color(theme::TEXT_MUTED) };
                if ui.selectable_label(hdr, label)
                    .on_hover_text("High Dynamic Range mode — improves ADC performance below 2 MHz.")
                    .clicked()
                {
                    self.config.source.hdr_mode = !hdr;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetHdrMode(!hdr));
                }
            });

            // AM notch, FM notch
            ui.horizontal(|ui| {
                let am = self.config.source.am_notch_enabled;
                let label = RichText::new("AM notch").small();
                let label = if am { label.color(theme::ACCENT).strong() } else { label.color(theme::TEXT_MUTED) };
                if ui.selectable_label(am, label)
                    .on_hover_text("AM broadcast notch filter — reduces LW/MW overload interference.")
                    .clicked()
                {
                    self.config.source.am_notch_enabled = !am;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetAmNotch(!am));
                }

                ui.add_space(6.0);

                let fm = self.config.source.fm_notch_enabled;
                let label = RichText::new("FM notch").small();
                let label = if fm { label.color(theme::ACCENT).strong() } else { label.color(theme::TEXT_MUTED) };
                if ui.selectable_label(fm, label)
                    .on_hover_text("FM broadcast / DAB notch filter — reduces overload from strong FM stations.")
                    .clicked()
                {
                    self.config.source.fm_notch_enabled = !fm;
                    self.config_dirty = true;
                    let _ = self.cmd_tx.try_send(SignalPathCommand::SetFmNotch(!fm));
                }
            });
        }

        // Signal path status
        let (center_freq, is_recording, rds_ps_name, rds_pty, rds_ta, rds_rt) = {
            let s = self.shared.read();
            (
                s.center_freq_hz,
                s.is_recording,
                s.rds_ps_name.clone(),
                s.rds_pty.map(|c| sdrapp_core::dsp::rds::pty_to_str(c).to_string()),
                s.rds_ta,
                s.rds_rt.clone(),
            )
        };

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let status_dot_color = if is_running {
                theme::STATUS_OK
            } else {
                theme::TEXT_DISABLED
            };
            ui.label(RichText::new("●").color(status_dot_color));
            ui.label(
                RichText::new(if is_running { "Running" } else { "Stopped" })
                    .color(theme::TEXT_PRIMARY),
            );
        });

        if center_freq > 0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⟳").color(theme::TEXT_MUTED));
                ui.label(
                    RichText::new(format_frequency(center_freq))
                        .color(theme::TEXT_MUTED)
                        .small(),
                );
            });
        }

        if rds_ps_name.is_some() || rds_rt.is_some() {
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(2.0);

            // PS name row: "RDS" badge · station name · PTY · TA badge
            ui.horizontal(|ui| {
                ui.label(RichText::new("RDS").color(theme::ACCENT).small().strong());
                if let Some(ref ps) = rds_ps_name {
                    ui.label(RichText::new(ps).color(theme::TEXT_PRIMARY).strong());
                }
                if let Some(ref pty) = rds_pty {
                    ui.label(RichText::new(pty).color(theme::TEXT_MUTED).small());
                }
                if rds_ta {
                    ui.label(RichText::new("TA").color(theme::AMBER).small().strong());
                }
            });

            // RadioText row
            if let Some(ref rt) = rds_rt {
                ui.horizontal(|ui| {
                    let avail = ui.available_width();
                    let rt_display = if rt.len() > 32 {
                        format!("{}…", &rt[..31])
                    } else {
                        rt.clone()
                    };
                    ui.label(
                        RichText::new(rt_display)
                            .color(theme::TEXT_MUTED)
                            .small()
                    )
                    .on_hover_text(rt.as_str());
                    let _ = avail; // suppress unused warning
                });
            }
        }

        if is_recording {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(theme::DANGER));
                ui.label(RichText::new("Recording").color(theme::AMBER).strong());
            });
        }
    }
}
