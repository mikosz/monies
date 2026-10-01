use std::error::Error;
use std::path::Path;

use crate::change::{Change, ChangeBuilder, Op};
use crate::currency::Currency;
use crate::date_format::DateFormat;
use crate::entry::ParsedEntry;
use crate::ledger::Ledger;
use crate::store::sqlite::SqliteStore;
use crate::store::{MemoryStore, Store};

/// Writes accounts and entries to a new database through the same validation as the UI.
/// Meant for generating sample data (see `examples/sample_db`); the app itself doesn't use it.
pub struct DatabaseWriter {
    store: SqliteStore,
    /// What has been added so far, to validate further additions against.
    ledger: Ledger,
    /// The operations adding it, written by [`Self::finish`].
    ops: Vec<Op>,
}

impl DatabaseWriter {
    /// Creates a database at `path`, which must not exist yet.
    pub fn create(path: &Path) -> Result<Self, Box<dyn Error>> {
        if path.exists() {
            return Err(format!("{} already exists", path.display()).into());
        }
        let store = SqliteStore::open(path)?;
        Ok(Self { store, ledger: Ledger::default(), ops: Vec::new() })
    }

    /// Adds an account given as it would be typed in the app. Nothing is written until
    /// [`Self::finish`].
    pub fn add_account(&mut self, name: &str, currency: &str) -> Result<(), Box<dyn Error>> {
        let currency = Currency::parse(currency).ok_or_else(|| format!("invalid currency {currency:?}"))?;
        let mut change = ChangeBuilder::new("Import", &self.ledger);
        change.add_account(name, currency)?;
        let change = change.build();
        self.stage(change)
    }

    /// Adds an entry to the account named `account` (added earlier), given as it would be
    /// typed in the app, except that the date is ISO `YYYY-MM-DD`. Entries are kept in the
    /// order they're added. Nothing is written until [`Self::finish`].
    pub fn add_entry(&mut self, account: &str, date: &str, name: &str, category: &str, amount: &str) -> Result<(), Box<dyn Error>> {
        let id = self.ledger.accounts().find(account).ok_or_else(|| format!("unknown account {account:?}"))?;
        let decimals = self.ledger.accounts().get(id).expect("found account exists").currency.decimals();
        let parsed = ParsedEntry::parse(&DateFormat::iso(), decimals, date, name, category, amount)?;
        let mut change = ChangeBuilder::new("Import", &self.ledger);
        change.add_entry(parsed, id);
        let change = change.build();
        self.stage(change)
    }

    /// Writes everything added in a single transaction.
    pub fn finish(mut self) -> Result<(), Box<dyn Error>> {
        self.store.apply(&Change { description: "Import".to_owned(), ops: self.ops })?;
        Ok(())
    }

    fn stage(&mut self, change: Change) -> Result<(), Box<dyn Error>> {
        self.ledger.apply(&mut MemoryStore, &change)?;
        self.ops.extend(change.ops);
        Ok(())
    }
}
