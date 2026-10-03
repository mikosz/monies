mod account;
mod category;
mod change;
mod currency;
mod database_writer;
mod date_format;
mod document;
mod duplicates;
mod entry;
mod history;
mod import;
mod import_file;
mod ledger;
mod listing;
mod review;
mod store;
mod view;

use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;

use uuid::Uuid;

use account::AccountId;
use category::CategoryPath;
use change::{Change, ChangeBuilder};
use currency::Currency;
use date_format::DateFormat;
use document::Document;
use duplicates::Match;
use entry::{EntryId, ParsedEntry};
use import::{ImportId, ImportRow, LinkKind, RowStatus};
use ledger::Ledger;
use store::Store;
use view::View;

#[doc(hidden)]
pub use database_writer::DatabaseWriter;

slint::include_modules!();

/// Opens the database and loads its contents.
fn open_store() -> Result<(impl Store + 'static, Ledger), Box<dyn Error>> {
    let store = store::sqlite::SqliteStore::open(&database_path()?)?;
    let ledger = store.load()?;
    Ok((store, ledger))
}

/// `--db <path>` if given, otherwise `Monies/monies.db` in the user's data directory
/// (`%APPDATA%` on Windows).
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
    // Entries can't be added without an account, so start by asking for one.
    if ledger.accounts().first_active().is_none() {
        main_window.invoke_show_settings();
    }
    let document = Rc::new(RefCell::new(Document::new(ledger, store)));

    let entries_store = main_window.global::<EntriesStore>();
    entries_store.on_add_entry({
        let (document, view) = (document.clone(), view.clone());
        move |account, date, name, category, amount| {
            let mut document = document.borrow_mut();
            let accounts = document.ledger().accounts();
            let Ok(parsed) = ParsedEntry::parse(view.date_format(), accounts, &account, &date, &name, &category, &amount)
            else {
                return false;
            };
            let account = parsed.account;
            let mut change = ChangeBuilder::new(format!("Add entry ‘{}’", parsed.name), document.ledger());
            change.add_entry(parsed);
            let change = change.build();
            if !perform(&mut document, &view, change) {
                return false;
            }
            view.entry_added(document.ledger(), account);
            true
        }
    });

    entries_store.on_update_entry({
        let (document, view) = (document.clone(), view.clone());
        move |id, account, date, name, category, amount| {
            let Ok(id) = Uuid::parse_str(&id).map(EntryId) else { return false };
            let mut document = document.borrow_mut();
            let Some(current) = document.ledger().entry(id).cloned() else { return false };
            let accounts = document.ledger().accounts();
            let Ok(parsed) = ParsedEntry::parse(view.date_format(), accounts, &account, &date, &name, &category, &amount)
            else {
                return false;
            };
            let mut change = ChangeBuilder::new(format!("Edit entry ‘{}’", current.name), document.ledger());
            // Imported entries can't be moved to another account; the editor doesn't offer it.
            if change.update_entry(id, &current, parsed).is_err() {
                return false;
            }
            let change = change.build();
            perform(&mut document, &view, change)
        }
    });

    entries_store.on_account_edited({
        let (document, view) = (document.clone(), view.clone());
        move |text| view.query_accounts(document.borrow().ledger(), &text)
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

    let settings_store = main_window.global::<SettingsStore>();
    settings_store.on_add_account({
        let (document, view) = (document.clone(), view.clone());
        move |name, currency| {
            let Some(currency) = Currency::parse(&currency) else { return false };
            let mut document = document.borrow_mut();
            let mut change = ChangeBuilder::new(format!("Add account ‘{}’", name.trim()), document.ledger());
            if change.add_account(&name, currency).is_err() {
                return false;
            }
            let change = change.build();
            perform(&mut document, &view, change)
        }
    });

    settings_store.on_update_account({
        let (document, view) = (document.clone(), view.clone());
        move |id, name, currency| {
            let Ok(id) = Uuid::parse_str(&id).map(AccountId) else { return false };
            let Some(currency) = Currency::parse(&currency) else { return false };
            let mut document = document.borrow_mut();
            let Some(current) = document.ledger().accounts().get(id) else { return false };
            let description = if name.trim() == current.name {
                format!("Edit account ‘{}’", current.name)
            } else {
                format!("Rename account ‘{}’", current.name)
            };
            let mut change = ChangeBuilder::new(description, document.ledger());
            if change.update_account(id, &name, currency).is_err() {
                return false;
            }
            let change = change.build();
            perform(&mut document, &view, change)
        }
    });

    settings_store.on_delete_account({
        let (document, view) = (document.clone(), view.clone());
        move |id| {
            let describe = |name: &str| format!("Delete account ‘{name}’");
            change_account(&document, &view, &id, describe, |change, id| change.delete_account(id));
        }
    });

    settings_store.on_restore_account({
        let (document, view) = (document.clone(), view.clone());
        move |id| {
            let describe = |name: &str| format!("Restore account ‘{name}’");
            change_account(&document, &view, &id, describe, |change, id| change.restore_account(id));
        }
    });

    settings_store.on_delete_account_permanently({
        let (document, view) = (document.clone(), view.clone());
        move |id| {
            let describe = |name: &str| format!("Delete account ‘{name}’ permanently");
            change_account(&document, &view, &id, describe, |change, id| change.delete_account_permanently(id));
        }
    });

    settings_store.on_currency_edited({
        let view = view.clone();
        move |text| view.query_currencies(&text)
    });

    let imports_store = main_window.global::<ImportsStore>();
    imports_store.on_import_file({
        let (document, view, window) = (document.clone(), view.clone(), main_window.as_weak());
        move || {
            let Some(window) = window.upgrade() else { return };
            // Modal to the main window, which can't be used meanwhile.
            let dialog = rfd::FileDialog::new()
                .set_title("Import file")
                .add_filter("Monies import file", &["json"])
                .set_parent(&window.window().window_handle());
            let Some(path) = dialog.pick_file() else { return };
            let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read_to_string(&path) {
                Ok(text) => add_imports(&document, &view, &file_name, &text),
                Err(error) => view.show_error(&format!("Can't read ‘{file_name}’"), &error.to_string()),
            }
        }
    });

    imports_store.on_discard_import({
        let (document, view) = (document.clone(), view.clone());
        move |id| {
            let Ok(id) = Uuid::parse_str(&id).map(ImportId) else { return };
            let mut document = document.borrow_mut();
            let Some(import) = document.ledger().import(id) else { return };
            let mut change = ChangeBuilder::new(format!("Discard import ‘{}’", import.source), document.ledger());
            change.discard_import(id);
            let change = change.build();
            perform(&mut document, &view, change);
        }
    });

    imports_store.on_review_import({
        let (document, view) = (document.clone(), view.clone());
        // "" isn't an id, so it goes back to the list.
        move |id| view.review(document.borrow().ledger(), Uuid::parse_str(&id).ok().map(ImportId))
    });

    imports_store.on_decide_row({
        let (document, view) = (document.clone(), view.clone());
        move |position, decision| {
            let verb = match decision {
                RowDecision::Accept => "Accept",
                RowDecision::Skip => "Skip",
                RowDecision::KeepEntry | RowDecision::UseImported => "Link",
            };
            let describe = |text: &str| format!("{verb} ‘{text}’");
            update_reviewed_row(&document, &view, position, describe, |ledger, row, found| {
                match decision {
                    RowDecision::Accept => Some(ImportRow { status: RowStatus::Accepted, ..row.clone() }),
                    RowDecision::Skip => Some(ImportRow { status: RowStatus::Skipped, ..row.clone() }),
                    RowDecision::KeepEntry => review::link(ledger, row, found, LinkKind::KeepEntry),
                    RowDecision::UseImported => review::link(ledger, row, found, LinkKind::UseImported),
                }
            });
        }
    });

    imports_store.on_edit_row({
        let (document, view) = (document.clone(), view.clone());
        move |position, name, category| {
            // Rows always have a name, see `ImportRow::name`.
            let name = name.trim();
            if name.is_empty() {
                return false;
            }
            let describe = |text: &str| format!("Edit ‘{text}’");
            update_reviewed_row(&document, &view, position, describe, |_, row, _| {
                Some(ImportRow { name: name.to_owned(), category: CategoryPath::parse(&category), ..row.clone() })
            });
            true
        }
    });

    imports_store.on_submit_review({
        let (document, view) = (document.clone(), view.clone());
        move || {
            let Some(id) = view.reviewing() else { return };
            let mut document = document.borrow_mut();
            let Some(import) = document.ledger().import(id) else { return };
            let mut change = ChangeBuilder::new(review::submit_description(document.ledger(), import), document.ledger());
            if let Err(error) = change.submit_import(id) {
                view.show_error(&format!("Can't submit ‘{}’", import.source), &error.to_string());
                return;
            }
            let change = change.build();
            perform(&mut document, &view, change);
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

/// Performs a change made by the user, unless it's empty, and shows the result. Returns
/// false when it couldn't be saved.
fn perform<S: Store>(document: &mut Document<S>, view: &View, change: Change) -> bool {
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
            eprintln!("Failed to save change: {error}");
            false
        }
    }
}

/// Performs a change to the account with the id `id` (as text) built by `build`; `describe`
/// makes the change's description from the account's name.
fn change_account<S: Store>(
    document: &RefCell<Document<S>>,
    view: &View,
    id: &str,
    describe: impl FnOnce(&str) -> String,
    build: impl FnOnce(&mut ChangeBuilder, AccountId),
) {
    let Ok(id) = Uuid::parse_str(id).map(AccountId) else { return };
    let mut document = document.borrow_mut();
    let Some(account) = document.ledger().accounts().get(id) else { return };
    let mut change = ChangeBuilder::new(describe(&account.name), document.ledger());
    build(&mut change, id);
    let change = change.build();
    perform(&mut document, view, change);
}

/// Replaces the row at `position` of the import under review with what `update` makes of it
/// with the ledger and the row's match, unless that's `None`. `describe` makes the change's
/// description from the row's bank text.
fn update_reviewed_row<S: Store>(
    document: &RefCell<Document<S>>,
    view: &View,
    position: i32,
    describe: impl FnOnce(&str) -> String,
    update: impl FnOnce(&Ledger, &ImportRow, Match) -> Option<ImportRow>,
) {
    let (Some(id), Ok(position)) = (view.reviewing(), usize::try_from(position)) else { return };
    let mut document = document.borrow_mut();
    let ledger = document.ledger();
    let Some(import) = ledger.import(id) else { return };
    let Some(current) = import.rows.get(position) else { return };
    let found = import.matches(ledger)[position];
    let Some(row) = update(ledger, current, found) else { return };
    let mut change = ChangeBuilder::new(describe(&current.line.text), ledger);
    change.update_import_row(id, position, row);
    let change = change.build();
    perform(&mut document, view, change);
}

/// Adds the statements of the import file `file_name` as pending imports in one change, or
/// shows why the file can't be imported.
fn add_imports<S: Store>(document: &RefCell<Document<S>>, view: &View, file_name: &str, text: &str) {
    // There may be a problem on every line; the dialog only has room for so many.
    const SHOWN_PROBLEMS: usize = 20;

    let mut document = document.borrow_mut();
    let imports = match import_file::load(text, file_name, document.ledger()) {
        Ok(imports) => imports,
        Err(error) => {
            view.show_error(&format!("Can't import ‘{file_name}’"), &error.summary(SHOWN_PROBLEMS));
            return;
        }
    };
    let mut change = ChangeBuilder::new(format!("Import ‘{file_name}’"), document.ledger());
    for import in imports {
        change.add_import(import.account, &import.source, import.rows);
    }
    let change = change.build();
    perform(&mut document, view, change);
}
