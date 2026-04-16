#![forbid(unsafe_code)]

//! All handbook sections — assembled into the full binder content.

mod controls;
mod first_signal;
mod layout;
mod midi_shortcuts;
mod signals_modes;
mod welcome;

use crate::handbook::content::HandbookSection;

/// Return all six handbook sections in tab display order.
pub fn all_sections() -> Vec<HandbookSection> {
    vec![
        welcome::section(),
        layout::section(),
        first_signal::section(),
        signals_modes::section(),
        controls::section(),
        midi_shortcuts::section(),
    ]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handbook::content::ContentBlock;

    #[test]
    fn six_sections_exist() {
        let sections = all_sections();
        assert_eq!(sections.len(), 6, "handbook must have exactly 6 sections");
    }

    #[test]
    fn each_section_has_at_least_two_pages() {
        for s in all_sections() {
            assert!(
                s.page_count() >= 2,
                "section '{}' has only {} page(s); need at least 2",
                s.full_title,
                s.page_count()
            );
        }
    }

    #[test]
    fn no_section_has_empty_pages() {
        for section in all_sections() {
            for (i, page) in section.pages.iter().enumerate() {
                assert!(
                    !page.blocks.is_empty(),
                    "section '{}' page {} has no content blocks",
                    section.full_title,
                    i + 1
                );
            }
        }
    }

    #[test]
    fn all_sections_have_unique_titles() {
        let sections = all_sections();
        let titles: Vec<_> = sections.iter().map(|s| s.title).collect();
        let mut unique = titles.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(titles.len(), unique.len(), "duplicate section titles found");
    }

    #[test]
    fn all_sections_have_unique_colors() {
        let sections = all_sections();
        let colors: Vec<_> = sections.iter().map(|s| s.tab_color).collect();
        let mut unique = colors.clone();
        unique.sort_unstable_by_key(|c| (c.r(), c.g(), c.b()));
        unique.dedup();
        assert_eq!(colors.len(), unique.len(), "duplicate section tab colours found");
    }

    #[test]
    fn headings_present_in_every_section() {
        for section in all_sections() {
            let has_heading = section.pages.iter().any(|p| {
                p.blocks
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Heading(_)))
            });
            assert!(
                has_heading,
                "section '{}' has no Heading blocks",
                section.full_title
            );
        }
    }

    #[test]
    fn no_placeholder_text_in_content() {
        // Guard against accidentally shipping TODO/Lorem Ipsum placeholder text.
        let forbidden = ["TODO", "TBD", "Lorem ipsum", "placeholder", "FIXME"];
        for section in all_sections() {
            for page in &section.pages {
                for block in &page.blocks {
                    let text = block_text(block);
                    for bad in &forbidden {
                        assert!(
                            !text.contains(bad),
                            "section '{}' contains placeholder text '{}'",
                            section.full_title,
                            bad
                        );
                    }
                }
            }
        }
    }

    /// Extract all text strings from a content block for inspection.
    fn block_text(block: &ContentBlock) -> String {
        match block {
            ContentBlock::Heading(t)
            | ContentBlock::Subheading(t)
            | ContentBlock::Body(t) => t.to_string(),
            ContentBlock::Callout { text, .. } => text.to_string(),
            ContentBlock::KeyBinding { action, .. } => action.to_string(),
            ContentBlock::BulletList(items) | ContentBlock::NumberedList(items) => {
                items.join(" ")
            }
            ContentBlock::Image { caption: Some(c), .. } => c.to_string(),
            _ => String::new(),
        }
    }
}
