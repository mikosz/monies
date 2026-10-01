use std::collections::BTreeMap;

use uuid::Uuid;

/// Separates category names in a path, e.g. `dogs.health.pills`.
pub const SEPARATOR: char = '.';

/// Identifies a category. UUIDv7, so it can be created on any device without coordination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CategoryId(pub Uuid);

impl CategoryId {
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug, Clone)]
struct Category {
    name: String,
    parent: Option<CategoryId>,
}

/// A validated category path such as `dogs.health.pills`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryPath(Vec<String>);

impl CategoryPath {
    /// Splits the input on [`SEPARATOR`]. Whitespace around each name is ignored and empty
    /// names are skipped, so `.bills.` is the same as `bills`. Returns `None` when no names
    /// remain.
    pub fn parse(input: &str) -> Option<Self> {
        let names: Vec<String> = input
            .split(SEPARATOR)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        (!names.is_empty()).then_some(Self(names))
    }

    pub fn names(&self) -> &[String] {
        &self.0
    }
}

/// Categories that have to be created for a path: a chain in which the first name is a child
/// of `parent` (top-level when `None`) and each following name is a child of the previous one.
///
/// When `names` is empty, the whole path exists and `parent` is the category it refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingCategories<'a> {
    pub parent: Option<CategoryId>,
    pub names: &'a [String],
}

/// The category tree. Categories are referred to by [`CategoryId`], so renaming or moving
/// a category is reflected everywhere it's used.
///
/// Names are matched case-insensitively; a category keeps the spelling it was created with.
#[derive(Debug, Clone, Default)]
pub struct Categories {
    categories: BTreeMap<CategoryId, Category>,
}

impl Categories {
    /// Adds a category. A parent may be inserted after its children (e.g. when loading), but
    /// must exist before the tree is queried.
    pub fn insert(&mut self, id: CategoryId, name: String, parent: Option<CategoryId>) {
        self.categories.insert(id, Category { name, parent });
    }

    /// Removes a category. Its children and entries must have been removed already.
    pub fn remove(&mut self, id: CategoryId) {
        self.categories.remove(&id);
    }

    pub fn find(&self, path: &CategoryPath) -> Option<CategoryId> {
        match self.missing(path) {
            MissingCategories { parent, names: [] } => parent,
            _ => None,
        }
    }

    /// Which categories of `path` don't exist yet, see [`MissingCategories`].
    pub fn missing<'a>(&self, path: &'a CategoryPath) -> MissingCategories<'a> {
        let mut parent = None;
        for (index, name) in path.names().iter().enumerate() {
            match self.find_child(parent, name) {
                Some(id) => parent = Some(id),
                None => return MissingCategories { parent, names: &path.names()[index..] },
            }
        }
        MissingCategories { parent, names: &[] }
    }

    /// Full path of the category, e.g. `dogs.health.pills`.
    pub fn path(&self, id: CategoryId) -> String {
        let mut names: Vec<&str> = self.ancestors_and_self(id).map(|c| c.name.as_str()).collect();
        names.reverse();
        names.join(&SEPARATOR.to_string())
    }

    /// Suggests full category paths for a partially typed path.
    ///
    /// The query is split at its last separator into a parent path and a fragment. Children
    /// of the parent (or top-level categories when there's no separator) whose name contains
    /// the fragment come first, followed by matching categories deeper in the parent's subtree.
    /// Within each group, names starting with the fragment come first, then alphabetically.
    /// Empty names in the parent path are skipped, as in [`CategoryPath::parse`].
    pub fn suggest(&self, query: &str) -> Vec<String> {
        let (parent, fragment) = match query.rsplit_once(SEPARATOR) {
            Some((parent, fragment)) => match CategoryPath::parse(parent) {
                None => (None, fragment),
                Some(path) => match self.find(&path) {
                    Some(parent) => (Some(parent), fragment),
                    None => return Vec::new(),
                },
            },
            None => (None, query),
        };
        let fragment = fragment.trim().to_lowercase();

        let mut level = Vec::new();
        let mut deeper = Vec::new();
        for (&id, category) in &self.categories {
            let name = category.name.to_lowercase();
            if !name.contains(&fragment) {
                continue;
            }
            let key = (!name.starts_with(&fragment), self.path(id));
            if category.parent == parent {
                level.push(key);
            } else if !fragment.is_empty() && self.is_descendant(id, parent) {
                deeper.push(key);
            }
        }

        let sort = |keys: &mut Vec<(bool, String)>| {
            keys.sort_by_cached_key(|(not_prefix, path)| (*not_prefix, path.to_lowercase()));
        };
        sort(&mut level);
        sort(&mut deeper);
        level.into_iter().chain(deeper).map(|(_, path)| path).collect()
    }

    fn find_child(&self, parent: Option<CategoryId>, name: &str) -> Option<CategoryId> {
        let name = name.to_lowercase();
        self.categories
            .iter()
            .find(|(_, c)| c.parent == parent && c.name.to_lowercase() == name)
            .map(|(&id, _)| id)
    }

    fn ancestors_and_self(&self, id: CategoryId) -> impl Iterator<Item = &Category> {
        std::iter::successors(Some(&self.categories[&id]), |c| {
            c.parent.map(|parent| &self.categories[&parent])
        })
    }

    /// Whether `id` lies in the subtree of `ancestor`; everything lies under the root (`None`).
    fn is_descendant(&self, id: CategoryId, ancestor: Option<CategoryId>) -> bool {
        match ancestor {
            None => true,
            Some(ancestor) => std::iter::successors(self.categories[&id].parent, |parent| {
                self.categories[parent].parent
            })
            .any(|parent| parent == ancestor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(input: &str) -> CategoryPath {
        CategoryPath::parse(input).unwrap()
    }

    impl Categories {
        /// Creates missing categories.
        fn get_or_create(&mut self, path: &CategoryPath) -> CategoryId {
            let missing = self.missing(path);
            let mut parent = missing.parent;
            for name in missing.names {
                let id = CategoryId::generate();
                self.insert(id, name.clone(), parent);
                parent = Some(id);
            }
            parent.expect("category path is never empty")
        }
    }

    fn categories(paths: &[&str]) -> Categories {
        let mut categories = Categories::default();
        for p in paths {
            categories.get_or_create(&path(p));
        }
        categories
    }

    #[test]
    fn parses_paths() {
        assert_eq!(path("bills").names(), ["bills"]);
        assert_eq!(path(" dogs . health.pills ").names(), ["dogs", "health", "pills"]);
    }

    #[test]
    fn skips_empty_names() {
        for input in ["bills.", "bills..", ".bills", "..bills.", " . bills . "] {
            assert_eq!(path(input).names(), ["bills"], "input: {input:?}");
        }
        assert_eq!(path("dogs..pills").names(), ["dogs", "pills"]);
    }

    #[test]
    fn rejects_empty_paths() {
        for input in ["", "  ", ".", "...", " . "] {
            assert_eq!(CategoryPath::parse(input), None, "input: {input:?}");
        }
    }

    #[test]
    fn reports_missing_categories() {
        let categories = categories(&["dogs.health"]);
        let dogs = categories.find(&path("dogs")).unwrap();
        let health = categories.find(&path("dogs.health")).unwrap();

        let pills = path("dogs.health.pills");
        assert_eq!(categories.missing(&pills), MissingCategories { parent: Some(health), names: &pills.names()[2..] });
        let food = path("Dogs.food.dry");
        assert_eq!(categories.missing(&food), MissingCategories { parent: Some(dogs), names: &food.names()[1..] });
        let cats = path("cats.food");
        assert_eq!(categories.missing(&cats), MissingCategories { parent: None, names: cats.names() });
        let existing = path("dogs.HEALTH");
        assert_eq!(categories.missing(&existing), MissingCategories { parent: Some(health), names: &[] });
    }

    #[test]
    fn allows_parents_inserted_after_children() {
        let (bills, rent) = (CategoryId::generate(), CategoryId::generate());
        let mut categories = Categories::default();
        categories.insert(rent, "rent".to_owned(), Some(bills));
        categories.insert(bills, "bills".to_owned(), None);
        assert_eq!(categories.path(rent), "bills.rent");
        assert_eq!(categories.find(&path("bills.rent")), Some(rent));

        categories.remove(rent);
        assert_eq!(categories.find(&path("bills.rent")), None);
    }

    #[test]
    fn finds_categories_case_insensitively() {
        let categories = categories(&["Bills.Rent"]);
        let rent = categories.find(&path("BILLS.RENT")).unwrap();
        assert_eq!(categories.path(rent), "Bills.Rent");
    }

    #[test]
    fn same_name_under_different_parents_is_different_category() {
        let mut categories = Categories::default();
        let dog_food = categories.get_or_create(&path("dogs.food"));
        let cat_food = categories.get_or_create(&path("cats.food"));
        assert_ne!(dog_food, cat_food);
        assert_eq!(categories.find(&path("food")), None);
    }

    #[test]
    fn suggests_top_level_for_empty_query() {
        let categories = categories(&["food", "bills.rent", "dogs.health"]);
        assert_eq!(categories.suggest(""), ["bills", "dogs", "food"]);
    }

    #[test]
    fn suggests_prefix_matches_first_then_deeper_matches() {
        let categories = categories(&["food", "dogs.health.pills", "bills.dentist", "cards"]);
        assert_eq!(categories.suggest("d"), ["dogs", "cards", "food", "bills.dentist"]);
    }

    #[test]
    fn suggests_children_after_separator() {
        let categories = categories(&["dogs.health.pills", "dogs.food", "food"]);
        assert_eq!(categories.suggest("dogs."), ["dogs.food", "dogs.health"]);
        assert_eq!(categories.suggest("Dogs.he"), ["dogs.health"]);
        assert_eq!(categories.suggest("dogs.pi"), ["dogs.health.pills"]);
    }

    #[test]
    fn suggests_deep_matches_by_name() {
        let categories = categories(&["dogs.health.pills", "bills.rent"]);
        assert_eq!(categories.suggest("pills"), ["dogs.health.pills"]);
    }

    #[test]
    fn suggests_nothing_for_unknown_parent() {
        let categories = categories(&["dogs.food"]);
        assert!(categories.suggest("cats.").is_empty());
    }

    #[test]
    fn suggests_top_level_for_empty_parent() {
        let categories = categories(&["dogs.food", "bills"]);
        assert_eq!(categories.suggest("."), ["bills", "dogs"]);
        assert_eq!(categories.suggest(".b"), categories.suggest("b"));
        assert_eq!(categories.suggest("dogs..f"), ["dogs.food"]);
    }
}
