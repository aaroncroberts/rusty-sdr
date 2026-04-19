//! Bookmark list UI: compact panel list + floating manager window.

use egui::{RichText, Ui, Vec2};

use sdrapp_core::{
    config::BookmarkConfig,
    signal_path::{BookmarkCmd, DemodMode, HardwareCommand, ReceiverCmd},
};

use super::super::SdrApp;
use super::category_color;
use crate::{frequency::FrequencyWidget, theme};

impl SdrApp {
    /// Compact bookmark list in the left panel.  Click a row to recall.
    /// The "Edit" button opens the floating bookmark manager window.
    pub(in crate::app) fn bookmarks_section(&mut self, ui: &mut Ui) {
        // ── Header ────────────────────────────────────────────────────────────
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
                if ui.small_button(RichText::new("Edit").color(theme::ACCENT))
                    .on_hover_text("Open bookmark manager")
                    .clicked()
                {
                    self.show_bookmark_manager = true;
                    self.bookmark_edit_idx = None;
                }
            });
        });

        if !open {
            return;
        }
        ui.add_space(2.0);

        // ── Category filter chips ─────────────────────────────────────────────
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
            cats.insert(0, String::new());
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

        // ── Read-only list (click to recall) ──────────────────────────────────
        let (mut bookmarks_snapshot, cursor) = {
            let s = self.shared.read();
            (s.bookmarks.clone(), s.bookmark_cursor)
        };
        if self.bookmark_sort_by_freq {
            bookmarks_snapshot.sort_by_key(|b| b.freq_hz);
        }
        let filtered: Vec<(usize, _)> = bookmarks_snapshot
            .iter()
            .enumerate()
            .filter(|(_, b)| self.bookmark_cat_filter.is_empty() || b.category == self.bookmark_cat_filter)
            .map(|(i, b)| (i, b.clone()))
            .collect();

        let mut recall_idx: Option<usize> = None;

        for (i, bm) in &filtered {
            let i = *i;
            let is_active = i == cursor;

            ui.horizontal(|ui| {
                // Active/inactive dot
                let (dot_rect, dot_resp) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::click());
                let dot_color = if is_active { theme::ACCENT } else { theme::TEXT_MUTED };
                let c = dot_rect.center();
                if is_active {
                    ui.painter().circle_filled(c, 4.5, dot_color);
                } else {
                    ui.painter().circle_stroke(c, 4.0, egui::Stroke::new(1.0, dot_color));
                }
                if dot_resp.on_hover_text("Recall").clicked() {
                    recall_idx = Some(i);
                }

                // Category dot
                if !bm.category.is_empty() {
                    let cat_color = category_color(&bm.category);
                    let (cr, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                    ui.painter().circle_filled(cr.center(), 3.5, cat_color);
                } else {
                    ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                }

                // Name + freq (click to recall)
                let freq_label = format!("{:.3} MHz", bm.freq_hz as f64 / 1_000_000.0);
                let text = format!("{} — {}", bm.name, freq_label);
                let hover = format!(
                    "Mode: {:?}  Category: {}  Antenna: {}",
                    bm.mode,
                    if bm.category.is_empty() { "—" } else { &bm.category },
                    bm.antenna.as_deref().unwrap_or("default"),
                );
                if ui
                    .selectable_label(is_active, RichText::new(&text).color(theme::TEXT_PRIMARY).small())
                    .on_hover_text(hover)
                    .clicked()
                {
                    recall_idx = Some(i);
                }
            });
        }

        // ── Bottom row: + Save ────────────────────────────────────────────────
        ui.add_space(2.0);
        if ui.small_button(RichText::new("+ Save current").color(theme::ACCENT_DIM)).clicked() {
            self.bookmark_save_current();
        }

        // Apply recall
        if let Some(i) = recall_idx {
            self.bookmark_recall(i);
        }
    }

    // ── Floating bookmark manager window ─────────────────────────────────────

    pub(in crate::app) fn bookmark_manager_window(&mut self, ctx: &egui::Context) {
        if !self.show_bookmark_manager {
            return;
        }

        let mut open = self.show_bookmark_manager;
        egui::Window::new("Bookmark Manager")
            .open(&mut open)
            .resizable(true)
            .default_size([520.0, 480.0])
            .min_width(400.0)
            .show(ctx, |ui| {
                self.bookmark_manager_contents(ui);
            });
        self.show_bookmark_manager = open;
    }

    fn bookmark_manager_contents(&mut self, ui: &mut Ui) {
        // Snapshot bookmarks for rendering
        let (bookmarks_snapshot, cursor) = {
            let s = self.shared.read();
            (s.bookmarks.clone(), s.bookmark_cursor)
        };

        let mut remove_idx: Option<usize> = None;
        let mut recall_idx: Option<usize> = None;
        let mut edit_start_idx: Option<usize> = None;
        let mut edit_commit_idx: Option<usize> = None;
        let mut edit_cancel = false;

        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for (i, bm) in bookmarks_snapshot.iter().enumerate() {
                    let is_active = i == cursor;
                    let is_editing = self.bookmark_edit_idx == Some(i);

                    if is_editing {
                        self.render_edit_form(ui, i, &mut edit_commit_idx, &mut edit_cancel);
                    } else {
                        ui.horizontal(|ui| {
                            // Active dot
                            let (dot_rect, dot_resp) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::click());
                            let dot_color = if is_active { theme::ACCENT } else { theme::TEXT_MUTED };
                            let c = dot_rect.center();
                            if is_active {
                                ui.painter().circle_filled(c, 4.5, dot_color);
                            } else {
                                ui.painter().circle_stroke(c, 4.0, egui::Stroke::new(1.0, dot_color));
                            }
                            if dot_resp.on_hover_text("Recall").clicked() {
                                recall_idx = Some(i);
                            }

                            // Category dot
                            if !bm.category.is_empty() {
                                let cat_color = category_color(&bm.category);
                                let (cr, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                                ui.painter().circle_filled(cr.center(), 3.5, cat_color);
                            } else {
                                ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            }

                            // Name — freq — mode
                            let freq_label = format!("{:.3} MHz", bm.freq_hz as f64 / 1_000_000.0);
                            let mode_label = format!("{:?}", bm.mode);
                            let ant_label = bm.antenna.as_deref().map(|a| format!(" · Ant {a}")).unwrap_or_default();
                            let cat_label = if bm.category.is_empty() { String::new() } else { format!(" · {}", bm.category) };
                            let text = format!("{} — {}{ant_label}{cat_label}", bm.name, freq_label);
                            ui.label(RichText::new(&text).color(theme::TEXT_PRIMARY).small());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if ui.small_button(RichText::new("✕").color(theme::DANGER).small())
                                    .on_hover_text("Delete bookmark")
                                    .clicked()
                                {
                                    remove_idx = Some(i);
                                }
                                if ui.small_button(RichText::new("Edit").color(theme::TEXT_MUTED).small())
                                    .on_hover_text("Edit this bookmark")
                                    .clicked()
                                {
                                    edit_start_idx = Some(i);
                                }
                                ui.label(RichText::new(mode_label).color(theme::TEXT_MUTED).small());
                            });
                        });
                        ui.add_space(1.0);
                    }
                }
            });

        ui.separator();

        // Bottom actions
        ui.horizontal(|ui| {
            if ui.button(RichText::new("+ Add current").color(theme::ACCENT)).clicked() {
                self.bookmark_save_current();
            }

            let export_path_str = self.config.bookmarks_file
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&self.config.bookmarks_export_path);
            if ui.button(RichText::new("Export CSV").color(theme::TEXT_MUTED))
                .on_hover_text(format!("Export to {export_path_str}"))
                .clicked()
            {
                match BookmarkConfig::save_to_csv(&self.config.bookmarks, export_path_str) {
                    Ok(path) => tracing::info!(path = %path.display(), "bookmarks exported"),
                    Err(e) => tracing::error!(err = %e, "bookmark export failed"),
                }
            }

            let import_path_str = self.config.bookmarks_file
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&self.config.bookmarks_export_path);
            if ui.button(RichText::new("Import CSV").color(theme::TEXT_MUTED))
                .on_hover_text(format!("Import from {import_path_str}"))
                .clicked()
            {
                self.bookmark_import(import_path_str.to_string());
            }
        });

        // Apply actions
        if let Some(i) = recall_idx {
            self.bookmark_recall(i);
        }
        if let Some(i) = remove_idx {
            self.bookmark_remove(i);
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
                    bm.antenna.clone(),
                );
                self.bookmark_edit_idx = Some(i);
            }
        }
        if let Some(i) = edit_commit_idx {
            self.bookmark_commit_edit(i);
        }
        if edit_cancel {
            self.bookmark_edit_idx = None;
        }
    }

    /// Render the inline edit form for bookmark at index `i`.
    fn render_edit_form(
        &mut self,
        ui: &mut Ui,
        i: usize,
        edit_commit_idx: &mut Option<usize>,
        edit_cancel: &mut bool,
    ) {
        ui.group(|ui| {
            ui.label(RichText::new("Edit bookmark").color(theme::ACCENT).small());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Name").color(theme::TEXT_MUTED).small());
                ui.text_edit_singleline(&mut self.bookmark_edit_buf.0);
            });
            ui.horizontal(|ui| {
                let freq_valid = self.bookmark_edit_buf.1.trim().parse::<u64>().is_ok_and(|f| f > 0);
                let freq_color = if freq_valid { theme::TEXT_MUTED } else { theme::DANGER };
                ui.label(RichText::new("Freq (Hz)").color(freq_color).small());
                ui.text_edit_singleline(&mut self.bookmark_edit_buf.1);
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Cat").color(theme::TEXT_MUTED).small());
                ui.text_edit_singleline(&mut self.bookmark_edit_buf.3);
            });
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
                    let txt = if sel {
                        RichText::new(label).small().color(theme::ACCENT).strong()
                    } else {
                        RichText::new(label).small().color(theme::TEXT_MUTED)
                    };
                    if ui.selectable_label(sel, txt).clicked() {
                        self.bookmark_edit_buf.2 = mode;
                    }
                }
            });
            if self.bookmark_edit_buf.2 == DemodMode::Nfm {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("BW").color(theme::TEXT_MUTED).small());
                    for (hz, label) in [(12_500_u32, "12.5k"), (25_000_u32, "25k")] {
                        let sel = self.bookmark_edit_buf.4 == hz;
                        let txt = if sel { RichText::new(label).small().color(theme::ACCENT).strong() } else { RichText::new(label).small().color(theme::TEXT_MUTED) };
                        if ui.selectable_label(sel, txt).clicked() { self.bookmark_edit_buf.4 = hz; }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SQ").color(theme::TEXT_MUTED).small());
                    ui.add(egui::Slider::new(&mut self.bookmark_edit_buf.5, -120.0_f32..=0.0_f32).suffix(" dBFS").text("").step_by(1.0));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("CTCSS").color(theme::TEXT_MUTED).small());
                    let ctcss_color = if self.bookmark_edit_buf.6 { theme::ACCENT } else { theme::TEXT_MUTED };
                    let lbl = RichText::new(if self.bookmark_edit_buf.6 { "ON" } else { "OFF" }).small().color(ctcss_color);
                    if ui.selectable_label(self.bookmark_edit_buf.6, lbl).clicked() {
                        self.bookmark_edit_buf.6 = !self.bookmark_edit_buf.6;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("Antenna").color(theme::TEXT_MUTED).small());
                for (val, label) in [(None::<&str>, "—"), (Some("A"), "A"), (Some("B"), "B"), (Some("C"), "C")] {
                    let sel = self.bookmark_edit_buf.7.as_deref() == val;
                    let txt = if sel { RichText::new(label).small().color(theme::ACCENT).strong() } else { RichText::new(label).small().color(theme::TEXT_MUTED) };
                    if ui.selectable_label(sel, txt).clicked() {
                        self.bookmark_edit_buf.7 = val.map(|s| s.to_string());
                    }
                }
            });
            ui.horizontal(|ui| {
                let freq_valid = self.bookmark_edit_buf.1.trim().parse::<u64>().is_ok_and(|f| f > 0);
                ui.add_enabled_ui(freq_valid, |ui| {
                    if ui.small_button(RichText::new("Save").color(theme::STATUS_OK)).clicked() {
                        *edit_commit_idx = Some(i);
                    }
                });
                if ui.small_button(RichText::new("Cancel").color(theme::TEXT_MUTED)).clicked() {
                    *edit_cancel = true;
                }
            });
        });
    }

    // ── Shared bookmark actions ───────────────────────────────────────────────

    pub(in crate::app) fn bookmark_save_current(&mut self) {
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

    pub(in crate::app) fn bookmark_recall(&mut self, i: usize) {
        let (bm_freq, bm_mode, bm_nfm_bw, bm_squelch, bm_ctcss, bm_antenna) = {
            let mut s = self.shared.write();
            s.bookmark_cursor = i;
            let bm = &s.bookmarks[i];
            (bm.freq_hz, bm.mode, bm.nfm_bandwidth_hz, bm.squelch_threshold_dbfs, bm.ctcss_enabled, bm.antenna.clone())
        };
        if let Some(ref ant) = bm_antenna {
            let port: u8 = match ant.as_str() { "B" => 1, "C" => 2, _ => 0 };
            if self.config.source.antenna != *ant {
                self.bookmark_prev_antenna = Some(self.config.source.antenna.clone());
                self.config.source.antenna = ant.clone();
                self.config_dirty = true;
                let _ = self.cmd_tx.try_send(HardwareCommand::SetAntenna(port).into());
                tracing::info!(antenna = port, bookmark = i, "bookmark antenna override applied");
            }
        } else {
            self.restore_bookmark_antenna();
        }
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

    fn bookmark_remove(&mut self, i: usize) {
        self.bookmark_edit_idx = None;
        let _ = self.cmd_tx.try_send(BookmarkCmd::Remove(i).into());
        if i < self.config.bookmarks.len() {
            self.config.bookmarks.remove(i);
            self.config_dirty = true;
        }
    }

    fn bookmark_commit_edit(&mut self, i: usize) {
        let freq: u64 = self.bookmark_edit_buf.1.trim().parse().unwrap_or(0);
        if freq > 0 {
            let name = self.bookmark_edit_buf.0.clone();
            let mode = self.bookmark_edit_buf.2;
            let cat = self.bookmark_edit_buf.3.clone();
            let nfm_bw = self.bookmark_edit_buf.4;
            let squelch = self.bookmark_edit_buf.5;
            let ctcss = self.bookmark_edit_buf.6;
            let antenna = self.bookmark_edit_buf.7.clone();
            let (nfm_bw_opt, squelch_opt, ctcss_opt) = if mode == DemodMode::Nfm {
                (Some(nfm_bw), Some(squelch), Some(ctcss))
            } else {
                (None, None, None)
            };
            let _ = self.cmd_tx.try_send(
                BookmarkCmd::Edit(i, name.clone(), freq, mode, cat.clone(), nfm_bw_opt, squelch_opt, ctcss_opt, antenna.clone()).into(),
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
                    antenna,
                };
                self.config_dirty = true;
            }
        }
        self.bookmark_edit_idx = None;
    }

    fn bookmark_import(&mut self, path: String) {
        let imported = BookmarkConfig::load_from_csv(&path);
        if !imported.is_empty() {
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
                    let mut bm = Bookmark::new(&b.name, b.freq_hz, mode).with_category(&b.category);
                    bm.antenna = b.antenna.clone();
                    bm
                })
                .collect();
            self.shared.write().bookmarks = new_bms;
            tracing::info!(count = self.config.bookmarks.len(), "bookmarks imported");
        } else {
            tracing::warn!(path = path, "bookmark import: no valid entries found");
        }
    }
}
