use std::fmt;
use std::path::Path;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, ToSql, Transaction, params};

use super::Store;
use crate::account::{Account, AccountId, Accounts};
use crate::category::{Categories, CategoryId};
use crate::change::{Change, Op};
use crate::currency::Currency;
use crate::entry::{Entry, EntryId};
use crate::ledger::Ledger;

/// Schema migrations; the database's `user_version` is the number of migrations applied.
/// Once there's real data, never edit a migration, append a new one instead.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema. Ids are UUIDv7 as 16-byte BLOBs, so they sort by creation. Dates are
    // ISO `YYYY-MM-DD` text, currencies ISO 4217 codes, amounts are in minor units of their
    // account's currency (e.g. cents). Deleted accounts are in the trash, restorable.
    "CREATE TABLE accounts (
        id       BLOB PRIMARY KEY CHECK (length(id) = 16),
        name     TEXT NOT NULL CHECK (name <> ''),
        currency TEXT NOT NULL CHECK (currency GLOB '[A-Z][A-Z][A-Z]'),
        deleted  INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
    );

    CREATE TABLE categories (
        id        BLOB PRIMARY KEY CHECK (length(id) = 16),
        name      TEXT NOT NULL CHECK (name <> ''),
        parent_id BLOB REFERENCES categories(id)
    );
    CREATE INDEX categories_parent ON categories(parent_id);

    CREATE TABLE entries (
        id          BLOB PRIMARY KEY CHECK (length(id) = 16),
        account_id  BLOB NOT NULL REFERENCES accounts(id),
        date        TEXT NOT NULL CHECK (date IS date(date)),
        name        TEXT NOT NULL,
        category_id BLOB NOT NULL REFERENCES categories(id),
        amount      INTEGER NOT NULL
    );
    CREATE INDEX entries_account ON entries(account_id);
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

    /// Reads all accounts, categories and entries.
    pub fn load(&self) -> Result<Ledger, StorageError> {
        let mut accounts = Accounts::default();
        let mut statement = self.connection.prepare("SELECT id, name, currency, deleted FROM accounts")?;
        let rows = statement.query_map([], |row| {
            let account = Account { name: row.get(1)?, currency: row.get(2)?, deleted: row.get(3)? };
            Ok((AccountId(row.get(0)?), account))
        })?;
        for row in rows {
            let (id, account) = row?;
            accounts.insert(id, account);
        }

        let mut categories = Categories::default();
        let mut statement = self.connection.prepare("SELECT id, name, parent_id FROM categories")?;
        let rows = statement.query_map([], |row| {
            Ok((CategoryId(row.get(0)?), row.get(1)?, row.get::<_, Option<_>>(2)?.map(CategoryId)))
        })?;
        for row in rows {
            let (id, name, parent) = row?;
            categories.insert(id, name, parent);
        }

        let mut statement =
            self.connection.prepare("SELECT id, account_id, date, name, category_id, amount FROM entries")?;
        let entries = statement
            .query_map([], |row| {
                let entry = Entry {
                    account: AccountId(row.get(1)?),
                    date: row.get(2)?,
                    name: row.get(3)?,
                    category: CategoryId(row.get(4)?),
                    amount: row.get(5)?,
                };
                Ok((EntryId(row.get(0)?), entry))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Ledger::new(accounts, categories, entries))
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
        Op::InsertAccount { id, account } => {
            transaction.execute(
                "INSERT INTO accounts (id, name, currency, deleted) VALUES (?1, ?2, ?3, ?4)",
                params![id.0, account.name, account.currency, account.deleted],
            )?;
        }
        Op::UpdateAccount { id, after, .. } => {
            let updated = transaction.execute(
                "UPDATE accounts SET name = ?2, currency = ?3, deleted = ?4 WHERE id = ?1",
                params![id.0, after.name, after.currency, after.deleted],
            )?;
            if updated != 1 {
                return Err(StorageError::MissingRow { table: "accounts" });
            }
        }
        Op::DeleteAccount { id, .. } => {
            let deleted = transaction.execute("DELETE FROM accounts WHERE id = ?1", params![id.0])?;
            if deleted != 1 {
                return Err(StorageError::MissingRow { table: "accounts" });
            }
        }
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
                "INSERT INTO entries (id, account_id, date, name, category_id, amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id.0, entry.account.0, entry.date, entry.name, entry.category.0, entry.amount],
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
                "UPDATE entries SET account_id = ?2, date = ?3, name = ?4, category_id = ?5, amount = ?6 WHERE id = ?1",
                params![id.0, after.account.0, after.date, after.name, after.category.0, after.amount],
            )?;
            if updated != 1 {
                return Err(StorageError::MissingRow { table: "entries" });
            }
        }
    }
    Ok(())
}

/// Currencies are stored as their code.
impl ToSql for Currency {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.code().into())
    }
}

impl FromSql for Currency {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let code = value.as_str()?;
        Currency::parse(code).ok_or_else(|| FromSqlError::Other(format!("invalid currency code {code:?}").into()))
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
    use uuid::Uuid;

    use super::*;
    use crate::change::ChangeBuilder;
    use crate::date_format::DateFormat;
    use crate::entry::ParsedEntry;

    /// Builds a change with `build` and applies it.
    fn perform(ledger: &mut Ledger, store: &mut SqliteStore, build: impl FnOnce(&mut ChangeBuilder)) -> Result<Change, StorageError> {
        let mut builder = ChangeBuilder::new("Change", ledger);
        build(&mut builder);
        let change = builder.build();
        ledger.apply(store, &change)?;
        Ok(change)
    }

    fn add_account(ledger: &mut Ledger, store: &mut SqliteStore, name: &str, currency: &str) -> AccountId {
        let mut id = None;
        perform(ledger, store, |builder| id = builder.add_account(name, Currency::parse(currency).unwrap()).ok()).unwrap();
        id.unwrap()
    }

    fn add(ledger: &mut Ledger, store: &mut SqliteStore, account: AccountId, rows: &[(&str, &str, &str, &str)]) -> Result<Change, StorageError> {
        perform(ledger, store, |builder| {
            for (date, name, category, amount) in rows {
                builder.add_entry(ParsedEntry::parse(&DateFormat::iso(), 2, date, name, category, amount).unwrap(), account);
            }
        })
    }

    /// A store and ledger with one account in PLN, and that account.
    fn with_account() -> (SqliteStore, Ledger, AccountId) {
        let mut store = SqliteStore::open_in_memory().unwrap();
        let mut ledger = Ledger::default();
        let account = add_account(&mut ledger, &mut store, "Cash", "PLN");
        (store, ledger, account)
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
        let ledger = store.load().unwrap();
        assert!(ledger.entries().is_empty());
        assert_eq!(ledger.accounts().iter().count(), 0, "no account is created");
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
    fn saves_and_loads_ledger() {
        let (mut store, mut ledger, cash) = with_account();
        let bank = add_account(&mut ledger, &mut store, "Bank", "JPY");
        add(&mut ledger, &mut store, cash, &[("2026-10-01", "Rent", "bills.rent", "-1200.50")]).unwrap();
        add(&mut ledger, &mut store, bank, &[("2026-09-15", "Vet", "dogs.health", "80"), ("2026-09-20", "Water", "Bills.water", "30")]).unwrap();
        perform(&mut ledger, &mut store, |builder| builder.delete_account(bank)).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded.accounts(), ledger.accounts());
        assert_eq!(loaded.entries(), ledger.entries());
        let paths: Vec<String> = loaded.entries().iter().map(|(_, e)| loaded.categories().path(e.category)).collect();
        assert_eq!(paths, ["bills.rent", "dogs.health", "bills.water"], "in the order entered");
        assert_eq!(loaded.categories().suggest(""), ["bills", "dogs"]);
    }

    #[test]
    fn inverse_change_restores_database() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills.rent", "1")]).unwrap();
        let change = add(&mut ledger, &mut store, account, &[("2026-10-02", "Vet", "dogs.health", "2"), ("2026-10-03", "Water", "bills.water", "3")]).unwrap();
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
        let (mut store, mut ledger, account) = with_account();
        store.connection.execute_batch("DROP TABLE entries").unwrap();

        assert!(add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills.rent", "1")]).is_err());
        assert!(ledger.entries().is_empty());
        assert!(ledger.categories().suggest("").is_empty());
        assert_eq!(count(&store, "categories"), 0, "category inserts are rolled back");
    }

    #[test]
    fn deleting_missing_row_fails() {
        let (mut store, mut ledger, account) = with_account();
        let change = add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        store.connection.execute_batch("DELETE FROM entries").unwrap();

        assert!(matches!(store.apply(&change.inverse()), Err(StorageError::MissingRow { table: "entries" })));
        assert_eq!(count(&store, "categories"), 1, "nothing was deleted");
    }

    #[test]
    fn updates_and_reverts_entry() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        let (id, current) = ledger.entries()[0].clone();

        let parsed = ParsedEntry::parse(&DateFormat::iso(), 2, "2026-10-02", "Flat", "bills.rent", "2").unwrap();
        let change = perform(&mut ledger, &mut store, |builder| builder.update_entry(id, &current, parsed)).unwrap();
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
        assert_eq!(ledger.entry(id).unwrap().name, "Flat");

        ledger.apply(&mut store, &change.inverse()).unwrap();
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
        assert_eq!(ledger.entry(id), Some(&current));
        assert_eq!(count(&store, "categories"), 1, "the category created by the edit is removed");
    }

    #[test]
    fn updating_missing_row_fails() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        let (id, current) = ledger.entries()[0].clone();
        store.connection.execute_batch("DELETE FROM entries").unwrap();

        let mut builder = ChangeBuilder::new("Edit", &ledger);
        let parsed = ParsedEntry::parse(&DateFormat::iso(), 2, "2026-10-01", "Flat", "bills", "1").unwrap();
        builder.update_entry(id, &current, parsed);
        assert!(matches!(store.apply(&builder.build()), Err(StorageError::MissingRow { table: "entries" })));
    }

    #[test]
    fn updates_and_reverts_accounts() {
        let (mut store, mut ledger, account) = with_account();
        let rename = perform(&mut ledger, &mut store, |builder| {
            builder.update_account(account, "Wallet", Currency::parse("EUR").unwrap()).unwrap();
        })
        .unwrap();
        perform(&mut ledger, &mut store, |builder| builder.delete_account(account)).unwrap();
        assert_eq!(store.load().unwrap().accounts(), ledger.accounts());
        let loaded = store.load().unwrap();
        let wallet = loaded.accounts().get(account).unwrap();
        assert_eq!((wallet.name.as_str(), wallet.currency.code(), wallet.deleted), ("Wallet", "EUR", true));

        perform(&mut ledger, &mut store, |builder| builder.restore_account(account)).unwrap();
        ledger.apply(&mut store, &rename.inverse()).unwrap();
        assert_eq!(store.load().unwrap().accounts(), ledger.accounts());
        assert_eq!(ledger.accounts().get(account).unwrap().name, "Cash");
    }

    #[test]
    fn updating_missing_account_fails() {
        let (mut store, mut ledger, account) = with_account();
        store.connection.execute_batch("DELETE FROM accounts").unwrap();
        let mut builder = ChangeBuilder::new("Delete", &ledger);
        builder.delete_account(account);
        let change = builder.build();
        assert!(matches!(ledger.apply(&mut store, &change), Err(StorageError::MissingRow { table: "accounts" })));
    }

    #[test]
    fn deletes_account_with_entries_permanently_and_back() {
        let (mut store, mut ledger, cash) = with_account();
        let bank = add_account(&mut ledger, &mut store, "Bank", "EUR");
        add(&mut ledger, &mut store, cash, &[("2026-10-01", "Rent", "bills", "1"), ("2026-10-02", "Vet", "dogs", "2")]).unwrap();
        add(&mut ledger, &mut store, bank, &[("2026-10-03", "Water", "bills", "3")]).unwrap();

        let change = perform(&mut ledger, &mut store, |builder| builder.delete_account_permanently(cash)).unwrap();
        assert_eq!((count(&store, "accounts"), count(&store, "entries"), count(&store, "categories")), (1, 1, 2));

        ledger.apply(&mut store, &change.inverse()).unwrap();
        assert_eq!((count(&store, "accounts"), count(&store, "entries")), (2, 3));
        let loaded = store.load().unwrap();
        assert_eq!((loaded.accounts(), loaded.entries()), (ledger.accounts(), ledger.entries()));
    }

    #[test]
    fn deleting_account_permanently_is_all_or_nothing() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills", "1"), ("2026-10-02", "Vet", "dogs", "2")]).unwrap();
        let mut builder = ChangeBuilder::new("Delete", &ledger);
        builder.delete_account_permanently(account);
        let change = builder.build();
        // The database and the app disagree about the last entry, so its deletion fails.
        store.connection.execute("DELETE FROM entries WHERE id = ?1", params![ledger.entries()[1].0.0]).unwrap();

        assert!(store.apply(&change).is_err());
        assert_eq!((count(&store, "accounts"), count(&store, "entries")), (1, 1), "nothing was deleted");
    }

    #[test]
    fn rejects_invalid_dates_and_unknown_references() {
        let store = SqliteStore::open_in_memory().unwrap();
        let (cash, bills) = (Uuid::now_v7(), Uuid::now_v7());
        store
            .connection
            .execute("INSERT INTO accounts (id, name, currency) VALUES (?1, 'Cash', 'PLN')", params![cash])
            .unwrap();
        store
            .connection
            .execute("INSERT INTO categories (id, name) VALUES (?1, 'bills')", params![bills])
            .unwrap();
        let insert = |account: Uuid, date: &str, category: Uuid| {
            store.connection.execute(
                "INSERT INTO entries (id, account_id, date, name, category_id, amount) VALUES (?1, ?2, ?3, 'x', ?4, 1)",
                params![Uuid::now_v7(), account, date, category],
            )
        };
        assert!(insert(cash, "2026-10-01", bills).is_ok());
        for date in ["2026-02-30", "1.10.2026", "2026-1-1", ""] {
            assert!(insert(cash, date, bills).is_err(), "date: {date:?}");
        }
        assert!(insert(cash, "2026-10-01", Uuid::now_v7()).is_err(), "categories must exist");
        assert!(insert(Uuid::now_v7(), "2026-10-01", bills).is_err(), "accounts must exist");
    }

    #[test]
    fn rejects_invalid_currencies() {
        let store = SqliteStore::open_in_memory().unwrap();
        let insert = |currency: &str| {
            store.connection.execute(
                "INSERT INTO accounts (id, name, currency) VALUES (?1, ?2, ?3)",
                params![Uuid::now_v7(), format!("Account {currency}"), currency],
            )
        };
        assert!(insert("PLN").is_ok());
        for currency in ["pln", "PL", "PLNN", "P1N", ""] {
            assert!(insert(currency).is_err(), "currency: {currency:?}");
        }
    }
}
