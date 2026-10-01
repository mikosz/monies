use std::error::Error;
use std::path::Path;

use crate::category::Categories;
use crate::change::ChangeBuilder;
use crate::date_format::DateFormat;
use crate::entry::ParsedEntry;
use crate::ledger::Ledger;
use crate::store::sqlite::SqliteStore;

/// Writes entries to a new database through the same validation as the UI. Meant for
/// generating sample data (see `examples/sample_db`); the app itself doesn't use it.
pub struct DatabaseWriter {
    store: SqliteStore,
    change: ChangeBuilder,
}

impl DatabaseWriter {
    /// Creates a database at `path`, which must not exist yet.
    pub fn create(path: &Path) -> Result<Self, Box<dyn Error>> {
        if path.exists() {
            return Err(format!("{} already exists", path.display()).into());
        }
        let store = SqliteStore::open(path)?;
        Ok(Self { store, change: ChangeBuilder::new("Import", &Categories::default()) })
    }

    /// Adds an entry given as it would be typed in the app, except that the date is ISO
    /// `YYYY-MM-DD`. Entries are kept in the order they're added. Nothing is written until
    /// [`Self::finish`].
    pub fn add_entry(&mut self, date: &str, name: &str, category: &str, amount: &str) -> Result<(), Box<dyn Error>> {
        let parsed = ParsedEntry::parse(&DateFormat::iso(), date, name, category, amount)?;
        self.change.add_entry(parsed);
        Ok(())
    }

    /// Writes all added entries in a single transaction.
    pub fn finish(mut self) -> Result<(), Box<dyn Error>> {
        Ledger::default().apply(&mut self.store, &self.change.build())?;
        Ok(())
    }
}
