mod category;
#[cfg(not(target_arch = "wasm32"))]
mod database_writer;
mod date_format;
mod entry;
mod ledger;
mod store;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use slint::{SharedString, VecModel};

use category::Categories;
use date_format::DateFormat;
use entry::{format_amount, Entry, ParsedEntry};
use ledger::Ledger;
use store::Store;

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub use database_writer::DatabaseWriter;

slint::include_modules!();

fn entry_row(entry: &Entry, categories: &Categories, date_format: &DateFormat) -> EntryRow {
    EntryRow {
        date: date_format.format(entry.date).into(),
        name: entry.name.as_str().into(),
        category: categories.path(entry.category).into(),
        amount: format_amount(entry.amount).into(),
    }
}

fn update_suggestions(model: &VecModel<SharedString>, suggestions: Vec<String>) {
    model.set_vec(suggestions.into_iter().map(SharedString::from).collect::<Vec<_>>());
}

/// Opens the database and loads its contents.
#[cfg(not(target_arch = "wasm32"))]
fn open_store() -> Result<(impl Store + 'static, Ledger), Box<dyn Error>> {
    let store = store::sqlite::SqliteStore::open(&database_path()?)?;
    let ledger = store.load()?;
    Ok((store, ledger))
}

/// There's no database in the browser yet: data lives only as long as the page.
#[cfg(target_arch = "wasm32")]
fn open_store() -> Result<(impl Store + 'static, Ledger), Box<dyn Error>> {
    Ok((store::MemoryStore::default(), Ledger::default()))
}

/// `--db <path>` if given, otherwise `Monies/monies.db` in the user's data directory
/// (`%APPDATA%` on Windows).
#[cfg(not(target_arch = "wasm32"))]
fn database_path() -> Result<std::path::PathBuf, Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--db" {
            return Ok(args.next().ok_or("--db requires a path")?.into());
        }
    }
    let dirs = directories::BaseDirs::new().ok_or("cannot determine the user's data directory")?;
    Ok(dirs.data_dir().join("Monies").join("monies.db"))
}

/// Creates the main window and runs the event loop until the window is closed.
pub fn run() -> Result<(), Box<dyn Error>> {
    let (store, ledger) = open_store()?;
    let main_window = MainWindow::new()?;

    let date_format = Rc::new(DateFormat::system());
    // The ledger is the source of truth; `rows` mirrors its entries for display.
    let rows: Vec<EntryRow> = ledger
        .entries()
        .iter()
        .map(|entry| entry_row(entry, ledger.categories(), &date_format))
        .collect();
    let rows = Rc::new(VecModel::from(rows));
    // Always kept in sync with the text of the category input, which starts out empty.
    let category_suggestions = Rc::new(VecModel::<SharedString>::default());
    update_suggestions(&category_suggestions, ledger.categories().suggest(""));
    // Entries are kept in insertion order, so the last one is the last *entered*. Without any,
    // the date input is prefilled with today (as of startup).
    let last_date = match ledger.entries().last() {
        Some(entry) => date_format.format(entry.date),
        None => date_format.format(chrono::Local::now().date_naive()),
    };

    let ledger = Rc::new(RefCell::new(ledger));
    let store = Rc::new(RefCell::new(store));

    let entries_store = main_window.global::<EntriesStore>();
    entries_store.set_date_placeholder(date_format.placeholder().into());
    entries_store.set_last_date(last_date.into());
    entries_store.set_entries(rows.clone().into());
    entries_store.set_category_suggestions(category_suggestions.clone().into());

    entries_store.on_add_entry({
        let window = main_window.as_weak();
        let date_format = date_format.clone();
        let ledger = ledger.clone();
        let category_suggestions = category_suggestions.clone();
        move |date, name, category, amount| {
            let Ok(parsed) = ParsedEntry::parse(&date_format, &date, &name, &category, &amount) else {
                return false;
            };
            let mut ledger = ledger.borrow_mut();
            if let Err(error) = ledger.add_entry(&mut *store.borrow_mut(), parsed) {
                // TODO: show storage errors in the UI.
                eprintln!("Failed to save entry: {error}");
                return false;
            }
            let entry = ledger.entries().last().expect("entry was just added");
            let row = entry_row(entry, ledger.categories(), &date_format);
            let window = window.upgrade().expect("window outlives its callbacks");
            window.global::<EntriesStore>().set_last_date(row.date.clone());
            rows.push(row);
            // The inputs are cleared after a successful add and new categories may exist.
            update_suggestions(&category_suggestions, ledger.categories().suggest(""));
            true
        }
    });

    entries_store.on_category_edited(move |text| {
        update_suggestions(&category_suggestions, ledger.borrow().categories().suggest(&text));
    });

    entries_store.on_shift_date(move |text, days| {
        let today = chrono::Local::now().date_naive();
        date_format.shift(&text, days.into(), today).unwrap_or_default().into()
    });

    main_window.run()?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    run().map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
}
