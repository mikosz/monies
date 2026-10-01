use crate::account::{Account, AccountError, AccountId, Accounts};
use crate::category::{Categories, CategoryId, CategoryPath};
use crate::currency::Currency;
use crate::entry::{Entry, EntryId, ParsedEntry};
use crate::ledger::Ledger;

/// A primitive modification of the ledger. Every operation carries enough data to be
/// reversed, see [`Op::inverse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    InsertAccount { id: AccountId, account: Account },
    /// Also moves an account to the trash and back, see [`Account::deleted`].
    UpdateAccount { id: AccountId, before: Account, after: Account },
    DeleteAccount { id: AccountId, account: Account },
    InsertCategory { id: CategoryId, name: String, parent: Option<CategoryId> },
    DeleteCategory { id: CategoryId, name: String, parent: Option<CategoryId> },
    InsertEntry { id: EntryId, entry: Entry },
    DeleteEntry { id: EntryId, entry: Entry },
    UpdateEntry { id: EntryId, before: Entry, after: Entry },
}

impl Op {
    pub fn inverse(&self) -> Op {
        match self.clone() {
            Op::InsertAccount { id, account } => Op::DeleteAccount { id, account },
            Op::UpdateAccount { id, before, after } => Op::UpdateAccount { id, before: after, after: before },
            Op::DeleteAccount { id, account } => Op::InsertAccount { id, account },
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

/// Builds a [`Change`] against the ledger, validating it and creating missing categories.
/// Categories and accounts created or changed earlier in the same change are taken into
/// account, so e.g. importing many entries into a new category creates it once. Entries are
/// always those of the ledger, without the ones added by the change.
pub struct ChangeBuilder<'a> {
    description: String,
    ledger: &'a Ledger,
    /// The current accounts as changed by this change so far.
    accounts: Accounts,
    /// The current categories plus those created by this change so far.
    categories: Categories,
    ops: Vec<Op>,
}

impl<'a> ChangeBuilder<'a> {
    pub fn new(description: impl Into<String>, ledger: &'a Ledger) -> Self {
        Self {
            description: description.into(),
            ledger,
            accounts: ledger.accounts().clone(),
            categories: ledger.categories().clone(),
            ops: Vec::new(),
        }
    }

    /// Adds an account with a name not used by any other account, deleted ones included.
    pub fn add_account(&mut self, name: &str, currency: Currency) -> Result<AccountId, AccountError> {
        let id = AccountId::generate();
        let account = Account { name: self.account_name(name, id)?, currency, deleted: false };
        self.accounts.insert(id, account.clone());
        self.ops.push(Op::InsertAccount { id, account });
        Ok(id)
    }

    /// Renames the account or changes its currency; the latter only while it has no entries.
    /// Records nothing when neither changes. Panics if there's no such account.
    pub fn update_account(&mut self, id: AccountId, name: &str, currency: Currency) -> Result<(), AccountError> {
        let current = self.account(id);
        let name = self.account_name(name, id)?;
        if currency != current.currency && self.ledger.account_entries(id).next().is_some() {
            return Err(AccountError::CurrencyInUse);
        }
        self.set_account(id, current.clone(), Account { name, currency, ..current });
        Ok(())
    }

    /// Moves the account to the trash. Panics if there's no such account.
    pub fn delete_account(&mut self, id: AccountId) {
        let current = self.account(id);
        self.set_account(id, current.clone(), Account { deleted: true, ..current });
    }

    /// Brings the account back from the trash. Panics if there's no such account.
    pub fn restore_account(&mut self, id: AccountId) {
        let current = self.account(id);
        self.set_account(id, current.clone(), Account { deleted: false, ..current });
    }

    /// Deletes the account and all its entries. Categories are kept, even if no longer used.
    /// Panics if there's no such account.
    pub fn delete_account_permanently(&mut self, id: AccountId) {
        let account = self.account(id);
        let ledger = self.ledger;
        for (entry_id, entry) in ledger.account_entries(id) {
            self.ops.push(Op::DeleteEntry { id: *entry_id, entry: entry.clone() });
        }
        self.accounts.remove(id);
        self.ops.push(Op::DeleteAccount { id, account });
    }

    /// Adds an entry to `parsed.account`.
    pub fn add_entry(&mut self, parsed: ParsedEntry) -> EntryId {
        let category = self.category(&parsed.category);
        let id = EntryId::generate();
        let entry =
            Entry { account: parsed.account, date: parsed.date, name: parsed.name, category, amount: parsed.amount };
        self.ops.push(Op::InsertEntry { id, entry });
        id
    }

    /// Replaces the entry `id`, currently `current`, with the parsed input. Records nothing
    /// when the input doesn't change the entry. A different account moves the entry there;
    /// the amount is then taken as it is, in the other account's currency.
    pub fn update_entry(&mut self, id: EntryId, current: &Entry, parsed: ParsedEntry) {
        // An unchanged entry keeps its existing category, so no categories are created then.
        let category = self.category(&parsed.category);
        let after =
            Entry { account: parsed.account, date: parsed.date, name: parsed.name, category, amount: parsed.amount };
        if after != *current {
            self.ops.push(Op::UpdateEntry { id, before: current.clone(), after });
        }
    }

    pub fn build(self) -> Change {
        Change { description: self.description, ops: self.ops }
    }

    fn account(&self, id: AccountId) -> Account {
        self.accounts.get(id).expect("changed account exists").clone()
    }

    /// The trimmed name, if it isn't empty and no account other than `id` has it.
    fn account_name(&self, name: &str, id: AccountId) -> Result<String, AccountError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AccountError::EmptyName);
        }
        match self.accounts.find(name) {
            Some(other) if other != id => Err(AccountError::DuplicateName),
            _ => Ok(name.to_owned()),
        }
    }

    fn set_account(&mut self, id: AccountId, before: Account, after: Account) {
        if after != before {
            self.accounts.replace(id, after.clone());
            self.ops.push(Op::UpdateAccount { id, before, after });
        }
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
    use crate::store::MemoryStore;

    fn parsed(account: AccountId, category: &str) -> ParsedEntry {
        ParsedEntry::test(account, "2026-09-30", "Rent", category, "1")
    }

    fn ledger_with(categories: Categories) -> Ledger {
        Ledger::new(Accounts::default(), categories, Vec::new())
    }

    fn currency(code: &str) -> Currency {
        Currency::parse(code).unwrap()
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
        let account = AccountId::generate();
        let ledger = Ledger::default();
        let mut builder = ChangeBuilder::new("Add", &ledger);
        builder.add_entry(parsed(account, "bills.rent"));
        let change = builder.build();

        assert_eq!(inserted_categories(&change), ["bills", "rent"]);
        let [Op::InsertCategory { id: bills, parent: None, .. }, Op::InsertCategory { id: rent, parent, .. }, Op::InsertEntry { entry, .. }] =
            change.ops.as_slice()
        else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!(*parent, Some(*bills));
        assert_eq!(entry.category, *rent);
        assert_eq!(entry.account, account);
    }

    #[test]
    fn reuses_existing_and_newly_created_categories() {
        let mut existing = Categories::default();
        let bills = CategoryId::generate();
        existing.insert(bills, "Bills".to_owned(), None);
        let ledger = ledger_with(existing);
        let account = AccountId::generate();

        let mut builder = ChangeBuilder::new("Import", &ledger);
        builder.add_entry(parsed(account, "bills.rent"));
        builder.add_entry(parsed(account, "BILLS.Rent"));
        builder.add_entry(parsed(account, "bills"));
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
        let rent = CategoryPath::parse("bills.rent").unwrap();
        assert_eq!(ledger.categories().find(&rent), None, "the ledger's categories are untouched");
    }

    #[test]
    fn entry_ids_follow_creation_order() {
        let (ledger, account) = (Ledger::default(), AccountId::generate());
        let mut builder = ChangeBuilder::new("Import", &ledger);
        let ids: Vec<EntryId> = (0..100).map(|_| builder.add_entry(parsed(account, "bills"))).collect();
        assert!(ids.is_sorted());
    }

    #[test]
    fn inverse_reverses_and_inverts_operations() {
        let ledger = Ledger::default();
        let mut builder = ChangeBuilder::new("Add", &ledger);
        builder.add_entry(parsed(AccountId::generate(), "bills.rent"));
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
        let date = chrono::NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();
        Entry { account: AccountId::generate(), date, name: "Rent".to_owned(), category, amount }
    }

    #[test]
    fn updates_entry_into_new_category() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "bills".to_owned(), None);
        let current = entry(&categories, "bills", 100);
        let ledger = ledger_with(categories);

        let id = EntryId::generate();
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(id, &current, parsed(current.account, "bills.rent"));
        let change = builder.build();

        assert_eq!(inserted_categories(&change), ["rent"]);
        let [_, Op::UpdateEntry { id: updated, before, after }] = change.ops.as_slice() else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!((*updated, before), (id, &current));
        assert_ne!(after.category, current.category);
        assert_eq!(after.amount, 100);
        assert_eq!(after.account, current.account, "the entry stays in its account");
    }

    #[test]
    fn update_moves_entry_to_another_account() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "bills".to_owned(), None);
        let current = entry(&categories, "bills", 100);
        let ledger = ledger_with(categories);

        let (id, other) = (EntryId::generate(), AccountId::generate());
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(id, &current, parsed(other, "bills"));
        let change = builder.build();

        let after = Entry { account: other, ..current.clone() };
        assert_eq!(change.ops, [Op::UpdateEntry { id, before: current, after }]);
    }

    #[test]
    fn unchanged_update_records_nothing() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "Bills".to_owned(), None);
        let current = entry(&categories, "bills", 100);
        let ledger = ledger_with(categories);

        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(EntryId::generate(), &current, parsed(current.account, "BILLS"));
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

    /// A ledger with the accounts "Cash" (with an entry) and "Bank" (without), both in PLN.
    fn ledger_with_accounts() -> (Ledger, AccountId, AccountId) {
        let mut ledger = Ledger::default();
        let (cash, bank) = (ledger.add_test_account("Cash"), ledger.add_test_account("Bank"));
        let mut builder = ChangeBuilder::new("Add", &ledger);
        builder.add_entry(parsed(cash, "bills"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        (ledger, cash, bank)
    }

    #[test]
    fn adds_accounts_with_trimmed_unique_names() {
        let (ledger, ..) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Add account", &ledger);
        let id = builder.add_account(" Savings ", currency("EUR")).unwrap();
        assert_eq!(builder.add_account("  ", currency("EUR")), Err(AccountError::EmptyName));
        assert_eq!(builder.add_account("CASH", currency("EUR")), Err(AccountError::DuplicateName));
        assert_eq!(builder.add_account("savings", currency("EUR")), Err(AccountError::DuplicateName), "added by this change");

        let change = builder.build();
        let account = Account { name: "Savings".to_owned(), currency: currency("EUR"), deleted: false };
        assert_eq!(change.ops, [Op::InsertAccount { id, account }]);
    }

    #[test]
    fn names_of_deleted_accounts_stay_taken() {
        let (mut ledger, cash, _) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Delete", &ledger);
        builder.delete_account(cash);
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let mut builder = ChangeBuilder::new("Add account", &ledger);
        assert_eq!(builder.add_account("Cash", currency("PLN")), Err(AccountError::DuplicateName));
    }

    #[test]
    fn renames_accounts() {
        let (ledger, cash, bank) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Rename", &ledger);
        assert_eq!(builder.update_account(cash, "bank", currency("PLN")), Err(AccountError::DuplicateName));
        assert_eq!(builder.update_account(cash, "", currency("PLN")), Err(AccountError::EmptyName));
        builder.update_account(bank, "BANK ", currency("PLN")).unwrap();
        let change = builder.build();

        let [Op::UpdateAccount { id, before, after }] = change.ops.as_slice() else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!((*id, before.name.as_str(), after.name.as_str()), (bank, "Bank", "BANK"), "case can change");
    }

    #[test]
    fn unchanged_account_update_records_nothing() {
        let (ledger, cash, _) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_account(cash, " Cash ", currency("pln")).unwrap();
        assert!(builder.build().is_empty());
    }

    #[test]
    fn currency_changes_only_without_entries() {
        let (ledger, cash, bank) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        assert_eq!(builder.update_account(cash, "Cash", currency("EUR")), Err(AccountError::CurrencyInUse));
        builder.update_account(bank, "Bank", currency("EUR")).unwrap();
        let change = builder.build();

        let [Op::UpdateAccount { after, .. }] = change.ops.as_slice() else { panic!("unexpected ops: {:?}", change.ops) };
        assert_eq!(after.currency, currency("EUR"));
    }

    #[test]
    fn deletes_and_restores_accounts() {
        let (ledger, cash, _) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Delete", &ledger);
        builder.delete_account(cash);
        builder.delete_account(cash);
        let change = builder.build();
        let [Op::UpdateAccount { id, before, after }] = change.ops.as_slice() else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!((*id, before.deleted, after.deleted), (cash, false, true), "deleting twice records it once");

        let mut builder = ChangeBuilder::new("Restore", &ledger);
        builder.restore_account(cash);
        assert!(builder.build().is_empty(), "the account isn't deleted");
    }

    #[test]
    fn deleting_account_permanently_deletes_its_entries_first() {
        let (mut ledger, cash, bank) = ledger_with_accounts();
        let mut builder = ChangeBuilder::new("Add", &ledger);
        builder.add_entry(parsed(bank, "food"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let mut builder = ChangeBuilder::new("Delete permanently", &ledger);
        builder.delete_account_permanently(cash);
        let change = builder.build();
        let (entry_id, entry) = ledger.entries()[0].clone();
        let account = ledger.accounts().get(cash).unwrap().clone();
        assert_eq!(
            change.ops,
            [Op::DeleteEntry { id: entry_id, entry }, Op::DeleteAccount { id: cash, account }],
            "only the account's entries, no categories"
        );
    }

    #[test]
    fn account_inverses() {
        let id = AccountId::generate();
        let account = Account { name: "Cash".to_owned(), currency: currency("PLN"), deleted: false };
        let deleted = Account { deleted: true, ..account.clone() };

        let insert = Op::InsertAccount { id, account: account.clone() };
        assert_eq!(insert.inverse(), Op::DeleteAccount { id, account: account.clone() });
        assert_eq!(insert.inverse().inverse(), insert);
        let update = Op::UpdateAccount { id, before: account.clone(), after: deleted.clone() };
        assert_eq!(update.inverse(), Op::UpdateAccount { id, before: deleted, after: account });
    }
}
