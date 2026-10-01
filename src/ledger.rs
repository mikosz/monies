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

/// How the list of entries changed while a [`Change`] was applied, in the order it happened.
/// Indices refer to the list at that moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryListUpdate {
    Inserted { index: usize, id: EntryId },
    Removed { index: usize },
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
    pub fn apply<S: Store>(&mut self, store: &mut S, change: &Change) -> Result<Vec<EntryListUpdate>, S::Error> {
        store.apply(change)?;
        Ok(change.ops.iter().filter_map(|op| self.apply_op(op)).collect())
    }

    fn apply_op(&mut self, op: &Op) -> Option<EntryListUpdate> {
        match op {
            Op::InsertCategory { id, name, parent } => {
                self.categories.insert(*id, name.clone(), *parent);
                None
            }
            Op::DeleteCategory { id, .. } => {
                self.categories.remove(*id);
                None
            }
            Op::InsertEntry { id, entry } => {
                let index = self.index_of(*id).expect_err("entry ids are unique");
                self.entries.insert(index, (*id, entry.clone()));
                Some(EntryListUpdate::Inserted { index, id: *id })
            }
            Op::DeleteEntry { id, .. } => {
                let index = self.index_of(*id).expect("deleted entry exists");
                self.entries.remove(index);
                Some(EntryListUpdate::Removed { index })
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

    fn add(ledger: &mut Ledger, category: &str) -> (Change, Vec<EntryListUpdate>) {
        let mut builder = ChangeBuilder::new("Add", ledger.categories());
        builder.add_entry(ParsedEntry::parse(&DateFormat::iso(), "2026-09-30", "Rent", category, "1").unwrap());
        let change = builder.build();
        let updates = ledger.apply(&mut MemoryStore, &change).unwrap();
        (change, updates)
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

        assert!(matches!(first[..], [EntryListUpdate::Inserted { index: 0, .. }]));
        assert!(matches!(second[..], [EntryListUpdate::Inserted { index: 1, .. }]));
        assert_eq!(paths(&ledger), ["bills.rent", "bills.rent", "dogs"]);
        assert_eq!(ledger.entries()[0].1.category, ledger.entries()[1].1.category);
    }

    #[test]
    fn inverse_restores_previous_state() {
        let mut ledger = Ledger::default();
        add(&mut ledger, "bills.rent");
        let (change, _) = add(&mut ledger, "dogs.health");
        add(&mut ledger, "bills.water");

        let updates = ledger.apply(&mut MemoryStore, &change.inverse()).unwrap();
        assert_eq!(updates, [EntryListUpdate::Removed { index: 1 }]);
        assert_eq!(paths(&ledger), ["bills.rent", "bills.water"]);
        assert!(ledger.categories().suggest("dogs").is_empty(), "categories created by the change are removed");

        let updates = ledger.apply(&mut MemoryStore, &change).unwrap();
        let [EntryListUpdate::Inserted { index: 1, id }] = updates[..] else { panic!("{updates:?}") };
        assert_eq!(paths(&ledger), ["bills.rent", "dogs.health", "bills.water"], "redo puts it back in place");
        assert!(ledger.entry(id).is_some());
    }
}
