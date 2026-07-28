//! The marketplace browser: search 270-odd published plugins and add them to your list.
//!
//! This is the half of plugin management that herdr's CLI cannot do at all — `plugin install`
//! needs you to already know the name. lazy.nvim has no equivalent either, because Neovim has
//! no registry to browse; herdr publishes one, so the manage pane can.
//!
//! Deliberately two steps: Enter adds the plugin to your list, and `s` installs it. Installing
//! directly from a search result would mean one keystroke on a fuzzy match runs a stranger's
//! build script. The list is the place where intent is recorded, so that is where a discovery
//! lands.

use crate::registry::Entry;

/// State of the browser overlay.
pub(crate) struct Browser {
    /// Everything the marketplace knows about, most-starred first.
    pub(crate) all: Vec<Entry>,
    pub(crate) query: String,
    pub(crate) cursor: usize,
    /// Where the entries came from — "just refreshed", "cached 3h ago", …
    pub(crate) source_note: String,
    /// Names already in the user's list, so the browser can mark what is already handled.
    pub(crate) listed: Vec<String>,
    /// The category filter, or `None` for everything. Cycled with tab; independent of the
    /// query, so "worktree plugins mentioning fzf" is two narrowings rather than one guess.
    pub(crate) category: Option<&'static str>,
}

impl Browser {
    pub(crate) fn new(all: Vec<Entry>, source_note: String, listed: Vec<String>) -> Browser {
        Browser {
            all,
            query: String::new(),
            cursor: 0,
            source_note,
            listed,
            category: None,
        }
    }

    pub(crate) fn results(&self) -> Vec<&Entry> {
        self.all
            .iter()
            .filter(|e| e.matches(&self.query))
            .filter(|e| match self.category {
                Some(c) => crate::category::classify(e) == c,
                None => true,
            })
            .collect()
    }

    /// Tab — step through the categories, ending back at "everything".
    ///
    /// A cycle rather than a menu: there are ten of them, the list is short enough to walk, and
    /// a second overlay on top of the search overlay is a worse way to narrow a list than one
    /// key you can hold.
    pub(crate) fn cycle_category(&mut self, forward: bool) {
        let names = crate::category::names();
        // `None` is position 0, so the ring is "everything" followed by each category.
        let at = match self.category {
            None => 0,
            Some(c) => names
                .iter()
                .position(|n| *n == c)
                .map(|i| i + 1)
                .unwrap_or(0),
        };
        let len = names.len() + 1;
        let next = if forward {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        };
        self.category = if next == 0 {
            None
        } else {
            names.get(next - 1).copied()
        };
        self.cursor = 0;
    }

    /// What the header says is being shown.
    pub(crate) fn category_label(&self) -> &'static str {
        self.category.unwrap_or("all")
    }

    pub(crate) fn selected(&self) -> Option<&Entry> {
        self.results().get(self.cursor).copied()
    }

    pub(crate) fn is_listed(&self, e: &Entry) -> bool {
        self.listed.iter().any(|l| l == &e.full_name)
    }

    /// Keep the cursor inside the results as the query narrows them.
    pub(crate) fn clamp(&mut self) {
        let n = self.results().len();
        if n == 0 {
            self.cursor = 0;
        } else if self.cursor >= n {
            self.cursor = n - 1;
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.cursor + 1 < self.results().len() {
            self.cursor += 1;
        }
    }

    pub(crate) fn move_up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Typing narrows the search, and always returns to the top of the new result set —
    /// otherwise the cursor appears to jump to an unrelated row as characters are added.
    pub(crate) fn push(&mut self, c: char) {
        self.query.push(c);
        self.cursor = 0;
    }

    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, stars: u64, desc: &str, topics: &[&str]) -> Entry {
        Entry {
            full_name: name.to_string(),
            description: desc.to_string(),
            stars,
            language: "Rust".to_string(),
            topics: topics.iter().map(|t| t.to_string()).collect(),
            url: format!("https://github.com/{}", name),
            pushed_at: "2026-07-01T00:00:00Z".to_string(),
        }
    }

    fn browser() -> Browser {
        Browser::new(
            vec![
                entry(
                    "a/herdr-file-viewer",
                    181,
                    "read-only file viewer",
                    &["tui"],
                ),
                entry(
                    "b/herdr-worktrunk",
                    41,
                    "fzf picker for worktrees",
                    &["git-worktree"],
                ),
                entry(
                    "c/herdr-remote",
                    100,
                    "approve agents from your phone",
                    &["mobile"],
                ),
            ],
            "just refreshed".into(),
            vec!["c/herdr-remote".to_string()],
        )
    }

    #[test]
    fn an_empty_query_lists_everything() {
        assert_eq!(browser().results().len(), 3);
    }

    #[test]
    fn typing_narrows_and_resets_the_cursor() {
        let mut b = browser();
        b.move_down();
        b.move_down();
        assert_eq!(b.cursor, 2);

        // Narrowing while the cursor sits deep in the old results must not leave it pointing
        // at an unrelated row.
        for c in "fzf".chars() {
            b.push(c);
        }
        assert_eq!(b.cursor, 0);
        assert_eq!(b.results().len(), 1);
        assert_eq!(b.selected().unwrap().full_name, "b/herdr-worktrunk");
    }

    #[test]
    fn backspacing_widens_again() {
        let mut b = browser();
        for c in "phone".chars() {
            b.push(c);
        }
        assert_eq!(b.results().len(), 1);
        for _ in 0..5 {
            b.backspace();
        }
        assert_eq!(b.results().len(), 3);
    }

    /// The cursor must never point past the end when a keystroke shrinks the result set.
    #[test]
    fn clamp_keeps_the_cursor_inside_the_results() {
        let mut b = browser();
        b.cursor = 2;
        b.query = "viewer".into();
        b.clamp();
        assert_eq!(b.cursor, 0);
        assert!(b.selected().is_some());
    }

    #[test]
    fn no_results_leaves_nothing_selected() {
        let mut b = browser();
        b.query = "nothing matches this".into();
        b.clamp();
        assert!(
            b.selected().is_none(),
            "must not offer to add a phantom row"
        );
    }

    /// Regression: the query is a text field, so every printable character must reach it.
    ///
    /// `a` was briefly bound to "add" and `o` to "open repo" here, which meant typing
    /// "worktree" searched for "wrktree" and any word containing an "a" wrote to the user's
    /// list as a side effect of typing. Commands in this view must take a modifier.
    #[test]
    fn every_printable_character_can_be_typed() {
        let mut b = browser();
        for c in "abcdefghijklmnopqrstuvwxyz0123456789-_./ ".chars() {
            b.push(c);
        }
        assert_eq!(b.query, "abcdefghijklmnopqrstuvwxyz0123456789-_./ ");
    }

    /// The words a user is most likely to type must survive intact.
    #[test]
    fn realistic_queries_are_not_mangled() {
        for q in [
            "worktree",
            "lazygit",
            "notification",
            "agent",
            "workspace manager",
        ] {
            let mut b = browser();
            for c in q.chars() {
                b.push(c);
            }
            assert_eq!(b.query, q, "{} was mangled", q);
        }
    }

    #[test]
    fn a_category_narrows_the_list_and_tab_gets_back_to_everything() {
        let mut b = browser();
        assert_eq!(b.category_label(), "all");

        b.category = Some("worktree");
        let r = b.results();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].full_name, "b/herdr-worktrunk");

        // The ring starts at "everything" and returns to it, so tab is never a trap.
        b.category = None;
        let steps = crate::category::names().len() + 1;
        for _ in 0..steps {
            b.cycle_category(true);
        }
        assert_eq!(b.category, None);
    }

    /// Filtering by category and typing a query are independent narrowings.
    #[test]
    fn a_category_and_a_query_both_apply() {
        let mut b = browser();
        b.category = Some("worktree");
        for c in "phone".chars() {
            b.push(c);
        }
        assert!(
            b.results().is_empty(),
            "the phone plugin is not a worktree one"
        );
        assert!(b.selected().is_none());
    }

    /// Changing the filter must not leave the cursor pointing into the old result set.
    #[test]
    fn changing_category_returns_to_the_top() {
        let mut b = browser();
        b.move_down();
        b.move_down();
        b.cycle_category(true);
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn already_listed_plugins_are_marked() {
        let b = browser();
        let remote = b
            .all
            .iter()
            .find(|e| e.full_name == "c/herdr-remote")
            .unwrap();
        let viewer = b
            .all
            .iter()
            .find(|e| e.full_name.contains("viewer"))
            .unwrap();
        assert!(b.is_listed(remote));
        assert!(!b.is_listed(viewer));
    }
}
