#![forbid(unsafe_code)]

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
//! - **Mapped** (has a MIDI-learn binding): accent fill
//! - **Pending** (learn mode active for any target): amber pulsing border
//!
//! No click interaction in this feature — that is Feature 3 (sdrpp-bml).

use egui::{Color32, Painter, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2};
use parking_lot::RwLock;
use std::sync::Arc;

use sdrapp_core::signal_path::SharedState;
use sdrapp_midi::{ControlType, ControllerLayout, MidiKey, MidiKeyKind, NanoKontrol2Layout};

use crate::theme;

// ── Window state ─────────────────────────────────────────────────────────────

/// Floating window that renders a MIDI controller layout diagram.
pub struct MidiMapperWindow {
    layout: Box<dyn ControllerLayout>,
    /// Minimum rendered width of the canvas area (px).  Window auto-resizes.
    min_canvas_w: f32,
}

impl MidiMapperWindow {
    /// Create a mapper window for the Korg nanoKONTROL2.
    pub fn new_nanokontrol2() -> Self {
        Self {
            layout: Box::new(NanoKontrol2Layout::new()),
            min_canvas_w: 700.0,
        }
    }

    /// Draw the window.  `open` is toggled when the user closes the window via its ✕ button.
    pub fn show(
        &self,
        ctx: &egui::Context,
        open: &mut bool,
        shared: &Arc<RwLock<SharedState>>,
    ) {
        let window_title = format!("MIDI Mapper — {}", self.layout.name());

        egui::Window::new(window_title)
            .open(open)
            .resizable(true)
            .min_width(self.min_canvas_w + 24.0)
            .min_height(120.0)
            .show(ctx, |ui| {
                self.show_contents(ui, shared);
            });
    }

    fn show_contents(&self, ui: &mut egui::Ui, shared: &Arc<RwLock<SharedState>>) {
        let (mapped_ccs, learn_active) = {
            let s = shared.read();
            let mapped: std::collections::HashSet<u8> =
                s.midi_cc_to_knob.keys().copied().collect();
            let learn = s.midi_learn_target.is_some();
            (mapped, learn)
        };

        // ── Canvas painter ────────────────────────────────────────────────────
        let (canvas_w, canvas_h) = self.layout.canvas_size();
        let available_w = ui.available_width().max(self.min_canvas_w);
        let scale = available_w / canvas_w;
        let canvas_px_h = canvas_h * scale;

        let (rect, _response) = ui.allocate_exact_size(
            Vec2::new(available_w, canvas_px_h),
            Sense::hover(),
        );

        let painter = ui.painter_at(rect);

        // Background for the canvas
        painter.rect_filled(rect, Rounding::same(4.0), theme::SURFACE);

        // Pulse factor for "pending" controls: 0.0 → 1.0 oscillation
        let t = ui.input(|i| i.time);
        let pulse = ((t * 3.0).sin() as f32 * 0.5 + 0.5).clamp(0.0, 1.0);

        // ── Draw controls ─────────────────────────────────────────────────────
        for ctrl in self.layout.controls() {
            let r = &ctrl.rect;
            let px = rect.min + Vec2::new(r.x * scale, r.y * scale);
            let pw = r.w * scale;
            let ph = r.h * scale;
            let screen_rect = Rect::from_min_size(px, Vec2::new(pw, ph));

            let state = control_state(&ctrl.midi_key, &mapped_ccs);
            let (fill, stroke) = colors_for_state(state, learn_active, pulse);

            match ctrl.control_type {
                ControlType::Knob => {
                    draw_knob(&painter, screen_rect, fill, stroke, ctrl.label);
                }
                ControlType::Fader => {
                    draw_fader(&painter, screen_rect, fill, stroke, ctrl.label);
                }
                ControlType::Button => {
                    draw_button(&painter, screen_rect, fill, stroke, ctrl.label);
                }
            }
        }

        ui.add_space(6.0);

        // ── Legend ────────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            legend_dot(ui, theme::WIDGET_BG, theme::BORDER);
            ui.label(RichText::new("Unassigned").color(theme::TEXT_MUTED).small());
            ui.add_space(8.0);
            legend_dot(ui, theme::ACCENT_DIM, theme::ACCENT);
            ui.label(RichText::new("Mapped").color(theme::TEXT_MUTED).small());
            ui.add_space(8.0);
            legend_dot(ui, theme::AMBER, theme::AMBER);
            ui.label(RichText::new("Pending").color(theme::TEXT_MUTED).small());
        });
    }
}

// ── State helpers ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum ControlState {
    Idle,
    Mapped,
}

fn control_state(
    key: &MidiKey,
    mapped_ccs: &std::collections::HashSet<u8>,
) -> ControlState {
    // Only CC keys participate in MIDI-learn knob binding right now.
    if key.kind == MidiKeyKind::ControlChange && mapped_ccs.contains(&key.number) {
        ControlState::Mapped
    } else {
        ControlState::Idle
    }
}

fn colors_for_state(
    state: ControlState,
    learn_active: bool,
    pulse: f32,
) -> (Color32, Stroke) {
    match state {
        ControlState::Mapped => (
            theme::ACCENT_DIM,
            Stroke::new(1.5, theme::ACCENT),
        ),
        ControlState::Idle if learn_active => {
            // Pulse the border amber to draw attention during learn mode
            let alpha = (pulse * 255.0) as u8;
            let border = Color32::from_rgba_premultiplied(255, 190, 40, alpha.max(60));
            (theme::WIDGET_BG, Stroke::new(1.5, border))
        }
        ControlState::Idle => (
            theme::WIDGET_BG,
            Stroke::new(1.0, theme::BORDER),
        ),
    }
}

// ── Shape renderers ───────────────────────────────────────────────────────────

/// Draw a rotary knob: circle with a small label below.
fn draw_knob(painter: &Painter, rect: Rect, fill: Color32, stroke: Stroke, label: &str) {
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.45;

    painter.circle(center, radius, fill, stroke);

    // Indicator pip at the bottom of the knob
    let pip = center + Vec2::new(0.0, radius * 0.65);
    painter.circle_filled(pip, 1.5, stroke.color);

    // Label text below the circle
    let text_pos = Pos2::new(center.x, rect.max.y + 2.0);
    painter.text(
        text_pos,
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::proportional(7.5),
        theme::TEXT_MUTED,
    );
}

/// Draw a vertical fader: tall rounded rect with a small cap.
fn draw_fader(painter: &Painter, rect: Rect, fill: Color32, stroke: Stroke, label: &str) {
    let rounding = Rounding::same(2.0);
    painter.rect(rect, rounding, fill, stroke);

    // Track centre line
    let mid_x = rect.center().x;
    painter.line_segment(
        [Pos2::new(mid_x, rect.min.y + 3.0), Pos2::new(mid_x, rect.max.y - 3.0)],
        Stroke::new(0.5, theme::BORDER),
    );

    // Label below the fader
    let text_pos = Pos2::new(rect.center().x, rect.max.y + 2.0);
    painter.text(
        text_pos,
        egui::Align2::CENTER_TOP,
        label,
        egui::FontId::proportional(7.5),
        theme::TEXT_MUTED,
    );
}

/// Draw a push button: rounded rect with centred label.
fn draw_button(painter: &Painter, rect: Rect, fill: Color32, stroke: Stroke, label: &str) {
    let rounding = Rounding::same(3.0);
    painter.rect(rect, rounding, fill, stroke);

    let font_size = (rect.height() * 0.5).clamp(6.0, 9.0);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(font_size),
        theme::TEXT_PRIMARY,
    );
}

/// Small coloured dot for the legend.
fn legend_dot(ui: &mut egui::Ui, fill: Color32, border: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle(rect.center(), 4.0, fill, Stroke::new(1.0, border));
}
