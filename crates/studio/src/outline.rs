//! outline.rs — the document's label definitions (pure).
//!
//! Drives an outline pane and go-to-definition. A label is what the syntax lexer
//! tags as one at the head of a line, so this agrees with what the editor draws.

use crate::syntax::{lex_line, TokKind};

/// Every label definition in `text`, as `(name, 0-based line)`, in source order.
pub fn labels(text: &str) -> Vec<(String, usize)> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| match lex_line(line).first() {
            Some(t) if t.kind == TokKind::Label => Some((line[t.start..t.end].to_string(), i)),
            _ => None,
        })
        .collect()
}

/// The line a label is defined on, if any.
pub fn line_of(text: &str, label: &str) -> Option<usize> {
    labels(text).into_iter().find(|(n, _)| n == label).map(|(_, l)| l)
}

/// Whether a label names code (jump/call targets, `proc`, `macro`) or data
/// (declarations, equates) — for colour-coding the go-to-label palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    Code,
    Data,
}

/// A navigable label: its `name`, 0-based `line`, and code/data `kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub line: usize,
    pub kind: LabelKind,
}

/// Every navigable label in `text`, classified code vs data, in source order.
/// Covers `name:` (code, or data when it heads a data directive), MASM data
/// declarations (`name BYTE …`), `proc`/`macro` definitions, and equates
/// (`name equ …` / `name = …`).
pub fn classified(text: &str) -> Vec<Label> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len()).filter_map(|i| classify_line(lines[i], &lines, i)).collect()
}

fn classify_line(line: &str, lines: &[&str], i: usize) -> Option<Label> {
    let toks = lex_line(line);
    let t0 = toks.first()?;

    // `name:` — code unless its body (this line, else the next content line) is
    // a data directive.
    if t0.kind == TokKind::Label {
        let name = line[t0.start..t0.end].to_string();
        let rest = line[toks.get(1).map(|t| t.end).unwrap_or(line.len())..].trim();
        let data = if rest.is_empty() {
            next_content_line(lines, i).is_some_and(first_word_is_data)
        } else {
            first_word_is_data(rest)
        };
        return Some(Label { name, line: i, kind: if data { LabelKind::Data } else { LabelKind::Code } });
    }

    // Non-colon forms.
    let w0 = &line[t0.start..t0.end];
    let w1 = toks.get(1).map(|t| &line[t.start..t.end]).unwrap_or("");
    let w2 = toks.get(2).map(|t| &line[t.start..t.end]).unwrap_or("");
    let (l0, l1) = (w0.to_ascii_lowercase(), w1.to_ascii_lowercase());

    if l0 == "proc" && is_ident(w1) {
        return Some(Label { name: w1.into(), line: i, kind: LabelKind::Code });
    }
    if l0 == "struct" && is_ident(w1) {
        return Some(Label { name: w1.into(), line: i, kind: LabelKind::Data });
    }
    if l1 == "macro" && is_ident(w0) {
        return Some(Label { name: w0.into(), line: i, kind: LabelKind::Code });
    }
    if (l1 == "equ" || w1 == "=") && is_ident(w0) {
        return Some(Label { name: w0.into(), line: i, kind: LabelKind::Data });
    }
    // MASM data declaration `name TYPE …` — but not an instruction's size
    // override (`mov dword ptr [..]`), which is a type word followed by `ptr`.
    if is_ident(w0) && first_word_is_data(w1) && !w2.eq_ignore_ascii_case("ptr") {
        return Some(Label { name: w0.into(), line: i, kind: LabelKind::Data });
    }
    None
}

fn next_content_line<'a>(lines: &[&'a str], i: usize) -> Option<&'a str> {
    lines[i + 1..].iter().copied().find(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with(';') && !t.starts_with('#')
    })
}

/// Does the first word of `s` name a data directive / type?
fn first_word_is_data(s: &str) -> bool {
    let w = s.trim().split_whitespace().next().unwrap_or("");
    let w = w.trim_start_matches('.').to_ascii_lowercase();
    matches!(
        w.as_str(),
        "byte" | "sbyte" | "word" | "sword" | "dword" | "sdword" | "qword" | "sqword" | "tbyte"
            | "wchar" | "db" | "dw" | "dd" | "dq" | "dt" | "long" | "quad" | "ascii" | "asciz"
            | "asciistring" | "widestring" | "string" | "zero" | "skip" | "space" | "single"
            | "double" | "real4" | "real8" | "real10"
    )
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "\
.globl main
main:
    mov rax, 1
    jmp done
loop_body:
    dec rax
done:
    ret
";

    #[test]
    fn collects_labels_with_line_numbers() {
        assert_eq!(
            labels(SRC),
            vec![
                ("main".to_string(), 1),
                ("loop_body".to_string(), 4),
                ("done".to_string(), 6),
            ]
        );
    }

    #[test]
    fn go_to_definition() {
        assert_eq!(line_of(SRC, "done"), Some(6));
        assert_eq!(line_of(SRC, "nope"), None);
    }

    #[test]
    fn a_jmp_target_is_not_a_definition() {
        // `jmp done` references done; only the `done:` line is a definition.
        let defs = labels(SRC);
        assert_eq!(defs.iter().filter(|(n, _)| n == "done").count(), 1);
    }

    #[test]
    fn classified_separates_code_and_data_labels() {
        let src = "\
.globl main
banner BYTE \"hi\", 0
buf BYTE 64 dup(0)
freq real8 1.5
COUNT equ 10
LIMIT = 20
main:
    mov rax, 1
    mov dword ptr [rsp], 0
    ret
table:
    .quad 1, 2, 3
proc helper uses rbx
    ret
APPEND macro chr
    inc rcx
endm";
        let found = classified(src);
        let got: Vec<(&str, LabelKind)> =
            found.iter().map(|l| (l.name.as_str(), l.kind)).collect();
        use LabelKind::{Code, Data};
        assert_eq!(
            got,
            vec![
                ("banner", Data),
                ("buf", Data),
                ("freq", Data),
                ("COUNT", Data),
                ("LIMIT", Data),
                ("main", Code),     // followed by an instruction
                ("table", Data),    // followed by `.quad`
                ("helper", Code),   // proc
                ("APPEND", Code),   // macro
            ]
        );
        // The `mov dword ptr` size override must NOT register as a data label.
        assert!(classified(src).iter().all(|l| l.name != "mov"));
    }
}
