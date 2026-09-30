mod category;
mod date_format;
mod entry;
mod ledger;

use std::cell::RefCell;
use std::rc::Rc;

use slint::{SharedString, VecModel};

use category::Categories;
use date_format::DateFormat;
use entry::{format_amount, Entry, ParsedEntry};
use ledger::Ledger;

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

/// Creates the main window and runs the event loop until the window is closed.
pub fn run() -> Result<(), slint::PlatformError> {
    let main_window = MainWindow::new()?;

    let date_format = Rc::new(DateFormat::system());
    // The ledger is the source of truth; `rows` mirrors its entries for display.
    let ledger = Rc::new(RefCell::new(Ledger::default()));
    let rows = Rc::new(VecModel::<EntryRow>::default());
    // Always kept in sync with the text of the category input.
    let category_suggestions = Rc::new(VecModel::<SharedString>::default());

    let store = main_window.global::<EntriesStore>();
    store.set_date_placeholder(date_format.placeholder().into());
    store.set_entries(rows.clone().into());
    store.set_category_suggestions(category_suggestions.clone().into());

    store.on_add_entry({
        let window = main_window.as_weak();
        let date_format = date_format.clone();
        let ledger = ledger.clone();
        let category_suggestions = category_suggestions.clone();
        move |date, name, category, amount| {
            let Ok(parsed) = ParsedEntry::parse(&date_format, &date, &name, &category, &amount) else {
                return false;
            };
            let mut ledger = ledger.borrow_mut();
            ledger.add_entry(parsed);
            // Relies on entries being kept in insertion order: this is the last *entered* entry.
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

    store.on_category_edited(move |text| {
        update_suggestions(&category_suggestions, ledger.borrow().categories().suggest(&text));
    });

    store.on_shift_date(move |text, days| {
        let today = chrono::Local::now().date_naive();
        date_format.shift(&text, days.into(), today).unwrap_or_default().into()
    });

    main_window.run()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    run().map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
}
