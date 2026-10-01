mod category;
mod change;
#[cfg(not(target_arch = "wasm32"))]
mod database_writer;
mod date_format;
mod document;
mod entry;
mod history;
mod ledger;
mod listing;
mod store;
mod view;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use uuid::Uuid;

use change::ChangeBuilder;
use date_format::DateFormat;
use document::Document;
use entry::{EntryId, ParsedEntry};
use ledger::Ledger;
use store::Store;
use view::View;

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub use database_writer::DatabaseWriter;

slint::include_modules!();

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
    Ok((store::MemoryStore, Ledger::default()))
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

    // The document is the source of truth; `view` mirrors it.
    let view = Rc::new(View::new(&main_window, DateFormat::system()));
    view.show(&ledger);
    let document = Rc::new(RefCell::new(Document::new(ledger, store)));

    let entries_store = main_window.global::<EntriesStore>();
    entries_store.on_add_entry({
        let (document, view) = (document.clone(), view.clone());
        move |date, name, category, amount| {
            let Ok(parsed) = ParsedEntry::parse(view.date_format(), &date, &name, &category, &amount) else {
                return false;
            };
            let mut document = document.borrow_mut();
            let mut change = ChangeBuilder::new(format!("Add entry ‘{}’", parsed.name), document.ledger().categories());
            change.add_entry(parsed);
            match document.perform(change.build()) {
                Ok(()) => {
                    view.show(document.ledger());
                    // The inputs are cleared after a successful add.
                    view.query_categories(document.ledger(), "");
                    true
                }
                Err(error) => {
                    // TODO: show storage errors in the UI.
                    eprintln!("Failed to save entry: {error}");
                    false
                }
            }
        }
    });

    entries_store.on_update_entry({
        let (document, view) = (document.clone(), view.clone());
        move |id, date, name, category, amount| {
            let Ok(id) = Uuid::parse_str(&id).map(EntryId) else { return false };
            let Ok(parsed) = ParsedEntry::parse(view.date_format(), &date, &name, &category, &amount) else {
                return false;
            };
            let mut document = document.borrow_mut();
            let Some(current) = document.ledger().entry(id).cloned() else { return false };
            let mut change = ChangeBuilder::new(format!("Edit entry ‘{}’", current.name), document.ledger().categories());
            change.update_entry(id, &current, parsed);
            let change = change.build();
            if change.is_empty() {
                return true;
            }
            match document.perform(change) {
                Ok(()) => {
                    view.show(document.ledger());
                    true
                }
                Err(error) => {
                    // TODO: show storage errors in the UI.
                    eprintln!("Failed to save entry: {error}");
                    false
                }
            }
        }
    });

    entries_store.on_category_edited({
        let (document, view) = (document.clone(), view.clone());
        move |text| view.query_categories(document.borrow().ledger(), &text)
    });

    entries_store.on_shift_date({
        let view = view.clone();
        move |text, days| {
            let today = chrono::Local::now().date_naive();
            view.date_format().shift(&text, days.into(), today).unwrap_or_default().into()
        }
    });

    let undo_redo = main_window.global::<UndoRedo>();
    undo_redo.on_undo({
        let (document, view) = (document.clone(), view.clone());
        move || {
            let mut document = document.borrow_mut();
            match document.undo() {
                Ok(true) => view.show(document.ledger()),
                Ok(false) => {}
                Err(error) => eprintln!("Failed to undo: {error}"),
            }
        }
    });
    undo_redo.on_redo(move || {
        let mut document = document.borrow_mut();
        match document.redo() {
            Ok(true) => view.show(document.ledger()),
            Ok(false) => {}
            Err(error) => eprintln!("Failed to redo: {error}"),
        }
    });

    main_window.run()?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    run().map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
}
