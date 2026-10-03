use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use chrono::NaiveDate;
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, ToSql, Transaction, params};

use super::Store;
use crate::account::{Account, AccountId, Accounts};
use crate::category::{Categories, CategoryId, CategoryPath};
use crate::change::{Change, Op};
use crate::currency::Currency;
use crate::entry::{Entry, EntryId};
use crate::import::{Import, ImportId, ImportRow, LinkKind, RowStatus, StatementLine};
use crate::ledger::Ledger;

/// Schema migrations; the database's `user_version` is the number of migrations applied.
/// Once there's real data, never edit a migration, append a new one instead.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema. Ids are UUIDv7 as 16-byte BLOBs, so they sort by creation. Dates are
    // ISO `YYYY-MM-DD` text, currencies ISO 4217 codes, amounts are in minor units of their
    // account's currency (e.g. cents). Deleted accounts are in the trash, restorable.
    // Imported entries keep the bank's statement line in the `statement_` columns, all NULL
    // for entries typed in. A bank's transaction ids, `statement_reference`, are unique per
    // account; lines without one may repeat. Pending imports are `imports` with their
    // `import_rows` in statement order; a row's category is a path such as `dogs.health`, NULL
    // when missing.
    // A linked row ('keep-entry' or 'use-imported', by what its entry is left with) refers to
    // its entry by `linked_entry_id`, without a foreign key: the entry may be deleted while the
    // import is pending, which submitting it then reports.
    // Timestamps are UTC, `YYYY-MM-DD HH:MM:SS.fffffffff+00:00`.
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
        id                  BLOB PRIMARY KEY CHECK (length(id) = 16),
        account_id          BLOB NOT NULL REFERENCES accounts(id),
        date                TEXT NOT NULL CHECK (date IS date(date)),
        name                TEXT NOT NULL,
        category_id         BLOB NOT NULL REFERENCES categories(id),
        amount              INTEGER NOT NULL,
        statement_date      TEXT CHECK (statement_date IS date(statement_date)),
        statement_amount    INTEGER,
        statement_text      TEXT,
        statement_reference TEXT,
        -- A statement line is complete or absent; only its reference is optional.
        CHECK ((statement_date IS NULL) = (statement_amount IS NULL)
            AND (statement_date IS NULL) = (statement_text IS NULL)
            AND (statement_reference IS NULL OR statement_date IS NOT NULL))
    );
    CREATE INDEX entries_account ON entries(account_id);
    CREATE INDEX entries_date ON entries(date);
    CREATE INDEX entries_category ON entries(category_id);
    CREATE UNIQUE INDEX entries_reference ON entries(account_id, statement_reference)
        WHERE statement_reference IS NOT NULL;

    CREATE TABLE imports (
        id         BLOB PRIMARY KEY CHECK (length(id) = 16),
        account_id BLOB NOT NULL REFERENCES accounts(id),
        created    TEXT NOT NULL CHECK (datetime(created) IS NOT NULL),
        source     TEXT NOT NULL
    );
    CREATE INDEX imports_account ON imports(account_id);

    CREATE TABLE import_rows (
        import_id BLOB NOT NULL REFERENCES imports(id),
        position  INTEGER NOT NULL CHECK (position >= 0),
        date      TEXT NOT NULL CHECK (date IS date(date)),
        amount    INTEGER NOT NULL,
        text      TEXT NOT NULL,
        reference TEXT,
        name      TEXT NOT NULL,
        category  TEXT CHECK (category <> ''),
        status    TEXT NOT NULL
                  CHECK (status IN ('pending', 'accepted', 'skipped', 'keep-entry', 'use-imported')),
        linked_entry_id BLOB CHECK (length(linked_entry_id) = 16),
        PRIMARY KEY (import_id, position),
        CHECK ((status IN ('keep-entry', 'use-imported')) = (linked_entry_id IS NOT NULL))
    );",
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

    /// Reads all accounts, categories, entries and pending imports.
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

        let mut statement = self.connection.prepare(
            "SELECT id, account_id, date, name, category_id, amount,
                    statement_date, statement_amount, statement_text, statement_reference
             FROM entries",
        )?;
        let entries = statement
            .query_map([], |row| {
                // The schema ensures the statement line is complete or absent.
                let statement = match (row.get::<_, Option<NaiveDate>>(6)?, row.get(7)?, row.get(8)?) {
                    (Some(date), Some(amount), Some(text)) => {
                        Some(StatementLine { date, amount, text, reference: row.get(9)? })
                    }
                    _ => None,
                };
                let entry = Entry {
                    account: AccountId(row.get(1)?),
                    date: row.get(2)?,
                    name: row.get(3)?,
                    category: CategoryId(row.get(4)?),
                    amount: row.get(5)?,
                    statement,
                };
                Ok((EntryId(row.get(0)?), entry))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        // Rows are only ever inserted with their import, so their positions are 0, 1, 2, ...
        let mut rows: BTreeMap<ImportId, Vec<ImportRow>> = BTreeMap::new();
        let mut statement = self.connection.prepare(
            "SELECT import_id, date, amount, text, reference, name, category, status, linked_entry_id
             FROM import_rows ORDER BY import_id, position",
        )?;
        let loaded = statement.query_map([], |row| {
            let line = StatementLine { date: row.get(1)?, amount: row.get(2)?, text: row.get(3)?, reference: row.get(4)? };
            let status = row_status(row.get_ref(7)?, row.get::<_, Option<_>>(8)?.map(EntryId))?;
            let import_row = ImportRow { line, name: row.get(5)?, category: row.get(6)?, status };
            Ok((ImportId(row.get(0)?), import_row))
        })?;
        for row in loaded {
            let (id, row) = row?;
            rows.entry(id).or_default().push(row);
        }

        let mut statement = self.connection.prepare("SELECT id, account_id, created, source FROM imports")?;
        let imports = statement
            .query_map([], |row| {
                let id = ImportId(row.get(0)?);
                let rows = rows.remove(&id).unwrap_or_default();
                Ok((id, Import { account: AccountId(row.get(1)?), created: row.get(2)?, source: row.get(3)?, rows }))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Ledger::new(accounts, categories, entries, imports))
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
            let statement = entry.statement.as_ref();
            transaction.execute(
                "INSERT INTO entries (id, account_id, date, name, category_id, amount,
                     statement_date, statement_amount, statement_text, statement_reference)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id.0,
                    entry.account.0,
                    entry.date,
                    entry.name,
                    entry.category.0,
                    entry.amount,
                    statement.map(|line| line.date),
                    statement.map(|line| line.amount),
                    statement.map(|line| &line.text),
                    statement.and_then(|line| line.reference.as_ref()),
                ],
            )?;
        }
        Op::DeleteEntry { id, .. } => {
            let deleted = transaction.execute("DELETE FROM entries WHERE id = ?1", params![id.0])?;
            if deleted != 1 {
                return Err(StorageError::MissingRow { table: "entries" });
            }
        }
        Op::UpdateEntry { id, after, .. } => {
            let statement = after.statement.as_ref();
            let updated = transaction.execute(
                "UPDATE entries SET account_id = ?2, date = ?3, name = ?4, category_id = ?5, amount = ?6,
                     statement_date = ?7, statement_amount = ?8, statement_text = ?9, statement_reference = ?10
                 WHERE id = ?1",
                params![
                    id.0,
                    after.account.0,
                    after.date,
                    after.name,
                    after.category.0,
                    after.amount,
                    statement.map(|line| line.date),
                    statement.map(|line| line.amount),
                    statement.map(|line| &line.text),
                    statement.and_then(|line| line.reference.as_ref()),
                ],
            )?;
            if updated != 1 {
                return Err(StorageError::MissingRow { table: "entries" });
            }
        }
        Op::InsertImport { id, import } => {
            transaction.execute(
                "INSERT INTO imports (id, account_id, created, source) VALUES (?1, ?2, ?3, ?4)",
                params![id.0, import.account.0, import.created, import.source],
            )?;
            for (position, row) in import.rows.iter().enumerate() {
                let (status, linked) = status_columns(row.status);
                transaction.execute(
                    "INSERT INTO import_rows (import_id, position, date, amount, text, reference, name, category, status,
                         linked_entry_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id.0,
                        position as i64,
                        row.line.date,
                        row.line.amount,
                        row.line.text,
                        row.line.reference,
                        row.name,
                        row.category,
                        status,
                        linked.map(|entry| entry.0),
                    ],
                )?;
            }
        }
        Op::DeleteImport { id, import } => {
            let deleted = transaction.execute("DELETE FROM import_rows WHERE import_id = ?1", params![id.0])?;
            if deleted != import.rows.len() {
                return Err(StorageError::MissingRow { table: "import_rows" });
            }
            let deleted = transaction.execute("DELETE FROM imports WHERE id = ?1", params![id.0])?;
            if deleted != 1 {
                return Err(StorageError::MissingRow { table: "imports" });
            }
        }
        Op::UpdateImportRow { id, position, after, .. } => {
            let (status, linked) = status_columns(after.status);
            let updated = transaction.execute(
                "UPDATE import_rows SET date = ?3, amount = ?4, text = ?5, reference = ?6, name = ?7, category = ?8,
                     status = ?9, linked_entry_id = ?10
                 WHERE import_id = ?1 AND position = ?2",
                params![
                    id.0,
                    *position as i64,
                    after.line.date,
                    after.line.amount,
                    after.line.text,
                    after.line.reference,
                    after.name,
                    after.category,
                    status,
                    linked.map(|entry| entry.0),
                ],
            )?;
            if updated != 1 {
                return Err(StorageError::MissingRow { table: "import_rows" });
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

/// Category paths are stored as text such as `dogs.health`.
impl ToSql for CategoryPath {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

impl FromSql for CategoryPath {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let path = value.as_str()?;
        CategoryPath::parse(path).ok_or_else(|| FromSqlError::Other(format!("invalid category path {path:?}").into()))
    }
}

/// Row statuses are stored as lowercase text, a linked row's by its kind of link and its entry
/// in a column of its own.
fn status_columns(status: RowStatus) -> (&'static str, Option<EntryId>) {
    match status {
        RowStatus::Pending => ("pending", None),
        RowStatus::Accepted => ("accepted", None),
        RowStatus::Skipped => ("skipped", None),
        RowStatus::Linked(entry, LinkKind::KeepEntry) => ("keep-entry", Some(entry)),
        RowStatus::Linked(entry, LinkKind::UseImported) => ("use-imported", Some(entry)),
    }
}

/// The status stored by [`status_columns`].
fn row_status(status: ValueRef<'_>, linked: Option<EntryId>) -> rusqlite::Result<RowStatus> {
    let text = status.as_str()?;
    match (text, linked) {
        ("pending", None) => Ok(RowStatus::Pending),
        ("accepted", None) => Ok(RowStatus::Accepted),
        ("skipped", None) => Ok(RowStatus::Skipped),
        ("keep-entry", Some(entry)) => Ok(RowStatus::Linked(entry, LinkKind::KeepEntry)),
        ("use-imported", Some(entry)) => Ok(RowStatus::Linked(entry, LinkKind::UseImported)),
        _ => Err(FromSqlError::Other(format!("invalid import row status {text:?}").into()).into()),
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
                builder.add_entry(ParsedEntry::test(account, date, name, category, amount));
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

        let parsed = ParsedEntry::test(account, "2026-10-02", "Flat", "bills.rent", "2");
        let change = perform(&mut ledger, &mut store, |builder| builder.update_entry(id, &current, parsed).unwrap()).unwrap();
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
        assert_eq!(ledger.entry(id).unwrap().name, "Flat");

        ledger.apply(&mut store, &change.inverse()).unwrap();
        assert_eq!(store.load().unwrap().entries(), ledger.entries());
        assert_eq!(ledger.entry(id), Some(&current));
        assert_eq!(count(&store, "categories"), 1, "the category created by the edit is removed");
    }

    #[test]
    fn moves_entry_to_another_account_and_back() {
        let (mut store, mut ledger, cash) = with_account();
        let bank = add_account(&mut ledger, &mut store, "Bank", "EUR");
        add(&mut ledger, &mut store, cash, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        let (id, current) = ledger.entries()[0].clone();

        let parsed = ParsedEntry::test(bank, "2026-10-01", "Rent", "bills", "1");
        let change = perform(&mut ledger, &mut store, |builder| builder.update_entry(id, &current, parsed).unwrap()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        assert_eq!(loaded.entry(id).unwrap().account, bank);

        ledger.apply(&mut store, &change.inverse()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        assert_eq!(loaded.entry(id), Some(&current));
    }

    #[test]
    fn updating_missing_row_fails() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-10-01", "Rent", "bills", "1")]).unwrap();
        let (id, current) = ledger.entries()[0].clone();
        store.connection.execute_batch("DELETE FROM entries").unwrap();

        let mut builder = ChangeBuilder::new("Edit", &ledger);
        let parsed = ParsedEntry::test(account, "2026-10-01", "Flat", "bills", "1");
        builder.update_entry(id, &current, parsed).unwrap();
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

    /// Rows of an import that can be submitted, one with a reference and one with a missing
    /// category.
    fn import_rows() -> Vec<ImportRow> {
        let mut groceries = ImportRow::test("2026-09-30", 1250, "CARD PAYMENT CORNER SHOP 0042", "Groceries", "food.shop", RowStatus::Accepted);
        groceries.line.reference = Some("TX-0001".to_owned());
        vec![
            groceries,
            ImportRow::test("2026-09-30", -240000, "SALARY FICTIONAL LTD", "Salary", "income", RowStatus::Accepted),
            ImportRow::test("2026-10-01", 500, "CARD PAYMENT BAKERY 0007", "", "", RowStatus::Skipped),
            ImportRow::test("2026-10-01", 9, "BANK FEE", "Fee", "bills", RowStatus::Skipped),
        ]
    }

    fn add_import(ledger: &mut Ledger, store: &mut SqliteStore, account: AccountId) -> (Change, ImportId) {
        let mut id = None;
        let change =
            perform(ledger, store, |builder| id = Some(builder.add_import(account, "statement-2026-09.csv", import_rows()))).unwrap();
        (change, id.unwrap())
    }

    fn imports(ledger: &Ledger) -> Vec<(ImportId, Import)> {
        ledger.imports().map(|(id, import)| (id, import.clone())).collect()
    }

    #[test]
    fn saves_reviews_and_discards_imports_and_back() {
        let (mut store, mut ledger, account) = with_account();
        let (add, id) = add_import(&mut ledger, &mut store, account);
        assert_eq!(imports(&store.load().unwrap()), imports(&ledger));
        assert_eq!(count(&store, "import_rows"), 4);
        let added = imports(&ledger);

        let row = ImportRow { name: "Bread".to_owned(), ..ledger.import(id).unwrap().rows[2].clone() };
        let review = perform(&mut ledger, &mut store, |builder| builder.update_import_row(id, 2, row)).unwrap();
        assert_eq!(imports(&store.load().unwrap()), imports(&ledger));
        ledger.apply(&mut store, &review.inverse()).unwrap();
        assert_eq!(imports(&store.load().unwrap()), added);

        let discard = perform(&mut ledger, &mut store, |builder| builder.discard_import(id)).unwrap();
        assert_eq!((count(&store, "imports"), count(&store, "import_rows")), (0, 0));
        ledger.apply(&mut store, &discard.inverse()).unwrap();
        assert_eq!(imports(&store.load().unwrap()), added);

        ledger.apply(&mut store, &add.inverse()).unwrap();
        assert_eq!((count(&store, "imports"), count(&store, "import_rows")), (0, 0));
    }

    #[test]
    fn submitted_entries_keep_their_statement_lines() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-09-29", "Rent", "bills", "1")]).unwrap();
        let (_, id) = add_import(&mut ledger, &mut store, account);

        perform(&mut ledger, &mut store, |builder| {
            builder.submit_import(id).unwrap();
        })
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        let references: Vec<Option<&str>> =
            loaded.entries().iter().map(|(_, e)| e.statement.as_ref().and_then(|line| line.reference.as_deref())).collect();
        assert_eq!(references, [None, Some("TX-0001"), None]);
        assert!(loaded.entries()[2].1.statement.is_some(), "a statement line without a reference");
        assert_eq!((count(&store, "imports"), count(&store, "import_rows")), (0, 0));

        let (entry_id, current) = ledger.entries()[1].clone();
        let parsed = ParsedEntry::test(account, "2026-10-02", "Shopping", "food", "13");
        perform(&mut ledger, &mut store, |builder| builder.update_entry(entry_id, &current, parsed).unwrap()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        assert_eq!(loaded.entry(entry_id).unwrap().statement, current.statement, "editing keeps the statement line");
    }

    #[test]
    fn undoing_submit_restores_import() {
        let (mut store, mut ledger, account) = with_account();
        let (_, id) = add_import(&mut ledger, &mut store, account);
        let pending = imports(&ledger);

        let submit = perform(&mut ledger, &mut store, |builder| {
            builder.submit_import(id).unwrap();
        })
        .unwrap();
        assert_eq!((count(&store, "entries"), count(&store, "categories")), (2, 3));

        ledger.apply(&mut store, &submit.inverse()).unwrap();
        assert_eq!((count(&store, "entries"), count(&store, "categories")), (0, 0));
        assert_eq!(imports(&store.load().unwrap()), pending, "with its row statuses");
    }

    #[test]
    fn saves_and_submits_linked_rows_and_back() {
        let (mut store, mut ledger, account) = with_account();
        add(&mut ledger, &mut store, account, &[("2026-09-29", "Groceries", "food", "12.50"), ("2026-10-02", "Fees", "food", "0.09")])
            .unwrap();
        let entries = ledger.entries().to_vec();
        let (kept, used) = (entries[0].0, entries[1].0);
        let rows = vec![
            ImportRow::test("2026-09-30", 1250, "CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending),
            ImportRow::test("2026-10-01", 9, "BANK FEE", "Fee", "bills", RowStatus::Pending),
        ];
        let mut id = None;
        perform(&mut ledger, &mut store, |builder| id = Some(builder.add_import(account, "statement-2026-09.csv", rows.clone())))
            .unwrap();
        let id = id.unwrap();
        assert_eq!(imports(&store.load().unwrap()), imports(&ledger));

        let keep = ImportRow { status: RowStatus::Linked(kept, LinkKind::KeepEntry), ..rows[0].clone() };
        let use_imported = ImportRow { status: RowStatus::Linked(used, LinkKind::UseImported), ..rows[1].clone() };
        perform(&mut ledger, &mut store, |builder| {
            builder.update_import_row(id, 0, keep);
            builder.update_import_row(id, 1, use_imported);
        })
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(imports(&loaded), imports(&ledger));
        let statuses: Vec<RowStatus> = loaded.import(id).unwrap().rows.iter().map(|row| row.status).collect();
        assert_eq!(statuses, [RowStatus::Linked(kept, LinkKind::KeepEntry), RowStatus::Linked(used, LinkKind::UseImported)]);

        let submit = perform(&mut ledger, &mut store, |builder| {
            builder.submit_import(id).unwrap();
        })
        .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), ledger.entries());
        assert_eq!(loaded.entry(kept).unwrap().statement.as_ref(), Some(&rows[0].line));
        let updated = loaded.entry(used).unwrap();
        assert_eq!((updated.name.as_str(), loaded.categories().path(updated.category)), ("Fee", "bills".to_owned()));
        assert_eq!((updated.amount, updated.statement.as_ref()), (9, Some(&rows[1].line)));
        assert_eq!((count(&store, "entries"), count(&store, "imports")), (2, 0));

        ledger.apply(&mut store, &submit.inverse()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.entries(), entries, "undo restores the entries");
        assert_eq!(count(&store, "categories"), 1, "and removes the category created");
        assert_eq!(imports(&loaded), imports(&ledger));
    }

    #[test]
    fn deletes_account_with_imports_permanently_and_back() {
        let (mut store, mut ledger, cash) = with_account();
        let bank = add_account(&mut ledger, &mut store, "Bank", "EUR");
        add_import(&mut ledger, &mut store, cash);
        add_import(&mut ledger, &mut store, bank);
        let all = imports(&ledger);

        let change = perform(&mut ledger, &mut store, |builder| builder.delete_account_permanently(cash)).unwrap();
        assert_eq!((count(&store, "accounts"), count(&store, "imports"), count(&store, "import_rows")), (1, 1, 4));

        ledger.apply(&mut store, &change.inverse()).unwrap();
        assert_eq!(imports(&store.load().unwrap()), all);
    }

    #[test]
    fn discarding_import_with_missing_rows_fails() {
        let (mut store, mut ledger, account) = with_account();
        let (_, id) = add_import(&mut ledger, &mut store, account);
        store.connection.execute_batch("DELETE FROM import_rows WHERE position = 3").unwrap();

        let mut builder = ChangeBuilder::new("Discard", &ledger);
        builder.discard_import(id);
        assert!(matches!(store.apply(&builder.build()), Err(StorageError::MissingRow { table: "import_rows" })));
        assert_eq!(count(&store, "import_rows"), 3, "nothing was deleted");
    }

    #[test]
    fn rejects_incomplete_statement_lines() {
        let (store, ..) = with_account();
        let (account, bills) = (store.load().unwrap().accounts().iter().next().unwrap().0, Uuid::now_v7());
        store.connection.execute("INSERT INTO categories (id, name) VALUES (?1, 'bills')", params![bills]).unwrap();
        let insert = |date: Option<&str>, amount: Option<i64>, text: Option<&str>, reference: Option<&str>| {
            store.connection.execute(
                "INSERT INTO entries (id, account_id, date, name, category_id, amount,
                     statement_date, statement_amount, statement_text, statement_reference)
                 VALUES (?1, ?2, '2026-10-01', 'x', ?3, 1, ?4, ?5, ?6, ?7)",
                params![Uuid::now_v7(), account.0, bills, date, amount, text, reference],
            )
        };
        assert!(insert(None, None, None, None).is_ok());
        assert!(insert(Some("2026-10-01"), Some(1), Some("BANK FEE"), None).is_ok());
        assert!(insert(Some("2026-10-01"), Some(1), Some("BANK FEE"), Some("TX-0001")).is_ok());
        assert!(insert(Some("2026-10-01"), Some(1), None, None).is_err(), "text missing");
        assert!(insert(Some("2026-10-01"), None, Some("BANK FEE"), None).is_err(), "amount missing");
        assert!(insert(None, Some(1), Some("BANK FEE"), None).is_err(), "date missing");
        assert!(insert(None, None, None, Some("TX-0001")).is_err(), "a reference needs a statement line");
        assert!(insert(Some("2026-02-30"), Some(1), Some("BANK FEE"), None).is_err(), "invalid date");
    }

    #[test]
    fn references_are_unique_per_account() {
        let (mut store, mut ledger, cash) = with_account();
        let bank = add_account(&mut ledger, &mut store, "Bank", "PLN");
        let bills = Uuid::now_v7();
        store.connection.execute("INSERT INTO categories (id, name) VALUES (?1, 'bills')", params![bills]).unwrap();
        let insert = |account: AccountId, reference: Option<&str>| {
            store.connection.execute(
                "INSERT INTO entries (id, account_id, date, name, category_id, amount,
                     statement_date, statement_amount, statement_text, statement_reference)
                 VALUES (?1, ?2, '2026-10-01', 'x', ?3, 1, '2026-10-01', 1, 'BANK FEE', ?4)",
                params![Uuid::now_v7(), account.0, bills, reference],
            )
        };
        assert!(insert(cash, Some("TX-0001")).is_ok());
        assert!(insert(cash, Some("TX-0001")).is_err(), "the same account");
        assert!(insert(bank, Some("TX-0001")).is_ok(), "another account");
        assert!(insert(cash, None).is_ok());
        assert!(insert(cash, None).is_ok(), "lines without a reference may repeat");
    }

    #[test]
    fn rejects_invalid_imports() {
        let (store, ..) = with_account();
        let account = store.load().unwrap().accounts().iter().next().unwrap().0;
        let import = |created: &str| {
            let id = Uuid::now_v7();
            store
                .connection
                .execute(
                    "INSERT INTO imports (id, account_id, created, source) VALUES (?1, ?2, ?3, 'Claude')",
                    params![id, account.0, created],
                )
                .map(|_| id)
        };
        let id = import("2026-10-01 12:30:00.123456789+00:00").unwrap();
        assert!(import("yesterday").is_err());
        assert!(import("").is_err());

        let row = |position: i64, date: &str, category: Option<&str>, status: &str| {
            store.connection.execute(
                "INSERT INTO import_rows (import_id, position, date, amount, text, name, category, status)
                 VALUES (?1, ?2, ?3, 1, 'BANK FEE', '', ?4, ?5)",
                params![id, position, date, category, status],
            )
        };
        assert!(row(0, "2026-10-01", None, "pending").is_ok());
        assert!(row(1, "2026-10-01", Some("bills.fees"), "accepted").is_ok());
        assert!(row(2, "2026-10-01", None, "skipped").is_ok());
        assert!(row(0, "2026-10-01", None, "pending").is_err(), "positions are unique");
        assert!(row(3, "2026-10-01", None, "Pending").is_err(), "status");
        assert!(row(3, "2026-10-01", None, "").is_err(), "status");
        assert!(row(3, "2026-02-30", None, "pending").is_err(), "date");
        assert!(row(3, "2026-10-01", Some(""), "pending").is_err(), "category");
        assert!(row(-1, "2026-10-01", None, "pending").is_err(), "position");
    }

    #[test]
    fn rejects_linked_rows_without_entry_and_entries_of_unlinked_rows() {
        let (store, ..) = with_account();
        let account = store.load().unwrap().accounts().iter().next().unwrap().0;
        let id = Uuid::now_v7();
        store
            .connection
            .execute(
                "INSERT INTO imports (id, account_id, created, source) VALUES (?1, ?2, '2026-10-01 12:30:00+00:00', 'Claude')",
                params![id, account.0],
            )
            .unwrap();
        let row = |position: i64, status: &str, linked: Option<&[u8]>| {
            store.connection.execute(
                "INSERT INTO import_rows (import_id, position, date, amount, text, name, status, linked_entry_id)
                 VALUES (?1, ?2, '2026-10-01', 1, 'BANK FEE', '', ?3, ?4)",
                params![id, position, status, linked],
            )
        };
        let entry = Uuid::now_v7();
        assert!(row(0, "keep-entry", Some(entry.as_bytes())).is_ok());
        assert!(row(1, "use-imported", Some(entry.as_bytes())).is_ok());
        assert!(row(2, "pending", None).is_ok());
        assert!(row(3, "keep-entry", None).is_err(), "a linked row needs its entry");
        assert!(row(3, "use-imported", None).is_err(), "a linked row needs its entry");
        assert!(row(3, "accepted", Some(entry.as_bytes())).is_err(), "only linked rows have an entry");
        assert!(row(3, "keep-entry", Some(&[1, 2, 3])).is_err(), "entry ids are 16 bytes");
        assert!(row(3, "linked", Some(entry.as_bytes())).is_err(), "status");
    }
}
