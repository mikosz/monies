//! Builds `sample.db` next to this file from `entries.csv`, for trying the app with data:
//!
//! ```text
//! cargo run --example sample_db [-- --force]
//! cargo run -- --db examples/sample_db/sample.db
//! ```
//!
//! Each CSV row is one entry, in the order they were entered: an ISO date, a name, a category
//! path and an amount (expenses positive, income negative). `--force` replaces an existing
//! database.

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

    match fill(&database, &directory.join("entries.csv")) {
        Ok(count) => {
            println!("Created {} with {count} entries", database.display());
            Ok(())
        }
        Err(error) => {
            // Don't leave a half-filled database behind. `fill` has closed it by now.
            let _ = std::fs::remove_file(&database);
            Err(error)
        }
    }
}

/// Creates the database and adds every CSV row; returns the number of entries.
fn fill(database: &Path, csv: &Path) -> Result<usize, Box<dyn Error>> {
    let mut writer = DatabaseWriter::create(database)?;
    let mut reader = csv::Reader::from_path(csv)?;

    let mut count = 0;
    for record in reader.records() {
        let record = record?;
        let line = record.position().map_or(0, |position| position.line());
        let [date, name, category, amount] = [0, 1, 2, 3].map(|field| record.get(field).unwrap_or(""));
        writer
            .add_entry(date, name, category, amount)
            .map_err(|error| format!("{} line {line}: {error}", csv.display()))?;
        count += 1;
    }
    Ok(count)
}
