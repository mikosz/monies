mod category;
mod entry;
mod ledger;

use std::cell::RefCell;
use std::rc::Rc;

use slint::{SharedString, VecModel};

use category::Categories;
use entry::{format_amount, Entry, DATE_FORMAT};
use ledger::Ledger;

slint::include_modules!();

fn entry_row(entry: &Entry, categories: &Categories) -> EntryRow {
    EntryRow {
        date: entry.date.format(DATE_FORMAT).to_string().into(),
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

    // The ledger is the source of truth; `rows` mirrors its entries for display.
    let ledger = Rc::new(RefCell::new(Ledger::default()));
    let rows = Rc::new(VecModel::<EntryRow>::default());
    // Always kept in sync with the text of the category input.
    let category_suggestions = Rc::new(VecModel::<SharedString>::default());

    let store = main_window.global::<EntriesStore>();
    store.set_entries(rows.clone().into());
    store.set_category_suggestions(category_suggestions.clone().into());

    store.on_add_entry({
        let ledger = ledger.clone();
        let category_suggestions = category_suggestions.clone();
        move |date, name, category, amount| {
            let mut ledger = ledger.borrow_mut();
            if ledger.add_entry(&date, &name, &category, &amount).is_err() {
                return false;
            }
            let entry = ledger.entries().last().expect("entry was just added");
            rows.push(entry_row(entry, ledger.categories()));
            // The inputs are cleared after a successful add and new categories may exist.
            update_suggestions(&category_suggestions, ledger.categories().suggest(""));
            true
        }
    });

    store.on_category_edited(move |text| {
        update_suggestions(&category_suggestions, ledger.borrow().categories().suggest(&text));
    });

    main_window.run()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    run().map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
}
