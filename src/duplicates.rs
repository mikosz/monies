//! Recognises statement lines that are already in the ledger, so that importing overlapping
//! statements doesn't add the same transactions twice.
//!
//! A line is compared with the entries of the account it's imported into, by these rules in
//! order of strength:
//!
//! 1. The line and an imported entry's statement line have the same reference: a duplicate.
//! 2. An imported entry's statement line has the same date, amount and text, ignoring case and
//!    whitespace: a duplicate.
//! 3. An entry has the same amount and a date at most [`POSSIBLE_MATCH_DAYS`] apart: possibly a
//!    duplicate. Imported entries are compared by their statement line, as entries may have been
//!    edited since; typed-in ones by their own date and amount.
//!
//! When both the line and the entry's statement line have references and they differ, they
//! never match: the bank's id is authoritative. Each entry matches at most one line. Every rule
//! is applied to all lines before the next one, so an entry goes to the line it matches most
//! strongly; among equally good entries the oldest is taken.
//!
//! Matches are computed against the current ledger rather than stored, so they stay right as
//! entries are edited, deleted or added, e.g. by submitting another import of an overlapping
//! statement. Other pending imports aren't compared with, for the same reason.

use std::collections::HashMap;

use chrono::NaiveDate;

use crate::account::AccountId;
use crate::entry::{Entry, EntryId};
use crate::import::{RowStatus, StatementLine};
use crate::ledger::Ledger;

/// How many days apart an entry's date may be from a line's for them to possibly match, e.g.
/// because the bank books card payments a few days late. To become configurable later.
pub const POSSIBLE_MATCH_DAYS: i64 = 3;

/// How a statement line relates to the entries already in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// No entry matches the line.
    New,
    /// The entry is certainly the line, imported before.
    Duplicate(EntryId),
    /// The entry may be the line, imported before or typed in.
    Possible(EntryId),
}

impl Match {
    /// The status the line's import row starts with: certain duplicates are skipped, the rest
    /// left for the user to review.
    pub fn initial_status(&self) -> RowStatus {
        match self {
            Match::Duplicate(_) => RowStatus::Skipped,
            Match::Possible(_) | Match::New => RowStatus::Pending,
        }
    }
}

/// How many lines match how, e.g. to sum up a pending import.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchCounts {
    pub new: usize,
    pub duplicates: usize,
    pub possible: usize,
}

impl MatchCounts {
    pub fn of(matches: &[Match]) -> Self {
        let mut counts = Self::default();
        for found in matches {
            match found {
                Match::New => counts.new += 1,
                Match::Duplicate(_) => counts.duplicates += 1,
                Match::Possible(_) => counts.possible += 1,
            }
        }
        counts
    }
}

/// Matches statement lines to be imported into `account` with its entries, see the module
/// documentation. Returns one match per line, in order.
pub fn find_matches<'a>(
    ledger: &Ledger,
    account: AccountId,
    lines: impl IntoIterator<Item = &'a StatementLine>,
) -> Vec<Match> {
    let entries: Vec<(EntryId, &Entry)> = ledger.account_entries(account).map(|(id, entry)| (*id, entry)).collect();

    // Indices into `entries` by what each rule looks up, in the order the entries were added.
    let mut by_reference: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_line: HashMap<LineKey, Vec<usize>> = HashMap::new();
    let mut by_amount: HashMap<i64, Vec<usize>> = HashMap::new();
    for (index, &(_, entry)) in entries.iter().enumerate() {
        if let Some(statement) = &entry.statement {
            if let Some(reference) = &statement.reference {
                by_reference.entry(reference).or_default().push(index);
            }
            by_line.entry(LineKey::of(statement)).or_default().push(index);
        }
        by_amount.entry(compared_values(entry).1).or_default().push(index);
    }

    let lines: Vec<&StatementLine> = lines.into_iter().collect();
    let mut matcher = Matcher {
        claimed: vec![false; entries.len()],
        matches: vec![Match::New; lines.len()],
        lines,
        entries,
    };
    matcher.pass(
        |line| by_reference.get(line.reference.as_deref()?).map(Vec::as_slice),
        |_, _| Some(()),
        Match::Duplicate,
    );
    matcher.pass(|line| by_line.get(&LineKey::of(line)).map(Vec::as_slice), |_, _| Some(()), Match::Duplicate);
    matcher.pass(
        |line| by_amount.get(&line.amount).map(Vec::as_slice),
        |line, entry| {
            let days = (compared_values(entry).0 - line.date).num_days().abs();
            (days <= POSSIBLE_MATCH_DAYS).then_some(days)
        },
        Match::Possible,
    );
    matcher.matches
}

/// Lines and the entries they're matched with so far.
struct Matcher<'a> {
    lines: Vec<&'a StatementLine>,
    /// The account's entries, in the order they were added.
    entries: Vec<(EntryId, &'a Entry)>,
    /// Whether the entry at the same index matches a line already.
    claimed: Vec<bool>,
    /// For the line at the same index.
    matches: Vec<Match>,
}

impl<'a> Matcher<'a> {
    /// Applies a rule to the lines that don't match yet, in order. `candidates` gives the
    /// indices of the entries the rule may match a line with and `rank` ranks each of them, or
    /// rules it out with `None`. The best ranked unclaimed entry, the oldest of equally ranked
    /// ones, is claimed and matches the line as `kind`.
    fn pass<'m, R: Ord>(
        &mut self,
        candidates: impl Fn(&StatementLine) -> Option<&'m [usize]>,
        rank: impl Fn(&StatementLine, &Entry) -> Option<R>,
        kind: fn(EntryId) -> Match,
    ) {
        for (line_index, &line) in self.lines.iter().enumerate() {
            if self.matches[line_index] != Match::New {
                continue;
            }
            let Some(candidates) = candidates(line) else { continue };
            let best = candidates
                .iter()
                .filter(|&&index| !self.claimed[index] && !references_differ(line, self.entries[index].1))
                .filter_map(|&index| Some((rank(line, self.entries[index].1)?, index)))
                .min();
            if let Some((_, index)) = best {
                self.claimed[index] = true;
                self.matches[line_index] = kind(self.entries[index].0);
            }
        }
    }
}

/// What rule 2 compares statement lines by: date, amount and normalised text.
#[derive(PartialEq, Eq, Hash)]
struct LineKey(NaiveDate, i64, String);

impl LineKey {
    fn of(line: &StatementLine) -> Self {
        Self(line.date, line.amount, normalise(&line.text))
    }
}

/// The text trimmed, with runs of whitespace as single spaces and in lowercase, as banks aren't
/// always consistent about those.
fn normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// The date and amount to compare lines with: the bank's for an imported entry, the entry's
/// own for one typed in.
fn compared_values(entry: &Entry) -> (NaiveDate, i64) {
    match &entry.statement {
        Some(statement) => (statement.date, statement.amount),
        None => (entry.date, entry.amount),
    }
}

/// Whether the line and the entry's statement line both have references and they differ.
fn references_differ(line: &StatementLine, entry: &Entry) -> bool {
    let entry_reference = entry.statement.as_ref().and_then(|statement| statement.reference.as_ref());
    matches!((&line.reference, entry_reference), (Some(line), Some(entry)) if line != entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::category::CategoryPath;
    use crate::change::ChangeBuilder;
    use crate::date_format::DateFormat;
    use crate::entry::ParsedEntry;
    use crate::import::ImportRow;
    use crate::store::MemoryStore;

    /// A statement line: the date is ISO `YYYY-MM-DD` and the amount in minor units.
    fn line(date: &str, amount: i64, text: &str, reference: Option<&str>) -> StatementLine {
        StatementLine {
            date: DateFormat::iso().parse(date).expect("valid date"),
            amount,
            text: text.to_owned(),
            reference: reference.map(str::to_owned),
        }
    }

    /// Imports `lines` into `account` as entries and returns their ids.
    fn import<'a>(ledger: &mut Ledger, account: AccountId, lines: impl IntoIterator<Item = &'a StatementLine>) -> Vec<EntryId> {
        let rows = lines
            .into_iter()
            .map(|line| ImportRow {
                line: line.clone(),
                name: "Groceries".to_owned(),
                category: CategoryPath::parse("food"),
                status: RowStatus::Accepted,
            })
            .collect();
        let mut builder = ChangeBuilder::new("Import", ledger);
        let id = builder.add_import(account, "statement-2026-09.csv", rows);
        let entries = builder.submit_import(id).unwrap();
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        entries
    }

    /// Adds an entry typed in: the amount has two decimal places.
    fn add(ledger: &mut Ledger, account: AccountId, date: &str, amount: &str) -> EntryId {
        let mut builder = ChangeBuilder::new("Add", ledger);
        let id = builder.add_entry(ParsedEntry::test(account, date, "Groceries", "food", amount));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        id
    }

    #[test]
    fn matches_by_reference() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let entries = import(&mut ledger, account, &[line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0001"))]);

        let lines = [line("2026-09-25", 9900, "TRANSFER FLAT 12", Some("TX-0001"))];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::Duplicate(entries[0])], "nothing else needs to agree");
    }

    #[test]
    fn matches_same_statement_line() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let entries = import(&mut ledger, account, &[
            line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", None),
            line("2026-09-16", 1250, "CARD PAYMENT BAKERY 0007", Some("TX-0001")),
        ]);

        let lines = [
            line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0002")),
            line("2026-09-16", 1250, "CARD PAYMENT BAKERY 0007", None),
        ];
        assert_eq!(
            find_matches(&ledger, account, &lines),
            [Match::Duplicate(entries[0]), Match::Duplicate(entries[1])],
            "a reference on only one side doesn't matter"
        );
    }

    #[test]
    fn differing_references_never_match() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        import(&mut ledger, account, &[line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0001"))]);

        let lines = [line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0002"))];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::New], "not even possibly");
    }

    #[test]
    fn compares_texts_ignoring_case_and_whitespace() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let entries = import(&mut ledger, account, &[
            line("2026-09-15", 1250, "CARD PAYMENT  CORNER SHOP 0042", None),
            line("2026-09-15", 1250, "Café Ölmühle", None),
            line("2026-09-15", 1250, "CARD PAYMENT BAKERY 0007", None),
        ]);

        let lines = [
            line("2026-09-15", 1250, " card payment corner\tshop  0042 ", None),
            line("2026-09-15", 1250, "CAFÉ ÖLMÜHLE", None),
            line("2026-09-15", 1250, "CARD PAYMENT BAKERY 0008", None),
        ];
        assert_eq!(find_matches(&ledger, account, &lines), [
            Match::Duplicate(entries[0]),
            Match::Duplicate(entries[1]),
            Match::Possible(entries[2]),
        ]);
    }

    #[test]
    fn matches_identical_lines_one_to_one() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let shop = line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", None);
        let entries = import(&mut ledger, account, &[shop.clone(), shop.clone()]);

        let lines = [shop.clone(), shop.clone(), shop];
        assert_eq!(
            find_matches(&ledger, account, &lines),
            [Match::Duplicate(entries[0]), Match::Duplicate(entries[1]), Match::New],
            "the oldest entries first"
        );
    }

    #[test]
    fn possible_matches_are_at_most_three_days_apart() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        let entry = add(&mut ledger, account, "2026-09-15", "12.50");

        let find = |date| find_matches(&ledger, account, &[line(date, 1250, "CARD PAYMENT CORNER SHOP 0042", None)]);
        assert_eq!(find("2026-09-12"), [Match::Possible(entry)]);
        assert_eq!(find("2026-09-18"), [Match::Possible(entry)]);
        assert_eq!(find("2026-09-11"), [Match::New]);
        assert_eq!(find("2026-09-19"), [Match::New]);
        assert_eq!(find_matches(&ledger, account, &[line("2026-09-15", 1251, "CORNER SHOP", None)]), [Match::New]);
    }

    #[test]
    fn possible_matches_prefer_the_closest_date_then_the_oldest_entry() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        let far = add(&mut ledger, account, "2026-09-12", "12.50");
        let close = add(&mut ledger, account, "2026-09-17", "12.50");
        let close_newer = add(&mut ledger, account, "2026-09-13", "12.50");

        let shop = line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", None);
        let lines = [shop.clone(), shop.clone(), shop];
        assert_eq!(find_matches(&ledger, account, &lines), [
            Match::Possible(close),
            Match::Possible(close_newer),
            Match::Possible(far),
        ]);
    }

    #[test]
    fn stronger_rules_claim_entries_first() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let shop = line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", None);
        let entries = import(&mut ledger, account, [&shop]);

        let lines = [line("2026-09-16", 1250, "CARD PAYMENT BAKERY 0007", None), shop.clone()];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::New, Match::Duplicate(entries[0])]);

        // The line with the reference takes the oldest entry, so the one without gets the other.
        let referenced = StatementLine { reference: Some("TX-0001".to_owned()), ..shop.clone() };
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let entries = import(&mut ledger, account, &[referenced.clone(), shop.clone()]);
        let lines = [shop, referenced];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::Duplicate(entries[1]), Match::Duplicate(entries[0])]);
    }

    #[test]
    fn ignores_entries_of_other_accounts() {
        let mut ledger = Ledger::default();
        let (cash, bank) = (ledger.add_test_account("Cash"), ledger.add_test_account("Bank"));
        let shop = line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0001"));
        import(&mut ledger, bank, [&shop]);
        add(&mut ledger, bank, "2026-09-15", "12.50");

        assert_eq!(find_matches(&ledger, cash, &[shop]), [Match::New]);
    }

    #[test]
    fn recognises_edited_imported_entries_by_their_statement_line() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Bank");
        let shop = line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", None);
        let id = import(&mut ledger, account, [&shop])[0];
        let current = ledger.entry(id).unwrap().clone();
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(id, &current, ParsedEntry::test(account, "2026-09-20", "Weekly shop", "food", "99.00"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        assert_eq!(find_matches(&ledger, account, &[shop]), [Match::Duplicate(id)]);
        let lines = [line("2026-09-17", 1250, "CARD PAYMENT BAKERY 0007", None)];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::Possible(id)], "compared with the bank's date and amount");
        let lines = [line("2026-09-20", 9900, "CARD PAYMENT BAKERY 0007", None)];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::New], "not with the edited ones");
    }

    #[test]
    fn entries_typed_in_match_possibly() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        let entry = add(&mut ledger, account, "2026-09-15", "12.50");

        let lines = [line("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0001"))];
        assert_eq!(find_matches(&ledger, account, &lines), [Match::Possible(entry)]);
    }

    #[test]
    fn only_certain_duplicates_start_skipped() {
        let entry = EntryId::generate();
        assert_eq!(Match::Duplicate(entry).initial_status(), RowStatus::Skipped);
        assert_eq!(Match::Possible(entry).initial_status(), RowStatus::Pending);
        assert_eq!(Match::New.initial_status(), RowStatus::Pending);
    }

    #[test]
    fn counts_matches_by_kind() {
        let entry = EntryId::generate();
        let matches = [Match::New, Match::Possible(entry), Match::New, Match::Duplicate(entry), Match::New];
        assert_eq!(MatchCounts::of(&matches), MatchCounts { new: 3, duplicates: 1, possible: 1 });
        assert_eq!(MatchCounts::of(&[]), MatchCounts::default());
    }
}
