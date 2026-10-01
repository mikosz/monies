use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString, VecModel};

use crate::category::Categories;
use crate::date_format::DateFormat;
use crate::entry::{self, Entry, EntryId, format_amount};
use crate::ledger::Ledger;
use crate::listing::{ListItem, Splice, list_items, splice};
use crate::{EntriesStore, EntryKind, EntryRow, MainWindow, RowType};

/// Mirrors the ledger into the UI.
pub struct View {
    window: slint::Weak<MainWindow>,
    date_format: DateFormat,
    /// The list (see `list_items`) followed by the row for adding a new entry.
    rows: Rc<VecModel<EntryRow>>,
    new_entry_id: SharedString,
    /// Suggestions for `category_query`.
    category_suggestions: Rc<VecModel<SharedString>>,
    /// The text last reported by a category input (on focus or edit), i.e. of the one in use.
    category_query: RefCell<String>,
}

impl View {
    pub fn new(window: &MainWindow, date_format: DateFormat) -> Self {
        let store = window.global::<EntriesStore>();
        let view = Self {
            window: window.as_weak(),
            date_format,
            rows: Rc::new(VecModel::default()),
            new_entry_id: store.get_new_entry_id(),
            category_suggestions: Rc::new(VecModel::default()),
            category_query: RefCell::default(),
        };
        store.set_date_placeholder(view.date_format.placeholder().into());
        store.set_entries(view.rows.clone().into());
        store.set_category_suggestions(view.category_suggestions.clone().into());
        view
    }

    pub fn date_format(&self) -> &DateFormat {
        &self.date_format
    }

    /// Shows the ledger, updating only the rows that changed.
    pub fn show(&self, ledger: &Ledger) {
        let new_entry = EntryRow { id: self.new_entry_id.clone(), row_type: RowType::NewEntry, ..Default::default() };
        let rows: Vec<EntryRow> = list_items(ledger)
            .into_iter()
            .map(|item| match item {
                ListItem::Period(start) => EntryRow {
                    row_type: RowType::Month,
                    title: start.format("%B %Y").to_string().into(),
                    ..Default::default()
                },
                ListItem::Entry(id, entry) => self.entry_row(id, entry, ledger.categories()),
            })
            .chain([new_entry])
            .collect();

        let old: Vec<EntryRow> = self.rows.iter().collect();
        let Splice { start, remove, insert } = splice(&old, rows);
        let replaced = remove.min(insert.len());
        let mut insert = insert.into_iter();
        for (offset, row) in insert.by_ref().take(replaced).enumerate() {
            self.rows.set_row_data(start + offset, row);
        }
        for _ in replaced..remove {
            self.rows.remove(start + replaced);
        }
        for (offset, row) in insert.enumerate() {
            self.rows.insert(start + replaced + offset, row);
        }

        self.show_last_date(ledger);
        self.suggest_categories(ledger);
    }

    /// Updates the category suggestions for the text of a category input.
    pub fn query_categories(&self, ledger: &Ledger, text: &str) {
        self.category_query.replace(text.to_owned());
        self.suggest_categories(ledger);
    }

    fn entry_row(&self, id: EntryId, entry: &Entry, categories: &Categories) -> EntryRow {
        EntryRow {
            row_type: RowType::Entry,
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
            ..Default::default()
        }
    }

    fn show_last_date(&self, ledger: &Ledger) {
        // The ledger is in the order entries were added, so its last one is the last *entered*.
        // Without any, the date input is prefilled with today.
        let date = match ledger.entries().last() {
            Some((_, entry)) => entry.date,
            None => chrono::Local::now().date_naive(),
        };
        if let Some(window) = self.window.upgrade() {
            window.global::<EntriesStore>().set_last_date(self.date_format.format(date).into());
        }
    }

    fn suggest_categories(&self, ledger: &Ledger) {
        let suggestions = ledger.categories().suggest(&self.category_query.borrow());
        let suggestions = suggestions.into_iter().map(SharedString::from);
        self.category_suggestions.set_vec(suggestions.collect::<Vec<_>>());
    }
}
