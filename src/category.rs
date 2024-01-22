/// Separates category names in a path, e.g. `dogs.health.pills`.
pub const SEPARATOR: char = '.';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CategoryId(usize);

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

/// The category tree. Categories are referred to by [`CategoryId`], so renaming or moving
/// a category is reflected everywhere it's used.
///
/// Names are matched case-insensitively; a category keeps the spelling it was created with.
#[derive(Debug, Default)]
pub struct Categories {
    categories: Vec<Category>,
}

impl Categories {
    pub fn find(&self, path: &CategoryPath) -> Option<CategoryId> {
        path.names()
            .iter()
            .try_fold(None, |parent, name| self.find_child(parent, name).map(Some))
            .flatten()
    }

    /// Returns the category at `path`, creating it and any missing ancestors.
    pub fn get_or_create(&mut self, path: &CategoryPath) -> CategoryId {
        let mut parent = None;
        for name in path.names() {
            let id = match self.find_child(parent, name) {
                Some(id) => id,
                None => {
                    self.categories.push(Category { name: name.clone(), parent });
                    CategoryId(self.categories.len() - 1)
                }
            };
            parent = Some(id);
        }
        parent.expect("category path is never empty")
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
        for (index, category) in self.categories.iter().enumerate() {
            let id = CategoryId(index);
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
            .position(|c| c.parent == parent && c.name.to_lowercase() == name)
            .map(CategoryId)
    }

    fn ancestors_and_self(&self, id: CategoryId) -> impl Iterator<Item = &Category> {
        std::iter::successors(Some(&self.categories[id.0]), |c| {
            c.parent.map(|parent| &self.categories[parent.0])
        })
    }

    /// Whether `id` lies in the subtree of `ancestor`; everything lies under the root (`None`).
    fn is_descendant(&self, id: CategoryId, ancestor: Option<CategoryId>) -> bool {
        match ancestor {
            None => true,
            Some(ancestor) => std::iter::successors(self.categories[id.0].parent, |parent| {
                self.categories[parent.0].parent
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
    fn creates_missing_ancestors() {
        let mut categories = Categories::default();
        let pills = categories.get_or_create(&path("dogs.health.pills"));
        assert_eq!(categories.path(pills), "dogs.health.pills");
        assert!(categories.find(&path("dogs")).is_some());
        assert!(categories.find(&path("dogs.health")).is_some());
    }

    #[test]
    fn reuses_existing_categories_case_insensitively() {
        let mut categories = Categories::default();
        let rent = categories.get_or_create(&path("Bills.Rent"));
        assert_eq!(categories.get_or_create(&path("bills.rent")), rent);
        assert_eq!(categories.find(&path("BILLS.RENT")), Some(rent));
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
