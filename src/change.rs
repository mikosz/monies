use crate::category::{Categories, CategoryId, CategoryPath};
use crate::entry::{Entry, EntryId, ParsedEntry};

/// A primitive modification of the ledger. Every operation carries enough data to be
/// reversed, see [`Op::inverse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    InsertCategory { id: CategoryId, name: String, parent: Option<CategoryId> },
    DeleteCategory { id: CategoryId, name: String, parent: Option<CategoryId> },
    InsertEntry { id: EntryId, entry: Entry },
    DeleteEntry { id: EntryId, entry: Entry },
    UpdateEntry { id: EntryId, before: Entry, after: Entry },
}

impl Op {
    pub fn inverse(&self) -> Op {
        match self.clone() {
            Op::InsertCategory { id, name, parent } => Op::DeleteCategory { id, name, parent },
            Op::DeleteCategory { id, name, parent } => Op::InsertCategory { id, name, parent },
            Op::InsertEntry { id, entry } => Op::DeleteEntry { id, entry },
            Op::DeleteEntry { id, entry } => Op::InsertEntry { id, entry },
            Op::UpdateEntry { id, before, after } => Op::UpdateEntry { id, before: after, after: before },
        }
    }
}

/// One user action (e.g. adding an entry, later importing many) as a sequence of operations
/// that's applied, undone and redone as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Shown to the user, e.g. "Add entry ‘Rent’".
    pub description: String,
    pub ops: Vec<Op>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The change that reverts this one: inverse operations in reverse order.
    pub fn inverse(&self) -> Change {
        Change {
            description: self.description.clone(),
            ops: self.ops.iter().rev().map(Op::inverse).collect(),
        }
    }
}

/// Builds a [`Change`] against the current categories, creating missing ones. Categories
/// created earlier in the same change are reused, so e.g. importing many entries into a new
/// category creates it once.
pub struct ChangeBuilder {
    description: String,
    /// The current categories plus those created by this change so far.
    categories: Categories,
    ops: Vec<Op>,
}

impl ChangeBuilder {
    pub fn new(description: impl Into<String>, categories: &Categories) -> Self {
        Self { description: description.into(), categories: categories.clone(), ops: Vec::new() }
    }

    pub fn add_entry(&mut self, parsed: ParsedEntry) -> EntryId {
        let category = self.category(&parsed.category);
        let id = EntryId::generate();
        let entry = Entry { date: parsed.date, name: parsed.name, category, amount: parsed.amount };
        self.ops.push(Op::InsertEntry { id, entry });
        id
    }

    /// Replaces the entry `id`, currently `current`, with the parsed input. Records nothing
    /// when the input doesn't change the entry.
    pub fn update_entry(&mut self, id: EntryId, current: &Entry, parsed: ParsedEntry) {
        // An unchanged entry keeps its existing category, so no categories are created then.
        let category = self.category(&parsed.category);
        let after = Entry { date: parsed.date, name: parsed.name, category, amount: parsed.amount };
        if after != *current {
            self.ops.push(Op::UpdateEntry { id, before: current.clone(), after });
        }
    }

    pub fn build(self) -> Change {
        Change { description: self.description, ops: self.ops }
    }

    /// The category at `path`, creating it and any missing ancestors.
    fn category(&mut self, path: &CategoryPath) -> CategoryId {
        let missing = self.categories.missing(path);
        let mut category = missing.parent;
        for name in missing.names {
            let id = CategoryId::generate();
            self.categories.insert(id, name.clone(), category);
            self.ops.push(Op::InsertCategory { id, name: name.clone(), parent: category });
            category = Some(id);
        }
        category.expect("category path is never empty")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date_format::DateFormat;

    fn parsed(category: &str) -> ParsedEntry {
        ParsedEntry::parse(&DateFormat::iso(), "2026-09-30", "Rent", category, "1").unwrap()
    }

    fn inserted_categories(change: &Change) -> Vec<&str> {
        change
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::InsertCategory { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn creates_missing_categories_before_the_entry() {
        let mut builder = ChangeBuilder::new("Add", &Categories::default());
        builder.add_entry(parsed("bills.rent"));
        let change = builder.build();

        assert_eq!(inserted_categories(&change), ["bills", "rent"]);
        let [Op::InsertCategory { id: bills, parent: None, .. }, Op::InsertCategory { id: rent, parent, .. }, Op::InsertEntry { entry, .. }] =
            change.ops.as_slice()
        else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!(*parent, Some(*bills));
        assert_eq!(entry.category, *rent);
    }

    #[test]
    fn reuses_existing_and_newly_created_categories() {
        let mut existing = Categories::default();
        let bills = CategoryId::generate();
        existing.insert(bills, "Bills".to_owned(), None);

        let mut builder = ChangeBuilder::new("Import", &existing);
        builder.add_entry(parsed("bills.rent"));
        builder.add_entry(parsed("BILLS.Rent"));
        builder.add_entry(parsed("bills"));
        let change = builder.build();

        assert_eq!(inserted_categories(&change), ["rent"]);
        let categories: Vec<CategoryId> = change
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::InsertEntry { entry, .. } => Some(entry.category),
                _ => None,
            })
            .collect();
        assert_eq!(categories[0], categories[1]);
        assert_eq!(categories[2], bills);
        assert_eq!(existing.find(&CategoryPath::parse("bills.rent").unwrap()), None, "input categories are untouched");
    }

    #[test]
    fn entry_ids_follow_creation_order() {
        let mut builder = ChangeBuilder::new("Import", &Categories::default());
        let ids: Vec<EntryId> = (0..100).map(|_| builder.add_entry(parsed("bills"))).collect();
        assert!(ids.is_sorted());
    }

    #[test]
    fn inverse_reverses_and_inverts_operations() {
        let mut builder = ChangeBuilder::new("Add", &Categories::default());
        builder.add_entry(parsed("bills.rent"));
        let change = builder.build();
        let inverse = change.inverse();

        assert_eq!(inverse.description, change.description);
        let expected: Vec<Op> = change.ops.iter().rev().map(Op::inverse).collect();
        assert_eq!(inverse.ops, expected);
        assert!(matches!(inverse.ops[0], Op::DeleteEntry { .. }));
        assert_eq!(inverse.inverse(), change);
    }

    fn entry(categories: &Categories, category: &str, amount: i64) -> Entry {
        let category = categories.find(&CategoryPath::parse(category).unwrap()).unwrap();
        Entry { date: parsed("x").date, name: "Rent".to_owned(), category, amount }
    }

    #[test]
    fn updates_entry_into_new_category() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "bills".to_owned(), None);
        let current = entry(&categories, "bills", 100);

        let id = EntryId::generate();
        let mut builder = ChangeBuilder::new("Edit", &categories);
        builder.update_entry(id, &current, parsed("bills.rent"));
        let change = builder.build();

        assert_eq!(inserted_categories(&change), ["rent"]);
        let [_, Op::UpdateEntry { id: updated, before, after }] = change.ops.as_slice() else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!((*updated, before), (id, &current));
        assert_ne!(after.category, current.category);
        assert_eq!(after.amount, 100);
    }

    #[test]
    fn unchanged_update_records_nothing() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "Bills".to_owned(), None);
        let current = entry(&categories, "bills", 100);

        let mut builder = ChangeBuilder::new("Edit", &categories);
        builder.update_entry(EntryId::generate(), &current, parsed("BILLS"));
        assert!(builder.build().is_empty());
    }

    #[test]
    fn update_inverse_swaps_before_and_after() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "bills".to_owned(), None);
        let (before, after) = (entry(&categories, "bills", 1), entry(&categories, "bills", 2));
        let id = EntryId::generate();

        let op = Op::UpdateEntry { id, before: before.clone(), after: after.clone() };
        assert_eq!(op.inverse(), Op::UpdateEntry { id, before: after, after: before });
    }
}
