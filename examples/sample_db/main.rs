//! Builds `sample.db` next to this file from `accounts.csv` and `entries.csv`, for trying the
//! app with data:
//!
//! ```text
//! cargo run --example sample_db [-- --force]
//! cargo run -- --db examples/sample_db/sample.db
//! ```
//!
//! Each row of `accounts.csv` is an account, in the order they were created: a name and a
//! currency code. Each row of `entries.csv` is one entry, in the order they were entered: the
//! account's name, an ISO date, a name, a category path and an amount in the account's
//! currency (expenses positive, income negative). `--force` replaces an existing database.
//!
//! `import-gold-bank.json` is a statement of "Gold bank" for trying imports (see
//! `docs/import-format.md`). It overlaps the holiday entries typed in, which are possible
//! duplicates of some of its lines.

use std::error::Error;
use std::path::Path;

use monies_app::DatabaseWriter;

fn main() -> Result<(), Box<dyn Error>> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join("sample_db");
    let database = directory.join("sample.db");

    if database.exists() {
        if !std::env::args().skip(1).any(|arg| arg == "--force") {
            return Err(format!("{} already exists, use --force to replace it", database.display()).into());
        }
        std::fs::remove_file(&database)?;
    }

    match fill(&database, &directory.join("accounts.csv"), &directory.join("entries.csv")) {
        Ok((accounts, entries)) => {
            println!("Created {} with {accounts} accounts and {entries} entries", database.display());
            Ok(())
        }
        Err(error) => {
            // Don't leave a half-filled database behind. `fill` has closed it by now.
            let _ = std::fs::remove_file(&database);
            Err(error)
        }
    }
}

/// Creates the database and adds every CSV row in one transaction; returns the number of
/// accounts and entries.
fn fill(database: &Path, accounts_csv: &Path, entries_csv: &Path) -> Result<(usize, usize), Box<dyn Error>> {
    let mut writer = DatabaseWriter::create(database)?;

    let accounts = read(accounts_csv, |[name, currency]| writer.add_account(name, currency))?;
    let entries = read(entries_csv, |[account, date, name, category, amount]| {
        writer.add_entry(account, date, name, category, amount)
    })?;
    writer.finish()?;
    Ok((accounts, entries))
}

/// Calls `add` with the `N` fields of every row of the CSV file; returns the number of rows.
/// Errors name the file and line.
fn read<const N: usize>(
    csv: &Path,
    mut add: impl FnMut([&str; N]) -> Result<(), Box<dyn Error>>,
) -> Result<usize, Box<dyn Error>> {
    let mut reader = csv::Reader::from_path(csv)?;
    let mut count = 0;
    for record in reader.records() {
        let record = record?;
        let line = record.position().map_or(0, |position| position.line());
        add(std::array::from_fn(|field| record.get(field).unwrap_or("")))
            .map_err(|error| format!("{} line {line}: {error}", csv.display()))?;
        count += 1;
    }
    Ok(count)
}
