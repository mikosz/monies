use chrono::{Datelike, NaiveDate};

use crate::entry::{Entry, EntryId};
use crate::ledger::Ledger;

/// The first day of the period `date` belongs to; the list is divided into these periods.
/// Currently calendar months.
pub fn period_start(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("every month has a first day")
}

/// An item of the list of entries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ListItem<'a> {
    /// Starts the period beginning on this date.
    Period(NaiveDate),
    Entry(EntryId, &'a Entry),
}

/// Entries by date, those on the same day in the order they were entered, each period preceded
/// by its start. Entries of deleted accounts are left out.
pub fn list_items(ledger: &Ledger) -> Vec<ListItem<'_>> {
    let mut entries: Vec<_> = ledger.visible_entries().collect();
    // The ledger is in the order entries were entered, and this sort is stable.
    entries.sort_by_key(|(_, entry)| entry.date);

    let mut items = Vec::with_capacity(entries.len());
    let mut period = None;
    for (id, entry) in entries {
        let start = period_start(entry.date);
        if period != Some(start) {
            items.push(ListItem::Period(start));
            period = Some(start);
        }
        items.push(ListItem::Entry(*id, entry));
    }
    items
}

/// Replacing `remove` items at `start` with `insert` turns one list into another.
#[derive(Debug, PartialEq)]
pub struct Splice<T> {
    pub start: usize,
    pub remove: usize,
    pub insert: Vec<T>,
}

/// Turns `old` into `new` by replacing what's between their common beginning and end. Small
/// changes (one entry added, edited or removed) touch only a few items that way.
pub fn splice<T: PartialEq>(old: &[T], new: Vec<T>) -> Splice<T> {
    let prefix = old.iter().zip(&new).take_while(|(old, new)| old == new).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(old, new)| old == new)
        .count();
    let insert_len = new.len() - prefix - suffix;
    Splice {
        start: prefix,
        remove: old.len() - prefix - suffix,
        insert: new.into_iter().skip(prefix).take(insert_len).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::ChangeBuilder;
    use crate::date_format::DateFormat;
    use crate::entry::ParsedEntry;
    use crate::store::MemoryStore;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn periods_are_calendar_months() {
        assert_eq!(period_start(date(2026, 4, 1)), date(2026, 4, 1));
        assert_eq!(period_start(date(2026, 4, 30)), date(2026, 4, 1));
        assert_eq!(period_start(date(2028, 2, 29)), date(2028, 2, 1));
    }

    fn ledger(entries: &[(&str, &str)]) -> Ledger {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        for (date, name) in entries {
            let mut builder = ChangeBuilder::new("Add", &ledger);
            builder.add_entry(ParsedEntry::parse(&DateFormat::iso(), 2, date, name, "bills", "1").unwrap(), account);
            let change = builder.build();
            ledger.apply(&mut MemoryStore, &change).unwrap();
        }
        ledger
    }

    fn describe(items: &[ListItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                ListItem::Period(start) => format!("== {start}"),
                ListItem::Entry(_, entry) => entry.name.clone(),
            })
            .collect()
    }

    #[test]
    fn sorts_by_date_and_groups_by_month() {
        let ledger = ledger(&[
            ("2026-05-02", "may"),
            ("2026-04-10", "april late"),
            ("2026-04-01", "april first"),
            ("2026-05-02", "may entered later"),
            ("2025-12-31", "december"),
        ]);
        assert_eq!(
            describe(&list_items(&ledger)),
            [
                "== 2025-12-01",
                "december",
                "== 2026-04-01",
                "april first",
                "april late",
                "== 2026-05-01",
                "may",
                "may entered later",
            ]
        );
    }

    #[test]
    fn empty_ledger_has_no_items() {
        assert!(list_items(&Ledger::default()).is_empty());
    }

    #[test]
    fn leaves_out_entries_of_deleted_accounts() {
        let mut ledger = ledger(&[("2026-04-10", "cash")]);
        let bank = ledger.add_test_account("Bank");
        let mut builder = ChangeBuilder::new("Add", &ledger);
        builder.add_entry(ParsedEntry::parse(&DateFormat::iso(), 2, "2026-05-02", "bank", "bills", "1").unwrap(), bank);
        builder.delete_account(bank);
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        assert_eq!(describe(&list_items(&ledger)), ["== 2026-04-01", "cash"], "nor is May shown");
    }

    #[test]
    fn splices_only_the_difference() {
        assert_eq!(splice(&[1, 2, 3], vec![1, 2, 3]), Splice { start: 3, remove: 0, insert: vec![] });
        assert_eq!(splice(&[1, 2, 3], vec![1, 9, 3]), Splice { start: 1, remove: 1, insert: vec![9] });
        assert_eq!(splice(&[1, 3], vec![1, 2, 3]), Splice { start: 1, remove: 0, insert: vec![2] });
        assert_eq!(splice(&[1, 2, 3], vec![1, 3]), Splice { start: 1, remove: 1, insert: vec![] });
        assert_eq!(splice(&[], vec![1, 2]), Splice { start: 0, remove: 0, insert: vec![1, 2] });
        assert_eq!(splice(&[1, 2], vec![]), Splice { start: 0, remove: 2, insert: vec![] });
        // Repeated items: the common beginning and end must not overlap.
        assert_eq!(splice(&[1, 1], vec![1, 1, 1]), Splice { start: 2, remove: 0, insert: vec![1] });
        assert_eq!(splice(&[1, 1, 1], vec![1, 1]), Splice { start: 2, remove: 1, insert: vec![] });
    }

    #[test]
    fn applying_splice_gives_new_list() {
        let cases: [(&[i32], &[i32]); 4] =
            [(&[1, 2, 3, 4], &[1, 4]), (&[5, 1, 2], &[1, 2, 5]), (&[1, 2, 2, 3], &[1, 2, 3]), (&[], &[])];
        for (old, new) in cases {
            let Splice { start, remove, insert } = splice(old, new.to_vec());
            let mut result = old.to_vec();
            result.splice(start..start + remove, insert);
            assert_eq!(result, new, "{old:?} -> {new:?}");
        }
    }
}
