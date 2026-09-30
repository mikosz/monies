use chrono::NaiveDate;

use crate::category::{CategoryId, MissingCategories};

#[cfg(not(target_arch = "wasm32"))]
pub mod sqlite;

/// An entry to be stored. Its category follows from the [`MissingCategories`] passed with it.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "the in-memory store keeps no entries"))]
pub struct NewEntry<'a> {
    pub date: NaiveDate,
    pub name: &'a str,
    pub amount: i64,
}

/// Persists ledger changes. Each operation is applied completely or not at all.
pub trait Store {
    type Error: std::error::Error + 'static;

    /// Creates the missing categories, then the entry in the last of them (or in
    /// `categories.parent` when none are missing). Returns the ids of the created categories
    /// in order.
    fn insert_entry(
        &mut self,
        categories: MissingCategories<'_>,
        entry: NewEntry<'_>,
    ) -> Result<Vec<CategoryId>, Self::Error>;
}

/// Stores nothing, only hands out ids. Used where there's no database (the browser) and in
/// tests.
#[cfg(any(test, target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct MemoryStore {
    last_id: i64,
}

#[cfg(any(test, target_arch = "wasm32"))]
impl Store for MemoryStore {
    type Error = std::convert::Infallible;

    fn insert_entry(
        &mut self,
        categories: MissingCategories<'_>,
        _entry: NewEntry<'_>,
    ) -> Result<Vec<CategoryId>, Self::Error> {
        Ok(categories
            .names
            .iter()
            .map(|_| {
                self.last_id += 1;
                CategoryId(self.last_id)
            })
            .collect())
    }
}
