use crate::category::Categories;
use crate::entry::{Entry, ParsedEntry};

/// All of the user's data: entries and the categories they refer to.
#[derive(Debug, Default)]
pub struct Ledger {
    categories: Categories,
    entries: Vec<Entry>,
}

impl Ledger {
    pub fn categories(&self) -> &Categories {
        &self.categories
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Adds a validated entry, creating its category if needed.
    pub fn add_entry(&mut self, parsed: ParsedEntry) {
        let category = self.categories.get_or_create(&parsed.category);
        self.entries.push(Entry {
            date: parsed.date,
            name: parsed.name,
            category,
            amount: parsed.amount,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date_format::DateFormat;

    fn parsed(category: &str) -> ParsedEntry {
        ParsedEntry::parse(&DateFormat::iso(), "2026-09-30", "Rent", category, "1").unwrap()
    }

    #[test]
    fn adds_entry_with_new_category() {
        let mut ledger = Ledger::default();
        ledger.add_entry(parsed("bills.rent"));

        let entry = &ledger.entries()[0];
        assert_eq!(ledger.categories().path(entry.category), "bills.rent");
        assert_eq!(ledger.categories().suggest(""), ["bills"]);
    }

    #[test]
    fn reuses_existing_category() {
        let mut ledger = Ledger::default();
        ledger.add_entry(parsed("bills.rent"));
        ledger.add_entry(parsed("Bills.Rent"));

        let [first, second] = ledger.entries() else { panic!("expected two entries") };
        assert_eq!(first.category, second.category);
    }

    #[test]
    fn empty_names_in_category_are_ignored() {
        let mut ledger = Ledger::default();
        for category in ["bills", "bills.", ".bills", "bills.."] {
            ledger.add_entry(parsed(category));
        }

        let first = ledger.entries()[0].category;
        assert!(ledger.entries().iter().all(|entry| entry.category == first));
        assert_eq!(ledger.categories().path(first), "bills");
    }
}
