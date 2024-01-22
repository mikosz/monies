use std::fmt;

use chrono::NaiveDate;

use crate::category::{CategoryId, CategoryPath};

/// Format used both for parsing user input and for displaying dates.
pub const DATE_FORMAT: &str = "%Y-%m-%d";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub date: NaiveDate,
    pub name: String,
    pub category: CategoryId,
    /// Amount in minor units (cents). Negative values are allowed.
    pub amount: i64,
}

/// Validated user input for an entry whose category path hasn't been resolved yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedEntry {
    pub date: NaiveDate,
    pub name: String,
    pub category: CategoryPath,
    /// Amount in minor units (cents). Negative values are allowed.
    pub amount: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryError {
    InvalidDate,
    EmptyName,
    EmptyCategory,
    InvalidAmount,
}

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryError::InvalidDate => write!(f, "date must be in YYYY-MM-DD format"),
            EntryError::EmptyName => write!(f, "name must not be empty"),
            EntryError::EmptyCategory => write!(f, "category must not be empty"),
            EntryError::InvalidAmount => write!(f, "amount must be a number with at most two decimal places"),
        }
    }
}

impl std::error::Error for EntryError {}

impl ParsedEntry {
    /// Validates raw user input. Surrounding whitespace is ignored.
    pub fn parse(date: &str, name: &str, category: &str, amount: &str) -> Result<Self, EntryError> {
        let date = NaiveDate::parse_from_str(date.trim(), DATE_FORMAT)
            .map_err(|_| EntryError::InvalidDate)?;

        let name = name.trim();
        if name.is_empty() {
            return Err(EntryError::EmptyName);
        }

        let category = CategoryPath::parse(category).ok_or(EntryError::EmptyCategory)?;
        let amount = parse_amount(amount)?;

        Ok(Self {
            date,
            name: name.to_owned(),
            category,
            amount,
        })
    }
}

/// Parses a decimal amount such as `12`, `-12.5` or `+12.50` into cents.
pub fn parse_amount(input: &str) -> Result<i64, EntryError> {
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

    let cents = match fraction {
        None => 0,
        Some(fraction) if is_digits(fraction) && fraction.len() <= 2 => {
            let value: i64 = fraction.parse().map_err(|_| EntryError::InvalidAmount)?;
            if fraction.len() == 1 { value * 10 } else { value }
        }
        Some(_) => return Err(EntryError::InvalidAmount),
    };

    let whole: i64 = whole.parse().map_err(|_| EntryError::InvalidAmount)?;
    let total = whole
        .checked_mul(100)
        .and_then(|whole| whole.checked_add(cents))
        .ok_or(EntryError::InvalidAmount)?;

    Ok(if negative { -total } else { total })
}

/// Formats cents as a decimal amount with exactly two decimal places.
pub fn format_amount(amount: i64) -> String {
    let sign = if amount < 0 { "-" } else { "" };
    let abs = amount.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_entry() {
        let entry = ParsedEntry::parse(" 2026-09-30 ", " Groceries ", " Food.Shop ", "12.5").unwrap();
        assert_eq!(entry.date, NaiveDate::from_ymd_opt(2026, 9, 30).unwrap());
        assert_eq!(entry.name, "Groceries");
        assert_eq!(entry.category.names(), ["Food", "Shop"]);
        assert_eq!(entry.amount, 1250);
    }

    #[test]
    fn rejects_empty_category() {
        assert_eq!(ParsedEntry::parse("2026-09-30", "a", " ", "1"), Err(EntryError::EmptyCategory));
        assert_eq!(ParsedEntry::parse("2026-09-30", "a", ".", "1"), Err(EntryError::EmptyCategory));
    }

    #[test]
    fn rejects_invalid_date() {
        assert_eq!(ParsedEntry::parse("30.09.2026", "a", "c", "1"), Err(EntryError::InvalidDate));
        assert_eq!(ParsedEntry::parse("2026-02-30", "a", "c", "1"), Err(EntryError::InvalidDate));
        assert_eq!(ParsedEntry::parse("", "a", "c", "1"), Err(EntryError::InvalidDate));
    }

    #[test]
    fn rejects_empty_name() {
        assert_eq!(ParsedEntry::parse("2026-09-30", "  ", "c", "1"), Err(EntryError::EmptyName));
    }

    #[test]
    fn parses_amounts() {
        assert_eq!(parse_amount("12"), Ok(1200));
        assert_eq!(parse_amount("12.5"), Ok(1250));
        assert_eq!(parse_amount("12.05"), Ok(1205));
        assert_eq!(parse_amount("-12.50"), Ok(-1250));
        assert_eq!(parse_amount("+0.01"), Ok(1));
        assert_eq!(parse_amount(" 7 "), Ok(700));
    }

    #[test]
    fn rejects_invalid_amounts() {
        for input in ["", "-", "abc", "1.", ".5", "1.234", "1,50", "--1", "1.-5", "99999999999999999999"] {
            assert_eq!(parse_amount(input), Err(EntryError::InvalidAmount), "input: {input:?}");
        }
    }

    #[test]
    fn formats_amounts() {
        assert_eq!(format_amount(0), "0.00");
        assert_eq!(format_amount(5), "0.05");
        assert_eq!(format_amount(1250), "12.50");
        assert_eq!(format_amount(-1250), "-12.50");
        assert_eq!(format_amount(-5), "-0.05");
        assert_eq!(format_amount(i64::MIN), "-92233720368547758.08");
    }
}
