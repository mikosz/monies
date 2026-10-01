use std::fmt;

use chrono::NaiveDate;
use uuid::Uuid;

use crate::account::{AccountId, Accounts};
use crate::category::{CategoryId, CategoryPath};
use crate::date_format::DateFormat;

/// Identifies an entry. UUIDv7, so ordering by id is ordering by creation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId(pub Uuid);

impl EntryId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub account: AccountId,
    pub date: NaiveDate,
    pub name: String,
    pub category: CategoryId,
    /// Amount in minor units of the account's currency (e.g. cents): positive for expenses,
    /// negative for income.
    pub amount: i64,
}

/// Whether an entry is money spent or received.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Expense,
    Income,
    /// A zero amount.
    Neutral,
}

impl Entry {
    pub fn kind(&self) -> EntryKind {
        match self.amount.cmp(&0) {
            std::cmp::Ordering::Greater => EntryKind::Expense,
            std::cmp::Ordering::Less => EntryKind::Income,
            std::cmp::Ordering::Equal => EntryKind::Neutral,
        }
    }
}

/// Validated user input for an entry whose category path hasn't been resolved yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEntry {
    pub account: AccountId,
    pub date: NaiveDate,
    pub name: String,
    pub category: CategoryPath,
    /// Amount in minor units of the account's currency (e.g. cents). Negative values are
    /// allowed.
    pub amount: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryError {
    /// No account that isn't deleted has the name.
    UnknownAccount,
    InvalidDate,
    EmptyName,
    EmptyCategory,
    InvalidAmount,
}

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryError::UnknownAccount => write!(f, "there's no account with this name"),
            EntryError::InvalidDate => write!(f, "date is not in the expected format"),
            EntryError::EmptyName => write!(f, "name must not be empty"),
            EntryError::EmptyCategory => write!(f, "category must not be empty"),
            EntryError::InvalidAmount => {
                write!(f, "amount must be a number with at most as many decimal places as its currency has")
            }
        }
    }
}

impl std::error::Error for EntryError {}

impl ParsedEntry {
    /// Validates raw user input. Surrounding whitespace is ignored.
    ///
    /// The account is one of `accounts` that isn't deleted, named ignoring case. With only
    /// one such account, its name may be left empty. The amount is in that account's currency.
    pub fn parse(
        date_format: &DateFormat,
        accounts: &Accounts,
        account: &str,
        date: &str,
        name: &str,
        category: &str,
        amount: &str,
    ) -> Result<Self, EntryError> {
        let (account, currency) = match account.trim() {
            "" => accounts.only_active(),
            name => accounts.find_active(name),
        }
        .map(|(id, account)| (id, account.currency))
        .ok_or(EntryError::UnknownAccount)?;

        let date = date_format.parse(date).ok_or(EntryError::InvalidDate)?;

        let name = name.trim();
        if name.is_empty() {
            return Err(EntryError::EmptyName);
        }

        let category = CategoryPath::parse(category).ok_or(EntryError::EmptyCategory)?;
        let amount = parse_amount(amount, currency.decimals())?;

        Ok(Self {
            account,
            date,
            name: name.to_owned(),
            category,
            amount,
        })
    }
}

#[cfg(test)]
impl ParsedEntry {
    /// Input for an entry of `account` as if it had been parsed, for tests that don't need
    /// the account to exist: the date is ISO `YYYY-MM-DD` and the amount has two decimal
    /// places. Panics on invalid input.
    pub fn test(account: AccountId, date: &str, name: &str, category: &str, amount: &str) -> Self {
        Self {
            account,
            date: DateFormat::iso().parse(date).expect("valid date"),
            name: name.to_owned(),
            category: CategoryPath::parse(category).expect("valid category"),
            amount: parse_amount(amount, 2).expect("valid amount"),
        }
    }
}

/// Parses a decimal amount such as `12`, `-12.5` or `+12.50` into minor units, of which there
/// are `10^decimals` in a unit (e.g. 100 cents for 2). At most `decimals` decimal places are
/// allowed.
pub fn parse_amount(input: &str, decimals: u32) -> Result<i64, EntryError> {
    let input = input.trim();
    let (negative, unsigned) = match input.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, input.strip_prefix('+').unwrap_or(input)),
    };
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };

    let is_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !is_digits(whole) {
        return Err(EntryError::InvalidAmount);
    }

    let minor = match fraction {
        None => 0,
        Some(fraction) if is_digits(fraction) && fraction.len() <= decimals as usize => {
            let value: i64 = fraction.parse().map_err(|_| EntryError::InvalidAmount)?;
            // Pad to `decimals` places, e.g. `.5` is 50 cents.
            value * 10_i64.pow(decimals - fraction.len() as u32)
        }
        Some(_) => return Err(EntryError::InvalidAmount),
    };

    let whole: i64 = whole.parse().map_err(|_| EntryError::InvalidAmount)?;
    let total = whole
        .checked_mul(10_i64.pow(decimals))
        .and_then(|whole| whole.checked_add(minor))
        .ok_or(EntryError::InvalidAmount)?;

    Ok(if negative { -total } else { total })
}

/// Formats minor units as a decimal amount with exactly `decimals` decimal places.
pub fn format_amount(amount: i64, decimals: u32) -> String {
    let sign = if amount < 0 { "-" } else { "" };
    let abs = amount.unsigned_abs();
    if decimals == 0 {
        return format!("{sign}{abs}");
    }
    let scale = 10_u64.pow(decimals);
    format!("{sign}{}.{:0width$}", abs / scale, abs % scale, width = decimals as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;
    use crate::currency::Currency;

    /// Accounts with the given names, currencies and whether they're deleted, and their ids.
    fn accounts_named(accounts: &[(&str, &str, bool)]) -> (Accounts, Vec<AccountId>) {
        let mut result = Accounts::default();
        let ids = accounts
            .iter()
            .map(|&(name, currency, deleted)| {
                let id = AccountId::generate();
                let currency = Currency::parse(currency).unwrap();
                result.insert(id, Account { name: name.to_owned(), currency, deleted });
                id
            })
            .collect();
        (result, ids)
    }

    /// Parses with a single account in PLN, implied by an empty account name.
    fn parse(date: &str, name: &str, category: &str, amount: &str) -> Result<ParsedEntry, EntryError> {
        let (accounts, _) = accounts_named(&[("Cash", "PLN", false)]);
        ParsedEntry::parse(&DateFormat::iso(), &accounts, "", date, name, category, amount)
    }

    #[test]
    fn parses_valid_entry() {
        let entry = parse(" 2026-09-30 ", " Groceries ", " Food.Shop ", "12.5").unwrap();
        assert_eq!(entry.date, NaiveDate::from_ymd_opt(2026, 9, 30).unwrap());
        assert_eq!(entry.name, "Groceries");
        assert_eq!(entry.category.names(), ["Food", "Shop"]);
        assert_eq!(entry.amount, 1250);
    }

    #[test]
    fn finds_accounts_by_name_ignoring_case() {
        let (accounts, ids) = accounts_named(&[("Silver bank", "PLN", false), ("Gold bank", "EUR", false)]);
        let account = |name| ParsedEntry::parse(&DateFormat::iso(), &accounts, name, "2026-09-30", "a", "c", "1");
        assert_eq!(account(" GOLD bank ").map(|entry| entry.account), Ok(ids[1]));
        assert_eq!(account("silver bank").map(|entry| entry.account), Ok(ids[0]));
        assert_eq!(account("Bronze bank"), Err(EntryError::UnknownAccount));
        assert_eq!(account("Gold"), Err(EntryError::UnknownAccount), "names must match fully");
        assert_eq!(account(" "), Err(EntryError::UnknownAccount), "there are several accounts");
    }

    #[test]
    fn rejects_deleted_accounts() {
        let (accounts, _) = accounts_named(&[("Silver bank", "PLN", false), ("Gold bank", "EUR", true)]);
        let account = |name| ParsedEntry::parse(&DateFormat::iso(), &accounts, name, "2026-09-30", "a", "c", "1");
        assert_eq!(account("Gold bank"), Err(EntryError::UnknownAccount));
    }

    #[test]
    fn empty_account_is_the_only_active_one() {
        let (accounts, ids) = accounts_named(&[("Silver bank", "PLN", true), ("Gold bank", "EUR", false)]);
        let parsed = ParsedEntry::parse(&DateFormat::iso(), &accounts, "", "2026-09-30", "a", "c", "1").unwrap();
        assert_eq!(parsed.account, ids[1], "deleted accounts don't count");

        let (accounts, _) = accounts_named(&[("Silver bank", "PLN", true)]);
        let parsed = ParsedEntry::parse(&DateFormat::iso(), &accounts, "", "2026-09-30", "a", "c", "1");
        assert_eq!(parsed, Err(EntryError::UnknownAccount));
    }

    #[test]
    fn parses_amounts_in_the_accounts_currency() {
        let (accounts, _) = accounts_named(&[("Silver bank", "PLN", false), ("Yen", "JPY", false), ("Dinar", "KWD", false)]);
        let amount = |account, amount| {
            ParsedEntry::parse(&DateFormat::iso(), &accounts, account, "2026-09-30", "a", "c", amount).map(|entry| entry.amount)
        };
        assert_eq!(amount("Silver bank", "12.5"), Ok(1250));
        assert_eq!(amount("yen", "1200"), Ok(1200));
        assert_eq!(amount("yen", "12.5"), Err(EntryError::InvalidAmount));
        assert_eq!(amount("Dinar", "1.234"), Ok(1234));
    }

    #[test]
    fn rejects_empty_category() {
        assert_eq!(parse("2026-09-30", "a", " ", "1"), Err(EntryError::EmptyCategory));
        assert_eq!(parse("2026-09-30", "a", ".", "1"), Err(EntryError::EmptyCategory));
    }

    #[test]
    fn rejects_invalid_date() {
        assert_eq!(parse("30.09.2026", "a", "c", "1"), Err(EntryError::InvalidDate));
        assert_eq!(parse("2026-02-30", "a", "c", "1"), Err(EntryError::InvalidDate));
        assert_eq!(parse("", "a", "c", "1"), Err(EntryError::InvalidDate));
    }

    #[test]
    fn rejects_empty_name() {
        assert_eq!(parse("2026-09-30", "  ", "c", "1"), Err(EntryError::EmptyName));
    }

    #[test]
    fn parses_amounts() {
        assert_eq!(parse_amount("12", 2), Ok(1200));
        assert_eq!(parse_amount("12.5", 2), Ok(1250));
        assert_eq!(parse_amount("12.05", 2), Ok(1205));
        assert_eq!(parse_amount("-12.50", 2), Ok(-1250));
        assert_eq!(parse_amount("+0.01", 2), Ok(1));
        assert_eq!(parse_amount(" 7 ", 2), Ok(700));
    }

    #[test]
    fn rejects_invalid_amounts() {
        for input in ["", "-", "abc", "1.", ".5", "1.234", "1,50", "--1", "1.-5", "99999999999999999999"] {
            assert_eq!(parse_amount(input, 2), Err(EntryError::InvalidAmount), "input: {input:?}");
        }
    }

    #[test]
    fn parses_amounts_without_decimals() {
        assert_eq!(parse_amount("1200", 0), Ok(1200));
        assert_eq!(parse_amount("-5", 0), Ok(-5));
        for input in ["1.5", "1.0", "1."] {
            assert_eq!(parse_amount(input, 0), Err(EntryError::InvalidAmount), "input: {input:?}");
        }
    }

    #[test]
    fn parses_amounts_with_three_decimals() {
        assert_eq!(parse_amount("12", 3), Ok(12000));
        assert_eq!(parse_amount("12.5", 3), Ok(12500));
        assert_eq!(parse_amount("12.05", 3), Ok(12050));
        assert_eq!(parse_amount("-0.001", 3), Ok(-1));
        assert_eq!(parse_amount("1.2345", 3), Err(EntryError::InvalidAmount));
    }

    #[test]
    fn formats_amounts() {
        assert_eq!(format_amount(0, 2), "0.00");
        assert_eq!(format_amount(5, 2), "0.05");
        assert_eq!(format_amount(1250, 2), "12.50");
        assert_eq!(format_amount(-1250, 2), "-12.50");
        assert_eq!(format_amount(-5, 2), "-0.05");
        assert_eq!(format_amount(i64::MIN, 2), "-92233720368547758.08");
    }

    #[test]
    fn formats_amounts_with_other_decimals() {
        assert_eq!(format_amount(0, 0), "0");
        assert_eq!(format_amount(-1250, 0), "-1250");
        assert_eq!(format_amount(5, 3), "0.005");
        assert_eq!(format_amount(-12500, 3), "-12.500");
    }

    #[test]
    fn expenses_are_positive_and_income_negative() {
        let kind = |amount| {
            let parsed = parse("2026-09-30", "a", "c", amount).unwrap();
            let entry = Entry {
                account: AccountId::generate(),
                date: parsed.date,
                name: parsed.name,
                category: CategoryId::generate(),
                amount: parsed.amount,
            };
            entry.kind()
        };
        assert_eq!(kind("12.50"), EntryKind::Expense);
        assert_eq!(kind("-7800"), EntryKind::Income);
        assert_eq!(kind("0"), EntryKind::Neutral);
    }
}
