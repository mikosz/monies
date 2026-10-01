use crate::change::Change;
use crate::history::History;
use crate::ledger::Ledger;
use crate::store::Store;

/// How many changes can be undone.
const HISTORY_LIMIT: usize = 1000;

/// The ledger together with where it's stored and its undo history. All user changes go
/// through here.
pub struct Document<S: Store> {
    ledger: Ledger,
    store: S,
    history: History,
}

impl<S: Store> Document<S> {
    pub fn new(ledger: Ledger, store: S) -> Self {
        Self { ledger, store, history: History::new(HISTORY_LIMIT) }
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Applies a change made by the user and makes it undoable.
    pub fn perform(&mut self, change: Change) -> Result<(), S::Error> {
        self.ledger.apply(&mut self.store, &change)?;
        self.history.record(change);
        Ok(())
    }

    /// Reverts the last change; `Ok(false)` when there's nothing to undo. On error nothing
    /// changes, including the history.
    pub fn undo(&mut self) -> Result<bool, S::Error> {
        let Some(change) = self.history.next_undo() else { return Ok(false) };
        self.ledger.apply(&mut self.store, &change.inverse())?;
        self.history.undone();
        Ok(true)
    }

    /// Applies the last undone change again; `Ok(false)` when there's nothing to redo. On error
    /// nothing changes, including the history.
    pub fn redo(&mut self) -> Result<bool, S::Error> {
        let Some(change) = self.history.next_redo() else { return Ok(false) };
        self.ledger.apply(&mut self.store, change)?;
        self.history.redone();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountId;
    use crate::change::ChangeBuilder;
    use crate::entry::ParsedEntry;
    use crate::store::MemoryStore;

    /// A ledger with one account, and that account.
    fn ledger() -> (Ledger, AccountId) {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        (ledger, account)
    }

    fn add<S: Store>(document: &mut Document<S>, account: AccountId, name: &str) -> Result<(), S::Error> {
        let mut builder = ChangeBuilder::new(format!("Add {name}"), document.ledger());
        builder.add_entry(ParsedEntry::test(account, "2026-09-30", name, "bills", "1"));
        let change = builder.build();
        document.perform(change)
    }

    fn names(document: &Document<MemoryStore>) -> Vec<&str> {
        document.ledger().entries().iter().map(|(_, e)| e.name.as_str()).collect()
    }

    #[test]
    fn undoes_and_redoes_changes() {
        let (ledger, account) = ledger();
        let mut document = Document::new(ledger, MemoryStore);
        add(&mut document, account, "a").unwrap();
        add(&mut document, account, "b").unwrap();

        assert!(document.undo().unwrap());
        assert_eq!(names(&document), ["a"]);
        assert!(document.undo().unwrap());
        assert!(names(&document).is_empty());
        assert!(!document.undo().unwrap(), "nothing left to undo");

        assert!(document.redo().unwrap());
        assert!(document.redo().unwrap());
        assert_eq!(names(&document), ["a", "b"]);
        assert!(!document.redo().unwrap(), "nothing left to redo");
    }

    #[test]
    fn new_change_discards_redo() {
        let (ledger, account) = ledger();
        let mut document = Document::new(ledger, MemoryStore);
        add(&mut document, account, "a").unwrap();
        document.undo().unwrap();
        add(&mut document, account, "b").unwrap();

        assert!(!document.redo().unwrap());
        assert_eq!(names(&document), ["b"]);
    }

    /// Fails every write while `failing` is set.
    struct FlakyStore {
        failing: bool,
    }

    impl Store for FlakyStore {
        type Error = std::fmt::Error;

        fn apply(&mut self, _change: &Change) -> Result<(), Self::Error> {
            if self.failing { Err(std::fmt::Error) } else { Ok(()) }
        }
    }

    #[test]
    fn failed_undo_and_redo_change_nothing() {
        let (ledger, account) = ledger();
        let mut document = Document::new(ledger, FlakyStore { failing: false });
        add(&mut document, account, "a").unwrap();

        document.store.failing = true;
        assert!(document.undo().is_err());
        assert_eq!(document.ledger().entries().len(), 1);
        document.store.failing = false;
        assert!(document.undo().unwrap(), "the change is still undoable");

        document.store.failing = true;
        assert!(document.redo().is_err());
        assert!(document.ledger().entries().is_empty());
        document.store.failing = false;
        assert!(document.redo().unwrap(), "the change is still redoable");
    }
}
