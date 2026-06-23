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
    /// The column vertical movement tries to keep, so the caret doesn't drift
    /// left when it passes through short lines. `None` after any horizontal move.
    goal_col: Option<usize>,
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
            goal_col: None,
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
        self.goal_col = None;
    }

    /// Insert text at the caret (replacing any selection) as one undo step.
    /// Splits the line on any `\n`; the caret ends just after the inserted text.
    pub fn insert(&mut self, s: &str) {
        self.push_undo();
        self.coalescing = false;
        self.goal_col = None;
        self.delete_selection();
        self.insert_raw(s);
    }

    /// Type a single character — coalescing into the current undo step and
    /// replacing any selection. Use this for keystrokes so undo groups a run.
    pub fn type_char(&mut self, c: char) {
        self.goal_col = None;
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
        self.goal_col = None;
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
        self.goal_col = None;
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
        self.goal_col = None;
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
        self.goal_col = None;
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
        self.goal_col = None;
        let Caret { row, col } = self.caret;
        let line = &self.lines[row];
        if col < line.len() {
            let next = line[col..].chars().next().unwrap();
            self.caret.col = col + next.len_utf8();
        } else if row + 1 < self.lines.len() {
            self.caret = Caret { row: row + 1, col: 0 };
        }
    }

    /// Move to the previous line, keeping the goal column (so the caret doesn't
    /// drift left through short lines).
    pub fn move_up(&mut self) {
        self.coalescing = false;
        if self.caret.row > 0 {
            let goal = *self.goal_col.get_or_insert(self.caret.col);
            let row = self.caret.row - 1;
            self.caret = Caret { row, col: self.snap(row, goal) };
        }
    }

    /// Move to the next line, keeping the goal column.
    pub fn move_down(&mut self) {
        self.coalescing = false;
        if self.caret.row + 1 < self.lines.len() {
            let goal = *self.goal_col.get_or_insert(self.caret.col);
            let row = self.caret.row + 1;
            self.caret = Caret { row, col: self.snap(row, goal) };
        }
    }

    /// Caret to the start of the line.
    pub fn home(&mut self) {
        self.coalescing = false;
        self.goal_col = None;
        self.caret.col = 0;
    }

    /// **Smart Home**: to the first non-blank character, or to column 0 if
    /// already there — the toggle most editors give `Home`.
    pub fn smart_home(&mut self) {
        self.coalescing = false;
        self.goal_col = None;
        let line = &self.lines[self.caret.row];
        let first = line.len() - line.trim_start().len();
        self.caret.col = if self.caret.col == first { 0 } else { first };
    }

    /// Caret to the end of the line.
    pub fn end(&mut self) {
        self.coalescing = false;
        self.goal_col = None;
        self.caret.col = self.lines[self.caret.row].len();
    }

    // ── word-wise movement & deletion ────────────────────────────────────────

    /// Move left to the previous word boundary (`Ctrl+Left`).
    pub fn move_word_left(&mut self) {
        self.coalescing = false;
        self.goal_col = None;
        let Caret { row, col } = self.caret;
        if col == 0 {
            if row > 0 {
                self.caret = Caret { row: row - 1, col: self.lines[row - 1].len() };
            }
        } else {
            self.caret.col = word_left(&self.lines[row], col);
        }
    }

    /// Move right to the next word boundary (`Ctrl+Right`).
    pub fn move_word_right(&mut self) {
        self.coalescing = false;
        self.goal_col = None;
        let Caret { row, col } = self.caret;
        let len = self.lines[row].len();
        if col >= len {
            if row + 1 < self.lines.len() {
                self.caret = Caret { row: row + 1, col: 0 };
            }
        } else {
            self.caret.col = word_right(&self.lines[row], col);
        }
    }

    /// Delete to the previous word boundary (`Ctrl+Backspace`); a selection is
    /// deleted instead. One undo step.
    pub fn delete_word_left(&mut self) {
        self.push_undo();
        self.coalescing = false;
        self.goal_col = None;
        if self.delete_selection() {
            return;
        }
        let Caret { row, col } = self.caret;
        if col > 0 {
            let start = word_left(&self.lines[row], col);
            self.lines[row].replace_range(start..col, "");
            self.caret.col = start;
        } else if row > 0 {
            let cur = self.lines.remove(row);
            let plen = self.lines[row - 1].len();
            self.lines[row - 1].push_str(&cur);
            self.caret = Caret { row: row - 1, col: plen };
        }
    }

    /// Delete to the next word boundary (`Ctrl+Delete`); a selection is deleted
    /// instead. One undo step.
    pub fn delete_word_right(&mut self) {
        self.push_undo();
        self.coalescing = false;
        self.goal_col = None;
        if self.delete_selection() {
            return;
        }
        let Caret { row, col } = self.caret;
        let len = self.lines[row].len();
        if col < len {
            let end = word_right(&self.lines[row], col);
            self.lines[row].replace_range(col..end, "");
        } else if row + 1 < self.lines.len() {
            let next = self.lines.remove(row + 1);
            self.lines[row].push_str(&next);
        }
    }

    // ── line operations & indentation ────────────────────────────────────────

    /// The inclusive row range the selection (or the caret) spans.
    fn selected_rows(&self) -> (usize, usize) {
        match self.selection() {
            // A selection ending at column 0 of a row doesn't include that row.
            Some((s, e)) => (s.row, if e.col == 0 && e.row > s.row { e.row - 1 } else { e.row }),
            None => (self.caret.row, self.caret.row),
        }
    }

    /// Insert a newline that copies the current line's leading indentation
    /// (`Enter`). A selection is replaced first. One undo step.
    pub fn insert_newline(&mut self) {
        self.push_undo();
        self.coalescing = false;
        self.goal_col = None;
        self.delete_selection();
        let line = &self.lines[self.caret.row];
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        // One extra level after a block opener (proc/.if/.while/…), asm-style.
        let extra = if opens_block(&line[..self.caret.col]) { INDENT } else { "" };
        self.insert_raw(&format!("\n{indent}{extra}"));
    }

    /// Indent the selected lines (or the caret line) by one level. One undo step.
    pub fn indent(&mut self) {
        self.push_undo();
        self.coalescing = false;
        let (r0, r1) = self.selected_rows();
        for r in r0..=r1 {
            self.lines[r].insert_str(0, INDENT);
        }
        self.shift_cols(r0, r1, INDENT.len() as isize);
    }

    /// Outdent the selected lines (or the caret line) by up to one level. One
    /// undo step.
    pub fn outdent(&mut self) {
        self.push_undo();
        self.coalescing = false;
        let (r0, r1) = self.selected_rows();
        for r in r0..=r1 {
            let line = &self.lines[r];
            let n = line.len() - line.trim_start_matches(' ').len();
            let remove = n.min(INDENT.len());
            self.lines[r].replace_range(0..remove, "");
            self.adjust_col(r, -(remove as isize));
        }
    }

    /// Shift caret/anchor columns on rows in `r0..=r1` by `delta` (for a uniform
    /// indent).
    fn shift_cols(&mut self, r0: usize, r1: usize, delta: isize) {
        for c in [Some(&mut self.caret), self.anchor.as_mut()].into_iter().flatten() {
            if c.row >= r0 && c.row <= r1 {
                c.col = (c.col as isize + delta).max(0) as usize;
            }
        }
    }

    /// Shift caret/anchor columns on a single row by `delta`, clamped to ≥0.
    fn adjust_col(&mut self, row: usize, delta: isize) {
        for c in [Some(&mut self.caret), self.anchor.as_mut()].into_iter().flatten() {
            if c.row == row {
                c.col = (c.col as isize + delta).max(0) as usize;
            }
        }
    }

    /// Duplicate the caret line below it (`Ctrl+D`). One undo step.
    pub fn duplicate_line(&mut self) {
        self.push_undo();
        self.coalescing = false;
        self.anchor = None;
        let row = self.caret.row;
        let dup = self.lines[row].clone();
        self.lines.insert(row + 1, dup);
        self.caret.row = row + 1;
    }

    /// Delete the caret line (`Ctrl+Shift+K`). One undo step.
    pub fn delete_line(&mut self) {
        self.push_undo();
        self.coalescing = false;
        self.anchor = None;
        let row = self.caret.row;
        if self.lines.len() == 1 {
            self.lines[0].clear();
            self.caret.col = 0;
            return;
        }
        self.lines.remove(row);
        let row = row.min(self.lines.len() - 1);
        self.caret = Caret { row, col: self.snap(row, self.caret.col) };
    }

    /// Swap the caret line with the one above (`Alt+Up`). One undo step.
    pub fn move_line_up(&mut self) {
        if self.caret.row == 0 {
            return;
        }
        self.push_undo();
        self.coalescing = false;
        self.anchor = None;
        let row = self.caret.row;
        self.lines.swap(row, row - 1);
        self.caret.row = row - 1;
    }

    /// Swap the caret line with the one below (`Alt+Down`). One undo step.
    pub fn move_line_down(&mut self) {
        if self.caret.row + 1 >= self.lines.len() {
            return;
        }
        self.push_undo();
        self.coalescing = false;
        self.anchor = None;
        let row = self.caret.row;
        self.lines.swap(row, row + 1);
        self.caret.row = row + 1;
    }

    /// All matches of `needle` as `(row, start, end)` byte ranges. Case-folding
    /// is ASCII (asm is ASCII), so byte offsets stay aligned. Empty if no needle.
    pub fn find_all(&self, needle: &str, case_sensitive: bool) -> Vec<(usize, usize, usize)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let nl = if case_sensitive { needle.to_string() } else { needle.to_ascii_lowercase() };
        let mut hits = Vec::new();
        for (r, line) in self.lines.iter().enumerate() {
            let hay = if case_sensitive { line.clone() } else { line.to_ascii_lowercase() };
            let mut from = 0;
            while let Some(i) = hay[from..].find(&nl) {
                let s = from + i;
                hits.push((r, s, s + needle.len()));
                from = s + needle.len().max(1);
            }
        }
        hits
    }

    /// Select the word at `(row, col)` (double-click); no-op on whitespace.
    pub fn select_word_at(&mut self, row: usize, col: usize) {
        self.goal_col = None;
        let row = row.min(self.lines.len().saturating_sub(1));
        let col = self.snap(row, col);
        let (s, e) = (word_start(&self.lines[row], col), word_end(&self.lines[row], col));
        if e > s {
            self.anchor = Some(Caret { row, col: s });
            self.caret = Caret { row, col: e };
        } else {
            self.set_caret(row, col);
        }
    }

    /// Select the whole of `row` (triple-click).
    pub fn select_line(&mut self, row: usize) {
        self.goal_col = None;
        let row = row.min(self.lines.len().saturating_sub(1));
        self.anchor = Some(Caret { row, col: 0 });
        self.caret = if row + 1 < self.lines.len() {
            Caret { row: row + 1, col: 0 }
        } else {
            Caret { row, col: self.lines[row].len() }
        };
    }

    /// Toggle a `;` line comment on the selected lines (or the caret line). If
    /// every non-blank affected line is already commented, uncomment; else
    /// comment them all (at each line's indent). One undo step.
    pub fn toggle_comment(&mut self) {
        self.push_undo();
        self.coalescing = false;
        let (r0, r1) = self.selected_rows();
        let commented = (r0..=r1).all(|r| {
            let t = self.lines[r].trim_start();
            t.is_empty() || t.starts_with(';')
        });
        for r in r0..=r1 {
            if self.lines[r].trim().is_empty() {
                continue;
            }
            let indent = self.lines[r].len() - self.lines[r].trim_start().len();
            if commented {
                let rest = self.lines[r][indent..].to_string();
                let s = rest.strip_prefix("; ").or_else(|| rest.strip_prefix(';')).unwrap_or(&rest);
                self.lines[r].replace_range(indent.., s);
            } else {
                self.lines[r].insert_str(indent, "; ");
            }
        }
        self.caret.col = self.snap(self.caret.row, self.caret.col);
        if let Some(a) = self.anchor.as_mut() {
            a.col = a.col.min(self.lines[a.row].len());
        }
    }

    /// The position of the bracket matching the one at (or just before) `(row,
    /// col)`, searched on the same line (asm brackets don't span lines). For the
    /// matching-bracket highlight. `()`/`[]`/`{}`.
    pub fn matching_bracket(&self, row: usize, col: usize) -> Option<(usize, usize)> {
        const OPEN: &str = "([{";
        const CLOSE: &str = ")]}";
        let line = self.line(row);
        let col = self.snap(row, col);
        let (ch, bcol) = match line[col..].chars().next() {
            Some(c) if OPEN.contains(c) || CLOSE.contains(c) => (c, col),
            _ => {
                let prev = line[..col].chars().next_back()?;
                if OPEN.contains(prev) || CLOSE.contains(prev) {
                    (prev, col - prev.len_utf8())
                } else {
                    return None;
                }
            }
        };
        let mut depth = 0i32;
        if let Some(i) = OPEN.find(ch) {
            let close = CLOSE.as_bytes()[i] as char;
            let mut c = bcol;
            while c < line.len() {
                let cc = line[c..].chars().next().unwrap();
                if cc == ch {
                    depth += 1;
                } else if cc == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((row, c));
                    }
                }
                c += cc.len_utf8();
            }
        } else if let Some(i) = CLOSE.find(ch) {
            let open = OPEN.as_bytes()[i] as char;
            let mut c = bcol + ch.len_utf8();
            while c > 0 {
                let cc = line[..c].chars().next_back().unwrap();
                c -= cc.len_utf8();
                if cc == ch {
                    depth += 1;
                } else if cc == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some((row, c));
                    }
                }
            }
        }
        None
    }

    /// The token whose other occurrences should be highlighted: a single-line,
    /// word-like selection, else the word under the caret. `None` when it isn't
    /// worth highlighting (too short, spans lines, or not a word).
    pub fn occurrence_needle(&self) -> Option<String> {
        if let Some((s, e)) = self.selection() {
            if s.row != e.row {
                return None;
            }
            let t = &self.lines[s.row][s.col..e.col];
            return (t.len() >= 2 && t.chars().all(is_word)).then(|| t.to_string());
        }
        let line = &self.lines[self.caret.row];
        let (s, e) = (word_start(line, self.caret.col), word_end(line, self.caret.col));
        let t = &line[s..e];
        (t.len() >= 2 && t.chars().all(is_word)).then(|| t.to_string())
    }
}

const INDENT: &str = "  ";

/// First byte of the word containing `col` on `line`.
fn word_start(line: &str, col: usize) -> usize {
    let mut c = col;
    while c > 0 {
        let ch = line[..c].chars().next_back().unwrap();
        if is_word(ch) {
            c -= ch.len_utf8();
        } else {
            break;
        }
    }
    c
}

/// One past the last byte of the word containing `col` on `line`.
fn word_end(line: &str, col: usize) -> usize {
    let mut c = col;
    while c < line.len() {
        let ch = line[c..].chars().next().unwrap();
        if is_word(ch) {
            c += ch.len_utf8();
        } else {
            break;
        }
    }
    c
}

/// Whether `line`'s first token opens an indented block in the WRASM dialect, so
/// Enter after it bumps the indent one level.
fn opens_block(line: &str) -> bool {
    let first = line.trim_start().split(char::is_whitespace).next().unwrap_or("");
    matches!(
        first.to_ascii_lowercase().as_str(),
        "proc" | ".if" | ".while" | ".repeat" | ".for" | "struct" | "macro"
    )
}

/// Whether `c` is part of a "word" for word-wise movement.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte column of the previous word boundary before `col` on `line`.
fn word_left(line: &str, col: usize) -> usize {
    let mut c = col;
    let back = |c: usize| line[..c].chars().next_back().unwrap();
    while c > 0 && back(c).is_whitespace() {
        c -= back(c).len_utf8();
    }
    if c > 0 {
        let word = is_word(back(c));
        while c > 0 {
            let ch = back(c);
            if ch.is_whitespace() || is_word(ch) != word {
                break;
            }
            c -= ch.len_utf8();
        }
    }
    c
}

/// Byte column of the next word boundary after `col` on `line`.
fn word_right(line: &str, col: usize) -> usize {
    let len = line.len();
    let mut c = col;
    let fwd = |c: usize| line[c..].chars().next().unwrap();
    if c < len && !fwd(c).is_whitespace() {
        let word = is_word(fwd(c));
        while c < len {
            let ch = fwd(c);
            if ch.is_whitespace() || is_word(ch) != word {
                break;
            }
            c += ch.len_utf8();
        }
    }
    while c < len && fwd(c).is_whitespace() {
        c += fwd(c).len_utf8();
    }
    c
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
    fn move_up_down_tracks_the_goal_column() {
        let mut d = Doc::from_str("longline\nhi\nanother");
        d.set_caret(0, 8); // end of "longline" — goal column 8
        d.move_down(); // "hi" (len 2) clamps to col 2
        assert_eq!(d.caret, Caret { row: 1, col: 2 });
        d.move_down(); // "another" (len 7) restores toward the goal → col 7
        assert_eq!(d.caret, Caret { row: 2, col: 7 });
        d.move_up();
        assert_eq!(d.caret, Caret { row: 1, col: 2 }); // "hi" clamps again
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

    // ── Core text-editing extensions ─────────────────────────────────────────

    #[test]
    fn word_movement_skips_words_and_whitespace() {
        // cols: 0-1 spaces, 2-8 foo_bar, 9-10 spaces, 11-13 baz, len 14
        let mut d = Doc::from_str("  foo_bar  baz");
        d.set_caret(0, 14);
        d.move_word_left();
        assert_eq!(d.caret.col, 11);
        d.move_word_left();
        assert_eq!(d.caret.col, 2);
        d.move_word_left();
        assert_eq!(d.caret.col, 0);
        d.move_word_right();
        assert_eq!(d.caret.col, 2);
        d.move_word_right();
        assert_eq!(d.caret.col, 11);
    }

    #[test]
    fn delete_word_left_and_right() {
        let mut d = Doc::from_str("hello world");
        d.set_caret(0, 11);
        d.delete_word_left();
        assert_eq!(d.text(), "hello ");
        let mut d = Doc::from_str("hello world");
        d.set_caret(0, 0);
        d.delete_word_right(); // word + the following whitespace
        assert_eq!(d.text(), "world");
    }

    #[test]
    fn smart_home_toggles() {
        let mut d = Doc::from_str("    mov rax, 1");
        d.set_caret(0, 10);
        d.smart_home();
        assert_eq!(d.caret.col, 4); // first non-blank
        d.smart_home();
        assert_eq!(d.caret.col, 0); // toggle to start
        d.smart_home();
        assert_eq!(d.caret.col, 4);
    }

    #[test]
    fn vertical_movement_keeps_the_goal_column() {
        let mut d = Doc::from_str("longer line\nx\nanother line");
        d.set_caret(0, 9);
        d.move_down(); // clamps to the short "x" line
        assert_eq!(d.caret.col, 1);
        d.move_down(); // goal column 9 is restored on the long line
        assert_eq!(d.caret.col, 9);
    }

    #[test]
    fn indent_and_outdent_a_selection() {
        let mut d = Doc::from_str("a\nb\nc");
        d.set_caret(0, 0);
        d.start_selection();
        d.set_caret(2, 1);
        d.indent();
        assert_eq!(d.text(), "  a\n  b\n  c");
        d.outdent();
        assert_eq!(d.text(), "a\nb\nc");
    }

    #[test]
    fn enter_auto_indents_to_the_previous_line() {
        let mut d = Doc::from_str("    mov rax, 1");
        d.set_caret(0, 14);
        d.insert_newline();
        assert_eq!(d.text(), "    mov rax, 1\n    ");
        assert_eq!(d.caret, Caret { row: 1, col: 4 });
    }

    #[test]
    fn enter_after_a_block_opener_adds_a_level() {
        let mut d = Doc::from_str("proc foo");
        d.set_caret(0, 8);
        d.insert_newline();
        assert_eq!(d.text(), "proc foo\n  ");

        let mut d = Doc::from_str("  .if rax == 0");
        d.set_caret(0, 14);
        d.insert_newline();
        assert_eq!(d.text(), "  .if rax == 0\n    "); // nested: 2 + 2

        let mut d = Doc::from_str("  mov rax, 1"); // a plain line just copies indent
        d.set_caret(0, 12);
        d.insert_newline();
        assert_eq!(d.text(), "  mov rax, 1\n  ");
    }

    #[test]
    fn line_ops_duplicate_delete_move() {
        let mut d = Doc::from_str("a\nb\nc");
        d.set_caret(1, 0);
        d.duplicate_line();
        assert_eq!(d.text(), "a\nb\nb\nc");
        assert_eq!(d.caret.row, 2);

        let mut d = Doc::from_str("a\nb\nc");
        d.set_caret(1, 0);
        d.delete_line();
        assert_eq!(d.text(), "a\nc");

        let mut d = Doc::from_str("a\nb\nc");
        d.set_caret(2, 0);
        d.move_line_up();
        assert_eq!(d.text(), "a\nc\nb");
        d.move_line_down();
        assert_eq!(d.text(), "a\nb\nc");
    }

    #[test]
    fn find_all_case_sensitive_and_insensitive() {
        let d = Doc::from_str("foo bar\nFOO foo");
        assert_eq!(d.find_all("foo", true), vec![(0, 0, 3), (1, 4, 7)]);
        assert_eq!(d.find_all("foo", false), vec![(0, 0, 3), (1, 0, 3), (1, 4, 7)]);
        assert!(d.find_all("", false).is_empty());
    }

    #[test]
    fn toggle_comment_round_trips() {
        let mut d = Doc::from_str("  mov rax, 1\n  ret");
        d.set_caret(0, 0);
        d.start_selection();
        d.set_caret(1, 5);
        d.toggle_comment();
        assert_eq!(d.text(), "  ; mov rax, 1\n  ; ret");
        d.toggle_comment();
        assert_eq!(d.text(), "  mov rax, 1\n  ret");
    }

    #[test]
    fn select_word_and_line() {
        let mut d = Doc::from_str("foo bar_baz qux");
        d.select_word_at(0, 6);
        assert_eq!(d.selected_text().as_deref(), Some("bar_baz"));
        let mut d = Doc::from_str("a\nb\nc");
        d.select_line(1);
        assert_eq!(d.selected_text().as_deref(), Some("b\n"));
    }

    #[test]
    fn matching_bracket_same_line() {
        let d = Doc::from_str("lea rax, [rbx + rcx*2]");
        assert_eq!(d.matching_bracket(0, 9), Some((0, 21))); // on '['
        assert_eq!(d.matching_bracket(0, 21), Some((0, 9))); // on ']'
        assert_eq!(d.matching_bracket(0, 0), None);
    }
}
