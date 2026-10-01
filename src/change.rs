use std::collections::BTreeMap;

use chrono::Utc;

use crate::account::{Account, AccountError, AccountId, Accounts};
use crate::category::{Categories, CategoryId, CategoryPath};
use crate::currency::Currency;
use crate::entry::{Entry, EntryId, ParsedEntry};
use crate::import::{Import, ImportError, ImportId, ImportRow, RowStatus};
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
    /// Inserts a pending import together with its rows.
    InsertImport { id: ImportId, import: Import },
    /// Deletes a pending import together with its rows.
    DeleteImport { id: ImportId, import: Import },
    /// Replaces the row at `position` of the import's rows.
    UpdateImportRow { id: ImportId, position: usize, before: ImportRow, after: ImportRow },
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
            Op::InsertImport { id, import } => Op::DeleteImport { id, import },
            Op::DeleteImport { id, import } => Op::InsertImport { id, import },
            Op::UpdateImportRow { id, position, before, after } => {
                Op::UpdateImportRow { id, position, before: after, after: before }
            }
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
/// Categories, accounts and pending imports created or changed earlier in the same change are
/// taken into account, so e.g. importing many entries into a new category creates it once.
/// Entries are always those of the ledger, without the ones added by the change.
pub struct ChangeBuilder<'a> {
    description: String,
    ledger: &'a Ledger,
    /// The current accounts as changed by this change so far.
    accounts: Accounts,
    /// The current categories plus those created by this change so far.
    categories: Categories,
    /// The current pending imports as changed by this change so far.
    imports: BTreeMap<ImportId, Import>,
    ops: Vec<Op>,
}

impl<'a> ChangeBuilder<'a> {
    pub fn new(description: impl Into<String>, ledger: &'a Ledger) -> Self {
        Self {
            description: description.into(),
            ledger,
            accounts: ledger.accounts().clone(),
            categories: ledger.categories().clone(),
            imports: ledger.imports().map(|(id, import)| (id, import.clone())).collect(),
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

    /// Deletes the account with all its entries and pending imports. Categories are kept, even
    /// if no longer used. Panics if there's no such account.
    pub fn delete_account_permanently(&mut self, id: AccountId) {
        let account = self.account(id);
        let ledger = self.ledger;
        for (entry_id, entry) in ledger.account_entries(id) {
            self.ops.push(Op::DeleteEntry { id: *entry_id, entry: entry.clone() });
        }
        let imports: Vec<ImportId> =
            self.imports.iter().filter(|(_, import)| import.account == id).map(|(&import_id, _)| import_id).collect();
        for import_id in imports {
            self.discard_import(import_id);
        }
        self.accounts.remove(id);
        self.ops.push(Op::DeleteAccount { id, account });
    }

    /// Adds an entry to `parsed.account`, typed in rather than imported.
    pub fn add_entry(&mut self, parsed: ParsedEntry) -> EntryId {
        let category = self.category(&parsed.category);
        let id = EntryId::generate();
        let entry = Entry {
            account: parsed.account,
            date: parsed.date,
            name: parsed.name,
            category,
            amount: parsed.amount,
            statement: None,
        };
        self.ops.push(Op::InsertEntry { id, entry });
        id
    }

    /// Replaces the entry `id`, currently `current`, with the parsed input. Records nothing
    /// when the input doesn't change the entry. A different account moves the entry there;
    /// the amount is then taken as it is, in the other account's currency. An imported entry
    /// keeps its statement line.
    pub fn update_entry(&mut self, id: EntryId, current: &Entry, parsed: ParsedEntry) {
        // An unchanged entry keeps its existing category, so no categories are created then.
        let category = self.category(&parsed.category);
        let after = Entry {
            account: parsed.account,
            date: parsed.date,
            name: parsed.name,
            category,
            amount: parsed.amount,
            statement: current.statement.clone(),
        };
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

/// Pending imports, reviewed row by row and then submitted or discarded.
#[cfg_attr(not(test), expect(dead_code, reason = "used by the import UI, a later step"))]
impl ChangeBuilder<'_> {
    /// Adds a pending import of statement lines into the account. Panics if there's no such
    /// account.
    pub fn add_import(&mut self, account: AccountId, source: &str, rows: Vec<ImportRow>) -> ImportId {
        assert!(self.accounts.get(account).is_some(), "imported account exists");
        let id = ImportId::generate();
        let import = Import { account, created: Utc::now(), source: source.to_owned(), rows };
        self.imports.insert(id, import.clone());
        self.ops.push(Op::InsertImport { id, import });
        id
    }

    /// Replaces the row at `position` of the import. Records nothing when the row doesn't
    /// change. Panics if there's no such import or row.
    pub fn update_import_row(&mut self, id: ImportId, position: usize, row: ImportRow) {
        let current = &mut self.imports.get_mut(&id).expect("changed import exists").rows[position];
        if row != *current {
            let before = std::mem::replace(current, row.clone());
            self.ops.push(Op::UpdateImportRow { id, position, before, after: row });
        }
    }

    /// Drops the import without adding any entries. Panics if there's no such import.
    pub fn discard_import(&mut self, id: ImportId) {
        let import = self.imports.remove(&id).expect("discarded import exists");
        self.ops.push(Op::DeleteImport { id, import });
    }

    /// Adds an entry to the import's account for every accepted row, in row order, and drops
    /// the import with the rows that weren't accepted. Entries have the date and amount of
    /// their statement line and keep the line; missing categories are created. Returns the
    /// ids of the added entries. Records nothing when an accepted row has an empty name or no
    /// category. Panics if there's no such import.
    pub fn submit_import(&mut self, id: ImportId) -> Result<Vec<EntryId>, ImportError> {
        let import = self.imports.get(&id).expect("submitted import exists");
        let account = import.account;
        let accepted = import
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.status == RowStatus::Accepted)
            .map(|(position, row)| {
                let name = row.name.trim();
                if name.is_empty() {
                    return Err(ImportError::EmptyName { position });
                }
                let category = row.category.clone().ok_or(ImportError::MissingCategory { position })?;
                Ok((name.to_owned(), category, row.line.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut entries = Vec::with_capacity(accepted.len());
        for (name, category, line) in accepted {
            let category = self.category(&category);
            let entry_id = EntryId::generate();
            let entry = Entry { account, date: line.date, name, category, amount: line.amount, statement: Some(line) };
            self.ops.push(Op::InsertEntry { id: entry_id, entry });
            entries.push(entry_id);
        }
        self.discard_import(id);
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::StatementLine;
    use crate::store::MemoryStore;

    fn parsed(account: AccountId, category: &str) -> ParsedEntry {
        ParsedEntry::test(account, "2026-09-30", "Rent", category, "1")
    }

    fn ledger_with(categories: Categories) -> Ledger {
        Ledger::new(Accounts::default(), categories, Vec::new(), Vec::new())
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
        Entry { account: AccountId::generate(), date, name: "Rent".to_owned(), category, amount, statement: None }
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

    /// Adds an import of `rows` into `account` to the ledger.
    fn add_import(ledger: &mut Ledger, account: AccountId, rows: Vec<ImportRow>) -> ImportId {
        let mut builder = ChangeBuilder::new("Import", ledger);
        let id = builder.add_import(account, "statement-2026-09.csv", rows);
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        id
    }

    fn row(text: &str, name: &str, category: &str, status: RowStatus) -> ImportRow {
        ImportRow::test("2026-09-30", 1250, text, name, category, status)
    }

    #[test]
    fn adds_imports() {
        let (ledger, _, bank) = ledger_with_accounts();
        let rows = vec![row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending)];
        let mut builder = ChangeBuilder::new("Import", &ledger);
        let id = builder.add_import(bank, "Claude", rows.clone());
        let change = builder.build();

        let [Op::InsertImport { id: inserted, import }] = change.ops.as_slice() else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!(*inserted, id);
        assert_eq!((import.account, import.source.as_str(), &import.rows), (bank, "Claude", &rows));
    }

    #[test]
    fn submits_accepted_rows_in_order_into_the_imports_account() {
        let (mut ledger, _, bank) = ledger_with_accounts();
        let rows = vec![
            row("CARD PAYMENT CORNER SHOP 0042", " Groceries ", "food.shop", RowStatus::Accepted),
            row("CARD PAYMENT BAKERY 0007", "Bread", "food.bakery", RowStatus::Pending),
            row("TRANSFER FLAT 12", "Rent", "BILLS", RowStatus::Accepted),
            row("CARD PAYMENT CORNER SHOP 0043", "Snacks", "Food.Shop", RowStatus::Accepted),
            row("CARD PAYMENT CORNER SHOP 0042", "Groceries", "food.shop", RowStatus::Skipped),
        ];
        let id = add_import(&mut ledger, bank, rows.clone());
        let import = ledger.import(id).unwrap().clone();

        let mut builder = ChangeBuilder::new("Submit", &ledger);
        let entry_ids = builder.submit_import(id).unwrap();
        let change = builder.build();

        assert_eq!(inserted_categories(&change), ["food", "shop"], "created once, the existing bills reused");
        let entries: Vec<(EntryId, &Entry)> = change
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::InsertEntry { id, entry } => Some((*id, entry)),
                _ => None,
            })
            .collect();
        assert_eq!(entries.iter().map(|(id, _)| *id).collect::<Vec<_>>(), entry_ids);
        let names: Vec<&str> = entries.iter().map(|(_, entry)| entry.name.as_str()).collect();
        assert_eq!(names, ["Groceries", "Rent", "Snacks"], "accepted rows in row order, names trimmed");
        let statements: Vec<Option<&StatementLine>> = entries.iter().map(|(_, entry)| entry.statement.as_ref()).collect();
        assert_eq!(statements, [Some(&rows[0].line), Some(&rows[2].line), Some(&rows[3].line)]);
        assert!(entries.iter().all(|(_, entry)| entry.account == bank && entry.date == rows[0].line.date && entry.amount == 1250));
        assert_eq!(entries[0].1.category, entries[2].1.category);
        assert_eq!(change.ops.last(), Some(&Op::DeleteImport { id, import }), "the import is dropped last");
    }

    #[test]
    fn submit_refuses_accepted_rows_without_name_or_category() {
        let (mut ledger, cash, _) = ledger_with_accounts();
        let unnamed = add_import(&mut ledger, cash, vec![
            row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending),
            row("CARD PAYMENT BAKERY 0007", "Bread", "food", RowStatus::Accepted),
            row("TRANSFER FLAT 12", "  ", "bills", RowStatus::Accepted),
        ]);
        let uncategorised = add_import(&mut ledger, cash, vec![
            row("CARD PAYMENT CORNER SHOP 0042", "Groceries", "", RowStatus::Skipped),
            row("CARD PAYMENT BAKERY 0007", "Bread", "", RowStatus::Accepted),
        ]);

        let mut builder = ChangeBuilder::new("Submit", &ledger);
        assert_eq!(builder.submit_import(unnamed), Err(ImportError::EmptyName { position: 2 }));
        assert_eq!(builder.submit_import(uncategorised), Err(ImportError::MissingCategory { position: 1 }));
        assert!(builder.build().is_empty(), "no categories are created either");
        assert_eq!(ImportError::EmptyName { position: 2 }.to_string(), "row 3: name must not be empty");
    }

    #[test]
    fn import_row_updates_build_on_each_other() {
        let (mut ledger, cash, _) = ledger_with_accounts();
        let pending = row("CARD PAYMENT CORNER SHOP 0042", "", "", RowStatus::Pending);
        let id = add_import(&mut ledger, cash, vec![pending.clone()]);
        let named = ImportRow { name: "Groceries".to_owned(), ..pending.clone() };
        let accepted = ImportRow { status: RowStatus::Accepted, ..named.clone() };

        let mut builder = ChangeBuilder::new("Review", &ledger);
        builder.update_import_row(id, 0, pending.clone());
        builder.update_import_row(id, 0, named.clone());
        builder.update_import_row(id, 0, named.clone());
        builder.update_import_row(id, 0, accepted.clone());
        let change = builder.build();

        assert_eq!(
            change.ops,
            [
                Op::UpdateImportRow { id, position: 0, before: pending, after: named.clone() },
                Op::UpdateImportRow { id, position: 0, before: named, after: accepted },
            ],
            "unchanged rows record nothing"
        );
    }

    #[test]
    fn update_keeps_statement_line() {
        let mut categories = Categories::default();
        categories.insert(CategoryId::generate(), "bills".to_owned(), None);
        let line = row("TRANSFER FLAT 12", "Rent", "bills", RowStatus::Accepted).line;
        let current = Entry { statement: Some(line), ..entry(&categories, "bills", 100) };
        let ledger = ledger_with(categories);

        let id = EntryId::generate();
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(id, &current, ParsedEntry::test(current.account, "2026-09-30", "Rent", "bills", "1"));
        builder.update_entry(id, &current, ParsedEntry::test(current.account, "2026-10-01", "Flat", "bills", "2"));
        let change = builder.build();

        let [Op::UpdateEntry { after, .. }] = change.ops.as_slice() else {
            panic!("unexpected ops: {:?}", change.ops)
        };
        assert_eq!((after.name.as_str(), after.amount), ("Flat", 200));
        assert_eq!(after.statement, current.statement);
    }

    #[test]
    fn deleting_account_permanently_deletes_its_imports() {
        let (mut ledger, cash, bank) = ledger_with_accounts();
        let import_id = add_import(&mut ledger, cash, vec![row("TRANSFER FLAT 12", "Rent", "bills", RowStatus::Accepted)]);
        add_import(&mut ledger, bank, vec![row("CARD PAYMENT BAKERY 0007", "Bread", "food", RowStatus::Pending)]);

        let mut builder = ChangeBuilder::new("Delete permanently", &ledger);
        builder.delete_account_permanently(cash);
        let change = builder.build();
        let (entry_id, entry) = ledger.entries()[0].clone();
        let import = ledger.import(import_id).unwrap().clone();
        let account = ledger.accounts().get(cash).unwrap().clone();
        assert_eq!(
            change.ops,
            [
                Op::DeleteEntry { id: entry_id, entry },
                Op::DeleteImport { id: import_id, import },
                Op::DeleteAccount { id: cash, account }
            ],
            "only the account's imports"
        );
    }

    #[test]
    fn import_inverses() {
        let (id, rows) = (ImportId::generate(), vec![row("TRANSFER FLAT 12", "Rent", "bills", RowStatus::Pending)]);
        let import = Import { account: AccountId::generate(), created: Utc::now(), source: "Claude".to_owned(), rows };
        let (before, after) = (import.rows[0].clone(), ImportRow { status: RowStatus::Skipped, ..import.rows[0].clone() });

        let insert = Op::InsertImport { id, import: import.clone() };
        assert_eq!(insert.inverse(), Op::DeleteImport { id, import });
        assert_eq!(insert.inverse().inverse(), insert);
        let update = Op::UpdateImportRow { id, position: 0, before: before.clone(), after: after.clone() };
        assert_eq!(update.inverse(), Op::UpdateImportRow { id, position: 0, before: after, after: before });
    }
}
