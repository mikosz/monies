use crate::category::Categories;
use crate::entry::{Entry, ParsedEntry};
use crate::store::{NewEntry, Store};

/// All of the user's data: entries and the categories they refer to.
#[derive(Debug, Default)]
pub struct Ledger {
    categories: Categories,
    /// In the order they were added.
    entries: Vec<Entry>,
}

impl Ledger {
    #[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "only used when loading from a database"))]
    pub fn new(categories: Categories, entries: Vec<Entry>) -> Self {
        Self { categories, entries }
    }

    pub fn categories(&self) -> &Categories {
        &self.categories
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Stores a validated entry, creating its category if needed. The ledger is only changed
    /// once the store has succeeded.
    pub fn add_entry<S: Store>(&mut self, store: &mut S, parsed: ParsedEntry) -> Result<(), S::Error> {
        let missing = self.categories.missing(&parsed.category);
        let entry = NewEntry { date: parsed.date, name: &parsed.name, amount: parsed.amount };
        let ids = store.insert_entry(missing, entry)?;
        debug_assert_eq!(ids.len(), missing.names.len());

        let mut category = missing.parent;
        for (&id, name) in ids.iter().zip(missing.names) {
            self.categories.insert(id, name.clone(), category);
            category = Some(id);
        }
        self.entries.push(Entry {
            date: parsed.date,
            name: parsed.name,
            category: category.expect("an entry always has a category"),
            amount: parsed.amount,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date_format::DateFormat;
    use crate::store::MemoryStore;

    fn parsed(category: &str) -> ParsedEntry {
        ParsedEntry::parse(&DateFormat::iso(), "2026-09-30", "Rent", category, "1").unwrap()
    }

    #[test]
    fn adds_entry_with_new_category() {
        let mut ledger = Ledger::default();
        ledger.add_entry(&mut MemoryStore::default(), parsed("bills.rent")).unwrap();

        let entry = &ledger.entries()[0];
        assert_eq!(ledger.categories().path(entry.category), "bills.rent");
        assert_eq!(ledger.categories().suggest(""), ["bills"]);
    }

    #[test]
    fn reuses_existing_category() {
        let mut store = MemoryStore::default();
        let mut ledger = Ledger::default();
        ledger.add_entry(&mut store, parsed("bills.rent")).unwrap();
        ledger.add_entry(&mut store, parsed("Bills.Rent")).unwrap();

        let [first, second] = ledger.entries() else { panic!("expected two entries") };
        assert_eq!(first.category, second.category);
    }

    #[test]
    fn empty_names_in_category_are_ignored() {
        let mut store = MemoryStore::default();
        let mut ledger = Ledger::default();
        for category in ["bills", "bills.", ".bills", "bills.."] {
            ledger.add_entry(&mut store, parsed(category)).unwrap();
        }

        let first = ledger.entries()[0].category;
        assert!(ledger.entries().iter().all(|entry| entry.category == first));
        assert_eq!(ledger.categories().path(first), "bills");
    }

    #[test]
    fn adds_only_missing_categories() {
        let mut store = MemoryStore::default();
        let mut ledger = Ledger::default();
        ledger.add_entry(&mut store, parsed("dogs.health")).unwrap();
        ledger.add_entry(&mut store, parsed("dogs.health.pills")).unwrap();

        let pills = ledger.entries()[1].category;
        assert_eq!(ledger.categories().path(pills), "dogs.health.pills");
        assert_eq!(ledger.categories().suggest("dogs."), ["dogs.health"]);
    }
}
