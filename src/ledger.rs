use crate::account::{AccountId, Accounts};
use crate::category::Categories;
use crate::change::{Change, Op};
use crate::entry::{Entry, EntryId};
use crate::store::Store;

/// All of the user's data: entries and the accounts and categories they refer to.
#[derive(Debug, Default)]
pub struct Ledger {
    accounts: Accounts,
    categories: Categories,
    /// Sorted by id, which is the order in which entries were added.
    entries: Vec<(EntryId, Entry)>,
}

impl Ledger {
    #[cfg_attr(target_arch = "wasm32", expect(dead_code, reason = "only used when loading from a database"))]
    pub fn new(accounts: Accounts, categories: Categories, mut entries: Vec<(EntryId, Entry)>) -> Self {
        entries.sort_by_key(|(id, _)| *id);
        Self { accounts, categories, entries }
    }

    pub fn accounts(&self) -> &Accounts {
        &self.accounts
    }

    pub fn categories(&self) -> &Categories {
        &self.categories
    }

    /// In the order they were added, including those of deleted accounts. The app only shows
    /// [`Self::visible_entries`].
    #[cfg(test)]
    pub fn entries(&self) -> &[(EntryId, Entry)] {
        &self.entries
    }

    /// Entries of accounts that aren't deleted, in the order they were added.
    pub fn visible_entries(&self) -> impl DoubleEndedIterator<Item = &(EntryId, Entry)> {
        self.entries.iter().filter(|(_, entry)| self.accounts.get(entry.account).is_some_and(|account| !account.deleted))
    }

    /// Entries of the account, in the order they were added.
    pub fn account_entries(&self, account: AccountId) -> impl Iterator<Item = &(EntryId, Entry)> {
        self.entries.iter().filter(move |(_, entry)| entry.account == account)
    }

    pub fn entry_count(&self, account: AccountId) -> usize {
        self.account_entries(account).count()
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
            Op::InsertAccount { id, account } => self.accounts.insert(*id, account.clone()),
            Op::UpdateAccount { id, after, .. } => self.accounts.replace(*id, after.clone()),
            Op::DeleteAccount { id, .. } => self.accounts.remove(*id),
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
impl Ledger {
    /// Adds an account in PLN, for tests that need somewhere to add entries.
    pub fn add_test_account(&mut self, name: &str) -> AccountId {
        let mut builder = crate::change::ChangeBuilder::new("Add account", self);
        let id = builder.add_account(name, crate::currency::Currency::parse("PLN").unwrap()).unwrap();
        let change = builder.build();
        self.apply(&mut crate::store::MemoryStore, &change).unwrap();
        id
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
        ParsedEntry::parse(&DateFormat::iso(), 2, "2026-09-30", "Rent", category, amount).unwrap()
    }

    fn add(ledger: &mut Ledger, account: AccountId, category: &str) -> (Change, EntryId) {
        let mut builder = ChangeBuilder::new("Add", ledger);
        let id = builder.add_entry(parsed(category, "1"), account);
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        (change, id)
    }

    /// Builds a change with `build` and applies it.
    fn perform(ledger: &mut Ledger, build: impl FnOnce(&mut ChangeBuilder)) -> Change {
        let mut builder = ChangeBuilder::new("Change", ledger);
        build(&mut builder);
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        change
    }

    fn paths(ledger: &Ledger) -> Vec<String> {
        ledger.entries().iter().map(|(_, e)| ledger.categories().path(e.category)).collect()
    }

    fn visible_paths(ledger: &Ledger) -> Vec<String> {
        ledger.visible_entries().map(|(_, e)| ledger.categories().path(e.category)).collect()
    }

    #[test]
    fn adds_entries_in_order() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        let (_, first) = add(&mut ledger, account, "bills.rent");
        let (_, second) = add(&mut ledger, account, "Bills.Rent");
        add(&mut ledger, account, "dogs");

        assert_eq!(ledger.entries()[0].0, first);
        assert_eq!(ledger.entries()[1].0, second);
        assert_eq!(paths(&ledger), ["bills.rent", "bills.rent", "dogs"]);
        assert_eq!(ledger.entries()[0].1.category, ledger.entries()[1].1.category);
        assert_eq!(ledger.entries()[0].1.account, account);
    }

    #[test]
    fn inverse_restores_previous_state() {
        let mut ledger = Ledger::default();
        let account = ledger.add_test_account("Cash");
        add(&mut ledger, account, "bills.rent");
        let (change, id) = add(&mut ledger, account, "dogs.health");
        add(&mut ledger, account, "bills.water");

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
        let account = ledger.add_test_account("Cash");
        add(&mut ledger, account, "bills.rent");
        add(&mut ledger, account, "food");
        let (id, current) = ledger.entries()[0].clone();

        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(id, &current, parsed("bills.water", "-5"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        assert_eq!(paths(&ledger), ["bills.water", "food"]);
        assert_eq!(ledger.entry(id).unwrap().amount, -500);

        ledger.apply(&mut MemoryStore, &change.inverse()).unwrap();
        assert_eq!(ledger.entry(id), Some(&current));
        assert!(ledger.categories().suggest("bills.").iter().all(|path| path != "bills.water"));
    }

    #[test]
    fn hides_entries_of_deleted_accounts() {
        let mut ledger = Ledger::default();
        let (cash, bank) = (ledger.add_test_account("Cash"), ledger.add_test_account("Bank"));
        add(&mut ledger, cash, "food");
        add(&mut ledger, bank, "bills");
        add(&mut ledger, cash, "dogs");
        assert_eq!(ledger.entry_count(cash), 2);

        let change = perform(&mut ledger, |builder| builder.delete_account(cash));
        assert_eq!(visible_paths(&ledger), ["bills"]);
        assert_eq!(paths(&ledger), ["food", "bills", "dogs"], "the entries are kept");
        assert_eq!(ledger.visible_entries().next_back().map(|(_, e)| e.account), Some(bank));
        assert!(ledger.accounts().get(cash).unwrap().deleted);

        ledger.apply(&mut MemoryStore, &change.inverse()).unwrap();
        assert_eq!(visible_paths(&ledger), ["food", "bills", "dogs"]);
    }

    #[test]
    fn deletes_account_with_entries_permanently() {
        let mut ledger = Ledger::default();
        let (cash, bank) = (ledger.add_test_account("Cash"), ledger.add_test_account("Bank"));
        add(&mut ledger, cash, "food");
        add(&mut ledger, bank, "bills");
        add(&mut ledger, cash, "dogs");
        let entries = ledger.entries().to_vec();

        let change = perform(&mut ledger, |builder| builder.delete_account_permanently(cash));
        assert_eq!(paths(&ledger), ["bills"]);
        assert_eq!(ledger.accounts().get(cash), None);
        assert_eq!(ledger.entry_count(cash), 0);

        ledger.apply(&mut MemoryStore, &change.inverse()).unwrap();
        assert_eq!(ledger.entries(), entries, "undo puts the entries back in place");
        assert!(ledger.accounts().get(cash).is_some());
    }
}
