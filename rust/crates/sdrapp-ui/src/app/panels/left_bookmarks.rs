//! Bookmark list UI: display, recall, add, edit, remove, export, import.

use egui::{RichText, Ui};

use sdrapp_core::{
    config::BookmarkConfig,
    signal_path::{BookmarkCmd, DemodMode, ReceiverCmd},
};

use super::super::SdrApp;
use super::category_color;
use crate::{frequency::FrequencyWidget, theme};

impl SdrApp {
    /// Render the BOOKMARKS section of the left panel (collapsible).
    pub(in crate::app) fn bookmarks_section(&mut self, ui: &mut Ui) {
        // Header row: collapsing label + sort toggle right-aligned
        let header_id = ui.make_persistent_id("bookmarks_open");
        let open = ui.ctx().data_mut(|d| *d.get_persisted_mut_or_insert_with(header_id, || true));

        ui.horizontal(|ui| {
            let arrow = if open { "▼" } else { "▶" };
            let header_text = RichText::new(format!("{arrow} BOOKMARKS")).color(theme::TEXT_MUTED).small();
            if ui.add(egui::Label::new(header_text).sense(egui::Sense::click())).clicked() {
                ui.ctx().data_mut(|d| {
                    let v: &mut bool = d.get_persisted_mut_or_insert_with(header_id, || true);
                    *v = !*v;
                });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let sort_color = if self.bookmark_sort_by_freq { theme::ACCENT } else { theme::TEXT_MUTED };
                if ui.small_button(RichText::new("↕f").color(sort_color))
                    .on_hover_text("Sort by frequency")
                    .clicked()
                {
                    self.bookmark_sort_by_freq = !self.bookmark_sort_by_freq;
                }
            });
        });

        if !open {
            return;
        }
        ui.add_space(2.0);

        // Category filter chips
        let all_cats: Vec<String> = {
            let s = self.shared.read();
            let mut cats: Vec<String> = s
                .bookmarks
                .iter()
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
                    let text = if selected {
                        text.color(theme::ACCENT).strong()
                    } else {
                        text.color(theme::TEXT_MUTED)
                    };
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
        let filtered: Vec<(usize, _)> = bookmarks_snapshot
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                self.bookmark_cat_filter.is_empty() || b.category == self.bookmark_cat_filter
            })
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
                        let freq_valid = self
                            .bookmark_edit_buf
                            .1
                            .trim()
                            .parse::<u64>()
                            .is_ok_and(|f| f > 0);
                        let freq_color = if freq_valid {
                            theme::TEXT_MUTED
                        } else {
                            theme::DANGER
                        };
                        ui.label(RichText::new("Freq (Hz)").color(freq_color).small());
                        ui.text_edit_singleline(&mut self.bookmark_edit_buf.1);
                        if !freq_valid && !self.bookmark_edit_buf.1.is_empty() {
                            ui.label(RichText::new("!").color(theme::DANGER).small())
                                .on_hover_text(
                                    "Enter frequency in Hz (e.g. 98100000 for 98.1 MHz)",
                                );
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
                        ui.text_edit_singleline(&mut self.bookmark_edit_buf.3);
                    });
                    // Mode selector
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Mode").color(theme::TEXT_MUTED).small());
                        for (mode, label) in [
                            (DemodMode::Wbfm, "WBFM"),
                            (DemodMode::Nfm, "NFM"),
                            (DemodMode::Am, "AM"),
                            (DemodMode::Usb, "USB"),
                            (DemodMode::Lsb, "LSB"),
                            (DemodMode::Dsb, "DSB"),
                            (DemodMode::Cw, "CW"),
                        ] {
                            let sel = self.bookmark_edit_buf.2 == mode;
                            let txt = RichText::new(label).small();
                            let txt = if sel {
                                txt.color(theme::ACCENT).strong()
                            } else {
                                txt.color(theme::TEXT_MUTED)
                            };
                            if ui.selectable_label(sel, txt).clicked() {
                                self.bookmark_edit_buf.2 = mode;
                            }
                        }
                    });
                    // NFM-specific settings (only shown when NFM is selected)
                    if self.bookmark_edit_buf.2 == DemodMode::Nfm {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("BW").color(theme::TEXT_MUTED).small());
                            for (hz, label) in [(12_500_u32, "12.5k"), (25_000_u32, "25k")] {
                                let sel = self.bookmark_edit_buf.4 == hz;
                                let txt = RichText::new(label).small();
                                let txt = if sel { txt.color(theme::ACCENT).strong() } else { txt.color(theme::TEXT_MUTED) };
                                if ui.selectable_label(sel, txt).clicked() {
                                    self.bookmark_edit_buf.4 = hz;
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("SQ").color(theme::TEXT_MUTED).small());
                            ui.add(
                                egui::Slider::new(&mut self.bookmark_edit_buf.5, -120.0_f32..=0.0_f32)
                                    .suffix(" dBFS")
                                    .text("")
                                    .step_by(1.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("CTCSS").color(theme::TEXT_MUTED).small());
                            let ctcss_color = if self.bookmark_edit_buf.6 { theme::ACCENT } else { theme::TEXT_MUTED };
                            let ctcss_label = RichText::new(if self.bookmark_edit_buf.6 { "ON" } else { "OFF" }).small().color(ctcss_color);
                            if ui.selectable_label(self.bookmark_edit_buf.6, ctcss_label).clicked() {
                                self.bookmark_edit_buf.6 = !self.bookmark_edit_buf.6;
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        let freq_valid = self
                            .bookmark_edit_buf
                            .1
                            .trim()
                            .parse::<u64>()
                            .is_ok_and(|f| f > 0);
                        ui.add_enabled_ui(freq_valid, |ui| {
                            if ui
                                .small_button(RichText::new("Save").color(theme::STATUS_OK))
                                .clicked()
                            {
                                edit_commit_idx = Some(i);
                            }
                        });
                        if ui
                            .small_button(RichText::new("Cancel").color(theme::TEXT_MUTED))
                            .clicked()
                        {
                            edit_cancel = true;
                        }
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    // Recall button (star for active, circle for inactive)
                    let icon = if is_active { "[*]" } else { "[ ]" };
                    let icon_color = if is_active {
                        theme::ACCENT
                    } else {
                        theme::TEXT_MUTED
                    };
                    if ui
                        .small_button(RichText::new(icon).color(icon_color))
                        .clicked()
                    {
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
                        .selectable_label(
                            is_active,
                            RichText::new(&text).color(theme::TEXT_PRIMARY).small(),
                        )
                        .on_hover_text(format!(
                            "Mode: {:?}  Category: {}",
                            bm.mode,
                            if bm.category.is_empty() {
                                "—"
                            } else {
                                &bm.category
                            }
                        ))
                        .clicked()
                    {
                        recall_idx = Some(i);
                    }
                    // Edit button
                    if ui
                        .small_button(RichText::new("Ed").color(theme::TEXT_MUTED))
                        .on_hover_text("Edit bookmark")
                        .clicked()
                    {
                        edit_start_idx = Some(i);
                    }
                    // Delete button
                    if ui
                        .small_button(RichText::new("×").color(theme::TEXT_MUTED))
                        .clicked()
                    {
                        remove_idx = Some(i);
                    }
                });
            }
        }

        // Apply bookmark actions
        if let Some(i) = recall_idx {
            let (bm_freq, bm_mode, bm_nfm_bw, bm_squelch, bm_ctcss) = {
                let mut s = self.shared.write();
                s.bookmark_cursor = i;
                let bm = &s.bookmarks[i];
                (bm.freq_hz, bm.mode, bm.nfm_bandwidth_hz, bm.squelch_threshold_dbfs, bm.ctcss_enabled)
            };
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetFrequency(bm_freq).into());
            let _ = self.cmd_tx.try_send(ReceiverCmd::SetDemodMode(bm_mode).into());
            if bm_mode == DemodMode::Nfm {
                if let Some(bw) = bm_nfm_bw {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetNfmBandwidth(bw).into());
                }
                if let Some(sq) = bm_squelch {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetSquelchThreshold(sq).into());
                }
                if let Some(ct) = bm_ctcss {
                    let _ = self.cmd_tx.try_send(ReceiverCmd::SetCtcssEnabled(ct).into());
                }
            }
            self.config.ui.frequency_hz = bm_freq;
            self.frequency_widget = FrequencyWidget::new(bm_freq);
            self.config_dirty = true;
        }
        if let Some(i) = remove_idx {
            self.bookmark_edit_idx = None;
            let _ = self.cmd_tx.try_send(BookmarkCmd::Remove(i).into());
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
                    bm.nfm_bandwidth_hz.unwrap_or(12_500),
                    bm.squelch_threshold_dbfs.unwrap_or(-50.0),
                    bm.ctcss_enabled.unwrap_or(false),
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
                let nfm_bw = self.bookmark_edit_buf.4;
                let squelch = self.bookmark_edit_buf.5;
                let ctcss = self.bookmark_edit_buf.6;
                let (nfm_bw_opt, squelch_opt, ctcss_opt) = if mode == DemodMode::Nfm {
                    (Some(nfm_bw), Some(squelch), Some(ctcss))
                } else {
                    (None, None, None)
                };
                let _ = self.cmd_tx.try_send(
                    BookmarkCmd::Edit(i, name.clone(), freq, mode, cat.clone(), nfm_bw_opt, squelch_opt, ctcss_opt).into(),
                );
                if i < self.config.bookmarks.len() {
                    let mode_str = match mode {
                        DemodMode::Nfm => "Nfm",
                        DemodMode::Am => "Am",
                        DemodMode::Usb => "Usb",
                        DemodMode::Lsb => "Lsb",
                        DemodMode::Dsb => "Dsb",
                        DemodMode::Cw => "Cw",
                        _ => "Wbfm",
                    };
                    self.config.bookmarks[i] = BookmarkConfig {
                        name,
                        freq_hz: freq,
                        mode: mode_str.into(),
                        category: cat,
                        nfm_bandwidth_hz: nfm_bw_opt,
                        squelch_threshold_dbfs: squelch_opt,
                        ctcss_enabled: ctcss_opt,
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
            if ui
                .small_button(RichText::new("+ Save").color(theme::ACCENT_DIM))
                .clicked()
            {
                let (freq, mode, nfm_bw, squelch, ctcss) = {
                    let s = self.shared.read();
                    let is_nfm = s.demod.demod_mode == DemodMode::Nfm;
                    (
                        s.center_freq_hz,
                        s.demod.demod_mode,
                        is_nfm.then_some(s.demod.nfm_bandwidth_hz),
                        is_nfm.then_some(s.demod.squelch_threshold),
                        is_nfm.then_some(s.demod.ctcss_squelch_enabled),
                    )
                };
                let name = format!("{:.3} MHz", freq as f64 / 1_000_000.0);
                let _ = self.cmd_tx.try_send(BookmarkCmd::Add(name.clone()).into());
                let mode_str = match mode {
                    DemodMode::Nfm => "Nfm",
                    DemodMode::Am => "Am",
                    DemodMode::Usb => "Usb",
                    DemodMode::Lsb => "Lsb",
                    DemodMode::Dsb => "Dsb",
                    DemodMode::Cw => "Cw",
                    _ => "Wbfm",
                };
                let mut bc = BookmarkConfig::new(name, freq, mode_str);
                bc.nfm_bandwidth_hz = nfm_bw;
                bc.squelch_threshold_dbfs = squelch;
                bc.ctcss_enabled = ctcss;
                self.config.bookmarks.push(bc);
                self.config_dirty = true;
            }

            // Export CSV
            if ui
                .small_button(RichText::new("Export").color(theme::TEXT_MUTED))
                .on_hover_text("Export bookmarks as CSV to ~/bookmarks.csv")
                .clicked()
            {
                let csv = self
                    .config
                    .bookmarks
                    .iter()
                    .map(|b| {
                        format!(
                            "{},{},{},{}",
                            b.name.replace(',', " "),
                            b.freq_hz,
                            b.mode,
                            b.category
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let path = dirs::home_dir().unwrap_or_default().join("bookmarks.csv");
                let header = "name,freq_hz,mode,category\n";
                let _ = std::fs::write(&path, format!("{header}{csv}"));
                tracing::info!(path = %path.display(), "bookmarks exported");
            }

            // Import CSV
            if ui
                .small_button(RichText::new("Import").color(theme::TEXT_MUTED))
                .on_hover_text("Import bookmarks from ~/bookmarks.csv")
                .clicked()
            {
                let path = dirs::home_dir().unwrap_or_default().join("bookmarks.csv");
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let mut imported: Vec<BookmarkConfig> = Vec::new();
                    for (line_no, line) in content.lines().enumerate().skip(1) {
                        let parts: Vec<&str> = line.splitn(4, ',').collect();
                        if parts.len() >= 3 {
                            let freq: u64 = parts[1].trim().parse().unwrap_or(0);
                            if freq > 0 {
                                let mut bc =
                                    BookmarkConfig::new(parts[0].trim(), freq, parts[2].trim());
                                if parts.len() >= 4 {
                                    bc.category = parts[3].trim().into();
                                }
                                imported.push(bc);
                            } else {
                                tracing::warn!(
                                    line = line_no + 1,
                                    raw = line,
                                    "bookmark import: skipped line — invalid frequency"
                                );
                            }
                        } else {
                            tracing::warn!(
                                line = line_no + 1,
                                raw = line,
                                "bookmark import: skipped line — too few columns"
                            );
                        }
                    }
                    if !imported.is_empty() {
                        // Replace all config bookmarks and rebuild SharedState
                        self.config.bookmarks = imported.clone();
                        self.config_dirty = true;
                        let new_bms: Vec<sdrapp_core::signal_path::Bookmark> = imported
                            .iter()
                            .map(|b| {
                                use sdrapp_core::signal_path::{Bookmark, DemodMode};
                                let mode = match b.mode.as_str() {
                                    "Nfm" => DemodMode::Nfm,
                                    "Am" => DemodMode::Am,
                                    "Usb" => DemodMode::Usb,
                                    "Lsb" => DemodMode::Lsb,
                                    "Dsb" => DemodMode::Dsb,
                                    "Cw" => DemodMode::Cw,
                                    _ => DemodMode::Wbfm,
                                };
                                Bookmark::new(&b.name, b.freq_hz, mode).with_category(&b.category)
                            })
                            .collect();
                        self.shared.write().bookmarks = new_bms;
                        tracing::info!(count = self.config.bookmarks.len(), "bookmarks imported");
                    }
                }
            }
        });
    }
}
