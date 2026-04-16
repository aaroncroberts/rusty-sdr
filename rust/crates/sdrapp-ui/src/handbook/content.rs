#![forbid(unsafe_code)]

//! Data model for Operators Handbook content.
//!
//! Content is fully static — authored as Rust structs and compiled into the
//! binary.  No runtime file I/O or markdown parsing is required.
//!
//! Hierarchy:
//!   `HandbookSection`  — one binder tab (e.g. "Welcome", "App Layout")
//!     └─ `HandbookPage`   — one visible page within a section
//!          └─ `ContentBlock` — individual rendered element on the page

use egui::Color32;

// ── Block types ───────────────────────────────────────────────────────────────

/// A single renderable element on a handbook page.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    /// Large section heading.
    Heading(&'static str),
    /// Smaller sub-heading.
    Subheading(&'static str),
    /// Flowing body paragraph.  Long strings are word-wrapped.
    Body(&'static str),
    /// Embedded screenshot/image from the asset store.
    Image {
        /// Key matching an asset registered in [`crate::handbook::assets::AssetLoader`].
        key: &'static str,
        /// Optional caption rendered below the image.
        caption: Option<&'static str>,
    },
    /// Highlighted tip/warning box with an icon prefix.
    Callout {
        /// Short emoji or symbol prefix, e.g. "💡" or "⚠".
        icon: &'static str,
        /// Body text of the callout.
        text: &'static str,
    },
    /// Keyboard shortcut entry — renders the key as a badge.
    KeyBinding {
        /// Key name, e.g. "F1" or "Ctrl+,".
        key: &'static str,
        /// Human-readable action description.
        action: &'static str,
    },
    /// Unordered bullet list.
    BulletList(&'static [&'static str]),
    /// Numbered (ordered) list.
    NumberedList(&'static [&'static str]),
    /// Visual separator line.
    Divider,
    /// Vertical blank space (adds breathing room).
    Spacer,
}

// ── Page / Section ────────────────────────────────────────────────────────────

/// One page of content inside a handbook section.
#[derive(Debug, Clone)]
pub struct HandbookPage {
    pub blocks: Vec<ContentBlock>,
}

impl HandbookPage {
    pub fn new(blocks: Vec<ContentBlock>) -> Self {
        Self { blocks }
    }
}

/// One tabbed section of the handbook binder.
#[derive(Debug, Clone)]
pub struct HandbookSection {
    /// Short label shown on the binder tab (≤ 8 chars recommended).
    pub title: &'static str,
    /// Full section title shown at the top of each page.
    pub full_title: &'static str,
    /// Tab colour — each section gets a distinct colour.
    pub tab_color: Color32,
    /// All pages in this section, in display order.
    pub pages: Vec<HandbookPage>,
}

impl HandbookSection {
    pub fn new(
        title: &'static str,
        full_title: &'static str,
        tab_color: Color32,
        pages: Vec<HandbookPage>,
    ) -> Self {
        Self {
            title,
            full_title,
            tab_color,
            pages,
        }
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_section() -> HandbookSection {
        HandbookSection::new(
            "TEST",
            "Test Section",
            Color32::RED,
            vec![
                HandbookPage::new(vec![
                    ContentBlock::Heading("Hello"),
                    ContentBlock::Body("Some body text."),
                    ContentBlock::Callout { icon: "💡", text: "A tip" },
                    ContentBlock::KeyBinding { key: "F1", action: "Open handbook" },
                    ContentBlock::BulletList(&["Item A", "Item B"]),
                    ContentBlock::NumberedList(&["Step 1", "Step 2"]),
                    ContentBlock::Divider,
                    ContentBlock::Spacer,
                ]),
                HandbookPage::new(vec![
                    ContentBlock::Subheading("Sub"),
                    ContentBlock::Image { key: "full_layout", caption: Some("Caption") },
                ]),
            ],
        )
    }

    #[test]
    fn section_has_pages() {
        let s = make_section();
        assert_eq!(s.page_count(), 2);
    }

    #[test]
    fn pages_have_blocks() {
        let s = make_section();
        assert!(!s.pages[0].blocks.is_empty());
        assert!(!s.pages[1].blocks.is_empty());
    }

    #[test]
    fn all_block_variants_constructible() {
        // This test acts as a compile-time exhaustiveness check — if a variant
        // is added to ContentBlock without being handled here, the match in
        // renderer.rs will produce a warning.
        let blocks: Vec<ContentBlock> = vec![
            ContentBlock::Heading("H"),
            ContentBlock::Subheading("S"),
            ContentBlock::Body("B"),
            ContentBlock::Image { key: "k", caption: None },
            ContentBlock::Image { key: "k", caption: Some("c") },
            ContentBlock::Callout { icon: "⚠", text: "t" },
            ContentBlock::KeyBinding { key: "X", action: "a" },
            ContentBlock::BulletList(&[]),
            ContentBlock::NumberedList(&[]),
            ContentBlock::Divider,
            ContentBlock::Spacer,
        ];
        assert_eq!(blocks.len(), 11);
    }

    #[test]
    fn tab_color_is_non_transparent() {
        let s = make_section();
        assert_eq!(s.tab_color.a(), 255, "tab colours must be fully opaque");
    }
}
