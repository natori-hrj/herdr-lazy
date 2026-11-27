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

use std::collections::HashSet;

use crate::{
    registry::{search_terms, Entry},
    Installed, Match, Spec,
};

#[derive(Default)]
enum InstallFilter {
    #[default]
    Any,
    Installed,
    NotInstalled,
}

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
    installed: Option<HashSet<String>>,
    install_filter: InstallFilter,
    sort_by_name: bool,
    // Keep the selection while an unavailable inventory hides the filtered results.
    unavailable_selection: Option<String>,
}

impl Browser {
    pub(crate) fn new(
        all: Vec<Entry>,
        source_note: String,
        listed: Vec<String>,
        installed: Option<&[Installed]>,
    ) -> Browser {
        let installed = installed.map(|snapshot| installed_entries(&all, snapshot));
        Browser {
            all,
            query: String::new(),
            cursor: 0,
            source_note,
            listed,
            category: None,
            installed,
            install_filter: InstallFilter::default(),
            sort_by_name: false,
            unavailable_selection: None,
        }
    }

    pub(crate) fn replace_entries(
        &mut self,
        entries: Vec<Entry>,
        note: String,
        installed: Option<&[Installed]>,
    ) {
        let selected = self
            .selected()
            .map(|entry| entry.full_name.clone())
            .or_else(|| self.unavailable_selection.take());
        self.installed = installed.map(|snapshot| installed_entries(&entries, snapshot));
        self.all = entries;
        self.source_note = note;
        self.restore_selection(selected.as_deref());
        self.unavailable_selection = selected.filter(|_| self.install_filter_unavailable());
    }

    fn restore_selection(&mut self, selected: Option<&str>) {
        self.cursor = selected
            .and_then(|name| {
                self.results()
                    .iter()
                    .position(|entry| entry.full_name.eq_ignore_ascii_case(name))
            })
            .unwrap_or(0);
    }

    pub(crate) fn results(&self) -> Vec<&Entry> {
        let terms = search_terms(&self.query);
        let is_eligible = |entry: &Entry| {
            let install_matches = match self.install_filter {
                InstallFilter::Any => true,
                InstallFilter::Installed => self.is_installed(entry),
                InstallFilter::NotInstalled => {
                    self.installed.is_some() && !self.is_installed(entry)
                }
            };
            install_matches
                && self.category.map_or(true, |category| {
                    crate::category::classify(entry) == category
                })
        };
        let mut results: Vec<_> = self
            .all
            .iter()
            .filter(|entry| entry.matches_terms(&terms) && is_eligible(entry))
            .collect();
        if results.is_empty() && terms.len() == 1 {
            let term = self.query.trim();
            if (3..=ABBREVIATION_LIMIT).contains(&term.len())
                && term.bytes().all(|byte| byte.is_ascii_alphanumeric())
            {
                results.extend(
                    self.all
                        .iter()
                        .filter(|entry| repo_abbreviation(entry, term) && is_eligible(entry)),
                );
            }
        }
        if self.sort_by_name {
            results.sort_by_cached_key(|&entry| {
                (
                    entry.full_name.to_ascii_lowercase(),
                    entry.full_name.as_str(),
                )
            });
        }
        results
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
        self.reset_selection();
    }

    /// What the header says is being shown.
    pub(crate) fn category_label(&self) -> &'static str {
        self.category.unwrap_or("all")
    }

    pub(crate) fn cycle_install_filter(&mut self) {
        self.install_filter = match self.install_filter {
            InstallFilter::Any => InstallFilter::Installed,
            InstallFilter::Installed => InstallFilter::NotInstalled,
            InstallFilter::NotInstalled => InstallFilter::Any,
        };
        self.reset_selection();
    }

    pub(crate) fn install_filter_label(&self) -> &'static str {
        match self.install_filter {
            InstallFilter::Any => "any",
            InstallFilter::Installed => "installed",
            InstallFilter::NotInstalled => "not installed",
        }
    }

    pub(crate) fn install_filter_unavailable(&self) -> bool {
        !matches!(self.install_filter, InstallFilter::Any) && self.inventory_unavailable()
    }

    pub(crate) fn inventory_unavailable(&self) -> bool {
        self.installed.is_none()
    }

    pub(crate) fn toggle_sort(&mut self) {
        let selected = self.selected().map(|entry| entry.full_name.clone());
        self.sort_by_name = !self.sort_by_name;
        self.restore_selection(selected.as_deref());
    }

    pub(crate) fn sort_label(&self) -> &'static str {
        if self.sort_by_name {
            "name"
        } else {
            "stars"
        }
    }

    pub(crate) fn reset(&mut self) {
        self.query.clear();
        self.category = None;
        self.install_filter = InstallFilter::Any;
        self.sort_by_name = false;
        self.reset_selection();
    }

    fn reset_selection(&mut self) {
        self.cursor = 0;
        self.unavailable_selection = None;
    }

    pub(crate) fn selected(&self) -> Option<&Entry> {
        self.results().get(self.cursor).copied()
    }

    pub(crate) fn is_listed(&self, e: &Entry) -> bool {
        self.listed
            .iter()
            .any(|l| l.eq_ignore_ascii_case(&e.full_name))
    }

    pub(crate) fn is_installed(&self, e: &Entry) -> bool {
        self.installed
            .as_ref()
            .is_some_and(|names| names.contains(&e.full_name))
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
        self.reset_selection();
    }

    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.reset_selection();
    }
}

fn installed_entries(entries: &[Entry], snapshot: &[Installed]) -> HashSet<String> {
    if snapshot.is_empty() {
        return HashSet::new();
    }
    let slugs: HashSet<_> = snapshot
        .iter()
        .filter_map(|plugin| plugin.slug.as_ref())
        .map(|slug| slug.to_ascii_lowercase())
        .collect();
    let legacy: Vec<_> = snapshot
        .iter()
        .filter(|plugin| plugin.slug.is_none())
        .collect();
    entries
        .iter()
        .filter(|entry| {
            if slugs.contains(&entry.full_name.to_ascii_lowercase()) {
                return true;
            }
            if legacy.is_empty() {
                return false;
            }
            // Older records can still identify the repository through their source URL.
            let spec = Spec::parse(&entry.full_name);
            legacy
                .iter()
                .any(|plugin| plugin.matches(&spec) == Match::Strong)
        })
        .map(|entry| entry.full_name.clone())
        .collect()
}

const ABBREVIATION_LIMIT: usize = 12;

fn repo_abbreviation(entry: &Entry, term: &str) -> bool {
    let Some((_, leaf)) = entry.full_name.rsplit_once('/') else {
        return false;
    };
    let mut reached = [false; ABBREVIATION_LIMIT + 1];
    reached[0] = true;
    for token in leaf.split(['-', '_', '.']) {
        // Walk backward so a path can consume only one prefix from each token.
        for offset in (0..term.len()).rev() {
            if reached[offset] {
                let prefix_len = token
                    .bytes()
                    .zip(term.bytes().skip(offset))
                    .take_while(|(actual, wanted)| actual.eq_ignore_ascii_case(wanted))
                    .count();
                reached[offset + 1..][..prefix_len].fill(true);
            }
        }
        if reached[term.len()] {
            return true;
        }
    }
    false
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
            Some(&[]),
        )
    }

    #[test]
    fn an_empty_query_lists_everything() {
        assert_eq!(browser().results().len(), 3);
    }

    #[test]
    fn whitespace_only_queries_keep_category_filtering_and_entry_order() {
        let mut b = browser();
        b.query = " \t\u{2003}".into();
        assert!(b.results().into_iter().eq(b.all.iter()));

        b.category = Some("worktree");
        assert_eq!(b.results(), vec![&b.all[1]]);
    }

    #[test]
    fn a_query_matches_unicode_terms_across_fields() {
        let mut b = browser();
        b.all[1].description = "fzf picker for worktrees: ΟΣ".into();
        b.query = "  GIT-WORKTREE\u{2003}WORKTRUNK\tΟΣ  ".into();
        assert_eq!(b.results(), vec![&b.all[1]]);

        b.query.push_str(" Rust");
        assert!(b.results().is_empty(), "language is excluded from search");
    }

    #[test]
    fn repository_abbreviations_are_only_used_without_an_eligible_direct_hit() {
        let mut b = Browser::new(
            vec![
                entry("wti/file-viewer", 100, "Plain viewer", &[]),
                entry("worktree-task-inspector/plain", 95, "Plain plugin", &[]),
                entry("a/file-viewer", 90, "Worktree Task Inspector", &[]),
                entry("a/work-tree-inspector", 80, "Worktree utility", &[]),
                entry("b/worktree-tools_icon", 70, "Worktree utility", &[]),
                entry("c/notify", 10, "WTI notifications", &[]),
            ],
            String::new(),
            Vec::new(),
            Some(&[]),
        );
        b.query = "WTI".into();
        assert_eq!(b.results(), vec![&b.all[0], &b.all[5]]);

        b.cycle_category(true);
        assert_eq!(b.results(), vec![&b.all[3], &b.all[4]]);
        for query in [
            "wt",
            "w.t.i",
            "wti utility",
            "workspacetaski",
            "wrei",
            "日本語",
        ] {
            b.query = query.into();
            assert!(b.results().is_empty(), "{query}");
        }

        b.category = None;
        b.query = "wtin".into();
        assert_eq!(b.results(), vec![&b.all[3]]);
    }

    #[test]
    fn marketplace_abbreviation_requires_ascii_in_the_original_query() {
        let mut b = browser();
        b.all[0].full_name = "a/keymap-helper".into();
        b.query = "  Khe\u{2003}".into();
        assert_eq!(b.results(), vec![&b.all[0]]);

        b.query = "  Khe\u{2003}".into();
        assert!(
            b.results().is_empty(),
            "only ASCII can use abbreviation matching"
        );

        b.all[1].description = "khe integration".into();
        assert_eq!(b.results(), vec![&b.all[1]]);
    }

    #[test]
    fn install_filter_uses_repository_identity_independently_of_the_list() {
        let installed = crate::parse_plugin_list(
            r#"{"result":{"plugins":[
                {"name":"different name","enabled":false,"source":{"url":"https://github.com/Z/tool.git"}},
                {"name":"also-tool","source":{"kind":"local"}}
            ]}}"#,
        )
        .unwrap();
        let mut b = Browser::new(
            vec![
                entry("a/also-tool", 50, "worktree viewer", &[]),
                entry("z/tool", 20, "worktree inspector", &[]),
                entry("y/tool", 10, "notifications", &["worktree"]),
            ],
            "cached".into(),
            vec!["a/also-tool".into()],
            Some(&installed),
        );
        b.push('t');
        b.cycle_category(true);
        b.cycle_install_filter();
        assert_eq!(b.results(), vec![&b.all[1]]);
        assert!(!b.is_listed(b.selected().unwrap()));

        b.cycle_install_filter();
        assert_eq!(b.results(), vec![&b.all[0], &b.all[2]]);
        assert!(b.is_listed(b.selected().unwrap()));

        b.move_down();
        b.toggle_sort();
        b.reset();
        assert_eq!(b.results(), b.all.iter().collect::<Vec<_>>());
        assert_eq!(b.selected(), b.all.first());
        assert!(b.query.is_empty());
        assert_eq!(b.category_label(), "all");
        assert_eq!(b.install_filter_label(), "any");
        assert_eq!(b.sort_label(), "stars");
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
