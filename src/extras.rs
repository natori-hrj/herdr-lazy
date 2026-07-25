//! Extras — opt-in, per-capability plugin picks, grouped by category.
//!
//! Where the default bundle (`init`) is one curated baseline, an extra is a single capability
//! you ask for by name — `init --extras worktrunk` — for the jobs the default set deliberately
//! leaves to you (which worktree tool, which notifier). Each extra is one coherent choice, not
//! a category dump: two plugins that do the same job are two extras you pick between, never a
//! stack. See CONTRIBUTING.md for the bar a new extra clears.
//!
//! Definitions are plain `owner/repo` lists embedded at build time, so listing or resolving an
//! extra never touches the network. Adding one is a data change: drop a file in `extras/` and
//! add a line to `REGISTRY` — the file is the definition, the line makes it visible.

/// A named, opt-in capability: one entry in the extras menu.
pub(crate) struct Extra {
    pub id: &'static str,
    pub category: String,
    pub description: String,
    pub plugins: Vec<String>,
}

/// `(id, embedded definition)`. Add an extra by dropping `extras/<id>.list` and a line here.
const REGISTRY: &[(&str, &str)] = &[
    ("pluck", include_str!("../extras/pluck.list")),
    ("worktrunk", include_str!("../extras/worktrunk.list")),
];

/// All extras, in registry order, parsed from their embedded definitions.
pub(crate) fn all() -> Vec<Extra> {
    REGISTRY.iter().map(|(id, body)| parse(id, body)).collect()
}

/// Look up one extra by id.
pub(crate) fn find(id: &str) -> Option<Extra> {
    REGISTRY
        .iter()
        .find(|(eid, _)| *eid == id)
        .map(|(eid, body)| parse(eid, body))
}

/// Parse an extra definition:
///   `# category: <name>`  — the grouping label (defaults to "other" if absent)
///   `# <text>`            — the one-line description (first comment that is not `category:`)
///   `owner/repo`          — one plugin per line; usually one, occasionally a non-overlapping pair
fn parse(id: &'static str, body: &str) -> Extra {
    let mut category = String::new();
    let mut description = String::new();
    let mut plugins = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim();
            if let Some(cat) = comment.strip_prefix("category:") {
                category = cat.trim().to_string();
            } else if description.is_empty() {
                description = comment.to_string();
            }
        } else {
            plugins.push(line.to_string());
        }
    }
    Extra {
        id,
        category: if category.is_empty() {
            "other".to_string()
        } else {
            category
        },
        description,
        plugins,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_category_description_and_plugins() {
        let e = parse(
            "x",
            "# category: worktree\n# does a thing\nowner/repo\nowner/two\n",
        );
        assert_eq!(e.category, "worktree");
        assert_eq!(e.description, "does a thing");
        assert_eq!(
            e.plugins,
            vec!["owner/repo".to_string(), "owner/two".to_string()]
        );
    }

    #[test]
    fn a_missing_category_defaults_to_other() {
        let e = parse("x", "# just a description\nowner/repo\n");
        assert_eq!(e.category, "other");
        assert_eq!(e.description, "just a description");
    }

    /// The `category:` line must not be consumed as the description.
    #[test]
    fn the_category_line_is_not_mistaken_for_the_description() {
        let e = parse("x", "# category: nav\n# real description\na/b\n");
        assert_eq!(e.description, "real description");
    }

    #[test]
    fn find_returns_none_for_an_unknown_id() {
        assert!(find("definitely-not-an-extra").is_none());
    }

    /// Every embedded seed file must parse into a usable extra — this fails the test step if a
    /// data file is malformed or a `REGISTRY` line points at nothing.
    #[test]
    fn every_registered_extra_is_well_formed() {
        let all = all();
        assert!(!all.is_empty());
        for e in &all {
            assert!(!e.id.is_empty());
            assert!(!e.category.is_empty(), "{} has no category", e.id);
            assert!(!e.description.is_empty(), "{} has no description", e.id);
            assert!(!e.plugins.is_empty(), "{} lists no plugins", e.id);
            for p in &e.plugins {
                assert!(p.contains('/'), "{}: '{}' is not owner/repo", e.id, p);
            }
        }
    }
}
