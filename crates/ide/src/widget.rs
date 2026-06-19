//! widget.rs — the interactive insert frame, as a tested model.
//!
//! The function card's centerpiece is a fill-in-the-blanks `invoke`: text inputs
//! for plain args, dropdowns for enum args, then double-click to drop a correct
//! line into the editor. The render engine (docpane, extended) draws it and
//! captures input — but the *model* (parse the frame → which holes exist, in tab
//! order → render the filled line) is pure logic we can prove in the terminal,
//! exactly as we did the rest of the brain.
//!
//! Syntax (also what `insert_frame` emits):
//!   {{field:NAME}}            a text input; NAME is the placeholder
//!   {{select:NAME|A,B,C}}     a dropdown; first option is the default
//! Anything else is literal text. A malformed `{{…}}` is kept verbatim as text,
//! so the model never loses characters.

use std::collections::BTreeMap;

/// One piece of an insert frame: literal text, or an interactive hole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Field { name: String },
    Select { name: String, options: Vec<String> },
}

impl Span {
    /// The hole's parameter name, or `None` for literal text.
    pub fn name(&self) -> Option<&str> {
        match self {
            Span::Text(_) => None,
            Span::Field { name } | Span::Select { name, .. } => Some(name),
        }
    }
    pub fn is_hole(&self) -> bool {
        !matches!(self, Span::Text(_))
    }
}

/// Parse a frame line into spans. Never panics; unknown/malformed `{{…}}` stays
/// literal.
pub fn parse(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut text = String::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(close) = s[i + 2..].find("}}") {
                let inner = &s[i + 2..i + 2 + close];
                if let Some(span) = parse_marker(inner) {
                    if !text.is_empty() {
                        out.push(Span::Text(std::mem::take(&mut text)));
                    }
                    out.push(span);
                    i = i + 2 + close + 2;
                    continue;
                }
            }
        }
        // Not a recognized marker: consume one char as literal text.
        let ch = s[i..].chars().next().unwrap();
        text.push(ch);
        i += ch.len_utf8();
    }
    if !text.is_empty() {
        out.push(Span::Text(text));
    }
    out
}

fn parse_marker(inner: &str) -> Option<Span> {
    if let Some(name) = inner.strip_prefix("field:") {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        return Some(Span::Field { name: name.to_string() });
    }
    if let Some(rest) = inner.strip_prefix("select:") {
        let (name, opts) = rest.split_once('|')?;
        let options: Vec<String> = opts
            .split(',')
            .map(|o| o.trim().to_string())
            .filter(|o| !o.is_empty())
            .collect();
        if name.trim().is_empty() || options.is_empty() {
            return None;
        }
        return Some(Span::Select { name: name.trim().to_string(), options });
    }
    None
}

/// The interactive holes in tab order — what the UI turns into controls.
pub fn holes(spans: &[Span]) -> Vec<&Span> {
    spans.iter().filter(|s| s.is_hole()).collect()
}

/// Render with every hole left at its default: a dropdown picks its first
/// option (a real constant); a text field shows `<name>` for the user to
/// replace. This equals the non-interactive snippet form.
pub fn defaults(spans: &[Span]) -> String {
    render(spans, &BTreeMap::new())
}

/// Render with user-supplied values keyed by parameter name. A field with no
/// value falls back to `<name>`; a dropdown with no (or an invalid) value falls
/// back to its first option.
pub fn render(spans: &[Span], values: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            Span::Text(t) => out.push_str(t),
            Span::Field { name } => match values.get(name) {
                Some(v) if !v.is_empty() => out.push_str(v),
                _ => out.push_str(&format!("<{name}>")),
            },
            Span::Select { name, options } => {
                let chosen = values
                    .get(name)
                    .filter(|v| options.iter().any(|o| o == *v))
                    .cloned()
                    .unwrap_or_else(|| options[0].clone());
                out.push_str(&chosen);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields_and_selects() {
        let spans = parse("invoke F, {{field:a}}, {{select:b|X,Y,Z}}");
        assert_eq!(
            spans,
            vec![
                Span::Text("invoke F, ".into()),
                Span::Field { name: "a".into() },
                Span::Text(", ".into()),
                Span::Select { name: "b".into(), options: vec!["X".into(), "Y".into(), "Z".into()] },
            ]
        );
    }

    #[test]
    fn holes_are_in_tab_order() {
        let spans = parse("{{field:a}} x {{select:b|P,Q}} y {{field:c}}");
        let names: Vec<_> = holes(&spans).iter().filter_map(|s| s.name()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn defaults_pick_first_option_and_placeholder_fields() {
        let spans = parse("invoke F, {{field:path}}, {{select:mode|READ,WRITE}}");
        assert_eq!(defaults(&spans), "invoke F, <path>, READ");
    }

    #[test]
    fn render_fills_values_and_validates_dropdown() {
        let spans = parse("invoke F, {{field:path}}, {{select:mode|READ,WRITE}}");
        let mut v = BTreeMap::new();
        v.insert("path".to_string(), "rcx".to_string());
        v.insert("mode".to_string(), "WRITE".to_string());
        assert_eq!(render(&spans, &v), "invoke F, rcx, WRITE");
        // An out-of-set dropdown value falls back to the first option.
        v.insert("mode".to_string(), "BOGUS".to_string());
        assert_eq!(render(&spans, &v), "invoke F, rcx, READ");
    }

    #[test]
    fn malformed_markers_stay_literal() {
        // Missing `|` in select, empty field, unclosed braces — all kept as text.
        let spans = parse("a {{select:x}} b {{field:}} c {{oops");
        assert_eq!(spans, vec![Span::Text("a {{select:x}} b {{field:}} c {{oops".into())]);
    }
}
