#![forbid(unsafe_code)]

//! Operators Handbook — a ring-binder–styled floating window.
//!
//! Visual design:
//! ```text
//! ┌─ Window ────────────────────────────────────────────────────┐
//! │  [dark leather background]                                   │
//! │ ┌──────┐ ┌───────────────────────────────┐ ┌─────────────┐  │
//! │ │      │ │  Section title                 │ │[Welc]       │  │
//! │ │  ○   │ │  ─────────────────────────────  │ │[Layo]       │  │
//! │ │  ○   │ │  Page content (scrollable)     │ │[Sig.]       │  │
//! │ │  ○   │ │                                │ │[Sign]       │  │
//! │ │  ○   │ │                                │ │[Ctrl]       │  │
//! │ │  ○   │ └───────────────────────────────┘ │[MIDI]       │  │
//! │ └──────┘        ◀  Page 1 / 3  ▶           └─────────────┘  │
//! └──────────────────────────────────────────────────────────────┘
//! ```

pub mod assets;
pub mod content;
pub mod renderer;
pub mod sections;

use egui::{
    Align, Color32, Context, Frame, Key, Layout, Pos2, Rect, RichText, Rounding, Stroke,
    Vec2,
};

use assets::AssetLoader;
use content::HandbookSection;
use renderer::render_page;

// ── Binder colour palette ─────────────────────────────────────────────────────

const LEATHER_BG: Color32 = Color32::from_rgb(52, 28, 12);
const LEATHER_DARK: Color32 = Color32::from_rgb(36, 18, 6);
const LEATHER_HIGHLIGHT: Color32 = Color32::from_rgb(75, 42, 18);
const PAGE_BG: Color32 = Color32::from_rgb(245, 240, 228);
const PAGE_RULE: Color32 = Color32::from_rgb(208, 196, 172);
const RING_BRASS: Color32 = Color32::from_rgb(184, 138, 20);
const RING_SHADOW: Color32 = Color32::from_rgb(60, 35, 8);
const TAB_INACTIVE_OVERLAY: Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 90);

const SPINE_WIDTH: f32 = 52.0;
const TAB_WIDTH: f32 = 68.0;
const RING_COUNT: usize = 4;

// ── HandbookWindow ────────────────────────────────────────────────────────────

/// The Operators Handbook floating window.
///
/// Owns the asset loader (textures) and section/page navigation state.
/// Wire into `SdrApp` and call [`HandbookWindow::show`] each frame.
pub struct HandbookWindow {
    sections: Vec<HandbookSection>,
    assets: AssetLoader,
    /// Active section index.
    pub section: usize,
    /// Active page index within the current section.
    pub page: usize,
    /// Tracks whether the OS viewport window is open. Set to false when the OS window
    /// close button is pressed; caller resets to true when it re-opens the window.
    pub viewport_open: bool,
}

impl HandbookWindow {
    /// Construct a window with all handbook sections pre-loaded.
    pub fn new() -> Self {
        Self {
            sections: sections::all_sections(),
            assets: AssetLoader::new(),
            section: 0,
            page: 0,
            viewport_open: true,
        }
    }

    /// Construct with persisted section/page from config.
    pub fn with_state(section: usize, page: usize) -> Self {
        let sections = sections::all_sections();
        let section = section.min(sections.len().saturating_sub(1));
        let page = page.min(
            sections
                .get(section)
                .map(|s| s.page_count().saturating_sub(1))
                .unwrap_or(0),
        );
        Self {
            sections,
            assets: AssetLoader::new(),
            section,
            page,
            viewport_open: true,
        }
    }

    /// Render the handbook window.
    ///
    /// Renders directly as viewport content (no egui::Window wrapper).
    /// `open` is set to false when the OS viewport close button is pressed,
    /// or when the user presses F1 inside the viewport.
    pub fn show(&mut self, ctx: &Context, open: &mut bool) {
        // Handle OS window close button
        if ctx.input(|i| i.viewport().close_requested()) {
            *open = false;
        }

        // F1 inside the viewport also closes it
        if ctx.input(|i| i.key_pressed(Key::F1)) {
            *open = !*open;
        }

        egui::CentralPanel::default()
            .frame(Frame::none().fill(LEATHER_BG))
            .show(ctx, |ui| {
                self.draw_binder(ui);
            });
    }

    // ── Top-level layout ──────────────────────────────────────────────────────

    fn draw_binder(&mut self, ui: &mut egui::Ui) {
        let total = ui.available_rect_before_wrap();

        // Leather background (already set by window frame, this adds texture)
        let p = ui.painter();
        // Subtle grain lines
        let grain_color = Color32::from_rgba_premultiplied(0, 0, 0, 12);
        let mut y = total.min.y;
        while y < total.max.y {
            p.line_segment(
                [Pos2::new(total.min.x, y), Pos2::new(total.max.x, y)],
                Stroke::new(0.5, grain_color),
            );
            y += 4.0;
        }

        // Draw the three columns manually
        let spine_rect = Rect::from_min_size(total.min, Vec2::new(SPINE_WIDTH, total.height()));
        let tab_rect = Rect::from_min_size(
            Pos2::new(total.max.x - TAB_WIDTH, total.min.y),
            Vec2::new(TAB_WIDTH, total.height()),
        );
        let page_rect = Rect::from_min_max(
            Pos2::new(total.min.x + SPINE_WIDTH, total.min.y + 6.0),
            Pos2::new(total.max.x - TAB_WIDTH, total.max.y - 34.0),
        );

        self.draw_spine(ui, spine_rect);
        let clicked_section = self.draw_tabs(ui, tab_rect);
        if let Some(idx) = clicked_section {
            self.section = idx;
            self.page = 0;
        }
        self.draw_page_area(ui, page_rect);
        self.draw_nav_bar(ui, total);
    }

    // ── Spine + rings ─────────────────────────────────────────────────────────

    fn draw_spine(&self, ui: &mut egui::Ui, rect: Rect) {
        let p = ui.painter();

        // Spine background — darker leather
        p.rect_filled(rect, Rounding::ZERO, LEATHER_DARK);

        // Edge highlight (right edge of spine)
        p.line_segment(
            [
                Pos2::new(rect.max.x, rect.min.y),
                Pos2::new(rect.max.x, rect.max.y),
            ],
            Stroke::new(1.5, LEATHER_HIGHLIGHT),
        );

        // Brass rings — evenly distributed vertically
        let usable_h = rect.height() - 40.0;
        let step = usable_h / (RING_COUNT + 1) as f32;
        let cx = rect.center().x;

        for i in 0..RING_COUNT {
            let cy = rect.min.y + 20.0 + step * (i + 1) as f32;
            let center = Pos2::new(cx, cy);

            // Shadow
            p.circle_filled(center, 11.0, RING_SHADOW);
            // Outer brass
            p.circle_filled(center, 9.0, RING_BRASS);
            // Mid ring (slightly darker)
            p.circle_filled(center, 7.0, Color32::from_rgb(140, 100, 10));
            // Inner highlight
            p.circle_filled(center, 5.0, Color32::from_rgb(210, 170, 60));
            // Hole
            p.circle_filled(center, 3.0, RING_SHADOW);
            // Shine dot
            p.circle_filled(
                Pos2::new(center.x - 2.0, center.y - 2.0),
                1.0,
                Color32::from_rgb(255, 240, 150),
            );
        }
    }

    // ── Section tabs (right side) ─────────────────────────────────────────────

    /// Draw all section tabs and return the index of any tab that was clicked.
    fn draw_tabs(&self, ui: &mut egui::Ui, rect: Rect) -> Option<usize> {
        // Use a standalone Painter (not borrowed from ui) so we can interleave
        // painting calls with ui.allocate_rect() mutable borrow calls.
        let painter = egui::Painter::new(
            ui.ctx().clone(),
            ui.layer_id(),
            ui.clip_rect(),
        );
        painter.rect_filled(rect, Rounding::ZERO, LEATHER_DARK);

        let n = self.sections.len();
        let tab_h = (rect.height() / n as f32).min(90.0);
        let mut clicked = None;

        for (i, section) in self.sections.iter().enumerate() {
            let y0 = rect.min.y + i as f32 * tab_h;
            let tab_rect = Rect::from_min_size(Pos2::new(rect.min.x, y0), Vec2::new(TAB_WIDTH, tab_h));

            let is_active = i == self.section;
            let color = section.tab_color;

            // Shadow behind inactive tabs
            if !is_active {
                painter.rect_filled(tab_rect, Rounding::ZERO, TAB_INACTIVE_OVERLAY);
            }

            // Tab body
            let tab_fill = if is_active {
                color
            } else {
                Color32::from_rgb(
                    (color.r() as f32 * 0.6) as u8,
                    (color.g() as f32 * 0.6) as u8,
                    (color.b() as f32 * 0.6) as u8,
                )
            };

            let rounding = if is_active {
                Rounding { nw: 0.0, sw: 0.0, ne: 4.0, se: 4.0 }
            } else {
                Rounding { nw: 0.0, sw: 0.0, ne: 3.0, se: 3.0 }
            };

            let inner = if is_active {
                tab_rect
            } else {
                Rect::from_min_size(
                    Pos2::new(tab_rect.min.x + 4.0, tab_rect.min.y + 1.0),
                    Vec2::new(TAB_WIDTH - 4.0, tab_h - 2.0),
                )
            };
            painter.rect_filled(inner, rounding, tab_fill);

            // Top/bottom borders on each tab
            painter.line_segment(
                [inner.left_top(), inner.right_top()],
                Stroke::new(1.0, Color32::from_rgba_premultiplied(255, 255, 255, 50)),
            );
            painter.line_segment(
                [inner.left_bottom(), inner.right_bottom()],
                Stroke::new(1.0, Color32::from_rgba_premultiplied(0, 0, 0, 60)),
            );

            // Label centred in tab
            let label_rect = inner.shrink(4.0);
            let text_color = if is_active {
                Color32::WHITE
            } else {
                Color32::from_white_alpha(180)
            };
            let font_size = if is_active { 11.0_f32 } else { 10.0 };
            painter.text(
                label_rect.center(),
                egui::Align2::CENTER_CENTER,
                section.title,
                egui::FontId::proportional(font_size),
                text_color,
            );

            // Click detection — mutable borrow of ui (OK: painter is independent)
            let response = ui.allocate_rect(tab_rect, egui::Sense::click());
            if response.clicked() {
                clicked = Some(i);
            }
        }

        clicked
    }

    // ── Page area ─────────────────────────────────────────────────────────────

    fn draw_page_area(&mut self, ui: &mut egui::Ui, rect: Rect) {
        // Page shadow
        let shadow_rect = rect.translate(Vec2::new(3.0, 3.0));
        ui.painter().rect_filled(
            shadow_rect,
            4.0,
            Color32::from_black_alpha(60),
        );

        // Page background (cream)
        ui.painter().rect_filled(rect, 2.0, PAGE_BG);

        // Horizontal rule lines
        let rule_spacing = 22.0;
        let mut ry = rect.min.y + 40.0;
        while ry < rect.max.y {
            ui.painter().line_segment(
                [
                    Pos2::new(rect.min.x + 10.0, ry),
                    Pos2::new(rect.max.x - 10.0, ry),
                ],
                Stroke::new(0.5, PAGE_RULE),
            );
            ry += rule_spacing;
        }

        // Red left margin line (classic notebook margin)
        ui.painter().line_segment(
            [
                Pos2::new(rect.min.x + 32.0, rect.min.y + 8.0),
                Pos2::new(rect.min.x + 32.0, rect.max.y - 8.0),
            ],
            Stroke::new(0.8, Color32::from_rgba_premultiplied(200, 60, 60, 120)),
        );

        // Section title header
        if let Some(section) = self.sections.get(self.section) {
            let title_rect = Rect::from_min_size(
                Pos2::new(rect.min.x + 38.0, rect.min.y + 4.0),
                Vec2::new(rect.width() - 48.0, 26.0),
            );
            ui.painter().text(
                title_rect.left_center(),
                egui::Align2::LEFT_CENTER,
                section.full_title,
                egui::FontId::proportional(15.0),
                Color32::from_rgb(80, 55, 20),
            );

            // Tab colour accent stripe on the title bar
            ui.painter().rect_filled(
                Rect::from_min_size(
                    Pos2::new(rect.min.x + 10.0, rect.min.y + 26.0),
                    Vec2::new(rect.width() - 20.0, 1.5),
                ),
                0.0,
                section.tab_color,
            );
        }

        // Content area — allocate an egui child UI inside the page rect
        let content_rect = Rect::from_min_max(
            Pos2::new(rect.min.x + 36.0, rect.min.y + 32.0),
            Pos2::new(rect.max.x - 10.0, rect.max.y),
        );
        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(content_rect).layout(Layout::top_down(Align::LEFT)));
        let max_w = content_rect.width();

        if let Some(section) = self.sections.get(self.section) {
            if let Some(page) = section.pages.get(self.page) {
                render_page(&mut child_ui, page, &mut self.assets, max_w);
            }
        }
    }

    // ── Navigation bar ────────────────────────────────────────────────────────

    fn draw_nav_bar(&mut self, ui: &mut egui::Ui, total: Rect) {
        let bar_rect = Rect::from_min_max(
            Pos2::new(total.min.x + SPINE_WIDTH, total.max.y - 30.0),
            Pos2::new(total.max.x - TAB_WIDTH, total.max.y),
        );

        // Bar background — slightly lighter leather
        ui.painter().rect_filled(bar_rect, 0.0, Color32::from_rgb(62, 34, 14));

        let total_pages = self
            .sections
            .get(self.section)
            .map(|s| s.page_count())
            .unwrap_or(1);

        let page_label = format!("  Page {}  /  {}  ", self.page + 1, total_pages);

        // Layout nav widgets inside bar
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar_rect.shrink2(Vec2::new(8.0, 3.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );

        let prev_btn = child.add_enabled(
            self.page > 0,
            egui::Button::new(RichText::new("◀").color(Color32::from_rgb(220, 190, 140))),
        );
        if prev_btn.clicked() && self.page > 0 {
            self.page -= 1;
        }

        child.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let next_btn = ui.add_enabled(
                self.page + 1 < total_pages,
                egui::Button::new(RichText::new("▶").color(Color32::from_rgb(220, 190, 140))),
            );
            if next_btn.clicked() && self.page + 1 < total_pages {
                self.page += 1;
            }

            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.label(
                    RichText::new(page_label)
                        .size(11.0)
                        .color(Color32::from_rgb(210, 180, 120)),
                );
            });
        });
    }
}

impl Default for HandbookWindow {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_section_and_page_are_zero() {
        let w = HandbookWindow::new();
        assert_eq!(w.section, 0);
        assert_eq!(w.page, 0);
    }

    #[test]
    fn with_state_clamps_out_of_bounds() {
        let w = HandbookWindow::with_state(999, 999);
        assert!(w.section < w.sections.len());
        let max_page = w.sections[w.section].page_count();
        assert!(w.page < max_page);
    }

    #[test]
    fn with_state_preserves_valid_indices() {
        let w = HandbookWindow::with_state(2, 1);
        assert_eq!(w.section, 2);
        assert_eq!(w.page, 1);
    }

    #[test]
    fn sections_count_matches_tabs() {
        let w = HandbookWindow::new();
        assert_eq!(w.sections.len(), 6);
    }
}
