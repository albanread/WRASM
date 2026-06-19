//! studio — the IDE front-end.
//!
//! The assistant pane is `ide`'s markdown drawn by `docpane`'s DirectWrite
//! engine. docpane splits cleanly: `parser` + `layout` are pure (markdown →
//! draw commands, with text measurement injected as a function), and only
//! `render` touches Direct2D. So the whole content pipeline — query → markdown →
//! parse → layout → draw commands + clickable regions — is testable here with a
//! stub measurer, before a window exists. That's this module: the headless seam.
//!
//!   let lo = studio::layout_markdown(&ide::answer(&kb, "CreateFileW")?, 800.0);
//!   // lo.cmds  — FillRect / StrokeLine / Text / … the renderer interprets
//!   // lo.hits  — clickable regions; our search links carry `was:<name>` hrefs
//!
//! Navigation is uniform: every card link uses the `was:` scheme, so the pane's
//! click handler is just "strip `was:`, answer() the rest, re-layout".

use docpane::layout::{self, Layout};
use docpane::parser;

pub mod complete;
pub mod diagnostics;
pub mod lang;
pub mod snippet;
pub mod syntax;

/// Text-measurement stub for headless layout and tests. The windowed app injects
/// a DirectWrite-backed measurer instead; only absolute widths differ, not the
/// structure of the produced draw commands.
pub fn stub_measure(text: &str, _font: &str, size: f32, _bold: bool, _italic: bool) -> f32 {
    text.chars().count() as f32 * size * 0.6
}

/// Parse and lay out markdown at a content width, using the stub measurer.
pub fn layout_markdown(md: &str, width: f32) -> Layout {
    let blocks = parser::parse(md);
    layout::layout(&blocks, 0.0, width, 0.0, stub_measure)
}

/// The `was:` target a link href points at, if any — what the pane navigates to.
/// `"was:CreateFileW"` → `Some("CreateFileW")`.
pub fn nav_target(href: &str) -> Option<&str> {
    href.strip_prefix("was:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use docpane::layout::DrawCmd;
    use winkb::Kb;

    fn kb() -> Option<Kb> {
        let path = std::env::var("WINKB_DB")
            .unwrap_or_else(|_| r"E:\windows_api\windows_api.db".to_string());
        Kb::open(&path).ok()
    }

    #[test]
    fn function_card_produces_draw_commands() {
        let Some(kb) = kb() else { return };
        let md = ide::answer(&kb, "CreateFileW").unwrap();
        let lo = layout_markdown(&md, 800.0);
        assert!(lo.total_h > 0.0, "non-empty layout");
        assert!(
            lo.cmds.iter().any(|c| matches!(c, DrawCmd::Text { .. })),
            "card lays out some text",
        );
        // The heading "# CreateFileW" should be extracted for the find feature.
        assert!(
            lo.headings.iter().any(|(h, _)| h.contains("CreateFileW")),
            "heading extracted: {:?}",
            lo.headings,
        );
    }

    #[test]
    fn search_card_yields_was_nav_links() {
        let Some(kb) = kb() else { return };
        let md = ide::answer(&kb, "CreateFile").unwrap();
        let lo = layout_markdown(&md, 800.0);
        let targets: Vec<&str> = lo.hits.iter().filter_map(|h| nav_target(&h.href)).collect();
        assert!(
            targets.iter().any(|t| t.contains("CreateFile")),
            "clickable was: nav links present: {:?}",
            lo.hits.iter().map(|h| &h.href).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn struct_card_lays_out_a_table() {
        let Some(kb) = kb() else { return };
        let md = ide::answer(&kb, "RECT").unwrap();
        let lo = layout_markdown(&md, 800.0);
        // A table draws cell backgrounds / rules as fills and strokes.
        assert!(
            lo.cmds.iter().any(|c| matches!(c, DrawCmd::FillRect { .. } | DrawCmd::StrokeLine { .. })),
            "table renders rects/lines",
        );
    }
}
