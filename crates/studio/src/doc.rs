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

/// A document snapshot for undo/redo.
#[derive(Debug, Clone)]
struct Snapshot {
    lines: Vec<String>,
    caret: Caret,
    anchor: Option<Caret>,
}

/// A text document with a single caret, an optional selection anchor, and
/// undo/redo history.
#[derive(Debug, Clone)]
pub struct Doc {
    lines: Vec<String>,
    pub caret: Caret,
    /// The fixed end of the selection (the caret is the moving end); `None` =
    /// no selection.
    anchor: Option<Caret>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// True while a run of typed characters may merge into one undo step.
    coalescing: bool,
}

impl Default for Doc {
    fn default() -> Self {
        Doc {
            lines: vec![String::new()],
            caret: Caret::default(),
            anchor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing: false,
        }
    }
}

impl Doc {
    pub fn new() -> Doc {
        Doc::default()
    }

    pub fn from_str(text: &str) -> Doc {
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        Doc { lines, ..Doc::default() }
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

    /// Clamp and set the caret (also ends an undo-coalesce run).
    pub fn set_caret(&mut self, row: usize, col: usize) {
        let row = row.min(self.lines.len().saturating_sub(1));
        self.caret = Caret { row, col: self.snap(row, col) };
        self.coalescing = false;
    }

    /// Insert text at the caret (replacing any selection) as one undo step.
    /// Splits the line on any `\n`; the caret ends just after the inserted text.
    pub fn insert(&mut self, s: &str) {
        self.push_undo();
        self.coalescing = false;
        self.delete_selection();
        self.insert_raw(s);
    }

    /// Type a single character — coalescing into the current undo step and
    /// replacing any selection. Use this for keystrokes so undo groups a run.
    pub fn type_char(&mut self, c: char) {
        if !self.coalescing {
            self.push_undo();
            self.coalescing = true;
        }
        self.delete_selection();
        let mut buf = [0u8; 4];
        self.insert_raw(c.encode_utf8(&mut buf));
    }

    /// The raw insertion: no undo bookkeeping, no selection handling.
    fn insert_raw(&mut self, s: &str) {
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

    /// Delete the character before the caret (or join the previous line); a
    /// selection is deleted instead. One undo step.
    pub fn backspace(&mut self) {
        self.push_undo();
        self.coalescing = false;
        if self.delete_selection() {
            return;
        }
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

    /// Delete the character after the caret (or join the next line); a selection
    /// is deleted instead. One undo step.
    pub fn delete_forward(&mut self) {
        self.push_undo();
        self.coalescing = false;
        if self.delete_selection() {
            return;
        }
        let Caret { row, col } = self.caret;
        if col < self.lines[row].len() {
            let next = self.lines[row][col..].chars().next().unwrap();
            self.lines[row].replace_range(col..col + next.len_utf8(), "");
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        }
    }

    /// Replace `line[replace_start..caret.col]` with `replacement` (apply an
    /// autocomplete choice) and place the caret just after it. One undo step.
    pub fn apply_completion(&mut self, replace_start: usize, replacement: &str) {
        self.push_undo();
        self.coalescing = false;
        let row = self.caret.row;
        let end = self.caret.col;
        let start = replace_start.min(end);
        self.lines[row].replace_range(start..end, replacement);
        self.caret.col = start + replacement.len();
    }

    // ── selection ────────────────────────────────────────────────────────────

    /// The selection as a normalized `(start, end)` with `start <= end`, or
    /// `None` if there is no (or an empty) selection.
    pub fn selection(&self) -> Option<(Caret, Caret)> {
        let a = self.anchor?;
        if a == self.caret {
            return None;
        }
        let (s, e) = ((a.row, a.col), (self.caret.row, self.caret.col));
        Some(if s <= e { (a, self.caret) } else { (self.caret, a) })
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection()?;
        Some(self.text_range(s, e))
    }

    /// Begin (or keep) a selection anchored at the current caret — call before a
    /// caret move to extend the selection (Shift+arrow / drag).
    pub fn start_selection(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
    }

    /// Drop any selection (a plain caret move / click).
    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Select the whole document.
    pub fn select_all(&mut self) {
        self.anchor = Some(Caret::default());
        let row = self.lines.len() - 1;
        self.caret = Caret { row, col: self.lines[row].len() };
    }

    fn text_range(&self, s: Caret, e: Caret) -> String {
        if s.row == e.row {
            return self.lines[s.row][s.col..e.col].to_string();
        }
        let mut out = self.lines[s.row][s.col..].to_string();
        for r in s.row + 1..e.row {
            out.push('\n');
            out.push_str(&self.lines[r]);
        }
        out.push('\n');
        out.push_str(&self.lines[e.row][..e.col]);
        out
    }

    /// Delete the selection (no undo bookkeeping); returns whether anything was
    /// removed. Caret ends at the selection start.
    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        if s.row == e.row {
            self.lines[s.row].replace_range(s.col..e.col, "");
        } else {
            let tail = self.lines[e.row][e.col..].to_string();
            self.lines[s.row].truncate(s.col);
            self.lines[s.row].push_str(&tail);
            self.lines.drain(s.row + 1..=e.row);
        }
        self.caret = s;
        self.anchor = None;
        true
    }

    // ── clipboard helpers (the GUI owns the actual system clipboard) ──────────

    /// Text to copy (the selection), without mutating.
    pub fn copy(&self) -> Option<String> {
        self.selected_text()
    }

    /// Cut: the selected text, removed as one undo step. `None` if no selection.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        self.push_undo();
        self.coalescing = false;
        self.delete_selection();
        Some(text)
    }

    // ── undo / redo ──────────────────────────────────────────────────────────

    fn snapshot(&self) -> Snapshot {
        Snapshot { lines: self.lines.clone(), caret: self.caret, anchor: self.anchor }
    }

    /// Record the current state as an undo point and drop the redo stack.
    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > 500 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Restore the previous state. Returns whether anything was undone.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(prev);
        true
    }

    /// Re-apply the last undone state. Returns whether anything was redone.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
        true
    }

    fn restore(&mut self, s: Snapshot) {
        self.lines = s.lines;
        self.caret = s.caret;
        self.anchor = s.anchor;
        self.coalescing = false;
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
        self.coalescing = false;
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
        self.coalescing = false;
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
        self.coalescing = false;
        if self.caret.row > 0 {
            let row = self.caret.row - 1;
            self.caret = Caret { row, col: self.snap(row, self.caret.col) };
        }
    }

    /// Move to the next line, keeping the column (clamped to that line).
    pub fn move_down(&mut self) {
        self.coalescing = false;
        if self.caret.row + 1 < self.lines.len() {
            let row = self.caret.row + 1;
            self.caret = Caret { row, col: self.snap(row, self.caret.col) };
        }
    }

    /// Caret to the start of the line.
    pub fn home(&mut self) {
        self.coalescing = false;
        self.caret.col = 0;
    }

    /// Caret to the end of the line.
    pub fn end(&mut self) {
        self.coalescing = false;
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

    #[test]
    fn selection_and_selected_text() {
        let mut d = Doc::from_str("hello\nworld");
        d.set_caret(0, 1);
        d.start_selection();
        d.set_caret(1, 3);
        assert_eq!(d.selected_text().as_deref(), Some("ello\nwor"));
        assert!(d.has_selection());
        d.clear_selection();
        assert!(!d.has_selection());
    }

    #[test]
    fn typing_over_a_selection_replaces_it() {
        let mut d = Doc::from_str("hello");
        d.set_caret(0, 0);
        d.start_selection();
        d.set_caret(0, 5); // select "hello"
        d.insert("bye");
        assert_eq!(d.text(), "bye");
        assert_eq!(d.caret, Caret { row: 0, col: 3 });
        assert!(!d.has_selection());
    }

    #[test]
    fn cut_copy_paste() {
        let mut d = Doc::from_str("abcdef");
        d.set_caret(0, 1);
        d.start_selection();
        d.set_caret(0, 4); // select "bcd"
        assert_eq!(d.copy().as_deref(), Some("bcd"));
        assert_eq!(d.cut().as_deref(), Some("bcd"));
        assert_eq!(d.text(), "aef");
        assert_eq!(d.caret, Caret { row: 0, col: 1 });
        d.insert("bcd"); // "paste"
        assert_eq!(d.text(), "abcdef");
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut d = Doc::new();
        d.insert("mov");
        d.insert(" rax");
        assert_eq!(d.text(), "mov rax");
        assert!(d.undo());
        assert_eq!(d.text(), "mov");
        assert!(d.undo());
        assert_eq!(d.text(), "");
        assert!(!d.undo());
        assert!(d.redo());
        assert_eq!(d.text(), "mov");
        assert!(d.redo());
        assert_eq!(d.text(), "mov rax");
    }

    #[test]
    fn typing_coalesces_into_one_undo() {
        let mut d = Doc::new();
        d.type_char('a');
        d.type_char('b');
        d.type_char('c');
        assert_eq!(d.text(), "abc");
        assert!(d.undo());
        assert_eq!(d.text(), "", "a typing run undoes as one step");
        // A caret move breaks the run into a new undo group.
        d.type_char('x');
        d.move_left();
        d.type_char('y');
        assert_eq!(d.text(), "yx");
        d.undo();
        assert_eq!(d.text(), "x");
    }

    #[test]
    fn edit_clears_redo() {
        let mut d = Doc::new();
        d.insert("a");
        d.undo();
        d.insert("b");
        assert!(!d.redo(), "a fresh edit discards the redo stack");
        assert_eq!(d.text(), "b");
    }

    #[test]
    fn select_all_then_replace() {
        let mut d = Doc::from_str("line1\nline2");
        d.select_all();
        assert_eq!(d.selected_text().as_deref(), Some("line1\nline2"));
        d.insert("x");
        assert_eq!(d.text(), "x");
    }

    #[test]
    fn delete_forward_and_join() {
        let mut d = Doc::from_str("ab\ncd");
        d.set_caret(0, 1);
        d.delete_forward();
        assert_eq!(d.text(), "a\ncd");
        d.set_caret(0, 1);
        d.delete_forward(); // join the next line
        assert_eq!(d.text(), "acd");
    }
}
