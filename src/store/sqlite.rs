use std::fmt;
use std::path::Path;

use rusqlite::{Connection, Transaction, params};

use super::Store;
use crate::category::{Categories, CategoryId};
use crate::change::{Change, Op};
use crate::entry::{Entry, EntryId};
use crate::ledger::Ledger;

/// Schema migrations; the database's `user_version` is the number of migrations applied.
/// Once there's real data, never edit a migration, append a new one instead.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema. Ids are UUIDv7 as 16-byte BLOBs, so they sort by creation. Dates are
    // ISO `YYYY-MM-DD` text, amounts are cents.
    "CREATE TABLE categories (
        id        BLOB PRIMARY KEY CHECK (length(id) = 16),
        name      TEXT NOT NULL CHECK (name <> ''),
        parent_id BLOB REFERENCES categories(id)
    );
    CREATE INDEX categories_parent ON categories(parent_id);

    CREATE TABLE entries (
        id          BLOB PRIMARY KEY CHECK (length(id) = 16),
        date        TEXT NOT NULL CHECK (date IS date(date)),
        name        TEXT NOT NULL,
        category_id BLOB NOT NULL REFERENCES categories(id),
        amount      INTEGER NOT NULL
    );
    CREATE INDEX entries_date ON entries(date);
    CREATE INDEX entries_category ON entries(category_id);",
];

#[derive(Debug)]
pub enum StorageError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The database was written by a newer version of the app.
    NewerSchema { found: i64, supported: usize },
    /// A row to be deleted doesn't exist: the database and the app's state disagree.
    MissingRow { table: &'static str },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::Sqlite(error) => write!(f, "database error: {error}"),
            StorageError::Io(error) => write!(f, "cannot access the database file: {error}"),
            StorageError::NewerSchema { found, supported } => write!(
                f,
                "the database has schema version {found}, but this version of the app supports up to {supported}"
            ),
            StorageError::MissingRow { table } => write!(f, "a row to be deleted is missing from {table}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Sqlite(error) => Some(error),
            StorageError::Io(error) => Some(error),
            StorageError::NewerSchema { .. } | StorageError::MissingRow { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        StorageError::Sqlite(error)
    }
}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        StorageError::Io(error)
    }
}

pub struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    /// Opens the database at `path`, creating it (and its directory) if needed, and brings its
    /// schema up to date.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self, StorageError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut connection: Connection) -> Result<Self, StorageError> {
        connection.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut connection)?;
        Ok(Self { connection })
    }

    /// Reads all categories and entries.
    pub fn load(&self) -> Result<Ledger, StorageError> {
        let mut categories = Categories::default();
        let mut statement = self.connection.prepare("SELECT id, name, parent_id FROM categories")?;
        let rows = statement.query_map([], |row| {
            Ok((CategoryId(row.get(0)?), row.get(1)?, row.get::<_, Option<_>>(2)?.map(CategoryId)))
        })?;
        for row in rows {
            let (id, name, parent) = row?;
            categories.insert(id, name, parent);
        }

        let mut statement = self.connection.prepare("SELECT id, date, name, category_id, amount FROM entries")?;
        let entries = statement
            .query_map([], |row| {
                let entry = Entry {
                    date: row.get(1)?,
                    name: row.get(2)?,
                    category: CategoryId(row.get(3)?),
                    amount: row.get(4)?,
                };
                Ok((EntryId(row.get(0)?), entry))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Ledger::new(categories, entries))
    }
}

impl Store for SqliteStore {
    type Error = StorageError;

    fn apply(&mut self, change: &Change) -> Result<(), StorageError> {
        let transaction = self.connection.transaction()?;
        for op in &change.ops {
            apply_op(&transaction, op)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

fn apply_op(transaction: &Transaction, op: &Op) -> Result<(), StorageError> {
    match op {
        Op::InsertCategory { id, name, parent } => {
            transaction.execute(
                "INSERT INTO categories (id, name, parent_id) VALUES (?1, ?2, ?3)",
                params![id.0, name, parent.map(|parent| parent.0)],
            )?;
        }
        Op::DeleteCategory { id, .. } => {
            let deleted = transaction.execute("DELETE FROM categories WHERE id = ?1", params![id.0])?;
            if deleted != 1 {
                return Err(StorageError::MissingRow { table: "categories" });
            }
        }
        Op::InsertEntry { id, entry } => {
            transaction.execute(
                "INSERT INTO entries (id, date, name, category_id, amount) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id.0, entry.date, entry.name, entry.category.0, entry.amount],
            )?;
        }
        Op::DeleteEntry { id, .. } => {
            let deleted = transaction.execute("DELETE FROM entries WHERE id = ?1", params![id.0])?;
            if deleted != 1 {
                return Err(StorageError::MissingRow { table: "entries" });
            }
        }
        Op::UpdateEntry { id, after, .. } => {
            let updated = transaction.execute(
                "UPDATE entries SET date = ?2, name = ?3, category_id = ?4, amount = ?5 WHERE id = ?1",
                params![id.0, after.date, after.name, after.category.0, after.amount],
            )?;
            if updated != 1 {
                return Err(StorageError::MissingRow { table: "entries" });
            }
        }
    }
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), StorageError> {
    let found: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let applied = usize::try_from(found)
        .ok()
        .filter(|&applied| applied <= MIGRATIONS.len())
        .ok_or(StorageError::NewerSchema { found, supported: MIGRATIONS.len() })?;

    let transaction = connection.transaction()?;
    for migration in &MIGRATIONS[applied..] {
        transaction.execute_batch(migration)?;
    }
    transaction.pragma_update(None, "user_version", MIGRATIONS.len() as i64)?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::change::ChangeBuilder;
    use crate::date_format::DateFormat;
    use crate::entry::ParsedEntry;

    fn add(ledger: &mut Ledger, store: &mut SqliteStore, rows: &[(&str, &str, &str, &str)]) -> Result<Change, StorageError> {
        let mut builder = ChangeBuilder::new("Add", ledger.categories());
        for (date, name, category, amount) in rows {
            builder.add_entry(ParsedEntry::parse(&DateFormat::iso(), date, name, category, amount).unwrap());
        }
        let change = builder.build();
        ledger.apply(store, &change)?;
        Ok(change)
    }

    fn count(store: &SqliteStore, table: &str) -> i64 {
        store.connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row.get(0)).unwrap()
    }

    fn user_version(store: &SqliteStore) -> i64 {
        store.connection.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap()
    }

    #[test]
    fn migrates_new_database() {
        let store = SqliteStore::open_in_memory().unwrap();
        assert_eq!(user_version(&store), MIGRATIONS.len() as i64);
        assert!(store.load().unwrap().entries().is_empty());
    }

    #[test]
    fn reopening_keeps_schema() {
        let store = SqliteStore::open_in_memory().unwrap();
        let reopened = SqliteStore::init(store.connection).unwrap();
        assert_eq!(user_version(&reopened), MIGRATIONS.len() as i64);
    }

    #[test]
    fn rejects_newer_schema() {
        let connection = Connection::open_in_memory().unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        assert!(matches!(
            SqliteStore::init(connection),
            Err(StorageError::NewerSchema { found: 99, .. })
        ));
    }

    #[test]
    fn saves_and_loads_entries() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        add(&mut ledger, &mut store, &[("2026-10-01", "Rent", "bills.rent", "-1200.50")]).unwrap();
        add(&mut ledger, &mut store, &[("2026-09-15", "Vet", "dogs.health", "80"), ("2026-09-20", "Water", "Bills.water", "30")]).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        let paths: Vec<String> = loaded.entries().iter().map(|(_, e)| loaded.categories().path(e.category)).collect();
        assert_eq!(paths, ["bills.rent", "dogs.health", "bills.water"], "in the order entered");
        assert_eq!(loaded.categories().suggest(""), ["bills", "dogs"]);
    }

    #[test]
    fn inverse_change_restores_database() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        add(&mut ledger, &mut store, &[("2026-10-01", "Rent", "bills.rent", "1")]).unwrap();
        let change = add(&mut ledger, &mut store, &[("2026-10-02", "Vet", "dogs.health", "2"), ("2026-10-03", "Water", "bills.water", "3")]).unwrap();
        assert_eq!((count(&store, "entries"), count(&store, "categories")), (3, 5));

        ledger.apply(&mut store, &change.inverse()).unwrap();
        assert_eq!((count(&store, "entries"), count(&store, "categories")), (1, 2));
        assert_eq!(store.load().unwrap().entries(), ledger.entries());

        ledger.apply(&mut store, &change).unwrap();
        assert_eq!((count(&store, "entries"), count(&store, "categories")), (3, 5));
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
    }

    #[test]
    fn failed_change_changes_nothing() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        store.connection.execute_batch("DROP TABLE entries").unwrap();

        assert!(add(&mut ledger, &mut store, &[("2026-10-01", "Rent", "bills.rent", "1")]).is_err());
        assert!(ledger.entries().is_empty());
        assert!(ledger.categories().suggest("").is_empty());
        assert_eq!(count(&store, "categories"), 0, "category inserts are rolled back");
    }

    #[test]
    fn deleting_missing_row_fails() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        let change = add(&mut ledger, &mut store, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        store.connection.execute_batch("DELETE FROM entries").unwrap();

        assert!(matches!(store.apply(&change.inverse()), Err(StorageError::MissingRow { table: "entries" })));
        assert_eq!(count(&store, "categories"), 1, "nothing was deleted");
    }

    #[test]
    fn updates_and_reverts_entry() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        add(&mut ledger, &mut store, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        let (id, current) = ledger.entries()[0].clone();

        let mut builder = ChangeBuilder::new("Edit", ledger.categories());
        let parsed = ParsedEntry::parse(&DateFormat::iso(), "2026-10-02", "Flat", "bills.rent", "2").unwrap();
        builder.update_entry(id, &current, parsed);
        let change = builder.build();
        ledger.apply(&mut store, &change).unwrap();
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
        assert_eq!(ledger.entry(id).unwrap().name, "Flat");

        ledger.apply(&mut store, &change.inverse()).unwrap();
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
        assert_eq!(ledger.entry(id), Some(&current));
        assert_eq!(count(&store, "categories"), 1, "the category created by the edit is removed");
    }

    #[test]
    fn updating_missing_row_fails() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        add(&mut ledger, &mut store, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        let (id, current) = ledger.entries()[0].clone();
        store.connection.execute_batch("DELETE FROM entries").unwrap();

        let mut builder = ChangeBuilder::new("Edit", ledger.categories());
        let parsed = ParsedEntry::parse(&DateFormat::iso(), "2026-10-01", "Flat", "bills", "1").unwrap();
        builder.update_entry(id, &current, parsed);
        assert!(matches!(store.apply(&builder.build()), Err(StorageError::MissingRow { table: "entries" })));
    }

    #[test]
    fn rejects_invalid_dates_and_unknown_categories() {
        let store = SqliteStore::open_in_memory().unwrap();
        let bills = Uuid::now_v7();
        store
            .connection
            .execute("INSERT INTO categories (id, name) VALUES (?1, 'bills')", params![bills])
            .unwrap();
        let insert = |date: &str, category: Uuid| {
            store.connection.execute(
                "INSERT INTO entries (id, date, name, category_id, amount) VALUES (?1, ?2, 'x', ?3, 1)",
                params![Uuid::now_v7(), date, category],
            )
        };
        assert!(insert("2026-10-01", bills).is_ok());
        for date in ["2026-02-30", "1.10.2026", "2026-1-1", ""] {
            assert!(insert(date, bills).is_err(), "date: {date:?}");
        }
        assert!(insert("2026-10-01", Uuid::now_v7()).is_err(), "foreign keys are enforced");
    }
}
