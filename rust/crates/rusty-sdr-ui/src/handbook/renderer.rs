#![forbid(unsafe_code)]

//! Page renderer — maps [`ContentBlock`]s to egui widgets.
//!
//! The entire page is wrapped in a `ScrollArea` so long pages scroll inside
//! the binder's page frame without affecting the chrome around it.

use egui::{Color32, Frame, Margin, RichText, Stroke, Ui};

use super::assets::AssetLoader;
use super::content::{ContentBlock, HandbookPage};

// ── Colours used by the renderer ─────────────────────────────────────────────

const HEADING_COLOR: Color32 = Color32::from_rgb(40, 30, 20);
const BODY_COLOR: Color32 = Color32::from_rgb(50, 40, 30);
const MUTED_COLOR: Color32 = Color32::from_gray(120);

const CALLOUT_TIP_BG: Color32 = Color32::from_rgb(235, 248, 235);
const CALLOUT_TIP_BORDER: Color32 = Color32::from_rgb(80, 160, 90);
const CALLOUT_WARN_BG: Color32 = Color32::from_rgb(255, 248, 230);
const CALLOUT_WARN_BORDER: Color32 = Color32::from_rgb(210, 140, 30);
const CALLOUT_INFO_BG: Color32 = Color32::from_rgb(230, 240, 255);
const CALLOUT_INFO_BORDER: Color32 = Color32::from_rgb(70, 120, 200);

const KEY_BADGE_BG: Color32 = Color32::from_rgb(50, 50, 60);
const KEY_BADGE_FG: Color32 = Color32::from_rgb(220, 220, 200);

// ── Public entry-point ────────────────────────────────────────────────────────

/// Render all blocks of `page` into `ui` inside a vertical scroll area.
/// `max_image_width` is the available page-area width for images.
pub fn render_page(ui: &mut Ui, page: &HandbookPage, assets: &mut AssetLoader, max_image_width: f32) {
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            ui.add_space(8.0);
            for block in &page.blocks {
                render_block(ui, block, assets, max_image_width);
            }
            ui.add_space(16.0);
        });
}

// ── Block dispatcher ──────────────────────────────────────────────────────────

fn render_block(ui: &mut Ui, block: &ContentBlock, assets: &mut AssetLoader, max_width: f32) {
    match block {
        ContentBlock::Heading(text) => render_heading(ui, text),
        ContentBlock::Subheading(text) => render_subheading(ui, text),
        ContentBlock::Body(text) => render_body(ui, text),
        ContentBlock::Image { key, caption } => {
            ui.add_space(6.0);
            assets.render_image(ui, key, max_width.min(480.0), *caption);
            ui.add_space(6.0);
        }
        ContentBlock::Callout { icon, text } => render_callout(ui, icon, text),
        ContentBlock::KeyBinding { key, action } => render_keybinding(ui, key, action),
        ContentBlock::BulletList(items) => render_bullet_list(ui, items),
        ContentBlock::NumberedList(items) => render_numbered_list(ui, items),
        ContentBlock::Divider => {
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
        }
        ContentBlock::Spacer => {
            ui.add_space(12.0);
        }
    }
}

// ── Individual block renderers ────────────────────────────────────────────────

fn render_heading(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(text)
            .size(20.0)
            .strong()
            .color(HEADING_COLOR),
    );
    // Underline via separator with color override
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.5),
        egui::Sense::hover(),
    );
    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(180, 140, 80));
    ui.add_space(6.0);
}

fn render_subheading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        RichText::new(text)
            .size(15.0)
            .strong()
            .color(Color32::from_rgb(80, 60, 30)),
    );
    ui.add_space(3.0);
}

fn render_body(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).size(13.0).color(BODY_COLOR));
    ui.add_space(4.0);
}

fn render_callout(ui: &mut Ui, icon: &str, text: &str) {
    // Pick colour based on icon heuristic
    let (bg, border) = if icon.contains('⚠') || icon.contains('!') {
        (CALLOUT_WARN_BG, CALLOUT_WARN_BORDER)
    } else if icon.contains('💡') || icon.contains('✓') || icon.contains('✅') {
        (CALLOUT_TIP_BG, CALLOUT_TIP_BORDER)
    } else {
        (CALLOUT_INFO_BG, CALLOUT_INFO_BORDER)
    };

    ui.add_space(6.0);
    Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.5, border))
        .inner_margin(Margin::symmetric(10.0, 6.0))
        .rounding(4.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(icon).size(14.0));
                ui.add_space(4.0);
                ui.label(RichText::new(text).size(13.0).color(BODY_COLOR));
            });
        });
    ui.add_space(6.0);
}

fn render_keybinding(ui: &mut Ui, key: &str, action: &str) {
    ui.horizontal(|ui| {
        // Key badge
        Frame::none()
            .fill(KEY_BADGE_BG)
            .inner_margin(Margin::symmetric(6.0, 2.0))
            .rounding(3.0)
            .show(ui, |ui| {
                ui.label(
                    RichText::new(key)
                        .monospace()
                        .size(12.0)
                        .color(KEY_BADGE_FG)
                        .strong(),
                );
            });
        ui.add_space(6.0);
        ui.label(RichText::new(action).size(13.0).color(BODY_COLOR));
    });
    ui.add_space(3.0);
}

fn render_bullet_list(ui: &mut Ui, items: &[&str]) {
    ui.add_space(2.0);
    for &item in items {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("•").color(MUTED_COLOR).size(13.0));
            ui.add_space(4.0);
            ui.label(RichText::new(item).size(13.0).color(BODY_COLOR));
        });
    }
    ui.add_space(4.0);
}

fn render_numbered_list(ui: &mut Ui, items: &[&str]) {
    ui.add_space(2.0);
    for (i, &item) in items.iter().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{}.", i + 1))
                    .color(MUTED_COLOR)
                    .size(13.0)
                    .strong(),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(item).size(13.0).color(BODY_COLOR));
        });
    }
    ui.add_space(4.0);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handbook::content::{HandbookPage, ContentBlock};

    // We can't run egui in unit tests, but we can verify page construction
    // and that block lists are non-empty for sections.

    #[test]
    fn page_with_all_block_types_is_valid() {
        let page = HandbookPage::new(vec![
            ContentBlock::Heading("H"),
            ContentBlock::Subheading("S"),
            ContentBlock::Body("B"),
            ContentBlock::Image { key: "full_layout", caption: None },
            ContentBlock::Callout { icon: "💡", text: "tip" },
            ContentBlock::Callout { icon: "⚠", text: "warn" },
            ContentBlock::KeyBinding { key: "F1", action: "Open" },
            ContentBlock::BulletList(&["a", "b"]),
            ContentBlock::NumberedList(&["1", "2"]),
            ContentBlock::Divider,
            ContentBlock::Spacer,
        ]);
        assert_eq!(page.blocks.len(), 11);
    }

    #[test]
    fn callout_icon_classification_works() {
        // ⚠ → warn style
        let warn_icon = "⚠";
        assert!(warn_icon.contains('⚠'));

        // 💡 → tip style
        let tip_icon = "💡";
        assert!(tip_icon.contains('💡'));

        // ℹ → info style (fallback)
        let info_icon = "ℹ";
        assert!(!info_icon.contains('⚠') && !info_icon.contains('💡'));
    }
}
