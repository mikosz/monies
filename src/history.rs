use std::collections::VecDeque;

use crate::change::Change;

/// Undo and redo stacks of changes. Only bookkeeping: applying changes is up to the caller,
/// which first applies a change and only then moves it between the stacks, so a failed apply
/// leaves the history untouched.
pub struct History {
    undo: VecDeque<Change>,
    redo: Vec<Change>,
    /// Maximum number of undoable changes; the oldest are dropped.
    limit: usize,
}

impl History {
    pub fn new(limit: usize) -> Self {
        Self { undo: VecDeque::new(), redo: Vec::new(), limit }
    }

    /// Records a newly applied change. Clears the redo stack.
    pub fn record(&mut self, change: Change) {
        self.redo.clear();
        self.undo.push_back(change);
        if self.undo.len() > self.limit {
            self.undo.pop_front();
        }
    }

    /// The change an undo would revert.
    pub fn next_undo(&self) -> Option<&Change> {
        self.undo.back()
    }

    /// The change a redo would apply again.
    pub fn next_redo(&self) -> Option<&Change> {
        self.redo.last()
    }

    /// Marks [`Self::next_undo`] as reverted.
    pub fn undone(&mut self) {
        let change = self.undo.pop_back().expect("there was a change to undo");
        self.redo.push(change);
    }

    /// Marks [`Self::next_redo`] as applied again.
    pub fn redone(&mut self) {
        let change = self.redo.pop().expect("there was a change to redo");
        self.undo.push_back(change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(description: &str) -> Change {
        Change { description: description.to_owned(), ops: Vec::new() }
    }

    fn next_undo(history: &History) -> Option<&str> {
        history.next_undo().map(|c| c.description.as_str())
    }

    fn next_redo(history: &History) -> Option<&str> {
        history.next_redo().map(|c| c.description.as_str())
    }

    #[test]
    fn undoes_and_redoes_in_order() {
        let mut history = History::new(10);
        history.record(change("a"));
        history.record(change("b"));
        assert_eq!(next_undo(&history), Some("b"));

        history.undone();
        assert_eq!(next_undo(&history), Some("a"));
        assert_eq!(next_redo(&history), Some("b"));

        history.undone();
        assert_eq!(next_undo(&history), None);
        assert_eq!(next_redo(&history), Some("a"));

        history.redone();
        assert_eq!(next_undo(&history), Some("a"));
        assert_eq!(next_redo(&history), Some("b"));
    }

    #[test]
    fn new_change_clears_redo() {
        let mut history = History::new(10);
        history.record(change("a"));
        history.undone();
        history.record(change("b"));
        assert_eq!(next_redo(&history), None);
        assert_eq!(next_undo(&history), Some("b"));
    }

    #[test]
    fn drops_oldest_changes_beyond_limit() {
        let mut history = History::new(2);
        for description in ["a", "b", "c"] {
            history.record(change(description));
        }
        history.undone();
        history.undone();
        assert_eq!(next_undo(&history), None, "'a' was dropped");
        assert_eq!(next_redo(&history), Some("b"));
    }
}
