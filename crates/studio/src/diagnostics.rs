//! diagnostics.rs — map a language-thread [`Diag`] to an editor underline.
//!
//! `was::check` (via the language thread) reports a diagnostic at a 1-based
//! `line:col`. To draw a squiggle the editor needs a *range*, and the natural
//! one is the token the column points at — so the underline hugs the offending
//! word (`OPEN_EXISTNG`) rather than a single character cell. We reuse the
//! [`syntax`](crate::syntax) lexer for that, which keeps the squiggle aligned
//! with exactly what gets highlighted. Pure and headless-testable.

use crate::lang::Diag;
use crate::syntax::lex_line;

/// The byte range within `line` to underline for a diagnostic at 1-based `col`.
/// Prefers the token starting at the column, else the token covering it, else a
/// one-character range; `col == 0` (a whole-file message) underlines the line.
pub fn underline(line: &str, col: usize) -> (usize, usize) {
    if col == 0 {
        return (0, line.len());
    }
    let target = col - 1; // 1-based column -> 0-based byte offset
    let tokens = lex_line(line);
    if let Some(t) = tokens.iter().find(|t| t.start == target) {
        return (t.start, t.end);
    }
    if let Some(t) = tokens.iter().find(|t| t.start <= target && target < t.end) {
        return (t.start, t.end);
    }
    let start = target.min(line.len());
    (start, (start + 1).min(line.len()).max(start))
}

/// A diagnostic resolved to an editor span: 0-based `row` and a byte range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Underline {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub message: String,
}

/// Resolve every diagnostic over the buffer's `lines` to an [`Underline`].
/// Diagnostics whose line is out of range (e.g. whole-file, line 0) map to row
/// 0 spanning that line.
pub fn underlines(lines: &[&str], diags: &[Diag]) -> Vec<Underline> {
    diags
        .iter()
        .map(|d| {
            let row = d.line.saturating_sub(1).min(lines.len().saturating_sub(1));
            let line = lines.get(row).copied().unwrap_or("");
            let (start, end) = underline(line, if d.line == 0 { 0 } else { d.col });
            Underline { row, start, end, message: d.message.clone() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underline_hugs_the_offending_token() {
        // "mov eax, OPEN_EXISTNG" — the constant starts at byte 9 (col 10).
        let line = "mov eax, OPEN_EXISTNG";
        assert_eq!(line.as_bytes()[9], b'O');
        let (s, e) = underline(line, 10);
        assert_eq!(&line[s..e], "OPEN_EXISTNG");
    }

    #[test]
    fn column_inside_a_token_still_selects_the_whole_token() {
        let line = "mov eax, OPEN_EXISTNG";
        let (s, e) = underline(line, 14); // somewhere in the middle of the constant
        assert_eq!(&line[s..e], "OPEN_EXISTNG");
    }

    #[test]
    fn col_zero_underlines_the_whole_line() {
        let line = "some whole-file note";
        assert_eq!(underline(line, 0), (0, line.len()));
    }

    #[test]
    fn col_past_end_clamps() {
        let line = "ret";
        let (s, e) = underline(line, 99);
        assert!(s <= line.len() && e <= line.len() && s <= e);
    }

    #[test]
    fn underlines_maps_rows_and_messages() {
        let lines = vec![".globl main", "main:", "  mov eax, OPEN_EXISTNG"];
        let diags = vec![Diag {
            line: 3,
            col: 12, // 1-based column of OPEN_EXISTNG on row 2 ("  mov eax, " = 11 chars)
            message: "unknown constant 'OPEN_EXISTNG'".to_string(),
            severity: crate::lang::Severity::Error,
        }];
        let u = underlines(&lines, &diags);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].row, 2);
        assert_eq!(&lines[u[0].row][u[0].start..u[0].end], "OPEN_EXISTNG");
        assert!(u[0].message.contains("unknown constant"));
    }

    #[test]
    fn whole_file_diag_maps_to_first_row() {
        let lines = vec!["a", "b"];
        let diags = vec![Diag { line: 0, col: 0, message: "file-level".to_string(), severity: crate::lang::Severity::Info }];
        let u = underlines(&lines, &diags);
        assert_eq!(u[0].row, 0);
        assert_eq!((u[0].start, u[0].end), (0, 1));
    }
}
