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
}
