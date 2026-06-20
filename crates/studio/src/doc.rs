//! doc.rs — the editor document: text, caret, edits, and a live token view.
//!
//! The minimal buffer the GUI sits on, and where the headless pieces meet real
//! editing: [`apply_completion`](Doc::apply_completion) lands an autocomplete
//! choice, [`insert`](Doc::insert) drops a (possibly multi-line) snippet, and
//! [`tokens`](Doc::tokens) re-lexes a line for highlighting. Columns are byte
//! offsets within a line (UTF-8 aware). Pure and fully testable.

use crate::syntax::{lex_line, Token};

/// A caret position: 0-based `row`, byte-offset `col` within that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caret {
    pub row: usize,
    pub col: usize,
}

/// A text document with a single caret.
#[derive(Debug, Clone)]
pub struct Doc {
    lines: Vec<String>,
    pub caret: Caret,
}

impl Default for Doc {
    fn default() -> Self {
        Doc { lines: vec![String::new()], caret: Caret::default() }
    }
}

impl Doc {
    pub fn new() -> Doc {
        Doc::default()
    }

    pub fn from_str(text: &str) -> Doc {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        Doc { lines, caret: Caret::default() }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> &str {
        self.lines.get(row).map(String::as_str).unwrap_or("")
    }

    /// Tokens for one line (highlighting).
    pub fn tokens(&self, row: usize) -> Vec<Token> {
        lex_line(self.line(row))
    }

    /// Tokens for every line (the row-of-tokens shape an editor keeps).
    pub fn all_tokens(&self) -> Vec<Vec<Token>> {
        self.lines.iter().map(|l| lex_line(l)).collect()
    }

    /// Clamp and set the caret.
    pub fn set_caret(&mut self, row: usize, col: usize) {
        let row = row.min(self.lines.len().saturating_sub(1));
        let col = col.min(self.lines[row].len());
        self.caret = Caret { row, col };
    }

    /// Insert text at the caret, splitting the line on any `\n`. The caret ends
    /// just after the inserted text.
    pub fn insert(&mut self, s: &str) {
        let Caret { row, col } = self.caret;
        let tail = self.lines[row].split_off(col);
        let mut pieces = s.split('\n');
        let first = pieces.next().unwrap_or("");
        self.lines[row].push_str(first);
        let rest: Vec<&str> = pieces.collect();
        if rest.is_empty() {
            self.caret = Caret { row, col: col + first.len() };
            self.lines[row].push_str(&tail);
        } else {
            let mut at = row + 1;
            for (i, piece) in rest.iter().enumerate() {
                let mut line = piece.to_string();
                if i == rest.len() - 1 {
                    self.caret = Caret { row: at, col: line.len() };
                    line.push_str(&tail);
                }
                self.lines.insert(at, line);
                at += 1;
            }
        }
    }

    /// Delete the character before the caret; at column 0, join with the
    /// previous line.
    pub fn backspace(&mut self) {
        let Caret { row, col } = self.caret;
        if col > 0 {
            let prev = self.lines[row][..col].chars().next_back().unwrap();
            let start = col - prev.len_utf8();
            self.lines[row].replace_range(start..col, "");
            self.caret.col = start;
        } else if row > 0 {
            let cur = self.lines.remove(row);
            let prev_len = self.lines[row - 1].len();
            self.lines[row - 1].push_str(&cur);
            self.caret = Caret { row: row - 1, col: prev_len };
        }
    }

    /// Replace `line[replace_start..caret.col]` with `replacement` (apply an
    /// autocomplete choice) and place the caret just after it.
    pub fn apply_completion(&mut self, replace_start: usize, replacement: &str) {
        let row = self.caret.row;
        let end = self.caret.col;
        let start = replace_start.min(end);
        self.lines[row].replace_range(start..end, replacement);
        self.caret.col = start + replacement.len();
    }

    /// Snap a byte column to the nearest char boundary at or before it, clamped
    /// to the line length — so a column carried across rows never lands mid-char.
    fn snap(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        let mut c = col.min(line.len());
        while c > 0 && !line.is_char_boundary(c) {
            c -= 1;
        }
        c
    }

    /// Move one character left, wrapping to the end of the previous line.
    pub fn move_left(&mut self) {
        let Caret { row, col } = self.caret;
        if col > 0 {
            let prev = self.lines[row][..col].chars().next_back().unwrap();
            self.caret.col = col - prev.len_utf8();
        } else if row > 0 {
            self.caret = Caret { row: row - 1, col: self.lines[row - 1].len() };
        }
    }

    /// Move one character right, wrapping to the start of the next line.
    pub fn move_right(&mut self) {
        let Caret { row, col } = self.caret;
        let line = &self.lines[row];
        if col < line.len() {
            let next = line[col..].chars().next().unwrap();
            self.caret.col = col + next.len_utf8();
        } else if row + 1 < self.lines.len() {
            self.caret = Caret { row: row + 1, col: 0 };
        }
    }

    /// Move to the previous line, keeping the column (clamped to that line).
    pub fn move_up(&mut self) {
        if self.caret.row > 0 {
            let row = self.caret.row - 1;
            self.caret = Caret { row, col: self.snap(row, self.caret.col) };
        }
    }

    /// Move to the next line, keeping the column (clamped to that line).
    pub fn move_down(&mut self) {
        if self.caret.row + 1 < self.lines.len() {
            let row = self.caret.row + 1;
            self.caret = Caret { row, col: self.snap(row, self.caret.col) };
        }
    }

    /// Caret to the start of the line.
    pub fn home(&mut self) {
        self.caret.col = 0;
    }

    /// Caret to the end of the line.
    pub fn end(&mut self) {
        self.caret.col = self.lines[self.caret.row].len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::TokKind;

    #[test]
    fn round_trips_text() {
        assert_eq!(Doc::from_str("a\nb\nc").text(), "a\nb\nc");
        assert_eq!(Doc::from_str("a\nb\nc").line_count(), 3);
    }

    #[test]
    fn insert_advances_caret() {
        let mut d = Doc::new();
        d.insert("mov");
        assert_eq!(d.text(), "mov");
        assert_eq!(d.caret, Caret { row: 0, col: 3 });
    }

    #[test]
    fn insert_with_newline_splits_and_moves_to_next_row() {
        let mut d = Doc::new();
        d.insert("a\nb");
        assert_eq!(d.text(), "a\nb");
        assert_eq!(d.caret, Caret { row: 1, col: 1 });
    }

    #[test]
    fn insert_in_the_middle_keeps_the_tail() {
        let mut d = Doc::from_str("ad");
        d.set_caret(0, 1);
        d.insert("bc");
        assert_eq!(d.text(), "abcd");
        assert_eq!(d.caret, Caret { row: 0, col: 3 });
    }

    #[test]
    fn backspace_within_and_across_lines() {
        let mut d = Doc::from_str("ab");
        d.set_caret(0, 2);
        d.backspace();
        assert_eq!(d.text(), "a");
        assert_eq!(d.caret, Caret { row: 0, col: 1 });

        let mut d = Doc::from_str("a\nb");
        d.set_caret(1, 0);
        d.backspace(); // joins line 1 onto line 0
        assert_eq!(d.text(), "ab");
        assert_eq!(d.caret, Caret { row: 0, col: 1 });
    }

    #[test]
    fn apply_completion_replaces_the_typed_prefix() {
        // "invoke Crea" with the caret at end; complete the prefix at col 7.
        let mut d = Doc::from_str("invoke Crea");
        d.set_caret(0, 11);
        d.apply_completion(7, "CreateFileW");
        assert_eq!(d.text(), "invoke CreateFileW");
        assert_eq!(d.caret, Caret { row: 0, col: 18 });
    }

    #[test]
    fn completion_context_then_apply_round_trips() {
        // The pieces compose: detect context, apply its replace range.
        let mut d = Doc::from_str("invoke Crea");
        d.set_caret(0, 11);
        let ctx = crate::complete::context(d.line(0), d.caret.col);
        d.apply_completion(ctx.start, "CreateFileW");
        assert_eq!(d.text(), "invoke CreateFileW");
    }

    #[test]
    fn tokens_reflect_edits() {
        let mut d = Doc::new();
        d.insert("mov rax, 1");
        let t = d.tokens(0);
        assert_eq!(t[0].kind, TokKind::Mnemonic);
        assert_eq!(t[1].kind, TokKind::Register);
    }

    #[test]
    fn insert_a_multiline_snippet() {
        let mut d = Doc::from_str("head\ntail");
        d.set_caret(0, 4); // end of "head"
        d.insert("\nmov rax, 1\nret");
        assert_eq!(d.text(), "head\nmov rax, 1\nret\ntail");
        assert_eq!(d.caret, Caret { row: 2, col: 3 });
    }

    #[test]
    fn move_left_right_wrap_across_lines() {
        let mut d = Doc::from_str("ab\ncd");
        d.set_caret(1, 0);
        d.move_left(); // wraps to end of line 0
        assert_eq!(d.caret, Caret { row: 0, col: 2 });
        d.move_right(); // wraps back to start of line 1
        assert_eq!(d.caret, Caret { row: 1, col: 0 });
        // At the very start, left is a no-op.
        d.set_caret(0, 0);
        d.move_left();
        assert_eq!(d.caret, Caret { row: 0, col: 0 });
        // At the very end, right is a no-op.
        d.set_caret(1, 2);
        d.move_right();
        assert_eq!(d.caret, Caret { row: 1, col: 2 });
    }

    #[test]
    fn move_up_down_clamps_the_column() {
        let mut d = Doc::from_str("longline\nhi\nanother");
        d.set_caret(0, 8); // end of "longline"
        d.move_down(); // "hi" is shorter → clamp to col 2
        assert_eq!(d.caret, Caret { row: 1, col: 2 });
        d.move_down(); // "another" is long enough → keep col 2
        assert_eq!(d.caret, Caret { row: 2, col: 2 });
        d.move_up();
        assert_eq!(d.caret, Caret { row: 1, col: 2 });
    }

    #[test]
    fn home_and_end() {
        let mut d = Doc::from_str("  mov rax, 1");
        d.set_caret(0, 5);
        d.home();
        assert_eq!(d.caret, Caret { row: 0, col: 0 });
        d.end();
        assert_eq!(d.caret, Caret { row: 0, col: 12 });
    }
}
