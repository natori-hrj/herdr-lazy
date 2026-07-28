//! Sorting the marketplace into categories, so it can be browsed and not only searched.
//!
//! GitHub topics cannot do this job. Half the published plugins carry no usable topic at all
//! (only `herdr` / `herdr-plugin`), and the ones that exist mix purpose with technology —
//! `worktree` next to `rust` and `ratatui`. Measured against the index, worktree plugins are
//! 13 by topic and 31 by what their name and description actually say.
//!
//! So a category is a set of keywords matched against name, description and topics. That is
//! the opposite choice from `extras`, deliberately: an extra is hand-picked because install
//! quality has to be certain, while browsing wants coverage of the long tail. A plugin landing
//! in a slightly odd category costs a moment; a plugin nobody can find costs everything.
//!
//! Categories share their names with the extras vocabulary so the two views agree, even though
//! membership is decided differently.

use crate::registry::Entry;

/// `(category, keywords)`, in priority order — the first category with a hit wins.
///
/// Order is the whole design. Specific purposes come before broad ones: nearly every plugin
/// mentions a pane or a workspace somewhere, so those keywords would swallow the list if they
/// were consulted first.
pub(crate) const CATEGORIES: &[(&str, &[&str])] = &[
    ("worktree", &["worktree", "worktrunk"]),
    (
        "notifications",
        &["notification", "notify", "ntfy", "alert", "blocked", "idle"],
    ),
    (
        "keybindings",
        &[
            "keymap",
            "keybinding",
            "which-key",
            "whichkey",
            "which key",
            "hotkey",
            "shortcut",
            "palette",
        ],
    ),
    (
        "navigation",
        &[
            "picker", "pick", "fuzzy", "fzf", "jump", "switch", "pluck", "grep", "search",
            "navigat",
        ],
    ),
    (
        "review",
        &["review", "diff", "pull request", "commit", "github"],
    ),
    ("files", &["file", "viewer", "tree", "directory", "sidebar"]),
    (
        "status",
        &["status", "monitor", "dashboard", "live", "progress"],
    ),
    (
        "session",
        &["session", "tmux", "resurrect", "persist", "restore"],
    ),
    (
        "workspace",
        &["workspace", "project", "layout", "template", "tab", "pane"],
    ),
    (
        "agents",
        &["agent", "claude", "codex", "coding", "llm", "prompt"],
    ),
];

/// Where a plugin that matches nothing goes. Named, not dropped: a bucket you can open is the
/// difference between "this filter is incomplete" and "my plugin has vanished".
pub(crate) const OTHER: &str = "other";

/// Every category name, in display order, with `other` last.
pub(crate) fn names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = CATEGORIES.iter().map(|(c, _)| *c).collect();
    v.push(OTHER);
    v
}

pub(crate) fn classify(e: &Entry) -> &'static str {
    let haystack = format!(
        "{} {} {}",
        e.full_name.to_lowercase(),
        e.description.to_lowercase(),
        e.topics.join(" ").to_lowercase()
    );
    CATEGORIES
        .iter()
        .find(|(_, keywords)| keywords.iter().any(|k| haystack.contains(k)))
        .map(|(c, _)| *c)
        .unwrap_or(OTHER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, desc: &str, topics: &[&str]) -> Entry {
        Entry {
            full_name: name.to_string(),
            description: desc.to_string(),
            stars: 0,
            language: "Rust".to_string(),
            topics: topics.iter().map(|t| t.to_string()).collect(),
            url: String::new(),
            pushed_at: String::new(),
        }
    }

    #[test]
    fn a_plugin_is_placed_by_what_it_says_it_does() {
        assert_eq!(
            classify(&entry("b/herdr-worktrunk", "switch git worktrees", &[])),
            "worktree"
        );
        assert_eq!(
            classify(&entry("a/herdr-ntfy", "push to your phone", &[])),
            "notifications"
        );
    }

    /// The measured failure of topic-only classification: the topic says nothing useful, and
    /// the description says everything.
    #[test]
    fn the_description_counts_even_when_the_topics_do_not() {
        let e = entry(
            "someone/herdr-thing",
            "Switch between worktrees from a picker",
            &["herdr", "rust"],
        );
        assert_eq!(classify(&e), "worktree");
    }

    /// Order is the design: almost every plugin mentions a pane, so a broad category must not
    /// be consulted before a specific one.
    #[test]
    fn a_specific_purpose_beats_a_broad_one() {
        let e = entry(
            "x/herdr-worktree-pane",
            "opens a workspace pane for each worktree",
            &[],
        );
        assert_eq!(classify(&e), "worktree", "not workspace");
    }

    /// Matching nothing is a category, not a disappearance.
    #[test]
    fn an_unclassifiable_plugin_lands_in_other() {
        assert_eq!(classify(&entry("x/herdr-thing", "", &[])), OTHER);
        assert_eq!(*names().last().unwrap(), OTHER);
    }

    #[test]
    fn matching_ignores_case() {
        assert_eq!(
            classify(&entry("X/Herdr-Worktree", "WORKTREE tooling", &[])),
            "worktree"
        );
    }
}
