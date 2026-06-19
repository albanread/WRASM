//! nav.rs — back/forward history for the assistant pane (pure).
//!
//! The card pane is browser-like: following a `was:` link visits a new query;
//! Back and Forward walk the trail. Visiting from a back position discards the
//! forward entries, exactly like a browser. Entries are the query strings the
//! pane feeds to `ide::answer`.

/// A linear visit history with a current position.
#[derive(Debug, Clone, Default)]
pub struct History {
    entries: Vec<String>,
    pos: usize,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    /// The query currently shown, if any.
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.pos).map(String::as_str)
    }

    /// Visit `query`: truncate any forward history and push it. A no-op if it
    /// equals the current entry (following a self-link shouldn't stack dupes).
    pub fn visit(&mut self, query: &str) {
        if self.current() == Some(query) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.pos + 1);
        }
        self.entries.push(query.to_string());
        self.pos = self.entries.len() - 1;
    }

    pub fn can_back(&self) -> bool {
        self.pos > 0
    }

    pub fn can_forward(&self) -> bool {
        !self.entries.is_empty() && self.pos + 1 < self.entries.len()
    }

    /// Step back; returns the now-current query (or `None` at the start).
    pub fn back(&mut self) -> Option<&str> {
        if self.can_back() {
            self.pos -= 1;
        }
        self.current()
    }

    /// Step forward; returns the now-current query (or `None` at the end).
    pub fn forward(&mut self) -> Option<&str> {
        if self.can_forward() {
            self.pos += 1;
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_has_no_current() {
        let h = History::new();
        assert_eq!(h.current(), None);
        assert!(!h.can_back() && !h.can_forward());
    }

    #[test]
    fn visiting_and_walking_back_and_forward() {
        let mut h = History::new();
        h.visit("CreateFileW");
        h.visit("RECT");
        h.visit("IShellItem");
        assert_eq!(h.current(), Some("IShellItem"));
        assert_eq!(h.back(), Some("RECT"));
        assert_eq!(h.back(), Some("CreateFileW"));
        assert!(!h.can_back());
        assert_eq!(h.back(), Some("CreateFileW")); // clamped
        assert_eq!(h.forward(), Some("RECT"));
        assert_eq!(h.forward(), Some("IShellItem"));
        assert!(!h.can_forward());
    }

    #[test]
    fn visiting_from_a_back_position_truncates_forward() {
        let mut h = History::new();
        h.visit("a");
        h.visit("b");
        h.visit("c");
        h.back(); // at "b"
        h.visit("d"); // discards "c"
        assert_eq!(h.current(), Some("d"));
        assert!(!h.can_forward());
        assert_eq!(h.back(), Some("b"));
    }

    #[test]
    fn revisiting_the_current_query_is_a_noop() {
        let mut h = History::new();
        h.visit("RECT");
        h.visit("RECT");
        assert_eq!(h.current(), Some("RECT"));
        assert!(!h.can_back(), "no duplicate entry pushed");
    }
}
