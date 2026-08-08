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
    pub id: String,
    pub category: String,
    pub description: String,
    pub plugins: Vec<String>,
    /// Where it came from. Shown in the listing, because "reviewed and shipped with the tool"
    /// and "a file I wrote last Tuesday" are different claims about the same menu entry.
    pub source: Source,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub(crate) enum Source {
    /// Embedded at build time from `extras/`, having cleared the bar in CONTRIBUTING.
    Bundled,
    /// A `.list` file the user wrote, beside their plugin list. Answerable to nobody.
    Local,
}

impl Extra {
    /// The comment written above an extra's plugins in `plugins.list`.
    ///
    /// One function because two callers write it — `init --extras` builds a fresh list, the
    /// pane appends to an existing one — and a list where the same extra is labelled two ways
    /// is a list nobody can read.
    pub(crate) fn header(&self) -> String {
        format!("# extra: {} — {}", self.id, self.description)
    }
}

/// `(id, embedded definition)`. Add an extra by dropping `extras/<id>.list` and a line here.
const REGISTRY: &[(&str, &str)] = &[
    ("pluck", include_str!("../extras/pluck.list")),
    ("worktrunk", include_str!("../extras/worktrunk.list")),
];

/// Where a user's own extras live: `extras/` beside their plugin list.
///
/// Beside the list rather than in the config directory, for the reason the lockfile is: someone
/// who moved their list into a dotfiles repo means to keep the things they wrote there too. A
/// hand-written extra is content, not state.
pub(crate) fn local_dir() -> std::path::PathBuf {
    crate::bundle_path().with_file_name("extras")
}

/// Every extra on offer: the bundled ones, plus whatever the user has written.
///
/// A local extra with the same id as a bundled one replaces it. That is the least surprising
/// outcome for the person who wrote the file — but the listing says so rather than letting it
/// happen quietly.
pub(crate) fn all() -> Vec<Extra> {
    let (local, _) = local();
    let mut out: Vec<Extra> = REGISTRY
        .iter()
        .filter(|(id, _)| !local.iter().any(|l| l.id == *id))
        .map(|(id, body)| parse(id, body, Source::Bundled))
        .collect();
    out.extend(local);
    out
}

/// The user's own extras, and the files that could not be used.
///
/// A hand-written file has no CI behind it, so a broken one is reported and skipped rather than
/// crashing the pane or vanishing without explanation. Having no directory at all is the normal
/// case and says nothing.
pub(crate) fn local() -> (Vec<Extra>, Vec<String>) {
    let (mut good, mut bad) = (Vec::new(), Vec::new());
    let Ok(dir) = std::fs::read_dir(local_dir()) else {
        return (good, bad);
    };
    let mut paths: Vec<std::path::PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "list"))
        .collect();
    paths.sort(); // readdir order is not stable, and a menu that reshuffles is a bad menu
    for path in paths {
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(body) = std::fs::read_to_string(&path) else {
            bad.push(format!("{}: could not be read", path.display()));
            continue;
        };
        let e = parse(id, &body, Source::Local);
        if e.plugins.is_empty() {
            bad.push(format!(
                "{}: lists no plugins — one `owner/repo` per line",
                path.display()
            ));
            continue;
        }
        good.push(e);
    }
    (good, bad)
}

/// Is this the id of an extra that ships with the tool?
pub(crate) fn is_bundled(id: &str) -> bool {
    REGISTRY.iter().any(|(eid, _)| *eid == id)
}

/// Look up one extra by id, local first.
pub(crate) fn find(id: &str) -> Option<Extra> {
    all().into_iter().find(|e| e.id == id)
}

/// Parse an extra definition:
///   `# category: <name>`  — the grouping label (defaults to "other" if absent)
///   `# <text>`            — the one-line description (first comment that is not `category:`)
///   `owner/repo`          — one plugin per line; usually one, occasionally a non-overlapping pair
fn parse(id: &str, body: &str, source: Source) -> Extra {
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
        id: id.to_string(),
        category: if category.is_empty() {
            "other".to_string()
        } else {
            category
        },
        description,
        plugins,
        source,
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
            Source::Bundled,
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
        let e = parse("x", "# just a description\nowner/repo\n", Source::Bundled);
        assert_eq!(e.category, "other");
        assert_eq!(e.description, "just a description");
    }

    /// The `category:` line must not be consumed as the description.
    #[test]
    fn the_category_line_is_not_mistaken_for_the_description() {
        let e = parse(
            "x",
            "# category: nav\n# real description\na/b\n",
            Source::Bundled,
        );
        assert_eq!(e.description, "real description");
    }

    #[test]
    fn find_returns_none_for_an_unknown_id() {
        assert!(find("definitely-not-an-extra").is_none());
    }

    /// A hand-written extra reaches the same parser as a bundled one — one format, or the two
    /// drift and a file that works in one place fails in the other.
    #[test]
    fn a_local_extra_is_read_exactly_like_a_bundled_one() {
        let body = "# category: mine\n# the three things I always install\na/one\nb/two\n";
        let local = parse("mine", body, Source::Local);
        let bundled = parse("mine", body, Source::Bundled);
        assert_eq!(local.category, bundled.category);
        assert_eq!(local.description, bundled.description);
        assert_eq!(local.plugins, bundled.plugins);
        assert_eq!(local.source, Source::Local);
    }

    #[test]
    fn bundled_ids_are_recognised() {
        assert!(is_bundled("worktrunk"));
        assert!(!is_bundled("mine"));
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
