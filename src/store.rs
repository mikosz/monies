use crate::change::Change;

#[cfg(not(target_arch = "wasm32"))]
pub mod sqlite;

/// Persists changes to the ledger.
pub trait Store {
    type Error: std::error::Error + 'static;

    /// Applies all operations of the change, or none of them.
    fn apply(&mut self, change: &Change) -> Result<(), Self::Error>;
}

/// Stores nothing. Used where there's no database (the browser), for changes that are only
/// staged in memory (see `DatabaseWriter`) and in tests.
#[derive(Debug, Default)]
pub struct MemoryStore;

impl Store for MemoryStore {
    type Error = std::convert::Infallible;

    fn apply(&mut self, _change: &Change) -> Result<(), Self::Error> {
        Ok(())
    }
}
