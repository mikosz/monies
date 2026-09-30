use std::fmt;
use std::path::Path;

use rusqlite::{Connection, params};

use super::{NewEntry, Store};
use crate::category::{Categories, CategoryId, MissingCategories};
use crate::entry::Entry;
use crate::ledger::Ledger;

/// Schema migrations; the database's `user_version` is the number of migrations applied.
/// Never edit a released migration, append a new one instead.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema. Dates are ISO `YYYY-MM-DD` text, amounts are cents.
    "CREATE TABLE categories (
        id        INTEGER PRIMARY KEY,
        name      TEXT NOT NULL CHECK (name <> ''),
        parent_id INTEGER REFERENCES categories(id)
    );
    CREATE INDEX categories_parent ON categories(parent_id);

    CREATE TABLE entries (
        id          INTEGER PRIMARY KEY,
        date        TEXT NOT NULL CHECK (date IS date(date)),
        name        TEXT NOT NULL,
        category_id INTEGER NOT NULL REFERENCES categories(id),
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
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Sqlite(error) => Some(error),
            StorageError::Io(error) => Some(error),
            StorageError::NewerSchema { .. } => None,
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

    /// Reads all categories and entries. Entries keep the order in which they were added.
    pub fn load(&self) -> Result<Ledger, StorageError> {
        let mut categories = Categories::default();
        let mut statement = self.connection.prepare("SELECT id, name, parent_id FROM categories")?;
        let rows = statement.query_map([], |row| {
            Ok((CategoryId(row.get(0)?), row.get(1)?, row.get::<_, Option<i64>>(2)?.map(CategoryId)))
        })?;
        for row in rows {
            let (id, name, parent) = row?;
            categories.insert(id, name, parent);
        }

        let mut statement = self
            .connection
            .prepare("SELECT date, name, category_id, amount FROM entries ORDER BY id")?;
        let entries = statement
            .query_map([], |row| {
                Ok(Entry {
                    date: row.get(0)?,
                    name: row.get(1)?,
                    category: CategoryId(row.get(2)?),
                    amount: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Ledger::new(categories, entries))
    }
}

impl Store for SqliteStore {
    type Error = StorageError;

    fn insert_entry(
        &mut self,
        categories: MissingCategories<'_>,
        entry: NewEntry<'_>,
    ) -> Result<Vec<CategoryId>, StorageError> {
        let transaction = self.connection.transaction()?;

        let mut ids = Vec::with_capacity(categories.names.len());
        let mut parent = categories.parent;
        for name in categories.names {
            transaction.execute(
                "INSERT INTO categories (name, parent_id) VALUES (?1, ?2)",
                params![name, parent.map(|id| id.0)],
            )?;
            let id = CategoryId(transaction.last_insert_rowid());
            ids.push(id);
            parent = Some(id);
        }

        let category = parent.expect("an entry always has a category");
        transaction.execute(
            "INSERT INTO entries (date, name, category_id, amount) VALUES (?1, ?2, ?3, ?4)",
            params![entry.date, entry.name, category.0, entry.amount],
        )?;

        transaction.commit()?;
        Ok(ids)
    }
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
    use super::*;
    use crate::date_format::DateFormat;
    use crate::entry::ParsedEntry;

    fn parsed(date: &str, name: &str, category: &str, amount: &str) -> ParsedEntry {
        ParsedEntry::parse(&DateFormat::iso(), date, name, category, amount).unwrap()
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
        ledger.add_entry(&mut store, parsed("2026-10-01", "Rent", "bills.rent", "-1200.50")).unwrap();
        ledger.add_entry(&mut store, parsed("2026-09-15", "Vet", "dogs.health", "80")).unwrap();
        ledger.add_entry(&mut store, parsed("2026-09-20", "Water", "Bills.water", "30")).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        let paths: Vec<String> = loaded.entries().iter().map(|e| loaded.categories().path(e.category)).collect();
        assert_eq!(paths, ["bills.rent", "dogs.health", "bills.water"]);
        assert_eq!(loaded.categories().suggest(""), ["bills", "dogs"]);
    }

    #[test]
    fn failed_write_changes_nothing() {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        store.connection.execute_batch("DROP TABLE entries").unwrap();

        assert!(ledger.add_entry(&mut store, parsed("2026-10-01", "Rent", "bills.rent", "1")).is_err());
        assert!(ledger.entries().is_empty());
        assert!(ledger.categories().suggest("").is_empty());
        let categories: i64 = store.connection.query_row("SELECT count(*) FROM categories", [], |row| row.get(0)).unwrap();
        assert_eq!(categories, 0, "category inserts are rolled back");
    }

    #[test]
    fn rejects_invalid_dates_and_unknown_categories() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.connection.execute("INSERT INTO categories (id, name) VALUES (1, 'bills')", []).unwrap();
        let insert = |date: &str, category: i64| {
            store.connection.execute(
                "INSERT INTO entries (date, name, category_id, amount) VALUES (?1, 'x', ?2, 1)",
                params![date, category],
            )
        };
        assert!(insert("2026-10-01", 1).is_ok());
        for date in ["2026-02-30", "1.10.2026", "2026-1-1", ""] {
            assert!(insert(date, 1).is_err(), "date: {date:?}");
        }
        assert!(insert("2026-10-01", 2).is_err(), "foreign keys are enforced");
    }
}
