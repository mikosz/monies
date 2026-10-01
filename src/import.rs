use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use crate::account::AccountId;
use crate::category::CategoryPath;

/// A transaction as the bank reported it on a statement.
///
/// Imported entries keep it as it was, with a date and amount of its own, so that the entry
/// can be edited while later imports still recognise the bank's original line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementLine {
    /// The booking date.
    pub date: NaiveDate,
    /// Amount in minor units of the account's currency, positive for expenses and negative
    /// for income, as for entries.
    pub amount: i64,
    /// The bank's description of the transaction.
    pub text: String,
    /// The bank's id of the transaction, if it gives one.
    pub reference: Option<String>,
}

/// Identifies a pending import. UUIDv7, so ordering by id is ordering by creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImportId(pub Uuid);

impl ImportId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

/// Statement lines of one account waiting to be reviewed. Submitting the import turns its
/// accepted rows into entries and drops it, see `ChangeBuilder::submit_import`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub account: AccountId,
    pub created: DateTime<Utc>,
    /// Where the lines come from, e.g. a statement's file name.
    pub source: String,
    /// In the order of the statement. Rows are identified by their position, so they're never
    /// added or removed individually.
    pub rows: Vec<ImportRow>,
}

/// A statement line together with the entry proposed for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    pub line: StatementLine,
    /// The proposed entry's name, possibly empty.
    pub name: String,
    /// The proposed entry's category, which may not exist yet. Categories are only created
    /// when the import is submitted, so rejected proposals leave none behind.
    pub category: Option<CategoryPath>,
    pub status: RowStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Not reviewed yet.
    Pending,
    /// Becomes an entry when the import is submitted.
    Accepted,
    /// Dropped when the import is submitted.
    Skipped,
}

/// Why an import can't be submitted. Positions are indices into [`Import::rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    /// An accepted row has an empty name.
    EmptyName { position: usize },
    /// An accepted row has no category.
    MissingCategory { position: usize },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Rows are numbered from 1 for the user.
        match self {
            ImportError::EmptyName { position } => write!(f, "row {}: name must not be empty", position + 1),
            ImportError::MissingCategory { position } => write!(f, "row {}: category must not be empty", position + 1),
        }
    }
}

impl std::error::Error for ImportError {}

#[cfg(test)]
impl ImportRow {
    /// A row for tests, without a reference: the date is ISO `YYYY-MM-DD`, the amount is in
    /// minor units and an empty category is a missing one. Panics on an invalid date.
    pub fn test(date: &str, amount: i64, text: &str, name: &str, category: &str, status: RowStatus) -> Self {
        let line = StatementLine {
            date: crate::date_format::DateFormat::iso().parse(date).expect("valid date"),
            amount,
            text: text.to_owned(),
            reference: None,
        };
        Self { line, name: name.to_owned(), category: CategoryPath::parse(category), status }
    }
}
