//! snippet.rs — the editable insert frame (tabstops), as a tested model.
//!
//! When the user double-clicks a function card, its insert frame drops into the
//! editor as a *template*: the caret lands on the first hole, Tab cycles the
//! holes, text fields are typed into and enum fields are chosen from a dropdown,
//! and what's left in the buffer is always a valid `invoke`. This is the engine
//! behind that — built on [`ide::widget`], pure, and headless-testable. The GUI
//! draws `text` with the active field boxed and renders a dropdown for a field
//! that has `options`; everything else here is plain editing arithmetic.

use ide::widget::{self, Span};

/// One tabstop in the inserted template. `text[start..end]` is its current
/// value. `options` non-empty means a dropdown; empty means a free text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub options: Vec<String>,
}

impl Field {
    pub fn is_dropdown(&self) -> bool {
        !self.options.is_empty()
    }
}

/// An inserted insert-frame with live tabstops.
#[derive(Debug, Clone)]
pub struct Snippet {
    /// The current text in the buffer (one line, for an `invoke`).
    pub text: String,
    /// Tabstops in tab order; ranges are kept consistent with `text`.
    pub fields: Vec<Field>,
    /// Index of the focused tabstop.
    pub active: usize,
}

impl Snippet {
    /// Build from a frame line (e.g. `ide::insert_frame` output): render each
    /// hole at its default (a dropdown's first option, a field's `<name>`
    /// placeholder) and record where each landed.
    pub fn from_frame(frame: &str) -> Snippet {
        let mut text = String::new();
        let mut fields = Vec::new();
        for span in widget::parse(frame) {
            match span {
                Span::Text(t) => text.push_str(&t),
                Span::Field { name } => {
                    let start = text.len();
                    text.push_str(&placeholder(&name));
                    let end = text.len();
                    fields.push(Field { name, start, end, options: Vec::new() });
                }
                Span::Select { name, options } => {
                    let start = text.len();
                    text.push_str(&options[0]);
                    let end = text.len();
                    fields.push(Field { name, start, end, options });
                }
            }
        }
        Snippet { text, fields, active: 0 }
    }

    /// The focused tabstop, if any.
    pub fn active_field(&self) -> Option<&Field> {
        self.fields.get(self.active)
    }

    /// The current value of the focused tabstop.
    pub fn active_value(&self) -> &str {
        match self.fields.get(self.active) {
            Some(f) => &self.text[f.start..f.end],
            None => "",
        }
    }

    /// Replace the focused tabstop's value, shifting later tabstops to match.
    /// (The GUI enforces that a dropdown's value is one of its options; this
    /// just edits the text.)
    pub fn set_active_value(&mut self, value: &str) {
        let Some(f) = self.fields.get(self.active).cloned() else { return };
        self.text.replace_range(f.start..f.end, value);
        let new_end = f.start + value.len();
        let delta = new_end as isize - f.end as isize;
        self.fields[self.active].end = new_end;
        for later in &mut self.fields[self.active + 1..] {
            later.start = (later.start as isize + delta) as usize;
            later.end = (later.end as isize + delta) as usize;
        }
    }

    /// Move focus to the next tabstop; returns false if already at the last.
    pub fn next(&mut self) -> bool {
        if self.active + 1 < self.fields.len() {
            self.active += 1;
            true
        } else {
            false
        }
    }

    /// Move focus to the previous tabstop; returns false if already at the first.
    pub fn prev(&mut self) -> bool {
        if self.active > 0 {
            self.active -= 1;
            true
        } else {
            false
        }
    }

    /// No free-text field still shows its `<name>` placeholder — i.e. every hole
    /// the user must fill has been filled. Dropdowns always count as filled.
    pub fn is_complete(&self) -> bool {
        self.fields.iter().all(|f| {
            f.is_dropdown() || self.text[f.start..f.end] != placeholder(&f.name)
        })
    }

    /// The finished line to commit to the buffer.
    pub fn finish(self) -> String {
        self.text
    }
}

fn placeholder(name: &str) -> String {
    format!("<{name}>")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: &str = "invoke F, {{field:path}}, {{select:mode|READ,WRITE}}, {{field:flags}}";

    #[test]
    fn defaults_render_and_fields_locate() {
        let s = Snippet::from_frame(FRAME);
        assert_eq!(s.text, "invoke F, <path>, READ, <flags>");
        assert_eq!(s.fields.len(), 3);
        // Each recorded range slices back to its current value.
        assert_eq!(&s.text[s.fields[0].start..s.fields[0].end], "<path>");
        assert_eq!(&s.text[s.fields[1].start..s.fields[1].end], "READ");
        assert_eq!(&s.text[s.fields[2].start..s.fields[2].end], "<flags>");
        assert!(!s.fields[0].is_dropdown());
        assert_eq!(s.fields[1].options, vec!["READ".to_string(), "WRITE".to_string()]);
    }

    #[test]
    fn fill_a_field_shifts_later_ranges() {
        let mut s = Snippet::from_frame(FRAME);
        s.set_active_value("rcx"); // path: 6 chars -> 3, shifts the rest left
        assert_eq!(s.text, "invoke F, rcx, READ, <flags>");
        // The later tabstops still slice to the right values after the shift.
        assert_eq!(&s.text[s.fields[1].start..s.fields[1].end], "READ");
        assert_eq!(&s.text[s.fields[2].start..s.fields[2].end], "<flags>");
    }

    #[test]
    fn choosing_a_dropdown_grows_ranges_correctly() {
        let mut s = Snippet::from_frame(FRAME);
        s.next();
        assert_eq!(s.active_value(), "READ");
        s.set_active_value("WRITE"); // 4 -> 5 chars, shifts the rest right
        assert_eq!(s.text, "invoke F, <path>, WRITE, <flags>");
        assert_eq!(&s.text[s.fields[2].start..s.fields[2].end], "<flags>");
    }

    #[test]
    fn tab_navigation_bounds() {
        let mut s = Snippet::from_frame(FRAME);
        assert_eq!(s.active, 0);
        assert!(s.next() && s.next());
        assert_eq!(s.active, 2);
        assert!(!s.next(), "stops at last");
        assert!(s.prev() && s.prev());
        assert_eq!(s.active, 0);
        assert!(!s.prev(), "stops at first");
    }

    #[test]
    fn completeness_tracks_unfilled_text_fields() {
        let mut s = Snippet::from_frame(FRAME);
        assert!(!s.is_complete(), "two text fields still placeholders");
        s.set_active_value("rcx"); // fill path
        assert!(!s.is_complete(), "flags still a placeholder");
        s.active = 2;
        s.set_active_value("0"); // fill flags
        assert!(s.is_complete(), "all text fields filled; dropdown counts as filled");
    }

    #[test]
    fn filled_snippet_is_a_clean_invoke() {
        let mut s = Snippet::from_frame(FRAME);
        s.set_active_value("rcx");
        s.active = 2;
        s.set_active_value("0");
        let line = s.finish();
        assert_eq!(line, "invoke F, rcx, READ, 0");
        assert!(!line.contains('<') && !line.contains("{{"));
    }

    #[test]
    fn frame_without_holes_is_inert() {
        let s = Snippet::from_frame("invoke GetLastError");
        assert_eq!(s.text, "invoke GetLastError");
        assert!(s.fields.is_empty());
        assert!(s.is_complete());
        assert!(s.active_field().is_none());
    }
}
