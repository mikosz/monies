use crate::category::Categories;
use crate::change::{Change, Op};
use crate::entry::{Entry, EntryId};
use crate::store::Store;

/// All of the user's data: entries and the categories they refer to.
#[derive(Debug, Default)]
pub struct Ledger {
    categories: Categories,
    /// Sorted by id, which is the order in which entries were added.
    entries: Vec<(EntryId, Entry)>,
}

impl Ledger {
    #[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "only used when loading from a database"))]
    pub fn new(categories: Categories, mut entries: Vec<(EntryId, Entry)>) -> Self {
        entries.sort_by_key(|(id, _)| *id);
        Self { categories, entries }
    }

    pub fn categories(&self) -> &Categories {
        &self.categories
    }

    /// In the order they were added.
    pub fn entries(&self) -> &[(EntryId, Entry)] {
        &self.entries
    }

    pub fn entry(&self, id: EntryId) -> Option<&Entry> {
        self.index_of(id).ok().map(|index| &self.entries[index].1)
    }

    /// Applies a change to the store and then, once that succeeded, to the ledger.
    pub fn apply<S: Store>(&mut self, store: &mut S, change: &Change) -> Result<(), S::Error> {
        store.apply(change)?;
        for op in &change.ops {
            self.apply_op(op);
        }
        Ok(())
    }

    fn apply_op(&mut self, op: &Op) {
        match op {
            Op::InsertCategory { id, name, parent } => self.categories.insert(*id, name.clone(), *parent),
            Op::DeleteCategory { id, .. } => self.categories.remove(*id),
            Op::InsertEntry { id, entry } => {
                let index = self.index_of(*id).expect_err("entry ids are unique");
                self.entries.insert(index, (*id, entry.clone()));
            }
            Op::DeleteEntry { id, .. } => {
                let index = self.index_of(*id).expect("deleted entry exists");
                self.entries.remove(index);
            }
            Op::UpdateEntry { id, after, .. } => {
                let index = self.index_of(*id).expect("updated entry exists");
                self.entries[index].1 = after.clone();
            }
        }
    }

    fn index_of(&self, id: EntryId) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&id, |(id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::ChangeBuilder;
    use crate::date_format::DateFormat;
    use crate::entry::ParsedEntry;
    use crate::store::MemoryStore;

    fn parsed(category: &str, amount: &str) -> ParsedEntry {
        ParsedEntry::parse(&DateFormat::iso(), "2026-09-30", "Rent", category, amount).unwrap()
    }

    fn add(ledger: &mut Ledger, category: &str) -> (Change, EntryId) {
        let mut builder = ChangeBuilder::new("Add", ledger.categories());
        let id = builder.add_entry(parsed(category, "1"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        (change, id)
    }

    fn paths(ledger: &Ledger) -> Vec<String> {
        ledger.entries().iter().map(|(_, e)| ledger.categories().path(e.category)).collect()
    }

    #[test]
    fn adds_entries_in_order() {
        let mut ledger = Ledger::default();
        let (_, first) = add(&mut ledger, "bills.rent");
        let (_, second) = add(&mut ledger, "Bills.Rent");
        add(&mut ledger, "dogs");

        assert_eq!(ledger.entries()[0].0, first);
        assert_eq!(ledger.entries()[1].0, second);
        assert_eq!(paths(&ledger), ["bills.rent", "bills.rent", "dogs"]);
        assert_eq!(ledger.entries()[0].1.category, ledger.entries()[1].1.category);
    }

    #[test]
    fn inverse_restores_previous_state() {
        let mut ledger = Ledger::default();
        add(&mut ledger, "bills.rent");
        let (change, id) = add(&mut ledger, "dogs.health");
        add(&mut ledger, "bills.water");

        ledger.apply(&mut MemoryStore, &change.inverse()).unwrap();
        assert_eq!(paths(&ledger), ["bills.rent", "bills.water"]);
        assert!(ledger.entry(id).is_none());
        assert!(ledger.categories().suggest("dogs").is_empty(), "categories created by the change are removed");

        ledger.apply(&mut MemoryStore, &change).unwrap();
        assert_eq!(paths(&ledger), ["bills.rent", "dogs.health", "bills.water"], "redo puts it back in place");
        assert!(ledger.entry(id).is_some());
    }

    #[test]
    fn updates_entry_in_place() {
        let mut ledger = Ledger::default();
        add(&mut ledger, "bills.rent");
        add(&mut ledger, "food");
        let (id, current) = ledger.entries()[0].clone();

        let mut builder = ChangeBuilder::new("Edit", ledger.categories());
        builder.update_entry(id, &current, parsed("bills.water", "-5"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        assert_eq!(paths(&ledger), ["bills.water", "food"]);
        assert_eq!(ledger.entry(id).unwrap().amount, -500);

        ledger.apply(&mut MemoryStore, &change.inverse()).unwrap();
        assert_eq!(ledger.entry(id), Some(&current));
        assert!(ledger.categories().suggest("bills.").iter().all(|path| path != "bills.water"));
    }
}
