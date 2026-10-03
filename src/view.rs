use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::{ComponentHandle, Model, SharedString, VecModel};

use crate::account::AccountId;
use crate::currency::Currency;
use crate::date_format::DateFormat;
use crate::duplicates::{Match, MatchCounts};
use crate::entry::{self, Entry, EntryId, format_amount};
use crate::import::{Import, ImportId, LinkKind, RowStatus};
use crate::ledger::Ledger;
use crate::listing::{ListItem, Splice, list_items, splice};
use crate::review::{self, Counts, RelatedEntry};
use crate::{
    AccountRow, Alert, EntriesStore, EntryKind, EntryRow, ImportsStore, MainWindow, PendingImport, ReviewEntry,
    ReviewRow, ReviewStatus, ReviewedImport, RowType, SettingsStore,
};

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
    /// Suggestions for `account_query`.
    account_suggestions: Rc<VecModel<SharedString>>,
    /// The text last reported by an account input, like `category_query`.
    account_query: RefCell<String>,
    /// The account of the entry added last since the app started; adding continues with it.
    /// Not saved: the next session starts by asking for the account.
    session_account: Cell<Option<AccountId>>,
    /// All accounts followed by the row for adding a new account.
    accounts: Rc<VecModel<AccountRow>>,
    new_account_id: SharedString,
    currency_suggestions: Rc<VecModel<SharedString>>,
    /// Pending imports of accounts that aren't deleted.
    imports: Rc<VecModel<PendingImport>>,
    /// The pending import under review, if any.
    reviewing: Cell<Option<ImportId>>,
    /// The rows of the import under review.
    review_rows: Rc<VecModel<ReviewRow>>,
}

impl View {
    pub fn new(window: &MainWindow, date_format: DateFormat) -> Self {
        let store = window.global::<EntriesStore>();
        let settings = window.global::<SettingsStore>();
        let view = Self {
            window: window.as_weak(),
            date_format,
            rows: Rc::new(VecModel::default()),
            new_entry_id: store.get_new_entry_id(),
            category_suggestions: Rc::new(VecModel::default()),
            category_query: RefCell::default(),
            account_suggestions: Rc::new(VecModel::default()),
            account_query: RefCell::default(),
            session_account: Cell::default(),
            accounts: Rc::new(VecModel::default()),
            new_account_id: settings.get_new_account_id(),
            currency_suggestions: Rc::new(VecModel::default()),
            imports: Rc::new(VecModel::default()),
            reviewing: Cell::default(),
            review_rows: Rc::new(VecModel::default()),
        };
        store.set_date_placeholder(view.date_format.placeholder().into());
        store.set_entries(view.rows.clone().into());
        store.set_category_suggestions(view.category_suggestions.clone().into());
        store.set_account_suggestions(view.account_suggestions.clone().into());
        settings.set_accounts(view.accounts.clone().into());
        settings.set_currency_suggestions(view.currency_suggestions.clone().into());
        let imports = window.global::<ImportsStore>();
        imports.set_imports(view.imports.clone().into());
        imports.set_review_rows(view.review_rows.clone().into());
        view
    }

    pub fn date_format(&self) -> &DateFormat {
        &self.date_format
    }

    /// Shows the ledger's entries, accounts and pending imports, and the import under review,
    /// updating only the rows that changed.
    pub fn show(&self, ledger: &Ledger) {
        let new_entry = EntryRow { id: self.new_entry_id.clone(), row_type: RowType::NewEntry, ..Default::default() };
        // Entries alternate in shade, starting afresh with every month.
        let mut odd = false;
        let rows: Vec<EntryRow> = list_items(ledger)
            .into_iter()
            .map(|item| match item {
                ListItem::Period(start) => {
                    odd = false;
                    EntryRow { row_type: RowType::Month, title: start.format("%B %Y").to_string().into(), ..Default::default() }
                }
                ListItem::Entry(id, entry) => {
                    let row = EntryRow { odd, ..self.entry_row(id, entry, ledger) };
                    odd = !odd;
                    row
                }
            })
            .chain([new_entry])
            .collect();
        update_model(&self.rows, rows);

        self.show_last_date(ledger);
        self.suggest_categories(ledger);
        self.suggest_accounts(ledger);
        self.show_session_account(ledger);
        self.show_accounts(ledger);
        self.show_imports(ledger);
        self.show_review(ledger);
    }

    /// Called after an entry was added to `account` (and shown): the inputs are cleared, and
    /// the next entry goes to the same account.
    pub fn entry_added(&self, ledger: &Ledger, account: AccountId) {
        self.query_categories(ledger, "");
        self.session_account.set(Some(account));
        self.show_session_account(ledger);
    }

    /// Updates the category suggestions for the text of a category input.
    pub fn query_categories(&self, ledger: &Ledger, text: &str) {
        self.category_query.replace(text.to_owned());
        self.suggest_categories(ledger);
    }

    /// Updates the account suggestions for the text of an account input.
    pub fn query_accounts(&self, ledger: &Ledger, text: &str) {
        self.account_query.replace(text.to_owned());
        self.suggest_accounts(ledger);
    }

    /// Updates the currency suggestions for the text of a currency input.
    pub fn query_currencies(&self, text: &str) {
        let suggestions = Currency::suggest(text).into_iter().map(SharedString::from);
        self.currency_suggestions.set_vec(suggestions.collect::<Vec<_>>());
    }

    /// The pending import under review, if any.
    pub fn reviewing(&self) -> Option<ImportId> {
        self.reviewing.get()
    }

    /// Reviews the import, or goes back to the list of imports with `None`.
    pub fn review(&self, ledger: &Ledger, id: Option<ImportId>) {
        self.reviewing.set(id);
        self.show_review(ledger);
    }

    /// Shows a message in a dialog over the window until the user dismisses it.
    pub fn show_error(&self, title: &str, message: &str) {
        if let Some(window) = self.window.upgrade() {
            let alert = window.global::<Alert>();
            alert.set_title(title.into());
            alert.set_message(message.into());
        }
    }

    fn entry_row(&self, id: EntryId, entry: &Entry, ledger: &Ledger) -> EntryRow {
        let account = ledger.accounts().get(entry.account).expect("entries refer to existing accounts");
        EntryRow {
            row_type: RowType::Entry,
            id: id.0.to_string().into(),
            account: account.name.as_str().into(),
            date: self.date_format.format(entry.date).into(),
            name: entry.name.as_str().into(),
            category: ledger.categories().path(entry.category).into(),
            amount: format_amount(entry.amount, account.currency.decimals()).into(),
            amount_display: account.currency.format_magnitude(entry.amount).into(),
            kind: entry_kind(entry.kind()),
            imported: entry.statement.is_some(),
            ..Default::default()
        }
    }

    fn show_last_date(&self, ledger: &Ledger) {
        // The ledger is in the order entries were added, so its last one is the last *entered*.
        // Entries of deleted accounts aren't shown, so they don't count. Without any, the date
        // input is prefilled with today.
        let date = match ledger.visible_entries().next_back() {
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

    fn suggest_accounts(&self, ledger: &Ledger) {
        let suggestions = ledger.accounts().suggest(&self.account_query.borrow());
        let suggestions = suggestions.into_iter().map(SharedString::from);
        self.account_suggestions.set_vec(suggestions.collect::<Vec<_>>());
    }

    /// Shows the current name of the session's account, which may have been renamed since,
    /// or "" when there's none or it's been deleted.
    fn show_session_account(&self, ledger: &Ledger) {
        let account = self.session_account.get().and_then(|id| ledger.accounts().get(id));
        let name = account.filter(|account| !account.deleted).map_or("", |account| account.name.as_str());
        if let Some(window) = self.window.upgrade() {
            window.global::<EntriesStore>().set_session_account(name.into());
        }
    }

    fn show_accounts(&self, ledger: &Ledger) {
        let new_account = AccountRow { id: self.new_account_id.clone(), ..Default::default() };
        let rows: Vec<AccountRow> = ledger
            .accounts()
            .iter()
            .map(|(id, account)| AccountRow {
                id: id.0.to_string().into(),
                name: account.name.as_str().into(),
                currency: account.currency.code().into(),
                entries: i32::try_from(ledger.entry_count(id)).unwrap_or(i32::MAX),
                deleted: account.deleted,
            })
            .chain([new_account])
            .collect();
        update_model(&self.accounts, rows);

        if let Some(window) = self.window.upgrade() {
            let settings = window.global::<SettingsStore>();
            settings.set_has_accounts(ledger.accounts().first_active().is_some());
            settings.set_has_deleted_accounts(ledger.accounts().iter().any(|(_, account)| account.deleted));
            // With a single account, it's implied: entries don't show it.
            let several = ledger.accounts().active().nth(1).is_some();
            window.global::<EntriesStore>().set_several_accounts(several);
        }
    }

    /// Shows the pending imports with how their lines match the entries now, which changes as
    /// entries are added, edited or deleted.
    fn show_imports(&self, ledger: &Ledger) {
        let rows: Vec<PendingImport> = ledger
            .visible_imports()
            .map(|(id, import)| {
                let account = ledger.accounts().get(import.account).expect("imports refer to existing accounts");
                let counts = MatchCounts::of(&import.matches(ledger));
                let created = import.created.with_timezone(&chrono::Local);
                PendingImport {
                    id: id.0.to_string().into(),
                    account: account.name.as_str().into(),
                    source: import.source.as_str().into(),
                    created: format!("{} {}", self.date_format.format(created.date_naive()), created.format("%H:%M")).into(),
                    new: count(counts.new),
                    duplicates: count(counts.duplicates),
                    possible: count(counts.possible),
                }
            })
            .collect();
        update_model(&self.imports, rows);
    }

    /// Shows the import under review with how its lines match the entries now, like
    /// `show_imports`. Goes back to the list once the import is gone, or hidden with its
    /// account.
    fn show_review(&self, ledger: &Ledger) {
        let reviewed = self.reviewing.get().and_then(|id| ledger.visible_imports().find(|&(other, _)| other == id));
        let (review, rows) = match reviewed {
            Some((id, import)) => self.review_of(ledger, id, import),
            None => {
                self.reviewing.set(None);
                (ReviewedImport::default(), Vec::new())
            }
        };
        update_model(&self.review_rows, rows);
        if let Some(window) = self.window.upgrade() {
            window.global::<ImportsStore>().set_review(review);
        }
    }

    fn review_of(&self, ledger: &Ledger, id: ImportId, import: &Import) -> (ReviewedImport, Vec<ReviewRow>) {
        let account = ledger.accounts().get(import.account).expect("imports refer to existing accounts");
        let matches = import.matches(ledger);
        // Rows whose line is certainly in Monies already come last, in a section of their own;
        // both parts are in statement order.
        let (duplicates, others): (Vec<_>, Vec<_>) = import
            .rows
            .iter()
            .zip(&matches)
            .enumerate()
            .partition(|(_, (_, found))| matches!(found, Match::Duplicate(_)));
        let mut rows = Vec::with_capacity(import.rows.len());
        // Rows with an entry take two lines.
        let mut lines_above = 0;
        for (position, (row, &found)) in others.into_iter().chain(duplicates) {
            let duplicate = matches!(found, Match::Duplicate(_));
            let related = review::related_entry(ledger, import.account, row, found);
            let used = !duplicate && row.uses_imported_data();
            rows.push(ReviewRow {
                position: count(position),
                // Left out whatever their status, see `ChangeBuilder::submit_import`.
                status: match row.status {
                    _ if duplicate => ReviewStatus::Skip,
                    RowStatus::Pending => ReviewStatus::Pending,
                    RowStatus::Accepted => ReviewStatus::Add,
                    RowStatus::Skipped => ReviewStatus::Skip,
                    RowStatus::Linked(_, LinkKind::KeepEntry) => ReviewStatus::KeepEntry,
                    RowStatus::Linked(_, LinkKind::UseImported) => ReviewStatus::UseImported,
                },
                duplicate,
                linkable: review::link_target(ledger, found).is_some(),
                date: self.date_format.format(row.line.date).into(),
                name: row.name.as_str().into(),
                category: row.category.as_ref().map(ToString::to_string).unwrap_or_default().into(),
                amount: account.currency.format_magnitude(row.line.amount).into(),
                kind: entry_kind(entry::EntryKind::of(row.line.amount)),
                missing_name: used && row.name.trim().is_empty(),
                missing_category: used && row.category.is_none(),
                has_entry: related.is_some(),
                entry: related.map(|related| self.review_entry(ledger, related)).unwrap_or_default(),
                lines_above: count(lines_above),
            });
            lines_above += if related.is_some() { 2 } else { 1 };
        }

        let counts = Counts::of(&import.rows, &matches);
        let review = ReviewedImport {
            id: id.0.to_string().into(),
            account: account.name.as_str().into(),
            source: import.source.as_str().into(),
            to_add: count(counts.to_add),
            skipped: count(counts.skipped),
            linked: count(counts.linked),
            to_review: count(counts.to_review),
            duplicates: count(counts.duplicates),
            duplicates_title: review::duplicates_title(counts.duplicates).into(),
            submit_label: review::submit_label(counts).into(),
            submit_hint: review::submit_hint(ledger, import).unwrap_or_default().into(),
        };
        (review, rows)
    }

    fn review_entry(&self, ledger: &Ledger, related: RelatedEntry) -> ReviewEntry {
        let label = related.label.into();
        let Some(entry) = related.entry else { return ReviewEntry { label, ..Default::default() } };
        let currency = ledger.accounts().get(entry.account).expect("entries refer to existing accounts").currency;
        ReviewEntry {
            label,
            exists: true,
            date: self.date_format.format(entry.date).into(),
            name: entry.name.as_str().into(),
            category: ledger.categories().path(entry.category).into(),
            amount: currency.format_magnitude(entry.amount).into(),
            kind: entry_kind(entry.kind()),
            imported: entry.statement.is_some(),
        }
    }
}

/// A count for the UI, whose integers are 32-bit.
fn count(count: usize) -> i32 {
    i32::try_from(count).unwrap_or(i32::MAX)
}

fn entry_kind(kind: entry::EntryKind) -> EntryKind {
    match kind {
        entry::EntryKind::Expense => EntryKind::Expense,
        entry::EntryKind::Income => EntryKind::Income,
        entry::EntryKind::Neutral => EntryKind::Neutral,
    }
}

/// Turns the model's rows into `rows`, changing only those that differ (see [`splice`]), so
/// the UI keeps the state of the others (e.g. an open editor).
fn update_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let old: Vec<T> = model.iter().collect();
    let Splice { start, remove, insert } = splice(&old, rows);
    let replaced = remove.min(insert.len());
    let mut insert = insert.into_iter();
    for (offset, row) in insert.by_ref().take(replaced).enumerate() {
        model.set_row_data(start + offset, row);
    }
    for _ in replaced..remove {
        model.remove(start + replaced);
    }
    for (offset, row) in insert.enumerate() {
        model.insert(start + replaced + offset, row);
    }
}
