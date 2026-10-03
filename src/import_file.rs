//! Import files: bank statements prepared outside the app, e.g. by an AI from a statement's
//! PDF, in the JSON format described in `docs/import-format.md`.
//!
//! A file is only imported when all of it is valid. Otherwise every problem found is reported
//! with where it is, so they can all be fixed at once.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::Value;

use crate::account::{Account, AccountId, Accounts};
use crate::category::CategoryPath;
use crate::currency::Currency;
use crate::date_format::DateFormat;
use crate::duplicates::{Match, find_matches};
use crate::entry::parse_amount;
use crate::import::{ImportRow, RowStatus, StatementLine};
use crate::ledger::Ledger;

/// The value of `format` that marks an import file.
const FORMAT: &str = "monies-import";
/// The only version of the format so far.
const VERSION: u64 = 1;

/// A statement of an import file, to be added as a pending import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedImport {
    pub account: AccountId,
    pub source: String,
    pub rows: Vec<ImportRow>,
}

/// Reads an import file: every statement becomes an import into its account. `file_name` is
/// the source of statements that don't name one. Rows start out as duplicate detection against
/// the ledger suggests, see [`Match::initial_status`]. Every row has a name: that of the entry
/// its line certainly is, which gives the category too; otherwise the proposed one, or the
/// bank's text.
pub fn load(text: &str, file_name: &str, ledger: &Ledger) -> Result<Vec<LoadedImport>, FileError> {
    // Windows editors may start UTF-8 files with a byte order mark, which JSON doesn't allow.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    // Any JSON file can be picked, so check that it's meant to be an import file before
    // complaining about its structure.
    let header: Value = serde_json::from_str(text).map_err(|error| FileError::of(ProblemKind::NotJson(error.to_string())))?;
    if header.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(FileError::of(ProblemKind::UnknownFormat));
    }
    if header.get("version").and_then(Value::as_u64) != Some(VERSION) {
        return Err(FileError::of(ProblemKind::UnsupportedVersion));
    }
    // Parsed from the text again, so errors tell where in the text they are.
    let file: RawFile =
        serde_json::from_str(text).map_err(|error| FileError::of(ProblemKind::Malformed(error.to_string())))?;

    let mut problems = Problems::default();
    if file.statements.is_empty() {
        problems.report(Location::File, ProblemKind::NoStatements);
    }
    let imports: Vec<LoadedImport> = file
        .statements
        .iter()
        .enumerate()
        .filter_map(|(index, statement)| load_statement(statement, index, file_name, ledger, &mut problems))
        .collect();
    if problems.0.is_empty() { Ok(imports) } else { Err(FileError { problems: problems.0 }) }
}

/// Validates a statement, recording its problems; returns it as an import when it has none.
fn load_statement(
    raw: &RawStatement,
    index: usize,
    file_name: &str,
    ledger: &Ledger,
    problems: &mut Problems,
) -> Option<LoadedImport> {
    let at = Location::Statement(index);
    let account = problems.check(at, find_account(raw.account.as_deref(), ledger.accounts()));
    let currency = problems.check(at, check_currency(raw.currency.as_deref(), account));
    if raw.lines.is_empty() {
        problems.report(at, ProblemKind::NoLines);
        return None;
    }

    // Amounts are checked even when the currency is wrong or missing: in the stated currency if
    // it's valid, otherwise in the account's, otherwise as if it had cents.
    let stated = raw.currency.as_deref().and_then(Currency::parse);
    let decimals = stated.or(account.map(|(_, account)| account.currency)).map_or(2, |currency| currency.decimals());
    // The lines first having each reference.
    let mut references = HashMap::new();
    let rows: Vec<Option<ImportRow>> = raw
        .lines
        .iter()
        .enumerate()
        .map(|(line, raw)| {
            let at = Location::Line { statement: index, line };
            let row = load_line(raw, at, decimals, problems);
            let unique = problems.check(at, check_reference(raw.reference.as_deref(), line, &mut references));
            row.filter(|_| unique.is_some())
        })
        .collect();

    let (account, _) = account?;
    currency?;
    let mut rows: Vec<ImportRow> = rows.into_iter().collect::<Option<_>>()?;
    let matches = find_matches(ledger, account, rows.iter().map(|row| &row.line));
    for (row, found) in rows.iter_mut().zip(matches) {
        row.status = found.initial_status();
        // The user's own data takes precedence over the file's proposals.
        if let Match::Duplicate(id) = found {
            let entry = ledger.entry(id).expect("matches are entries of the ledger");
            row.name = entry.name.clone();
            row.category = Some(ledger.categories().category_path(entry.category));
        }
    }
    let source = given(raw.source.as_deref()).unwrap_or(file_name).to_owned();
    Some(LoadedImport { account, source, rows })
}

/// Validates a statement line, recording its problems; returns it as a row when it has none.
/// Without a proposed name, the row is named by the bank's text. Its status is left for
/// duplicate detection.
fn load_line(raw: &RawLine, at: Location, decimals: u32, problems: &mut Problems) -> Option<ImportRow> {
    let date = problems.check(at, parse_date(raw.date.as_deref()));
    let amount = problems.check(at, parse_bank_amount(raw.amount.as_ref(), decimals));
    let text = problems.check(at, given(raw.text.as_deref()).ok_or(ProblemKind::MissingText));
    let text = text?;
    let line = StatementLine {
        date: date?,
        amount: amount?,
        text: text.to_owned(),
        reference: given(raw.reference.as_deref()).map(str::to_owned),
    };
    Some(ImportRow {
        line,
        name: given(raw.name.as_deref()).unwrap_or(text).to_owned(),
        category: raw.category.as_deref().and_then(CategoryPath::parse),
        status: RowStatus::Pending,
    })
}

/// The trimmed text, unless there's none: empty and missing values are the same.
fn given(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|text| !text.is_empty())
}

/// Checks that no earlier line of the statement, recorded in `references` with the lines first
/// having them, has the reference of `line`, if it has one; records it otherwise. The bank's
/// ids are unique per account, see [`StatementLine::reference`].
fn check_reference<'a>(
    reference: Option<&'a str>,
    line: usize,
    references: &mut HashMap<&'a str, usize>,
) -> Result<(), ProblemKind> {
    let Some(reference) = given(reference) else { return Ok(()) };
    match references.entry(reference) {
        Entry::Occupied(first) => {
            Err(ProblemKind::DuplicateReference { reference: reference.to_owned(), first: *first.get() })
        }
        Entry::Vacant(vacant) => {
            vacant.insert(line);
            Ok(())
        }
    }
}

/// The active account with the name, ignoring case.
fn find_account<'a>(name: Option<&str>, accounts: &'a Accounts) -> Result<(AccountId, &'a Account), ProblemKind> {
    let name = given(name).ok_or(ProblemKind::MissingAccount)?;
    if let Some(found) = accounts.find_active(name) {
        return Ok(found);
    }
    Err(match accounts.find(name) {
        Some(_) => ProblemKind::DeletedAccount(name.to_owned()),
        None => ProblemKind::UnknownAccount {
            name: name.to_owned(),
            available: accounts.active().map(|(_, account)| account.name.clone()).collect(),
        },
    })
}

/// Checks that the statement's currency is the account's, if the account is known: the file
/// says which account it's for by name only, so this guards against picking the wrong one.
fn check_currency(code: Option<&str>, account: Option<(AccountId, &Account)>) -> Result<(), ProblemKind> {
    let code = given(code).ok_or(ProblemKind::MissingCurrency)?;
    let currency = Currency::parse(code).ok_or_else(|| ProblemKind::InvalidCurrency(code.to_owned()))?;
    match account {
        Some((_, account)) if account.currency != currency => {
            Err(ProblemKind::CurrencyMismatch { currency, account: account.name.clone(), expected: account.currency })
        }
        _ => Ok(()),
    }
}

/// Dates are always ISO `YYYY-MM-DD`, whatever the user's date format.
fn parse_date(text: Option<&str>) -> Result<NaiveDate, ProblemKind> {
    let text = given(text).ok_or(ProblemKind::MissingDate)?;
    DateFormat::iso().parse(text).ok_or_else(|| ProblemKind::InvalidDate(text.to_owned()))
}

/// Parses an amount given as a JSON string or number in the bank's sign convention, money out
/// negative, into minor units in the app's, expenses positive. A number is taken by its JSON
/// text, e.g. `12.5` as `"12.5"`.
fn parse_bank_amount(value: Option<&Value>, decimals: u32) -> Result<i64, ProblemKind> {
    let text = match value {
        None | Some(Value::Null) => return Err(ProblemKind::MissingAmount),
        Some(Value::String(text)) => text.trim().to_owned(),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => return Err(ProblemKind::InvalidAmount(other.to_string())),
    };
    if text.is_empty() {
        return Err(ProblemKind::MissingAmount);
    }
    match parse_amount(&text, decimals) {
        Ok(amount) => Ok(-amount),
        Err(_) if too_precise(&text, decimals) => Err(ProblemKind::TooManyDecimals(decimals)),
        Err(_) => Err(ProblemKind::InvalidAmount(text)),
    }
}

/// Whether the text is a valid amount, only with more than `decimals` decimal places: likely
/// the wrong currency, or a computed amount that wasn't rounded.
fn too_precise(text: &str, decimals: u32) -> bool {
    let places = text.split_once('.').map_or(0, |(_, fraction)| fraction.len());
    // More places than this don't fit minor units into an `i64` anyway.
    places > decimals as usize && places <= 18 && parse_amount(text, places as u32).is_ok()
}

/// An import file as written, before validation. Missing fields are reported together with
/// all other problems, so they're optional here. Unknown fields are ignored: AIs like to add
/// some, e.g. balances.
#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    statements: Vec<RawStatement>,
}

#[derive(Deserialize)]
struct RawStatement {
    account: Option<String>,
    currency: Option<String>,
    source: Option<String>,
    #[serde(default)]
    lines: Vec<RawLine>,
}

#[derive(Deserialize)]
struct RawLine {
    date: Option<String>,
    /// A string or a number.
    amount: Option<Value>,
    text: Option<String>,
    reference: Option<String>,
    name: Option<String>,
    category: Option<String>,
}

/// Why an import file can't be imported: every problem found, in the order of the file.
/// Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub problems: Vec<Problem>,
}

impl FileError {
    fn of(kind: ProblemKind) -> Self {
        Self { problems: vec![Problem { location: Location::File, kind }] }
    }

    /// The problems one per line like [`Display`](fmt::Display), but at most `limit` of them,
    /// followed by how many more there are.
    pub fn summary(&self, limit: usize) -> String {
        let mut lines: Vec<String> = self.problems.iter().take(limit).map(Problem::to_string).collect();
        match self.problems.len().saturating_sub(limit) {
            0 => {}
            1 => lines.push("and 1 more problem".to_owned()),
            more => lines.push(format!("and {more} more problems")),
        }
        lines.join("\n")
    }
}

/// The problems, one per line.
impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary(usize::MAX))
    }
}

impl std::error::Error for FileError {}

/// A problem of an import file and where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub location: Location,
    pub kind: ProblemKind,
}

/// Statements and their lines are numbered from 1 for the user, e.g. "statement 1, line 14:
/// amount has more than 2 decimal places".
impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.location {
            Location::File => write!(f, "{}", self.kind),
            Location::Statement(statement) => write!(f, "statement {}: {}", statement + 1, self.kind),
            Location::Line { statement, line } => write!(f, "statement {}, line {}: {}", statement + 1, line + 1, self.kind),
        }
    }
}

/// Where in an import file a problem is. Indices start at 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// The file as a whole.
    File,
    Statement(usize),
    Line { statement: usize, line: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    /// With serde_json's message, which tells where in the text the error is.
    NotJson(String),
    /// Valid JSON, but e.g. a number where text is expected; with serde_json's message.
    Malformed(String),
    /// `format` isn't [`FORMAT`]: probably some other JSON file.
    UnknownFormat,
    UnsupportedVersion,
    NoStatements,
    NoLines,
    MissingAccount,
    /// No account has the name; with the names of the active accounts.
    UnknownAccount { name: String, available: Vec<String> },
    /// The account with the name is in the trash.
    DeletedAccount(String),
    MissingCurrency,
    InvalidCurrency(String),
    /// The statement's currency isn't that of its account.
    CurrencyMismatch { currency: Currency, account: String, expected: Currency },
    MissingDate,
    InvalidDate(String),
    MissingAmount,
    InvalidAmount(String),
    /// The amount has more decimal places than its currency, which has this many.
    TooManyDecimals(u32),
    MissingText,
    /// An earlier line of the statement, the one at index `first`, has the line's reference.
    DuplicateReference { reference: String, first: usize },
}

impl fmt::Display for ProblemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProblemKind::NotJson(error) => write!(f, "the file isn't valid JSON: {error}"),
            ProblemKind::Malformed(error) => write!(f, "the file isn't structured as expected: {error}"),
            ProblemKind::UnknownFormat => write!(f, "this isn't a Monies import file: \"format\" must be \"{FORMAT}\""),
            ProblemKind::UnsupportedVersion => write!(f, "unsupported version: \"version\" must be {VERSION}"),
            ProblemKind::NoStatements => write!(f, "there are no statements"),
            ProblemKind::NoLines => write!(f, "there are no lines"),
            ProblemKind::MissingAccount => write!(f, "account is missing"),
            ProblemKind::UnknownAccount { name, available } if available.is_empty() => {
                write!(f, "there's no account ‘{name}’ (there are no accounts)")
            }
            ProblemKind::UnknownAccount { name, available } => {
                let available: Vec<String> = available.iter().map(|name| format!("‘{name}’")).collect();
                write!(f, "there's no account ‘{name}’ (accounts: {})", available.join(", "))
            }
            ProblemKind::DeletedAccount(name) => write!(f, "account ‘{name}’ is in the trash"),
            ProblemKind::MissingCurrency => write!(f, "currency is missing"),
            ProblemKind::InvalidCurrency(code) => write!(f, "currency ‘{code}’ isn't a three-letter code"),
            ProblemKind::CurrencyMismatch { currency, account, expected } => {
                write!(f, "currency is {}, but ‘{account}’ is in {}", currency.code(), expected.code())
            }
            ProblemKind::MissingDate => write!(f, "date is missing"),
            ProblemKind::InvalidDate(date) => write!(f, "date ‘{date}’ isn't a valid date in the form YYYY-MM-DD"),
            ProblemKind::MissingAmount => write!(f, "amount is missing"),
            ProblemKind::InvalidAmount(amount) => write!(f, "amount ‘{amount}’ isn't a number like -12.50"),
            ProblemKind::TooManyDecimals(0) => write!(f, "amount must be a whole number"),
            ProblemKind::TooManyDecimals(1) => write!(f, "amount has more than 1 decimal place"),
            ProblemKind::TooManyDecimals(decimals) => write!(f, "amount has more than {decimals} decimal places"),
            ProblemKind::MissingText => write!(f, "text is missing"),
            // Lines are numbered from 1 for the user.
            ProblemKind::DuplicateReference { reference, first } => {
                write!(f, "reference ‘{reference}’ is already used by line {}", first + 1)
            }
        }
    }
}

/// Problems found so far while validating.
#[derive(Default)]
struct Problems(Vec<Problem>);

impl Problems {
    fn report(&mut self, location: Location, kind: ProblemKind) {
        self.0.push(Problem { location, kind });
    }

    /// The value, or `None` after reporting the problem at `location`.
    fn check<T>(&mut self, location: Location, result: Result<T, ProblemKind>) -> Option<T> {
        result.map_err(|kind| self.report(location, kind)).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::ChangeBuilder;
    use crate::duplicates::MatchCounts;
    use crate::entry::ParsedEntry;
    use crate::review::Counts;
    use crate::store::MemoryStore;

    /// A ledger with the accounts "Silver bank" in PLN, "Gold bank" in EUR and "Bronze bank" in
    /// USD, and their ids.
    fn ledger() -> (Ledger, [AccountId; 3]) {
        let mut ledger = Ledger::default();
        let mut builder = ChangeBuilder::new("Add accounts", &ledger);
        let ids = [("Silver bank", "PLN"), ("Gold bank", "EUR"), ("Bronze bank", "USD")]
            .map(|(name, currency)| builder.add_account(name, Currency::parse(currency).unwrap()).unwrap());
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        (ledger, ids)
    }

    /// An import file with the statements, given as JSON.
    fn file(statements: &str) -> String {
        format!(r#"{{"format": "monies-import", "version": 1, "statements": [{statements}]}}"#)
    }

    /// An import file with one statement of "Gold bank" with the lines, given as JSON.
    fn gold_file(lines: &str) -> String {
        file(&format!(r#"{{"account": "Gold bank", "currency": "EUR", "lines": [{lines}]}}"#))
    }

    /// The problems of loading the text, as shown to the user.
    fn problems(text: &str, ledger: &Ledger) -> Vec<String> {
        let error = load(text, "statement.json", ledger).expect_err("the file is invalid");
        error.problems.iter().map(Problem::to_string).collect()
    }

    /// A row of a line that matches no entry, so it's accepted.
    fn row(date: &str, amount: i64, text: &str, reference: Option<&str>, name: &str, category: &str) -> ImportRow {
        let mut row = ImportRow::test(date, amount, text, name, category, RowStatus::Accepted);
        row.line.reference = reference.map(str::to_owned);
        row
    }

    #[test]
    fn loads_statements_with_amounts_as_expenses() {
        let (ledger, [_, gold, _]) = ledger();
        let text = file(
            r#"{
                "account": " gold BANK ",
                "currency": "eur",
                "source": "statement-2026-09.pdf",
                "lines": [
                    {"date": "2026-09-15", "amount": "-12.50", "text": " CARD PAYMENT CORNER SHOP 0042 ",
                     "reference": "TX-0001", "name": " Groceries ", "category": "food. shop"},
                    {"date": "2026-09-16", "amount": "+1500", "text": "TRANSFER SALARY"}
                ]
            }"#,
        );
        let imports = load(&text, "statement-2026-09.json", &ledger).unwrap();
        assert_eq!(imports, [LoadedImport {
            account: gold,
            source: "statement-2026-09.pdf".to_owned(),
            rows: vec![
                row("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", Some("TX-0001"), "Groceries", "food.shop"),
                row("2026-09-16", -150000, "TRANSFER SALARY", None, "TRANSFER SALARY", ""),
            ],
        }]);
    }

    #[test]
    fn empty_optional_values_are_missing() {
        let (ledger, [_, gold, _]) = ledger();
        let text = file(
            r#"{
                "account": "Gold bank", "currency": "EUR", "source": " ",
                "lines": [{"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT CORNER SHOP 0042",
                           "reference": "", "name": "  ", "category": " . "}]
            }"#,
        );
        let imports = load(&text, "statement-2026-09.json", &ledger).unwrap();
        assert_eq!(imports, [LoadedImport {
            account: gold,
            source: "statement-2026-09.json".to_owned(),
            rows: vec![row("2026-09-15", 1250, "CARD PAYMENT CORNER SHOP 0042", None, "CARD PAYMENT CORNER SHOP 0042", "")],
        }]);
    }

    #[test]
    fn each_statement_becomes_an_import() {
        let (ledger, [silver, gold, _]) = ledger();
        let text = file(
            r#"
            {"account": "Silver bank", "currency": "PLN", "source": "silver.pdf",
             "lines": [{"date": "2026-09-01", "amount": "-2400.00", "text": "TRANSFER RENT FLAT 12"}]},
            {"account": "Gold bank", "currency": "EUR",
             "lines": [{"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT CORNER SHOP 0042"}]}
            "#,
        );
        let imports = load(&text, "statements.json", &ledger).unwrap();
        let summary: Vec<(AccountId, &str, i64)> =
            imports.iter().map(|import| (import.account, import.source.as_str(), import.rows[0].line.amount)).collect();
        assert_eq!(summary, [(silver, "silver.pdf", 240000), (gold, "statements.json", 1250)]);
    }

    #[test]
    fn amounts_are_strings_or_numbers() {
        let (ledger, _) = ledger();
        let amount = |amount: &str| {
            let text = gold_file(&format!(r#"{{"date": "2026-09-15", "amount": {amount}, "text": "CARD PAYMENT"}}"#));
            match load(&text, "statement.json", &ledger) {
                Ok(imports) => Ok(imports[0].rows[0].line.amount),
                Err(error) => Err(error.to_string()),
            }
        };
        assert_eq!(amount(r#""-12.50""#), Ok(1250));
        assert_eq!(amount(r#"" 7 ""#), Ok(-700));
        assert_eq!(amount("-12.5"), Ok(1250));
        assert_eq!(amount("78"), Ok(-7800));
        let invalid = |amount: &str| format!("statement 1, line 1: amount ‘{amount}’ isn't a number like -12.50");
        assert_eq!(amount(r#""12,50""#), Err(invalid("12,50")));
        assert_eq!(amount(r#""€12.50""#), Err(invalid("€12.50")));
        assert_eq!(amount("true"), Err(invalid("true")));
        assert_eq!(amount("1e-7"), Err(invalid("1e-7")), "a number is taken by its text");
        let too_precise = Err("statement 1, line 1: amount has more than 2 decimal places".to_owned());
        assert_eq!(amount(r#""-12.505""#), too_precise);
        assert_eq!(amount("-12.505"), too_precise);
        let missing = Err("statement 1, line 1: amount is missing".to_owned());
        assert_eq!(amount("null"), missing);
        assert_eq!(amount(r#"" ""#), missing);
    }

    #[test]
    fn amounts_have_the_decimals_of_the_currency() {
        let (mut ledger, _) = ledger();
        let mut builder = ChangeBuilder::new("Add account", &ledger);
        builder.add_account("Yen bank", Currency::parse("JPY").unwrap()).unwrap();
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let text = |amount| {
            let line = format!(r#"{{"date": "2026-09-15", "amount": "{amount}", "text": "CARD PAYMENT"}}"#);
            file(&format!(r#"{{"account": "Yen bank", "currency": "JPY", "lines": [{line}]}}"#))
        };
        assert_eq!(load(&text("-1200"), "statement.json", &ledger).unwrap()[0].rows[0].line.amount, 1200);
        assert_eq!(problems(&text("-1200.5"), &ledger), ["statement 1, line 1: amount must be a whole number"]);
    }

    #[test]
    fn rejects_files_that_arent_import_files() {
        let (ledger, _) = ledger();
        let problem = |text: &str| problems(text, &ledger).remove(0);
        assert!(problem("statement").starts_with("the file isn't valid JSON: expected value at line 1 column 1"));
        let unknown = "this isn't a Monies import file: \"format\" must be \"monies-import\"";
        assert_eq!(problem("[]"), unknown);
        assert_eq!(problem(r#"{"format": "csv", "version": 1, "statements": []}"#), unknown);
        let unsupported = "unsupported version: \"version\" must be 1";
        assert_eq!(problem(r#"{"format": "monies-import", "version": 2}"#), unsupported);
        assert_eq!(problem(r#"{"format": "monies-import"}"#), unsupported);
    }

    #[test]
    fn rejects_unexpected_structure_with_its_position() {
        let (ledger, _) = ledger();
        let text = gold_file(r#"{"date": 20260915, "amount": "-12.50", "text": "CARD PAYMENT"}"#);
        let problems = problems(&text, &ledger);
        let expected = "the file isn't structured as expected: invalid type: integer `20260915`, expected a string at line 1 column ";
        assert!(problems.len() == 1 && problems[0].starts_with(expected), "problems: {problems:?}");
    }

    #[test]
    fn rejects_files_without_statements_or_lines() {
        let (ledger, _) = ledger();
        assert_eq!(problems(&file(""), &ledger), ["there are no statements"]);
        assert_eq!(problems(r#"{"format": "monies-import", "version": 1}"#, &ledger), ["there are no statements"]);
        assert_eq!(problems(&gold_file(""), &ledger), ["statement 1: there are no lines"]);
        let text = file(r#"{"account": "Gold bank", "currency": "EUR"}"#);
        assert_eq!(problems(&text, &ledger), ["statement 1: there are no lines"]);
    }

    #[test]
    fn reports_every_problem_with_its_location() {
        let (ledger, _) = ledger();
        let text = file(
            r#"
            {"account": "Gold bank", "currency": "EUR", "lines": [
                {"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT CORNER SHOP 0042"},
                {"date": "15.09.2026", "amount": "-12.505"},
                {"amount": "-1,5", "text": " "}
            ]},
            {"lines": [{"date": "2026-02-30", "amount": "-12.50", "text": "CARD PAYMENT"}]},
            {"account": "Silver bank", "currency": "EUR", "lines": []}
            "#,
        );
        assert_eq!(problems(&text, &ledger), [
            "statement 1, line 2: date ‘15.09.2026’ isn't a valid date in the form YYYY-MM-DD",
            "statement 1, line 2: amount has more than 2 decimal places",
            "statement 1, line 2: text is missing",
            "statement 1, line 3: date is missing",
            "statement 1, line 3: amount ‘-1,5’ isn't a number like -12.50",
            "statement 1, line 3: text is missing",
            "statement 2: account is missing",
            "statement 2: currency is missing",
            "statement 2, line 1: date ‘2026-02-30’ isn't a valid date in the form YYYY-MM-DD",
            "statement 3: currency is EUR, but ‘Silver bank’ is in PLN",
            "statement 3: there are no lines",
        ]);
    }

    #[test]
    fn unknown_accounts_are_listed_with_the_available_ones() {
        let (mut ledger, [_, gold, _]) = ledger();
        let mut builder = ChangeBuilder::new("Delete account", &ledger);
        builder.delete_account(gold);
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let text = |account| {
            let line = r#"{"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT"}"#;
            file(&format!(r#"{{"account": "{account}", "currency": "EUR", "lines": [{line}]}}"#))
        };
        assert_eq!(problems(&text("Platinum bank"), &ledger), [
            "statement 1: there's no account ‘Platinum bank’ (accounts: ‘Silver bank’, ‘Bronze bank’)"
        ]);
        assert_eq!(problems(&text("gold bank"), &ledger), ["statement 1: account ‘gold bank’ is in the trash"]);
    }

    #[test]
    fn references_are_unique_within_a_statement() {
        let (ledger, _) = ledger();
        let line = |text, reference| {
            format!(r#"{{"date": "2026-09-15", "amount": "-12.50", "text": "{text}", "reference": "{reference}"}}"#)
        };
        let lines = [
            line("CARD PAYMENT CORNER SHOP 0042", "TX-0001"),
            line("CARD PAYMENT BAKERY 0007", "TX-0002"),
            line("CARD PAYMENT CORNER SHOP 0043", " TX-0001 "),
            line("BANK FEE", ""),
            line("BANK FEE", ""),
        ];
        assert_eq!(problems(&gold_file(&lines.join(",")), &ledger), [
            "statement 1, line 3: reference ‘TX-0001’ is already used by line 1"
        ]);

        let statement = |account, currency| {
            format!(r#"{{"account": "{account}", "currency": "{currency}", "lines": [{}]}}"#, lines[0])
        };
        let text = file(&[statement("Gold bank", "EUR"), statement("Silver bank", "PLN")].join(","));
        assert!(load(&text, "statements.json", &ledger).is_ok(), "in statements of different accounts");
    }

    #[test]
    fn currency_must_be_the_accounts() {
        let (ledger, _) = ledger();
        let text = |currency| {
            let line = r#"{"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT"}"#;
            file(&format!(r#"{{"account": "Bronze bank", "currency": "{currency}", "lines": [{line}]}}"#))
        };
        assert!(load(&text(" usd "), "statement.json", &ledger).is_ok());
        assert_eq!(problems(&text("EUR"), &ledger), ["statement 1: currency is EUR, but ‘Bronze bank’ is in USD"]);
        assert_eq!(problems(&text("$"), &ledger), ["statement 1: currency ‘$’ isn't a three-letter code"]);
    }

    #[test]
    fn ignores_unknown_fields() {
        let (ledger, _) = ledger();
        let text = r#"{
            "format": "monies-import", "version": 1, "generator": "an AI",
            "statements": [{
                "account": "Gold bank", "currency": "EUR", "iban": "XX00 0000", "period": {"from": "2026-09-01"},
                "lines": [{"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT", "balance": -87.5}]
            }]
        }"#;
        assert_eq!(load(text, "statement.json", &ledger).unwrap()[0].rows.len(), 1);
    }

    #[test]
    fn accepts_a_byte_order_mark() {
        let (ledger, _) = ledger();
        let text = format!("\u{feff}{}", gold_file(r#"{"date": "2026-09-15", "amount": "-12.50", "text": "CARD PAYMENT"}"#));
        assert!(load(&text, "statement.json", &ledger).is_ok());
    }

    #[test]
    fn rows_start_as_duplicate_detection_suggests() {
        let (mut ledger, [silver, ..]) = ledger();
        let mut builder = ChangeBuilder::new("Import", &ledger);
        let rent = ImportRow::test("2026-09-01", 240000, "TRANSFER RENT FLAT 12", "Rent", "bills.rent", RowStatus::Accepted);
        let id = builder.add_import(silver, "statement-2026-08.json", vec![rent]);
        builder.submit_import(id).unwrap();
        builder.add_entry(ParsedEntry::test(silver, "2026-09-14", "Groceries", "food", "104.88"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let text = file(
            r#"{"account": "Silver bank", "currency": "PLN", "lines": [
                {"date": "2026-09-01", "amount": "-2400.00", "text": "TRANSFER RENT FLAT 12"},
                {"date": "2026-09-15", "amount": "-104.88", "text": "CARD PAYMENT CORNER SHOP 0042"},
                {"date": "2026-09-16", "amount": "-12.50", "text": "CARD PAYMENT BAKERY 0007"}
            ]}"#,
        );
        let imports = load(&text, "statement-2026-09.json", &ledger).unwrap();
        let statuses: Vec<RowStatus> = imports[0].rows.iter().map(|row| row.status).collect();
        assert_eq!(
            statuses,
            [RowStatus::Skipped, RowStatus::Pending, RowStatus::Accepted],
            "an imported line is certainly a duplicate, one typed in only possibly, and a new one is added"
        );
    }

    #[test]
    fn names_rows_by_the_entry_they_certainly_are_then_the_file_then_the_bank_text() {
        let (mut ledger, [silver, ..]) = ledger();
        let mut builder = ChangeBuilder::new("Import", &ledger);
        let rent = ImportRow::test("2026-09-01", 240000, "TRANSFER RENT FLAT 12", "Rent", "bills.Rent", RowStatus::Accepted);
        let id = builder.add_import(silver, "statement-2026-08.json", vec![rent]);
        let entry = builder.submit_import(id).unwrap()[0];
        builder.add_entry(ParsedEntry::test(silver, "2026-09-14", "Groceries", "food", "104.88"));
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        // Renamed since it was imported.
        let current = ledger.entry(entry).unwrap().clone();
        let mut builder = ChangeBuilder::new("Edit", &ledger);
        builder.update_entry(entry, &current, ParsedEntry::test(silver, "2026-09-01", "Flat", "home.rent", "2400.00")).unwrap();
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let text = file(
            r#"{"account": "Silver bank", "currency": "PLN", "lines": [
                {"date": "2026-09-01", "amount": "-2400.00", "text": "TRANSFER RENT FLAT 12", "name": "Rent", "category": "bills"},
                {"date": "2026-09-15", "amount": "-104.88", "text": "CARD PAYMENT CORNER SHOP 0042", "category": "food.shop"},
                {"date": "2026-09-16", "amount": "-12.50", "text": " CARD PAYMENT BAKERY 0007 ", "name": "Bread"}
            ]}"#,
        );
        let imports = load(&text, "statement-2026-09.json", &ledger).unwrap();
        let proposals: Vec<(&str, Option<String>)> = imports[0]
            .rows
            .iter()
            .map(|row| (row.name.as_str(), row.category.as_ref().map(ToString::to_string)))
            .collect();
        assert_eq!(proposals, [
            ("Flat", Some("home.rent".to_owned())),
            ("CARD PAYMENT CORNER SHOP 0042", Some("food.shop".to_owned())),
            ("Bread", None),
        ]);
    }

    #[test]
    fn summary_shows_the_first_problems() {
        let problem = |line| Problem { location: Location::Line { statement: 0, line }, kind: ProblemKind::MissingText };
        let error = FileError { problems: (0..4).map(problem).collect() };
        assert_eq!(error.summary(2), "statement 1, line 1: text is missing\nstatement 1, line 2: text is missing\nand 2 more problems");
        assert!(error.summary(3).ends_with("line 3: text is missing\nand 1 more problem"));
        assert_eq!(error.summary(4), error.to_string());
        assert_eq!(error.to_string().lines().count(), 4);
    }

    /// The ledger of the sample database, see `examples/sample_db`.
    fn sample_ledger() -> Ledger {
        let rows = |csv: &'static str| csv.lines().skip(1).map(|line| line.split(',').collect::<Vec<_>>());
        let mut ledger = Ledger::default();
        let mut builder = ChangeBuilder::new("Add accounts", &ledger);
        for fields in rows(include_str!("../examples/sample_db/accounts.csv")) {
            builder.add_account(fields[0], Currency::parse(fields[1]).unwrap()).unwrap();
        }
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();

        let mut builder = ChangeBuilder::new("Add entries", &ledger);
        for fields in rows(include_str!("../examples/sample_db/entries.csv")) {
            let [account, date, name, category, amount] = fields[..] else { panic!("unexpected row: {fields:?}") };
            let parsed = ParsedEntry::parse(&DateFormat::iso(), ledger.accounts(), account, date, name, category, amount);
            builder.add_entry(parsed.unwrap());
        }
        let change = builder.build();
        ledger.apply(&mut MemoryStore, &change).unwrap();
        ledger
    }

    #[test]
    fn sample_file_loads_into_the_sample_database() {
        let ledger = sample_ledger();
        let text = include_str!("../examples/sample_db/import-gold-bank.json");
        let imports = load(text, "import-gold-bank.json", &ledger).unwrap();
        let [import] = imports.as_slice() else { panic!("expected one statement, got {}", imports.len()) };
        let matches = find_matches(&ledger, import.account, import.rows.iter().map(|row| &row.line));
        assert_eq!(
            MatchCounts::of(&matches),
            MatchCounts { new: 4, duplicates: 0, possible: 9 },
            "the holiday entries typed in are possible duplicates"
        );
        assert_eq!(
            Counts::of(&import.rows, &matches),
            Counts { to_add: 4, skipped: 0, linked: 0, to_review: 9, duplicates: 0 },
            "new lines start accepted"
        );
    }
}
