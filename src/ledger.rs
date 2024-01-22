use crate::category::Categories;
use crate::entry::{Entry, EntryError, ParsedEntry};

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

    /// Validates the input and adds an entry, creating its category if needed.
    /// Nothing is changed when the input is invalid.
    pub fn add_entry(&mut self, date: &str, name: &str, category: &str, amount: &str) -> Result<(), EntryError> {
        let parsed = ParsedEntry::parse(date, name, category, amount)?;
        let category = self.categories.get_or_create(&parsed.category);
        self.entries.push(Entry {
            date: parsed.date,
            name: parsed.name,
            category,
            amount: parsed.amount,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_entry_with_new_category() {
        let mut ledger = Ledger::default();
        ledger.add_entry("2026-09-30", "Rent", "bills.rent", "-1200").unwrap();

        let entry = &ledger.entries()[0];
        assert_eq!(ledger.categories().path(entry.category), "bills.rent");
        assert_eq!(ledger.categories().suggest(""), ["bills"]);
    }

    #[test]
    fn reuses_existing_category() {
        let mut ledger = Ledger::default();
        ledger.add_entry("2026-09-30", "Rent", "bills.rent", "1").unwrap();
        ledger.add_entry("2026-10-31", "Rent", "Bills.Rent", "1").unwrap();

        let [first, second] = ledger.entries() else { panic!("expected two entries") };
        assert_eq!(first.category, second.category);
    }

    #[test]
    fn empty_names_in_category_are_ignored() {
        let mut ledger = Ledger::default();
        for category in ["bills", "bills.", ".bills", "bills.."] {
            ledger.add_entry("2026-09-30", "Rent", category, "1").unwrap();
        }

        let first = ledger.entries()[0].category;
        assert!(ledger.entries().iter().all(|entry| entry.category == first));
        assert_eq!(ledger.categories().path(first), "bills");
    }

    #[test]
    fn invalid_entry_creates_no_category() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.add_entry("2026-09-30", "Rent", "bills.rent", "abc"), Err(EntryError::InvalidAmount));
        assert!(ledger.entries().is_empty());
        assert!(ledger.categories().suggest("").is_empty());
    }
}
