use std::collections::{HashMap, HashSet};
use std::fmt;

use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

use crate::account::AccountId;
use crate::category::CategoryPath;
use crate::duplicates::{Match, find_matches};
use crate::entry::EntryId;
use crate::ledger::Ledger;

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
    /// The bank's id of the transaction, if it gives one. Unique within the account.
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
/// accepted rows into entries, gives linked entries their lines and drops it, see
/// `ChangeBuilder::submit_import`.
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

impl Import {
    /// How the rows' lines match the ledger's entries now, one match per row, see
    /// [`find_matches`].
    pub fn matches(&self, ledger: &Ledger) -> Vec<Match> {
        find_matches(ledger, self.account, self.rows.iter().map(|row| &row.line))
    }

    /// Checks that the import can be submitted with the ledger's entries: every row has been
    /// reviewed, accepted rows and those linked to use the imported data have a name and a
    /// category, accepted and linked rows have no reference of an entry of the account, and
    /// linked rows are linked to distinct entries of the import's account that have no
    /// statement line yet. Rows whose line is certainly in the ledger already are left out, see
    /// [`ChangeBuilder::submit_import`](crate::change::ChangeBuilder::submit_import). Rows still
    /// to review are reported first, as they're most of the work left; other problems in row
    /// order.
    pub fn check(&self, ledger: &Ledger) -> Result<(), ImportError> {
        // Whatever their status: a duplicate still to review, or one without a name, doesn't
        // hold up the rest.
        let matches = self.matches(ledger);
        let rows = || {
            self.rows.iter().enumerate().filter(|&(position, _)| !matches!(matches[position], Match::Duplicate(_)))
        };
        if let Some((position, _)) = rows().find(|(_, row)| row.status == RowStatus::Pending) {
            return Err(ImportError::Undecided { position });
        }
        // References the bank gave the account's entries. A bank's ids are unique per account,
        // which the database enforces. A row with one of them is normally a certain duplicate,
        // so this only guards against e.g. two lines with the same reference.
        let references: HashSet<&str> = ledger
            .account_entries(self.account)
            .filter_map(|(_, entry)| entry.statement.as_ref()?.reference.as_deref())
            .collect();
        // Linked entries and the rows linking them.
        let mut linked: HashMap<EntryId, usize> = HashMap::new();
        for (position, row) in rows() {
            if row.uses_imported_data() {
                if row.name.trim().is_empty() {
                    return Err(ImportError::EmptyName { position });
                }
                if row.category.is_none() {
                    return Err(ImportError::MissingCategory { position });
                }
            }
            let entered = matches!(row.status, RowStatus::Accepted | RowStatus::Linked(..));
            if entered && row.line.reference.as_deref().is_some_and(|reference| references.contains(reference)) {
                return Err(ImportError::ReferenceInUse { position });
            }
            if let RowStatus::Linked(id, _) = row.status {
                let entry = ledger
                    .entry(id)
                    .filter(|entry| entry.account == self.account)
                    .ok_or(ImportError::LinkedEntryMissing { position })?;
                if entry.statement.is_some() {
                    return Err(ImportError::AlreadyLinked { position });
                }
                if let Some(first) = linked.insert(id, position) {
                    return Err(ImportError::LinkedTwice { position, first });
                }
            }
        }
        Ok(())
    }
}

/// A statement line together with the entry proposed for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRow {
    pub line: StatementLine,
    /// The proposed entry's name. Rows loaded from a file always have one, see
    /// [`crate::import_file::load`], and edits can't empty it.
    pub name: String,
    /// The proposed entry's category, which may not exist yet. Categories are only created
    /// when the import is submitted, so rejected proposals leave none behind.
    pub category: Option<CategoryPath>,
    pub status: RowStatus,
}

impl ImportRow {
    /// Whether submitting the import makes an entry of the row's name and category: an added
    /// one, or a linked entry taking them over.
    pub fn uses_imported_data(&self) -> bool {
        matches!(self.status, RowStatus::Accepted | RowStatus::Linked(_, LinkKind::UseImported))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    /// Not reviewed yet.
    Pending,
    /// Becomes an entry when the import is submitted.
    Accepted,
    /// Dropped when the import is submitted.
    Skipped,
    /// The line is that of the entry, typed in before: submitting the import gives the entry
    /// the line instead of adding one, so later imports recognise it. The entry keeps its own
    /// date and amount; whether it keeps its name and category too depends on the kind.
    Linked(EntryId, LinkKind),
}

/// What a linked entry is left with once the import is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// The entry keeps its name and category; only the statement line is used.
    KeepEntry,
    /// The entry takes the row's name and category.
    UseImported,
}

/// Why an import can't be submitted. Positions are indices into [`Import::rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    /// A row hasn't been reviewed yet.
    Undecided { position: usize },
    /// A row whose data is used (see [`ImportRow::uses_imported_data`]) has an empty name.
    EmptyName { position: usize },
    /// A row whose data is used has no category.
    MissingCategory { position: usize },
    /// A linked row's entry has been deleted or moved to another account.
    LinkedEntryMissing { position: usize },
    /// A linked row's entry has a statement line already, e.g. from an earlier import.
    AlreadyLinked { position: usize },
    /// A linked row's entry is linked by the row at `first` too.
    LinkedTwice { position: usize, first: usize },
    /// An accepted or linked row's line has the reference of an entry of the account: the
    /// bank's ids are unique per account.
    ReferenceInUse { position: usize },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Rows are numbered from 1 for the user.
        match self {
            ImportError::Undecided { position } => write!(f, "row {} hasn't been reviewed", position + 1),
            ImportError::EmptyName { position } => write!(f, "row {}: name must not be empty", position + 1),
            ImportError::MissingCategory { position } => write!(f, "row {}: category must not be empty", position + 1),
            ImportError::LinkedEntryMissing { position } => {
                write!(f, "row {}: the linked entry is no longer in the account", position + 1)
            }
            ImportError::AlreadyLinked { position } => {
                write!(f, "row {}: the linked entry has a statement line already", position + 1)
            }
            ImportError::LinkedTwice { position, first } => {
                write!(f, "row {}: the linked entry is linked by row {} too", position + 1, first + 1)
            }
            ImportError::ReferenceInUse { position } => {
                write!(f, "row {}: an entry of the account has the same reference", position + 1)
            }
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
