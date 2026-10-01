mod category;
mod change;
#[cfg(not(target_arch = "wasm32"))]
mod database_writer;
mod date_format;
mod document;
mod entry;
mod history;
mod ledger;
mod store;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use slint::{SharedString, VecModel};

use category::Categories;
use change::ChangeBuilder;
use date_format::DateFormat;
use document::Document;
use entry::{format_amount, Entry, ParsedEntry};
use ledger::{EntryListUpdate, Ledger};
use store::Store;

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

/// Mirrors the ledger into the UI.
struct View {
    window: slint::Weak<MainWindow>,
    date_format: DateFormat,
    rows: Rc<VecModel<EntryRow>>,
    /// Suggestions for the current text of the category input.
    category_suggestions: Rc<VecModel<SharedString>>,
}

impl View {
    fn entry_row(&self, entry: &Entry, categories: &Categories) -> EntryRow {
        EntryRow {
            date: self.date_format.format(entry.date).into(),
            name: entry.name.as_str().into(),
            category: categories.path(entry.category).into(),
            amount: format_amount(entry.amount).into(),
        }
    }

    fn show_ledger(&self, ledger: &Ledger) {
        let rows: Vec<EntryRow> = ledger.entries().iter().map(|(_, e)| self.entry_row(e, ledger.categories())).collect();
        self.rows.set_vec(rows);
        self.show_last_date(ledger);
        self.suggest_categories(ledger, "");
    }

    /// Updates what's shown after a change was applied. `category_text` is the text the
    /// category input has (or is about to have).
    fn update(&self, ledger: &Ledger, updates: &[EntryListUpdate], category_text: &str) {
        for update in updates {
            match *update {
                EntryListUpdate::Inserted { index, id } => {
                    let entry = ledger.entry(id).expect("inserted entry exists");
                    self.rows.insert(index, self.entry_row(entry, ledger.categories()));
                }
                EntryListUpdate::Removed { index } => {
                    self.rows.remove(index);
                }
            }
        }
        self.show_last_date(ledger);
        self.suggest_categories(ledger, category_text);
    }

    fn show_last_date(&self, ledger: &Ledger) {
        // Entries are in the order they were added, so the last one is the last *entered*.
        // Without any, the date input is prefilled with today.
        let date = match ledger.entries().last() {
            Some((_, entry)) => entry.date,
            None => chrono::Local::now().date_naive(),
        };
        self.entries_store(|store| store.set_last_date(self.date_format.format(date).into()));
    }

    fn suggest_categories(&self, ledger: &Ledger, text: &str) {
        let suggestions = ledger.categories().suggest(text).into_iter().map(SharedString::from);
        self.category_suggestions.set_vec(suggestions.collect::<Vec<_>>());
    }

    fn category_text(&self) -> SharedString {
        self.entries_store(|store| store.get_category_text()).unwrap_or_default()
    }

    fn entries_store<T>(&self, f: impl FnOnce(EntriesStore) -> T) -> Option<T> {
        self.window.upgrade().map(|window| f(window.global::<EntriesStore>()))
    }
}

/// Creates the main window and runs the event loop until the window is closed.
pub fn run() -> Result<(), Box<dyn Error>> {
    let (store, ledger) = open_store()?;
    let main_window = MainWindow::new()?;

    let view = Rc::new(View {
        window: main_window.as_weak(),
        date_format: DateFormat::system(),
        rows: Rc::new(VecModel::default()),
        category_suggestions: Rc::new(VecModel::default()),
    });
    // The document is the source of truth; `view` mirrors it.
    view.show_ledger(&ledger);
    let document = Rc::new(RefCell::new(Document::new(ledger, store)));

    let entries_store = main_window.global::<EntriesStore>();
    entries_store.set_date_placeholder(view.date_format.placeholder().into());
    entries_store.set_entries(view.rows.clone().into());
    entries_store.set_category_suggestions(view.category_suggestions.clone().into());

    entries_store.on_add_entry({
        let (document, view) = (document.clone(), view.clone());
        move |date, name, category, amount| {
            let Ok(parsed) = ParsedEntry::parse(&view.date_format, &date, &name, &category, &amount) else {
                return false;
            };
            let mut document = document.borrow_mut();
            let mut change = ChangeBuilder::new(format!("Add entry ‘{}’", parsed.name), document.ledger().categories());
            change.add_entry(parsed);
            match document.perform(change.build()) {
                Ok(updates) => {
                    // The inputs are cleared after a successful add.
                    view.update(document.ledger(), &updates, "");
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
        move |text| view.suggest_categories(document.borrow().ledger(), &text)
    });

    entries_store.on_shift_date({
        let view = view.clone();
        move |text, days| {
            let today = chrono::Local::now().date_naive();
            view.date_format.shift(&text, days.into(), today).unwrap_or_default().into()
        }
    });

    let undo_redo = main_window.global::<UndoRedo>();
    undo_redo.on_undo({
        let (document, view) = (document.clone(), view.clone());
        move || {
            let mut document = document.borrow_mut();
            match document.undo() {
                Ok(Some(updates)) => view.update(document.ledger(), &updates, &view.category_text()),
                Ok(None) => {}
                Err(error) => eprintln!("Failed to undo: {error}"),
            }
        }
    });
    undo_redo.on_redo(move || {
        let mut document = document.borrow_mut();
        match document.redo() {
            Ok(Some(updates)) => view.update(document.ledger(), &updates, &view.category_text()),
            Ok(None) => {}
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
