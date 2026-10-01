use std::collections::BTreeMap;
use std::fmt;

use uuid::Uuid;

use crate::currency::Currency;

/// Identifies an account. UUIDv7, so ordering by id is ordering by creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccountId(pub Uuid);

impl AccountId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

/// Where money is kept, e.g. a bank account. Amounts of its entries are in its currency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub currency: Currency,
    /// In the trash: the account and its entries are hidden, but can be restored.
    pub deleted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountError {
    EmptyName,
    /// Another account, possibly a deleted one, has the same name.
    DuplicateName,
    /// The currency of an account with entries can't change, as their amounts are in it.
    CurrencyInUse,
}

impl fmt::Display for AccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccountError::EmptyName => write!(f, "name must not be empty"),
            AccountError::DuplicateName => write!(f, "another account has this name"),
            AccountError::CurrencyInUse => write!(f, "the currency of an account with entries can't be changed"),
        }
    }
}

impl std::error::Error for AccountError {}

/// All accounts, deleted ones included, in the order they were created.
///
/// Names are unique among all accounts, ignoring case, so restoring a deleted account never
/// conflicts with another one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Accounts {
    accounts: BTreeMap<AccountId, Account>,
}

impl Accounts {
    pub fn insert(&mut self, id: AccountId, account: Account) {
        self.accounts.insert(id, account);
    }

    pub fn get(&self, id: AccountId) -> Option<&Account> {
        self.accounts.get(&id)
    }

    pub fn replace(&mut self, id: AccountId, account: Account) {
        *self.accounts.get_mut(&id).expect("replaced account exists") = account;
    }

    /// Removes an account. Its entries must have been removed already.
    pub fn remove(&mut self, id: AccountId) {
        self.accounts.remove(&id);
    }

    /// All accounts, deleted ones included, in the order they were created.
    pub fn iter(&self) -> impl Iterator<Item = (AccountId, &Account)> {
        self.accounts.iter().map(|(&id, account)| (id, account))
    }

    /// Accounts that aren't deleted, in the order they were created.
    pub fn active(&self) -> impl Iterator<Item = (AccountId, &Account)> {
        self.iter().filter(|(_, account)| !account.deleted)
    }

    pub fn first_active(&self) -> Option<(AccountId, &Account)> {
        self.active().next()
    }

    /// The account that isn't deleted, if there's exactly one.
    pub fn only_active(&self) -> Option<(AccountId, &Account)> {
        let mut active = self.active();
        match (active.next(), active.next()) {
            (Some(only), None) => Some(only),
            _ => None,
        }
    }

    /// The account with this name, ignoring case and surrounding whitespace; deleted ones
    /// included.
    pub fn find(&self, name: &str) -> Option<AccountId> {
        let name = name.trim().to_lowercase();
        self.iter().find(|(_, account)| account.name.to_lowercase() == name).map(|(id, _)| id)
    }

    /// The account with this name, ignoring case and surrounding whitespace, unless it's
    /// deleted.
    pub fn find_active(&self, name: &str) -> Option<(AccountId, &Account)> {
        let id = self.find(name)?;
        self.get(id).filter(|account| !account.deleted).map(|account| (id, account))
    }

    /// Names of the accounts that aren't deleted and whose name contains the typed text,
    /// ignoring case and surrounding whitespace. Names starting with it come first, then
    /// alphabetically.
    pub fn suggest(&self, query: &str) -> Vec<&str> {
        let query = query.trim().to_lowercase();
        let mut matches: Vec<(bool, String, &str)> = self
            .active()
            .filter_map(|(_, account)| {
                let name = account.name.to_lowercase();
                name.contains(&query).then(|| (!name.starts_with(&query), name, account.name.as_str()))
            })
            .collect();
        matches.sort();
        matches.into_iter().map(|(.., name)| name).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(name: &str, deleted: bool) -> Account {
        Account { name: name.to_owned(), currency: Currency::parse("PLN").unwrap(), deleted }
    }

    /// Accounts with the given names and whether they're deleted, and their ids.
    fn accounts_named(names: &[(&str, bool)]) -> (Accounts, Vec<AccountId>) {
        let mut accounts = Accounts::default();
        let ids = names
            .iter()
            .map(|&(name, deleted)| {
                let id = AccountId::generate();
                accounts.insert(id, account(name, deleted));
                id
            })
            .collect();
        (accounts, ids)
    }

    #[test]
    fn lists_accounts_in_creation_order() {
        let (accounts, ids) = accounts_named(&[("Silver", false), ("Gold", true), ("Bronze", false)]);
        assert_eq!(accounts.iter().map(|(id, _)| id).collect::<Vec<_>>(), ids);
        let active: Vec<&str> = accounts.active().map(|(_, account)| account.name.as_str()).collect();
        assert_eq!(active, ["Silver", "Bronze"]);
    }

    #[test]
    fn first_active_skips_deleted_accounts() {
        let (accounts, ids) = accounts_named(&[("Silver", true), ("Gold", false), ("Bronze", false)]);
        assert_eq!(accounts.first_active().map(|(id, _)| id), Some(ids[1]));
        let (accounts, _) = accounts_named(&[("Silver", true)]);
        assert_eq!(accounts.first_active(), None);
    }

    #[test]
    fn finds_accounts_by_name_ignoring_case() {
        let (accounts, ids) = accounts_named(&[("Silver bank", false), ("Gold bank", true)]);
        assert_eq!(accounts.find("SILVER BANK"), Some(ids[0]));
        assert_eq!(accounts.find(" gold bank "), Some(ids[1]), "deleted accounts are found too");
        assert_eq!(accounts.find("Bronze bank"), None);
    }

    #[test]
    fn finds_only_active_accounts_by_name() {
        let (accounts, ids) = accounts_named(&[("Silver bank", false), ("Gold bank", true)]);
        assert_eq!(accounts.find_active(" silver BANK ").map(|(id, _)| id), Some(ids[0]));
        assert_eq!(accounts.find_active("Gold bank"), None, "deleted");
        assert_eq!(accounts.find_active("Bronze bank"), None);
    }

    #[test]
    fn only_active_requires_exactly_one() {
        let (accounts, ids) = accounts_named(&[("Silver", true), ("Gold", false)]);
        assert_eq!(accounts.only_active().map(|(id, _)| id), Some(ids[1]));
        let (accounts, _) = accounts_named(&[("Silver", false), ("Gold", false)]);
        assert_eq!(accounts.only_active(), None);
        let (accounts, _) = accounts_named(&[("Silver", true)]);
        assert_eq!(accounts.only_active(), None);
    }

    #[test]
    fn suggests_active_accounts_prefix_matches_first() {
        let (accounts, _) = accounts_named(&[
            ("Silver bank", false),
            ("bank of fiction", false),
            ("Gold bank", false),
            ("Bronze bank", true),
            ("Cash", false),
            ("Banknotes", false),
        ]);
        assert_eq!(accounts.suggest(""), ["bank of fiction", "Banknotes", "Cash", "Gold bank", "Silver bank"]);
        assert_eq!(accounts.suggest(" BANK"), ["bank of fiction", "Banknotes", "Gold bank", "Silver bank"]);
        assert_eq!(accounts.suggest("s"), ["Silver bank", "Banknotes", "Cash"]);
        assert!(accounts.suggest("bronze").is_empty(), "deleted accounts aren't suggested");
    }

    #[test]
    fn replaces_and_removes_accounts() {
        let (mut accounts, ids) = accounts_named(&[("Silver", false), ("Gold", false)]);
        accounts.replace(ids[0], account("Platinum", true));
        assert_eq!(accounts.get(ids[0]), Some(&account("Platinum", true)));
        accounts.remove(ids[1]);
        assert_eq!(accounts.get(ids[1]), None);
        assert_eq!(accounts.iter().count(), 1);
    }
}
