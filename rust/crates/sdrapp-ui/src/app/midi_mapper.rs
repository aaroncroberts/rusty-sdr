#![forbid(unsafe_code)]
#![allow(clippy::too_many_arguments)]

//! MIDI controller mapper window.
//!
//! Renders a scale-accurate diagram of a MIDI controller using [`ControllerLayout`].
//! Each physical control is drawn as a shape matching its type:
//! - [`ControlType::Knob`]   → filled circle with label below
//! - [`ControlType::Fader`]  → tall rounded rectangle (vertical slider)
//! - [`ControlType::Button`] → rounded rectangle
//!
//! Controls are colour-coded by mapping state:
//! - **Idle** (no binding):     dim fill, subtle border
//! - **Mapped** (has a MIDI-learn binding): accent fill + action label
//! - **Pending** (selected, waiting for UI click): pulsing amber fill + border
//!
//! Interaction:
//! - Left-click CC control → sets `midi_map_pending`; next knob click completes the bind
//! - Right-click any control → context menu with Unmap / Map to…
//! - Hover → tooltip with full action description
//! - Page tabs → switch which page's bindings are shown

use egui::{Color32, Painter, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2};
use parking_lot::RwLock;
use std::sync::Arc;
use serde_json;

use sdrapp_core::signal_path::SharedState;
use sdrapp_midi::{ControlType, ControllerLayout, MidiKey, MidiKeyKind, NanoKontrol2Layout};

use crate::theme;

// ── Window state ─────────────────────────────────────────────────────────────

/// Floating window that renders a MIDI controller layout diagram.
pub struct MidiMapperWindow {
    layout: Box<dyn ControllerLayout>,
    /// Minimum rendered width of the canvas area (px).  Window auto-resizes.
    min_canvas_w: f32,
    /// Currently selected page tab index.
    selected_page: usize,
    /// True when reset-confirmation dialog is open.
    confirm_reset: bool,
    /// Path field for export.
    export_path: String,
    /// Path field for import.
    import_path: String,
    /// One-frame status message for export/import feedback.
    io_status: Option<(String, bool)>, // (message, is_error)
}

impl MidiMapperWindow {
    /// Create a mapper window for the Korg nanoKONTROL2.
    pub fn new_nanokontrol2() -> Self {
        let default_path = dirs::home_dir()
            .unwrap_or_default()
            .join("midi_map.json")
            .to_string_lossy()
            .into_owned();
        Self {
            layout: Box::new(NanoKontrol2Layout::new()),
            min_canvas_w: 700.0,
            selected_page: 0,
            confirm_reset: false,
            export_path: default_path.clone(),
            import_path: default_path,
            io_status: None,
        }
    }

    /// Draw the window.  `open` is toggled when the user closes the window via its ✕ button.
    /// `midi_bindings` is the list of `(page, key_name, action_name)` from the live config.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        open: &mut bool,
        shared: &Arc<RwLock<SharedState>>,
        midi_bindings: &[(usize, String, String)],
    ) {
        let window_title = format!("MIDI Mapper — {}", self.layout.name());

        egui::Window::new(window_title)
            .open(open)
            .resizable(true)
            .min_width(self.min_canvas_w + 24.0)
            .min_height(120.0)
            .show(ctx, |ui| {
                self.show_contents(ui, shared, midi_bindings);
            });
    }

    fn show_contents(
        &mut self,
        ui: &mut egui::Ui,
        shared: &Arc<RwLock<SharedState>>,
        midi_bindings: &[(usize, String, String)],
    ) {
        let (mapped_ccs, learn_active, pending_cc) = {
            let s = shared.read();
            let mapped: std::collections::HashMap<u8, String> = s
                .midi_cc_to_knob
                .iter()
                .map(|(&k, v)| (k, v.clone()))
                .collect();
            let learn = s.midi_learn_target.is_some();
            (mapped, learn, s.midi_map_pending)
        };

        // ── Page tabs ─────────────────────────────────────────────────────────
        let page_names = self.layout.page_names();
        ui.horizontal(|ui| {
            for (i, &name) in page_names.iter().enumerate() {
                let selected = self.selected_page == i;
                let color = theme::midi_page_color(i);
                let text = RichText::new(name)
                    .small()
                    .color(if selected { color } else { theme::TEXT_MUTED });
                let btn = egui::Button::new(text)
                    .fill(if selected { theme::WIDGET_ACTIVE } else { theme::WIDGET_BG })
                    .stroke(Stroke::new(
                        if selected { 1.5 } else { 1.0 },
                        if selected { color } else { theme::BORDER },
                    ));
                if ui.add(btn).clicked() {
                    self.selected_page = i;
                }
            }
        });
        ui.add_space(4.0);

        // ── Canvas painter ────────────────────────────────────────────────────
        let (canvas_w, canvas_h) = self.layout.canvas_size();
        let available_w = ui.available_width().max(self.min_canvas_w);
        let scale = available_w / canvas_w;
        let canvas_px_h = canvas_h * scale;

        let (rect, _canvas_resp) = ui.allocate_exact_size(
            Vec2::new(available_w, canvas_px_h),
            Sense::hover(),
        );

        let painter = ui.painter_at(rect);

        // Background for the canvas
        painter.rect_filled(rect, Rounding::same(4.0), theme::SURFACE);

        // Pulse factor for "pending" controls: 0.0 → 1.0 oscillation
        let t = ui.input(|i| i.time);
        let pulse = ((t * 3.0).sin() as f32 * 0.5 + 0.5).clamp(0.0, 1.0);

        // ── Draw + interact controls ──────────────────────────────────────────
        // Collect any state changes to apply after the loop (avoid borrow conflicts).
        let mut new_pending: Option<Option<u8>> = None; // Some(Some(cc)) = set, Some(None) = clear
        let mut unmap_cc: Option<u8> = None;

        for ctrl in self.layout.controls() {
            let r = &ctrl.rect;
            let px = rect.min + Vec2::new(r.x * scale, r.y * scale);
            let pw = r.w * scale;
            let ph = r.h * scale;
            let screen_rect = Rect::from_min_size(px, Vec2::new(pw, ph));

            let is_cc = ctrl.midi_key.kind == MidiKeyKind::ControlChange;
            let cc_num = ctrl.midi_key.number;
            let is_pending = pending_cc == Some(cc_num);

            // Look up the action binding for this control on the selected page.
            let action_label = find_action(ctrl.midi_key.kind.clone(), ctrl.midi_key.number, self.selected_page, midi_bindings);
            let knob_binding = if is_cc { mapped_ccs.get(&cc_num).cloned() } else { None };

            let state = derive_state(&ctrl.midi_key, &mapped_ccs, action_label.is_some());
            let (fill, stroke) = colors_for_state(state, learn_active, is_pending, pulse);

            match ctrl.control_type {
                ControlType::Knob => draw_knob(&painter, screen_rect, fill, stroke, ctrl.label, action_label.as_deref()),
                ControlType::Fader => draw_fader(&painter, screen_rect, fill, stroke, ctrl.label, action_label.as_deref()),
                ControlType::Button => draw_button(&painter, screen_rect, fill, stroke, ctrl.label, action_label.as_deref()),
            }

            // ── Per-control interaction via ui.interact ───────────────────────
            let ctrl_id = ui.id().with(ctrl.id);
            let sense = if is_cc { Sense::click() } else { Sense::hover() };
            let resp = ui.interact(screen_rect, ctrl_id, sense);

            // Tooltip — chain consumes resp and returns it
            let tooltip = build_tooltip(ctrl, &action_label, &knob_binding, self.selected_page);
            let resp = resp.on_hover_text_at_pointer(tooltip);

            // Left-click on CC control → start bind
            if is_cc && resp.clicked() {
                new_pending = Some(Some(cc_num));
            }

            // Right-click context menu
            resp.context_menu(|ui| {
                if let Some(ref _binding) = knob_binding {
                    if ui.button("Unmap (MIDI learn)").clicked() {
                        unmap_cc = Some(cc_num);
                        ui.close_menu();
                    }
                }
                if is_cc {
                    let label = if pending_cc == Some(cc_num) { "Cancel pending bind" } else { "Map to UI control…" };
                    if ui.button(label).clicked() {
                        if pending_cc == Some(cc_num) {
                            new_pending = Some(None); // cancel
                        } else {
                            new_pending = Some(Some(cc_num));
                        }
                        ui.close_menu();
                    }
                }
            });
        }

        // Apply deferred state changes.
        if let Some(val) = new_pending {
            shared.write().midi_map_pending = val;
        }
        if let Some(cc) = unmap_cc {
            shared.write().midi_cc_to_knob.remove(&cc);
        }

        ui.add_space(6.0);

        // ── Legend ────────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            legend_dot(ui, theme::WIDGET_BG, theme::BORDER);
            ui.label(RichText::new("Unassigned").color(theme::TEXT_MUTED).small());
            ui.add_space(8.0);
            legend_dot(ui, theme::ACCENT_DIM, theme::ACCENT);
            ui.label(RichText::new("Mapped (learn)").color(theme::TEXT_MUTED).small());
            ui.add_space(8.0);
            legend_dot(ui, Color32::from_rgb(30, 70, 50), theme::STATUS_OK);
            ui.label(RichText::new("Action bound").color(theme::TEXT_MUTED).small());
            ui.add_space(8.0);
            legend_dot(ui, theme::AMBER, theme::AMBER);
            ui.label(RichText::new("Pending bind").color(theme::TEXT_MUTED).small());
        });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(4.0);

        // ── Toolbar ───────────────────────────────────────────────────────────
        self.show_toolbar(ui, shared);
    }

    fn show_toolbar(&mut self, ui: &mut egui::Ui, shared: &Arc<RwLock<SharedState>>) {
        // IO status message (shown at the top of toolbar)
        if let Some((ref msg, is_err)) = self.io_status.clone() {
            let color = if is_err { theme::DANGER } else { theme::STATUS_OK };
            ui.label(RichText::new(msg).color(color).small());
            ui.add_space(2.0);
        }

        // ── Reset confirmation dialog ─────────────────────────────────────────
        if self.confirm_reset {
            egui::Frame::none()
                .fill(Color32::from_rgba_premultiplied(80, 10, 10, 200))
                .stroke(Stroke::new(1.0, theme::DANGER))
                .inner_margin(egui::Margin::same(6.0))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("This will clear all MIDI-learn bindings. Continue?")
                            .color(theme::DANGER)
                            .small(),
                    );
                    ui.horizontal(|ui| {
                        let yes = egui::Button::new(RichText::new("Yes, reset").color(theme::DANGER))
                            .fill(theme::WIDGET_BG)
                            .stroke(Stroke::new(1.0, theme::DANGER));
                        if ui.add(yes).clicked() {
                            shared.write().midi_cc_to_knob.clear();
                            shared.write().midi_map_pending = None;
                            self.confirm_reset = false;
                            self.io_status = Some(("MIDI-learn bindings cleared.".into(), false));
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_reset = false;
                        }
                    });
                });
            ui.add_space(4.0);
        }

        // ── Main toolbar row ──────────────────────────────────────────────────
        ui.horizontal(|ui| {
            // Reset button
            let reset_btn = egui::Button::new(
                RichText::new("⟳ Reset bindings").color(theme::DANGER).small(),
            )
            .fill(theme::WIDGET_BG)
            .stroke(Stroke::new(1.0, theme::DANGER));
            if ui
                .add(reset_btn)
                .on_hover_text("Clear all MIDI-learn CC→knob bindings")
                .clicked()
            {
                self.confirm_reset = true;
                self.io_status = None;
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // Export row
            ui.label(RichText::new("Export:").color(theme::TEXT_MUTED).small());
            ui.add(
                egui::TextEdit::singleline(&mut self.export_path)
                    .desired_width(200.0)
                    .hint_text("/path/to/midi_map.json"),
            );
            let save_btn = egui::Button::new(RichText::new("⬇ Save").small())
                .fill(theme::WIDGET_BG)
                .stroke(Stroke::new(1.0, theme::BORDER));
            if ui.add(save_btn).on_hover_text("Export current MIDI-learn bindings to JSON").clicked() {
                let bindings: std::collections::HashMap<String, u8> = shared
                    .read()
                    .midi_cc_to_knob
                    .iter()
                    .map(|(&cc, id)| (id.clone(), cc))
                    .collect();
                match serde_json::to_string_pretty(&bindings) {
                    Ok(json) => match std::fs::write(&self.export_path, json) {
                        Ok(()) => self.io_status = Some((format!("Saved to {}", self.export_path), false)),
                        Err(e) => self.io_status = Some((format!("Save failed: {e}"), true)),
                    },
                    Err(e) => self.io_status = Some((format!("Serialize error: {e}"), true)),
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);

            // Import row
            ui.label(RichText::new("Import:").color(theme::TEXT_MUTED).small());
            ui.add(
                egui::TextEdit::singleline(&mut self.import_path)
                    .desired_width(200.0)
                    .hint_text("/path/to/midi_map.json"),
            );
            let load_btn = egui::Button::new(RichText::new("⬆ Load").small())
                .fill(theme::WIDGET_BG)
                .stroke(Stroke::new(1.0, theme::BORDER));
            if ui.add(load_btn).on_hover_text("Import MIDI-learn bindings from JSON").clicked() {
                match std::fs::read_to_string(&self.import_path) {
                    Err(e) => self.io_status = Some((format!("Read failed: {e}"), true)),
                    Ok(text) => {
                        match serde_json::from_str::<std::collections::HashMap<String, u8>>(&text) {
                            Err(e) => self.io_status = Some((format!("Invalid JSON: {e}"), true)),
                            Ok(map) => {
                                let cc_to_knob: std::collections::HashMap<u8, String> = map
                                    .into_iter()
                                    .map(|(id, cc)| (cc, id))
                                    .collect();
                                shared.write().midi_cc_to_knob = cc_to_knob;
                                self.io_status = Some(("Bindings loaded.".into(), false));
                            }
                        }
                    }
                }
            }
        });
    }
}

// ── Binding helpers ───────────────────────────────────────────────────────────

/// Find the action name for a given MIDI key + page in the binding table.
fn find_action(
    kind: MidiKeyKind,
    number: u8,
    page: usize,
    bindings: &[(usize, String, String)],
) -> Option<String> {
    let key_name = match kind {
        MidiKeyKind::ControlChange => format!("CC {number}"),
        MidiKeyKind::NoteOn => format!("Note {number}"),
    };
    bindings
        .iter()
        .find(|(p, k, _)| *p == page && k == &key_name)
        .map(|(_, _, action)| action.clone())
}

fn build_tooltip(
    ctrl: &sdrapp_midi::ControlDef,
    action_label: &Option<String>,
    knob_binding: &Option<String>,
    page: usize,
) -> String {
    let mut parts = vec![format!("{} ({})", ctrl.id, ctrl.label)];
    let key_desc = match ctrl.midi_key.kind {
        MidiKeyKind::ControlChange => format!("CC {}", ctrl.midi_key.number),
        MidiKeyKind::NoteOn => format!("Note {}", ctrl.midi_key.number),
    };
    parts.push(format!("MIDI: ch0 {key_desc}"));
    if let Some(action) = action_label {
        parts.push(format!("Action (page {}): {action}", page));
    }
    if let Some(knob) = knob_binding {
        parts.push(format!("UI knob binding: {knob}"));
    }
    parts.join("\n")
}

// ── State helpers ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum ControlState {
    Idle,
    ActionBound, // has an action in the nanoKontrol2 profile for this page
    Mapped,      // has a MIDI-learn CC→knob binding
}

fn derive_state(
    key: &MidiKey,
    mapped_ccs: &std::collections::HashMap<u8, String>,
    has_action: bool,
) -> ControlState {
    if key.kind == MidiKeyKind::ControlChange && mapped_ccs.contains_key(&key.number) {
        ControlState::Mapped
    } else if has_action {
        ControlState::ActionBound
    } else {
        ControlState::Idle
    }
}

fn colors_for_state(
    state: ControlState,
    learn_active: bool,
    is_pending: bool,
    pulse: f32,
) -> (Color32, Stroke) {
    if is_pending {
        let alpha = (pulse * 180.0 + 75.0) as u8;
        let fill = Color32::from_rgba_premultiplied(80, 60, 0, alpha);
        return (fill, Stroke::new(2.0, theme::AMBER));
    }
    match state {
        ControlState::Mapped => (theme::ACCENT_DIM, Stroke::new(1.5, theme::ACCENT)),
        ControlState::ActionBound => (
            Color32::from_rgb(30, 70, 50),
            Stroke::new(1.5, theme::STATUS_OK),
        ),
        ControlState::Idle if learn_active => {
            let alpha = (pulse * 255.0) as u8;
            let border = Color32::from_rgba_premultiplied(255, 190, 40, alpha.max(60));
            (theme::WIDGET_BG, Stroke::new(1.5, border))
        }
        ControlState::Idle => (theme::WIDGET_BG, Stroke::new(1.0, theme::BORDER)),
    }
}

// ── Shape renderers ───────────────────────────────────────────────────────────

/// Shorten an action name to at most `max` characters for display inside a control.
fn short_action(action: &str, max: usize) -> &str {
    if action.len() <= max {
        action
    } else {
        &action[..max]
    }
}

/// Draw a rotary knob: circle with a small label below.
fn draw_knob(
    painter: &Painter,
    rect: Rect,
    fill: Color32,
    stroke: Stroke,
    label: &str,
    action: Option<&str>,
) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.45;

    painter.circle(center, radius, fill, stroke);

    let pip = center + Vec2::new(0.0, radius * 0.65);
    painter.circle_filled(pip, 1.5, stroke.color);

    let text_pos = Pos2::new(center.x, rect.max.y + 2.0);
    painter.text(
        text_pos,
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::proportional(7.5),
        theme::TEXT_MUTED,
    );

    if let Some(act) = action {
        painter.text(
            Pos2::new(center.x, rect.max.y + 10.0),
            egui::Align2::CENTER_TOP,
            short_action(act, 8),
            egui::FontId::proportional(6.0),
            theme::STATUS_OK,
        );
    }
}

/// Draw a vertical fader: tall rounded rect with a small cap.
fn draw_fader(
    painter: &Painter,
    rect: Rect,
    fill: Color32,
    stroke: Stroke,
    label: &str,
    action: Option<&str>,
) {
    painter.rect(rect, Rounding::same(2.0), fill, stroke);

    let mid_x = rect.center().x;
    painter.line_segment(
        [Pos2::new(mid_x, rect.min.y + 3.0), Pos2::new(mid_x, rect.max.y - 3.0)],
        Stroke::new(0.5, theme::BORDER),
    );

    let text_pos = Pos2::new(rect.center().x, rect.max.y + 2.0);
    painter.text(
        text_pos,
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::proportional(7.5),
        theme::TEXT_MUTED,
    );

    if let Some(act) = action {
        painter.text(
            Pos2::new(rect.center().x, rect.max.y + 10.0),
            egui::Align2::CENTER_TOP,
            short_action(act, 6),
            egui::FontId::proportional(6.0),
            theme::STATUS_OK,
        );
    }
}

/// Draw a push button: rounded rect with centred label.
fn draw_button(
    painter: &Painter,
    rect: Rect,
    fill: Color32,
    stroke: Stroke,
    label: &str,
    action: Option<&str>,
) {
    painter.rect(rect, Rounding::same(3.0), fill, stroke);

    let font_size = (rect.height() * 0.5).clamp(6.0, 9.0);
    let display_label = action.map_or(label, |a| short_action(a, 5));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        display_label,
        egui::FontId::proportional(font_size),
        theme::TEXT_PRIMARY,
    );
}

/// Small coloured dot for the legend.
fn legend_dot(ui: &mut egui::Ui, fill: Color32, border: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle(rect.center(), 4.0, fill, Stroke::new(1.0, border));
}
