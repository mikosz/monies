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

use slint::{Model, SharedString, VecModel};
use uuid::Uuid;

use category::Categories;
use change::ChangeBuilder;
use date_format::DateFormat;
use document::Document;
use entry::{format_amount, Entry, EntryId, ParsedEntry};
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
    /// One per entry, in the same order as the ledger's, followed by the row for adding a new
    /// entry (see `EntriesStore.new-entry-id`). Indices of entries are therefore the same here
    /// as in the ledger.
    rows: Rc<VecModel<EntryRow>>,
    new_entry_id: SharedString,
    /// Suggestions for `category_query`.
    category_suggestions: Rc<VecModel<SharedString>>,
    /// The text last reported by a category input (on focus or edit), i.e. of the one in use.
    category_query: RefCell<String>,
}

impl View {
    fn entry_row(&self, id: EntryId, entry: &Entry, categories: &Categories) -> EntryRow {
        EntryRow {
            id: id.0.to_string().into(),
            date: self.date_format.format(entry.date).into(),
            name: entry.name.as_str().into(),
            category: categories.path(entry.category).into(),
            amount: format_amount(entry.amount).into(),
            kind: match entry.kind() {
                entry::EntryKind::Expense => EntryKind::Expense,
                entry::EntryKind::Income => EntryKind::Income,
                entry::EntryKind::Neutral => EntryKind::Neutral,
            },
        }
    }

    fn show_ledger(&self, ledger: &Ledger) {
        let rows = ledger.entries().iter().map(|(id, entry)| self.entry_row(*id, entry, ledger.categories()));
        let new_entry = EntryRow { id: self.new_entry_id.clone(), ..Default::default() };
        self.rows.set_vec(rows.chain([new_entry]).collect::<Vec<_>>());
        self.show_last_date(ledger);
        self.suggest_categories(ledger);
    }

    /// Updates what's shown after a change was applied.
    fn update(&self, ledger: &Ledger, updates: &[EntryListUpdate]) {
        for update in updates {
            match *update {
                EntryListUpdate::Inserted { index, id } => {
                    let entry = ledger.entry(id).expect("inserted entry exists");
                    self.rows.insert(index, self.entry_row(id, entry, ledger.categories()));
                }
                EntryListUpdate::Removed { index } => {
                    self.rows.remove(index);
                }
                EntryListUpdate::Updated { index } => {
                    let (id, entry) = &ledger.entries()[index];
                    self.rows.set_row_data(index, self.entry_row(*id, entry, ledger.categories()));
                }
            }
        }
        self.show_last_date(ledger);
        self.suggest_categories(ledger);
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

    fn query_categories(&self, ledger: &Ledger, text: &str) {
        self.category_query.replace(text.to_owned());
        self.suggest_categories(ledger);
    }

    fn suggest_categories(&self, ledger: &Ledger) {
        let suggestions = ledger.categories().suggest(&self.category_query.borrow());
        let suggestions = suggestions.into_iter().map(SharedString::from);
        self.category_suggestions.set_vec(suggestions.collect::<Vec<_>>());
    }

    fn entries_store<T>(&self, f: impl FnOnce(EntriesStore) -> T) -> Option<T> {
        self.window.upgrade().map(|window| f(window.global::<EntriesStore>()))
    }
}

/// Creates the main window and runs the event loop until the window is closed.
pub fn run() -> Result<(), Box<dyn Error>> {
    let (store, ledger) = open_store()?;
    let main_window = MainWindow::new()?;
    let entries_store = main_window.global::<EntriesStore>();

    let view = Rc::new(View {
        window: main_window.as_weak(),
        date_format: DateFormat::system(),
        rows: Rc::new(VecModel::default()),
        new_entry_id: entries_store.get_new_entry_id(),
        category_suggestions: Rc::new(VecModel::default()),
        category_query: RefCell::default(),
    });
    // The document is the source of truth; `view` mirrors it.
    view.show_ledger(&ledger);
    let document = Rc::new(RefCell::new(Document::new(ledger, store)));

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
                    view.category_query.replace(String::new());
                    view.update(document.ledger(), &updates);
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

    entries_store.on_update_entry({
        let (document, view) = (document.clone(), view.clone());
        move |id, date, name, category, amount| {
            let Ok(id) = Uuid::parse_str(&id).map(EntryId) else { return false };
            let Ok(parsed) = ParsedEntry::parse(&view.date_format, &date, &name, &category, &amount) else {
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
                Ok(updates) => {
                    view.update(document.ledger(), &updates);
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
                Ok(Some(updates)) => view.update(document.ledger(), &updates),
                Ok(None) => {}
                Err(error) => eprintln!("Failed to undo: {error}"),
            }
        }
    });
    undo_redo.on_redo(move || {
        let mut document = document.borrow_mut();
        match document.redo() {
            Ok(Some(updates)) => view.update(document.ledger(), &updates),
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
