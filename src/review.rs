//! Reviewing a pending import: what the review tells about its rows and what's left to do
//! before the import can be submitted. Like the matches it builds on, this is worked out from
//! the current ledger, so it follows changes to the entries, including undo and redo.

use crate::account::AccountId;
use crate::duplicates::Match;
use crate::entry::{Entry, EntryId};
use crate::import::{Import, ImportError, ImportRow, LinkKind, RowStatus};
use crate::ledger::Ledger;

/// How many rows of an import are decided how. Rows whose line is certainly in Monies already
/// are only counted as such: they're left out whatever their status, see
/// [`ChangeBuilder::submit_import`](crate::change::ChangeBuilder::submit_import).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Accepted rows, which become entries.
    pub to_add: usize,
    pub skipped: usize,
    pub linked: usize,
    /// Rows not reviewed yet.
    pub to_review: usize,
    /// Rows whose line is certainly in Monies already.
    pub duplicates: usize,
}

impl Counts {
    /// Counts the rows with their matches, one per row.
    pub fn of(rows: &[ImportRow], matches: &[Match]) -> Self {
        let mut counts = Self::default();
        for (row, found) in rows.iter().zip(matches) {
            if let Match::Duplicate(_) = found {
                counts.duplicates += 1;
                continue;
            }
            match row.status {
                RowStatus::Pending => counts.to_review += 1,
                RowStatus::Accepted => counts.to_add += 1,
                RowStatus::Skipped => counts.skipped += 1,
                RowStatus::Linked(..) => counts.linked += 1,
            }
        }
        counts
    }
}

/// The entry a row with the match `found` can be linked to: the one its line possibly matches,
/// if it was typed in. Imported entries have a statement line already.
pub fn link_target(ledger: &Ledger, found: Match) -> Option<EntryId> {
    match found {
        Match::Possible(id) => ledger.entry(id).filter(|entry| entry.statement.is_none()).map(|_| id),
        Match::New | Match::Duplicate(_) => None,
    }
}

/// The row with the match `found` linked to the entry it may be (see [`link_target`]), or
/// `None` when it can't be linked. Linking to keep the entry gives the row the entry's name
/// and category, so it shows what remains once the import is submitted; linking to use the
/// imported data leaves the row as it is.
pub fn link(ledger: &Ledger, row: &ImportRow, found: Match, kind: LinkKind) -> Option<ImportRow> {
    let id = link_target(ledger, found)?;
    let status = RowStatus::Linked(id, kind);
    let row = match kind {
        LinkKind::KeepEntry => {
            let entry = ledger.entry(id).expect("link targets exist");
            let category = Some(ledger.categories().category_path(entry.category));
            ImportRow { name: entry.name.clone(), category, status, ..row.clone() }
        }
        LinkKind::UseImported => ImportRow { status, ..row.clone() },
    };
    Some(row)
}

/// An entry shown with a row of an import, see [`related_entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelatedEntry<'a> {
    /// How the entry relates to the row, e.g. "Already in Monies".
    pub label: &'static str,
    /// `None` when a linked entry is no longer in the import's account.
    pub entry: Option<&'a Entry>,
}

/// The entry to show with a row of an import into `account`: the one it's linked to,
/// otherwise the one its line matches, if any.
pub fn related_entry<'a>(ledger: &'a Ledger, account: AccountId, row: &ImportRow, found: Match) -> Option<RelatedEntry<'a>> {
    let entry = |id| ledger.entry(id).filter(|entry| entry.account == account);
    let matched = |id| entry(id).expect("matches are entries of the account");
    let related = match (row.status, found) {
        (RowStatus::Linked(id, _), _) => match entry(id) {
            Some(entry) => RelatedEntry { label: "Linked", entry: Some(entry) },
            None => RelatedEntry { label: "Linked entry is gone", entry: None },
        },
        (_, Match::Duplicate(id)) => RelatedEntry { label: "Already in Monies", entry: Some(matched(id)) },
        (_, Match::Possible(id)) => RelatedEntry { label: "Maybe in Monies", entry: Some(matched(id)) },
        (_, Match::New) => return None,
    };
    Some(related)
}

/// Why the import can't be submitted yet, briefly, or `None` when it can (see
/// [`Import::check`]).
pub fn submit_hint(ledger: &Ledger, import: &Import) -> Option<String> {
    // Rows are numbered from 1 for the user.
    let hint = match import.check(ledger).err()? {
        ImportError::Undecided { .. } => {
            format!("{} to review", rows(Counts::of(&import.rows, &import.matches(ledger)).to_review))
        }
        ImportError::EmptyName { position } => format!("Row {} needs a name", position + 1),
        ImportError::MissingCategory { position } => format!("Row {} needs a category", position + 1),
        ImportError::LinkedEntryMissing { position } => {
            format!("Row {} is linked to an entry no longer in the account", position + 1)
        }
        ImportError::AlreadyLinked { position } => format!("Row {} is linked to an imported entry", position + 1),
        ImportError::LinkedTwice { position, first } => {
            format!("Rows {} and {} are linked to the same entry", first + 1, position + 1)
        }
        ImportError::ReferenceInUse { position } => format!("Row {} has the reference of an entry", position + 1),
    };
    Some(hint)
}

/// The label of the button submitting an import with these counts, e.g. "Add 3 entries".
pub fn submit_label(counts: Counts) -> String {
    match (counts.to_add, counts.linked) {
        (0, 0) => "Finish".to_owned(),
        (0, linked) => format!("Link {}", entries(linked)),
        (added, 0) => format!("Add {}", entries(added)),
        (added, linked) => format!("Add {}, link {linked}", entries(added)),
    }
}

/// The description of the change submitting the import, e.g. "Import 3 entries from
/// ‘statement.pdf’".
pub fn submit_description(ledger: &Ledger, import: &Import) -> String {
    let counts = Counts::of(&import.rows, &import.matches(ledger));
    let source = &import.source;
    match (counts.to_add, counts.linked) {
        (0, 0) => format!("Finish import ‘{source}’"),
        (0, linked) => format!("Link {} from ‘{source}’", entries(linked)),
        (added, 0) => format!("Import {} from ‘{source}’", entries(added)),
        (added, linked) => format!("Import {} and link {linked} from ‘{source}’", entries(added)),
    }
}

/// The title of the section of rows whose line is certainly in Monies already, e.g. "3 entries
/// already in Monies".
pub fn duplicates_title(count: usize) -> String {
    format!("{} already in Monies", entries(count))
}

fn entries(count: usize) -> String {
    if count == 1 { "1 entry".to_owned() } else { format!("{count} entries") }
}

fn rows(count: usize) -> String {
    if count == 1 { "1 row".to_owned() } else { format!("{count} rows") }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::category::CategoryPath;
    use crate::change::ChangeBuilder;
    use crate::entry::ParsedEntry;
    use crate::store::MemoryStore;

    fn row(text: &str, name: &str, category: &str, status: RowStatus) -> ImportRow {
        ImportRow::test("2026-09-15", 1250, text, name, category, status)
    }

    fn import(account: AccountId, rows: Vec<ImportRow>) -> Import {
        Import { account, created: Utc::now(), source: "statement-2026-09.pdf".to_owned(), rows }
    }

    /// A ledger with the account "Cash" in PLN, with an entry of 12.50 typed in on 2026-09-14
    /// and one imported on 2026-09-15; their ids.
    fn ledger() -> (Ledger, AccountId, EntryId, EntryId) {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        let mut builder = ChangeBuilder::new("Add", &ledger);
        let typed_in = builder.add_entry(ParsedEntry::test(account, "2026-09-14", "Groceries", "food", "12.50"));
        let id = builder.add_import(account, "statement-2026-08.pdf", vec![row("TRANSFER FLAT 12", "Rent", "bills", RowStatus::Accepted)]);
        let imported = builder.submit_import(id).unwrap()[0];
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        (ledger, account, typed_in, imported)
    }

    #[test]
    fn counts_rows_by_decision() {
        let entry = EntryId::generate();
        let rows = [
            row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending),
            row("CARD PAYMENT BAKERY 0007", "", "", RowStatus::Accepted),
            row("TRANSFER FLAT 12", "", "", RowStatus::Linked(entry, LinkKind::KeepEntry)),
            row("BANK FEE", "", "", RowStatus::Pending),
            row("CARD PAYMENT CORNER SHOP 0043", "", "", RowStatus::Skipped),
            row("CARD PAYMENT CORNER SHOP 0044", "", "", RowStatus::Linked(entry, LinkKind::UseImported)),
            row("CARD PAYMENT CORNER SHOP 0045", "", "", RowStatus::Accepted),
            row("CARD PAYMENT CORNER SHOP 0046", "", "", RowStatus::Pending),
        ];
        let mut matches = vec![Match::New; 6];
        matches.extend([Match::Duplicate(entry), Match::Duplicate(entry)]);
        assert_eq!(
            Counts::of(&rows, &matches),
            Counts { to_add: 1, skipped: 1, linked: 2, to_review: 2, duplicates: 2 },
            "duplicates count only as such"
        );
    }

    #[test]
    fn links_only_entries_typed_in() {
        let (ledger, _, typed_in, imported) = ledger();
        assert_eq!(link_target(&ledger, Match::Possible(typed_in)), Some(typed_in));
        assert_eq!(link_target(&ledger, Match::Possible(imported)), None, "it has a statement line");
        assert_eq!(link_target(&ledger, Match::Duplicate(imported)), None);
        assert_eq!(link_target(&ledger, Match::New), None);
    }

    #[test]
    fn linking_to_keep_the_entry_takes_its_name_and_category() {
        let (ledger, _, typed_in, imported) = ledger();
        let pending = row("CARD PAYMENT CORNER SHOP 0042", "Shop", "food.shop", RowStatus::Pending);
        let link_typed_in = |kind| link(&ledger, &pending, Match::Possible(typed_in), kind);

        let kept = ImportRow {
            name: "Groceries".to_owned(),
            category: CategoryPath::parse("food"),
            status: RowStatus::Linked(typed_in, LinkKind::KeepEntry),
            ..pending.clone()
        };
        assert_eq!(link_typed_in(LinkKind::KeepEntry), Some(kept));
        let used = ImportRow { status: RowStatus::Linked(typed_in, LinkKind::UseImported), ..pending.clone() };
        assert_eq!(link_typed_in(LinkKind::UseImported), Some(used), "the row keeps its name and category");
        assert_eq!(link(&ledger, &pending, Match::Possible(imported), LinkKind::KeepEntry), None, "it's imported");
        assert_eq!(link(&ledger, &pending, Match::New, LinkKind::UseImported), None);
    }

    #[test]
    fn rows_are_shown_with_their_linked_or_matching_entry() {
        let (ledger, account, typed_in, imported) = ledger();
        let pending = row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending);
        let related = |row: &ImportRow, found| {
            related_entry(&ledger, account, row, found).map(|related| (related.label, related.entry.map(|entry| entry.name.as_str())))
        };

        assert_eq!(related(&pending, Match::New), None);
        assert_eq!(related(&pending, Match::Possible(typed_in)), Some(("Maybe in Monies", Some("Groceries"))));
        assert_eq!(related(&pending, Match::Duplicate(imported)), Some(("Already in Monies", Some("Rent"))));
        let linked = ImportRow { status: RowStatus::Linked(typed_in, LinkKind::UseImported), ..pending.clone() };
        assert_eq!(related(&linked, Match::New), Some(("Linked", Some("Groceries"))), "whatever the match");
        let gone = ImportRow { status: RowStatus::Linked(EntryId::generate(), LinkKind::KeepEntry), ..pending };
        assert_eq!(related(&gone, Match::New), Some(("Linked entry is gone", None)));
    }

    #[test]
    fn hints_tell_what_is_left_before_submitting() {
        let (ledger, account, typed_in, imported) = ledger();
        let hint = |rows| submit_hint(&ledger, &import(account, rows));
        let pending = row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending);
        let linked = |entry| row("CARD PAYMENT BAKERY 0007", "", "", RowStatus::Linked(entry, LinkKind::KeepEntry));

        assert_eq!(hint(vec![pending.clone(), pending.clone()]).as_deref(), Some("2 rows to review"));
        assert_eq!(hint(vec![row("BANK FEE", "", "", RowStatus::Accepted), pending]).as_deref(), Some("1 row to review"));
        assert_eq!(hint(vec![row("BANK FEE", "Fee", "", RowStatus::Accepted)]).as_deref(), Some("Row 1 needs a category"));
        assert_eq!(hint(vec![row("BANK FEE", " ", "bills", RowStatus::Accepted)]).as_deref(), Some("Row 1 needs a name"));
        let used = row("CARD PAYMENT BAKERY 0007", "Bread", "", RowStatus::Linked(typed_in, LinkKind::UseImported));
        assert_eq!(hint(vec![used]).as_deref(), Some("Row 1 needs a category"), "its data is used");
        assert_eq!(hint(vec![linked(imported)]).as_deref(), Some("Row 1 is linked to an imported entry"));
        assert_eq!(
            hint(vec![linked(typed_in), linked(typed_in)]).as_deref(),
            Some("Rows 1 and 2 are linked to the same entry")
        );
        assert_eq!(hint(vec![linked(typed_in), row("BANK FEE", "Fee", "bills", RowStatus::Accepted)]), None);
        assert_eq!(hint(vec![row("BANK FEE", "", "", RowStatus::Skipped)]), None, "an import of skipped rows only");

        // The imported entry's line, which is left out.
        let pending = row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending);
        let duplicate = row("TRANSFER FLAT 12", "", "", RowStatus::Pending);
        assert_eq!(hint(vec![duplicate.clone()]), None, "a duplicate doesn't need reviewing or a name");
        assert_eq!(hint(vec![pending.clone(), duplicate, pending.clone()]).as_deref(), Some("2 rows to review"));
    }

    #[test]
    fn duplicates_are_titled_by_their_count() {
        assert_eq!(duplicates_title(1), "1 entry already in Monies");
        assert_eq!(duplicates_title(3), "3 entries already in Monies");
    }

    #[test]
    fn submitting_is_labelled_and_described_by_what_it_does() {
        let ledger = Ledger::default();
        let account = AccountId::generate();
        let accepted = row("BANK FEE", "Fee", "bills", RowStatus::Accepted);
        let linked = row("TRANSFER FLAT 12", "", "", RowStatus::Linked(EntryId::generate(), LinkKind::KeepEntry));
        let skipped = row("CARD PAYMENT BAKERY 0007", "", "", RowStatus::Skipped);
        let cases = [
            (vec![accepted.clone(), accepted.clone(), skipped.clone()], "Add 2 entries", "Import 2 entries from ‘statement-2026-09.pdf’"),
            (vec![accepted.clone(), linked.clone()], "Add 1 entry, link 1", "Import 1 entry and link 1 from ‘statement-2026-09.pdf’"),
            (vec![linked.clone(), linked], "Link 2 entries", "Link 2 entries from ‘statement-2026-09.pdf’"),
            (vec![skipped], "Finish", "Finish import ‘statement-2026-09.pdf’"),
        ];
        for (rows, label, description) in cases {
            let import = import(account, rows);
            assert_eq!(submit_label(Counts::of(&import.rows, &import.matches(&ledger))), label);
            assert_eq!(submit_description(&ledger, &import), description);
        }
    }
}
