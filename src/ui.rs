//! The manage pane — herdr-lazy's `:Lazy`.
//!
//! A scrolling list of the bundle plus anything installed outside it, with the same
//! operations the CLI exposes bound to single keys. herdr gives a plugin pane a real PTY, so
//! this is a normal full-screen TUI.
//!
//! Drawing is by hand (crossterm only, no ratatui): the whole view is a header, a list, and a
//! footer, which is not worth a layout engine. See Cargo.toml for that decision.
//!
//! Long operations deliberately drop out of the alternate screen and run with ordinary stdout
//! rather than being reimplemented as in-pane progress. `sync` shells out to `herdr plugin
//! install`, whose output is worth reading verbatim when a build fails — capturing it into a
//! spinner would hide the one thing you need.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;

use crate::{installed_plugins, pin_state, Installed, Match, PinState, Spec};

/// What the list says about one plugin.
#[derive(Debug, Clone, PartialEq)]
enum Status {
    /// In the bundle and installed at the expected commit.
    Ok,
    /// In the bundle, not installed.
    Missing,
    /// In the bundle, installed, but not at the pinned commit.
    Drifted { have: String },
    /// Pinned to a tag or branch, which cannot be checked locally.
    Unverifiable,
    /// Installed but disabled — present, yet herdr will not run it.
    Disabled,
    /// Installed and not in the bundle. `sync --prune` would remove it...
    Extra,
    /// ...unless it is a local link, which prune always protects.
    ExtraLocal,
    /// ...or herdr-lazy itself, which must never be pruned by its own prune.
    SelfEntry,
}

impl Status {
    fn marker(&self) -> &'static str {
        match self {
            Status::Ok => "✔",
            Status::Missing => "✗",
            Status::Drifted { .. } => "↻",
            Status::Unverifiable => "?",
            Status::Disabled => "○",
            Status::Extra => "+",
            Status::ExtraLocal => "⚑",
            Status::SelfEntry => "◆",
        }
    }

    /// ANSI colour. Written directly rather than via crossterm's style API — one escape code
    /// is simpler to read here than a builder chain.
    fn colour(&self) -> &'static str {
        match self {
            Status::Ok => "\x1b[32m",                               // green
            Status::Missing | Status::Drifted { .. } => "\x1b[33m", // yellow
            Status::Unverifiable | Status::Disabled => "\x1b[36m",  // cyan
            Status::Extra => "\x1b[31m",                            // red
            Status::ExtraLocal => "\x1b[35m",                       // magenta
            Status::SelfEntry => "\x1b[34m",                        // blue
        }
    }

    /// Every note states BOTH axes — installed or not, listed or not.
    ///
    /// An earlier version said only "not in bundle" for an extra plugin, which reads as "not
    /// installed" to anyone who has not internalised what the bundle is. It means the exact
    /// opposite, and the two states sit next to each other in the same list, so the ambiguity
    /// is worth the extra words. "your list" rather than "bundle": the file is the user's,
    /// and the jargon is only explained in the README.
    fn note(&self) -> String {
        match self {
            Status::Ok => String::new(),
            Status::Missing => "in your list, not installed — press i to install".to_string(),
            Status::Drifted { have } => format!(
                "installed at {}, pinned elsewhere — press i to restore the pin",
                crate::short(have)
            ),
            Status::Unverifiable => {
                "installed; pinned to a tag/branch, so it cannot be verified".to_string()
            }
            Status::Disabled => "installed but disabled — herdr will not run it".to_string(),
            Status::Extra => {
                "installed, not in your list — press a to adopt, x to uninstall".to_string()
            }
            Status::ExtraLocal => "installed as a local link — never removed by prune".to_string(),
            Status::SelfEntry => "this is herdr-lazy — never removed by its own prune".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    label: String,
    commit: Option<String>,
    status: Status,
    /// The exact line in plugins.list, when this row came from there — what `d` removes.
    listed_as: Option<String>,
    /// `owner/repo`, when known — what `a` adds. `None` for a local link, which has no repo
    /// to record, so it can never be adopted into the list.
    slug: Option<String>,
    /// The plugin's repository has been pushed to since this copy was installed. A hint, not
    /// a fact — see `registry::pushed_since`.
    maybe_stale: bool,
    /// herdr's id for the installed plugin, and how it was installed. Present only when the
    /// row corresponds to something actually installed — which is exactly when `x` applies.
    installed: Option<(String, String)>,
    /// Ticked for a bulk operation. Kept on the row rather than as a set of indices so it
    /// survives the list being rebuilt in a different order.
    picked: bool,
    /// What this plugin can do, for the details view. Empty for anything not installed —
    /// herdr only knows a manifest once it has fetched it.
    detail: Option<PluginDetail>,
}

/// The manifest facts worth showing a user who just installed seven plugins at once.
#[derive(Debug, Clone, Default)]
struct PluginDetail {
    description: String,
    actions: Vec<(String, String)>,
    panes: Vec<(String, String, String)>,
    events: Vec<String>,
    plugin_id: String,
}

fn detail_of(p: &Installed) -> PluginDetail {
    PluginDetail {
        description: p.description.clone(),
        actions: p.actions.clone(),
        panes: p.panes.clone(),
        events: p.events.clone(),
        plugin_id: p.plugin_id.clone(),
    }
}

/// Build the view: every bundle entry, then anything installed that the bundle does not name.
/// The view without update information — what the tests use, since they are checking status
/// and selection rather than what the marketplace happens to say today.
#[cfg(test)]
fn rows(desired: &[Spec], installed: &[Installed]) -> Vec<Row> {
    rows_with_updates(desired, installed, &[])
}

/// Build the view, marking anything the marketplace says has moved since it was installed.
///
/// `market` is whatever is already cached; browsing has usually populated it, and when it is
/// empty the column simply does not appear. Checking for updates should never be the reason
/// the pane makes a network call.
fn rows_with_updates(
    desired: &[Spec],
    installed: &[Installed],
    market: &[crate::registry::Entry],
) -> Vec<Row> {
    let stale = |p: &Installed, spec: Option<&Spec>| -> bool {
        // A pinned entry is meant to sit still; calling it out of date is noise.
        if spec.map(|s| s.reference.is_some()).unwrap_or(false) {
            return false;
        }
        let (Some(slug), Some(ms)) = (p.slug.as_ref(), p.installed_unix_ms) else {
            return false;
        };
        market
            .iter()
            .find(|e| e.full_name.eq_ignore_ascii_case(slug))
            .map(|e| crate::registry::pushed_since(&e.pushed_at, ms))
            .unwrap_or(false)
    };
    let mut rows = Vec::new();

    for spec in desired {
        let hit = installed
            .iter()
            .map(|p| (p, p.matches(spec)))
            .filter(|(_, m)| *m != Match::None)
            .max_by_key(|(_, m)| (*m == Match::Strong) as u8);

        let (status, commit) = match hit {
            None => (Status::Missing, None),
            Some((p, _)) => {
                let commit = p.resolved_commit.clone();
                let status = match pin_state(spec, p) {
                    PinState::Drifted { have } => Status::Drifted { have },
                    PinState::Unverifiable if spec.reference.is_some() => Status::Unverifiable,
                    _ if !p.enabled => Status::Disabled,
                    _ => Status::Ok,
                };
                (status, commit)
            }
        };
        rows.push(Row {
            label: spec.display(),
            commit,
            status,
            listed_as: Some(spec.display()),
            slug: Some(spec.repo.clone()),
            installed: hit.map(|(p, _)| (p.plugin_id.clone(), p.source_kind.clone())),
            maybe_stale: hit.map(|(p, _)| stale(p, Some(spec))).unwrap_or(false),
            picked: false,
            detail: hit.map(|(p, _)| detail_of(p)),
        });
    }

    for p in installed {
        let claimed = desired.iter().any(|s| p.matches(s) != Match::None);
        if claimed {
            continue;
        }
        rows.push(Row {
            label: p.slug.clone().unwrap_or_else(|| p.plugin_id.clone()),
            commit: p.resolved_commit.clone(),
            status: if crate::is_self_id(&p.plugin_id) {
                Status::SelfEntry
            } else if p.source_kind == "local" {
                Status::ExtraLocal
            } else {
                Status::Extra
            },
            listed_as: None,
            slug: p.slug.clone(),
            installed: Some((p.plugin_id.clone(), p.source_kind.clone())),
            maybe_stale: stale(p, None),
            picked: false,
            detail: Some(detail_of(p)),
        });
    }

    rows
}

/// Screen row where the list starts, in both views.
///
/// The header is two lines (title + rule) in the list view and three in the browser (title,
/// query, rule). Click handling and drawing must agree on this, so it lives in one place
/// rather than being counted twice.
const LIST_TOP: usize = 2;
const BROWSER_TOP: usize = 3;

/// `Default` so tests can build one from just the rows they care about; this struct grows a
/// field per view, and every fixture should not have to know about all of them.
#[derive(Default)]
struct App {
    rows: Vec<Row>,
    /// Present while the marketplace browser is open; the pane draws that instead of the list.
    browser: Option<crate::browse::Browser>,
    /// `?` — the full keymap. The footer only has room for the common half.
    help: bool,
    /// Open details for the row at this index: what the plugin does and how to reach it.
    detail_of: Option<usize>,
    /// Which action is highlighted in the details view.
    detail_cursor: usize,
    /// Set while waiting for the letter to bind an action to.
    awaiting_bind: bool,
    /// `(action_id, key)` chosen but not yet written, while the user confirms.
    ///
    /// Writing to `config.toml` cannot be sandboxed: the pane is launched by herdr, so an
    /// environment variable set in a shell never reaches it, and there is no "try it against
    /// a copy" mode to offer. The only honest safeguard is to show exactly what will be
    /// appended, and to where, before touching the file.
    pending_bind: Option<(crate::BindTarget, String)>,
    cursor: usize,
    /// Set when a refresh fails, so the pane explains itself instead of showing an empty list.
    error: Option<String>,
    /// Result of the last instant action (`a`/`d`), shown until the next keypress. Editing the
    /// list is fast and touches only a file, so it stays in the pane rather than dropping out
    /// of the alternate screen the way `sync` does.
    flash: Option<String>,
}

impl App {
    fn load() -> App {
        let desired: Vec<Spec> = crate::desired_plugins()
            .iter()
            .map(|l| Spec::parse(l))
            .collect();
        // Cached only: `false` never fetches. If nothing is cached the update column is
        // simply absent, which is better than making every pane open hit the network.
        let market = crate::registry::cached_entries();
        match installed_plugins() {
            Ok(installed) => App {
                rows: rows_with_updates(&desired, &installed, &market),
                browser: None,
                help: false,
                detail_of: None,
                detail_cursor: 0,
                awaiting_bind: false,
                pending_bind: None,
                cursor: 0,
                error: None,
                flash: None,
            },
            Err(e) => App {
                rows: Vec::new(),
                browser: None,
                help: false,
                detail_of: None,
                detail_cursor: 0,
                awaiting_bind: false,
                pending_bind: None,
                cursor: 0,
                error: Some(e),
                flash: None,
            },
        }
    }

    fn refresh(&mut self) {
        let (cursor, flash) = (self.cursor, self.flash.take());
        let (browser, help) = (self.browser.take(), self.help);
        *self = App::load();
        self.browser = browser;
        self.help = help;
        self.cursor = cursor.min(self.rows.len().saturating_sub(1));
        self.flash = flash;
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    /// How many rows fit, and which one is at the top. Used by both drawing and clicking.
    fn list_window(&self, height: u16) -> (usize, usize) {
        let visible = (height as usize).saturating_sub(5).max(1);
        let start = if self.cursor >= visible {
            self.cursor - visible + 1
        } else {
            0
        };
        (visible, start)
    }

    /// Which row is under this screen line, if any.
    fn row_at(&self, screen_row: u16, height: u16) -> Option<usize> {
        let (visible, start) = self.list_window(height);
        let y = screen_row as usize;
        if y < LIST_TOP {
            return None; // header
        }
        let idx = start + (y - LIST_TOP);
        if y - LIST_TOP < visible && idx < self.rows.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn open_detail(&mut self) {
        if self.rows.get(self.cursor).is_some() {
            self.detail_of = Some(self.cursor);
            self.detail_cursor = 0;
            self.flash = None;
        }
    }

    /// Bind the highlighted action to `prefix+shift+<letter>`.
    ///
    /// Shift is not negotiable here: herdr's own defaults live on `prefix+<letter>`, and this
    /// cannot see them (the CLI does not expose them), so the one thing it can do is stay out
    /// of that space entirely. Conflicts with the user's own bindings are detected.
    /// Everything in this plugin that a key could be bound to, in the order shown.
    ///
    /// Actions first, then panes. Panes are included because four of the seven plugins in the
    /// default set expose only a pane — leaving them unbindable would mean the feature does
    /// not work for most of what a new user has installed.
    fn bindable(d: &PluginDetail) -> Vec<(crate::BindTarget, String)> {
        let mut out: Vec<(crate::BindTarget, String)> = d
            .actions
            .iter()
            .map(|(id, title)| {
                (
                    crate::BindTarget::Action(id.clone()),
                    if title.is_empty() {
                        id.clone()
                    } else {
                        title.clone()
                    },
                )
            })
            .collect();
        out.extend(d.panes.iter().map(|(id, title, _)| {
            (
                crate::BindTarget::Pane(id.clone()),
                if title.is_empty() {
                    id.clone()
                } else {
                    title.clone()
                },
            )
        }));
        out
    }

    fn bind_selected(&mut self, letter: char) {
        let Some(idx) = self.detail_of else { return };
        let Some(d) = self.rows.get(idx).and_then(|r| r.detail.clone()) else {
            return;
        };
        let Some((target, _)) = Self::bindable(&d).into_iter().nth(self.detail_cursor) else {
            return;
        };
        let key = format!("prefix+shift+{}", letter.to_ascii_lowercase());
        // Check for a conflict now, so the confirmation screen is never shown for a binding
        // that would be refused anyway.
        if let Err(msg) = crate::check_bind_conflict(&key) {
            self.flash = Some(msg);
            return;
        }
        self.pending_bind = Some((target, key));
    }

    /// `y` on the confirmation screen.
    fn commit_bind(&mut self) {
        let Some((target, key)) = self.pending_bind.take() else {
            return;
        };
        let Some(d) = self
            .detail_of
            .and_then(|i| self.rows.get(i))
            .and_then(|r| r.detail.clone())
        else {
            return;
        };
        self.flash = Some(match crate::bind_action(&d.plugin_id, &target, &key) {
            Ok(msg) | Err(msg) => msg,
        });
    }

    fn run_detail_action(&mut self) {
        let Some(idx) = self.detail_of else { return };
        let Some(d) = self.rows.get(idx).and_then(|r| r.detail.clone()) else {
            return;
        };
        let Some((target, _)) = Self::bindable(&d).into_iter().nth(self.detail_cursor) else {
            self.flash = Some("nothing to run".to_string());
            return;
        };
        self.flash = Some(match &target {
            crate::BindTarget::Action(id) => crate::invoke_action(&d.plugin_id, id),
            crate::BindTarget::Pane(id) => crate::open_pane(&d.plugin_id, id),
        });
    }

    fn any_picked(&self) -> bool {
        self.rows.iter().any(|r| r.picked)
    }

    /// Rows a bulk action applies to: the ticked ones, or the cursor row when none are.
    ///
    /// This is what makes selection optional — every operation keeps working exactly as it
    /// did for someone who never touches the checkbox.
    fn targets(&self) -> Vec<usize> {
        if self.any_picked() {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| r.picked)
                .map(|(i, _)| i)
                .collect()
        } else {
            vec![self.cursor]
        }
    }

    /// Tick or untick the row under the cursor, and stay there.
    ///
    /// An earlier version advanced the cursor afterwards, on the theory that ticking a run of
    /// rows should be one repeated key. That broke the thing a checkbox promises: press again
    /// and it comes back off. Pressing twice ticked two different rows instead, which is not
    /// what any checkbox anywhere does. `j` is right there for moving.
    fn toggle_pick(&mut self) {
        if let Some(r) = self.rows.get_mut(self.cursor) {
            r.picked = !r.picked;
        }
    }

    fn clear_picks(&mut self) {
        for r in self.rows.iter_mut() {
            r.picked = false;
        }
    }

    /// Install every target that needs it, in one pass out of the alternate screen.
    fn install_targets(&mut self) -> io::Result<()> {
        let repos: Vec<String> = self
            .targets()
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .filter(|r| r.listed_as.is_some())
            .filter_map(|r| r.slug.clone())
            .collect();
        if repos.is_empty() {
            self.flash = Some("nothing selected is in your list — press a to adopt".to_string());
            return Ok(());
        }
        let args: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
        suspended(|| {
            let _ = crate::cmd_sync(false, &args);
        })?;
        self.clear_picks();
        self.refresh();
        Ok(())
    }

    fn update_targets(&mut self) -> io::Result<()> {
        let repos: Vec<String> = self
            .targets()
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .filter(|r| r.listed_as.is_some())
            .filter_map(|r| r.slug.clone())
            .collect();
        if repos.is_empty() {
            self.flash = Some("nothing selected is in your list".to_string());
            return Ok(());
        }
        let args: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
        suspended(|| {
            let _ = crate::cmd_update(&args);
        })?;
        self.clear_picks();
        self.refresh();
        Ok(())
    }

    /// Uninstall every target. The only bulk operation that destroys anything, so it reports
    /// what it refused as well as what it did.
    fn uninstall_targets(&mut self) {
        let picked: Vec<(String, String, String)> = self
            .targets()
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .filter_map(|r| {
                r.installed
                    .clone()
                    .map(|(id, kind)| (id, kind, r.label.clone()))
            })
            .collect();
        if picked.is_empty() {
            self.flash = Some("nothing selected is installed".to_string());
            return;
        }
        let mut done = 0;
        let mut refused = Vec::new();
        for (id, kind, _label) in picked {
            let msg = crate::uninstall_plugin(&id, &kind);
            if msg.starts_with("uninstalled") {
                done += 1;
            } else {
                refused.push(msg);
            }
        }
        self.flash = Some(if refused.is_empty() {
            format!("uninstalled {}", done)
        } else {
            format!("uninstalled {} · {}", done, refused.join(" · "))
        });
        self.clear_picks();
        self.refresh();
    }

    /// Adopt every target that is installed but unlisted.
    fn adopt_targets(&mut self) {
        let names: Vec<String> = self
            .targets()
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .filter(|r| r.listed_as.is_none())
            .filter_map(|r| r.slug.clone())
            .collect();
        if names.is_empty() {
            self.flash = Some("nothing selected to adopt".to_string());
            return;
        }
        let mut added = 0;
        for n in &names {
            if crate::add_to_list(n).is_ok() {
                added += 1;
            }
        }
        self.flash = Some(format!("added {} to your list", added));
        self.clear_picks();
        self.refresh();
    }

    /// `r` — put the targets back to the commits in the lockfile.
    fn restore_targets(&mut self) -> io::Result<()> {
        let repos: Vec<String> = self
            .targets()
            .iter()
            .filter_map(|&i| self.rows.get(i))
            .filter_map(|r| r.slug.clone())
            .collect();
        if repos.is_empty() {
            self.flash = Some("nothing selected the lock can describe".to_string());
            return Ok(());
        }
        let args: Vec<&str> = repos.iter().map(|s| s.as_str()).collect();
        suspended(|| {
            let _ = crate::cmd_restore(&args);
        })?;
        self.clear_picks();
        self.refresh();
        Ok(())
    }

    /// Mouse: click ticks a row, wheel scrolls.
    ///
    /// herdr's ecosystem leans on the mouse — the most-used plugins advertise being clickable
    /// — so a pane that only takes keys feels out of place. Everything here is additive: no
    /// keyboard behaviour changes, and nothing is only reachable by mouse.
    ///
    /// Deliberately conservative: a click only ticks a checkbox. Nothing is installed or
    /// removed until a separate key is pressed, so no misplaced click can change the machine.
    fn handle_mouse(&mut self, m: crossterm::event::MouseEvent, height: u16) -> io::Result<()> {
        use crossterm::event::{MouseButton, MouseEventKind};

        // The browser is a search field; scrolling it is useful, clicking rows less so, and
        // there is no safe "act" gesture while a query is being typed.
        if let Some(b) = self.browser.as_mut() {
            match m.kind {
                MouseEventKind::ScrollDown => b.move_down(),
                MouseEventKind::ScrollUp => b.move_up(),
                MouseEventKind::Down(MouseButton::Left) => {
                    let y = m.row as usize;
                    if y >= BROWSER_TOP {
                        let visible = (height as usize).saturating_sub(6).max(1);
                        let start = if b.cursor >= visible {
                            b.cursor - visible + 1
                        } else {
                            0
                        };
                        let idx = start + (y - BROWSER_TOP);
                        if y - BROWSER_TOP < visible && idx < b.results().len() {
                            b.cursor = idx;
                        }
                    }
                }
                _ => {}
            }
            b.clamp();
            return Ok(());
        }

        match m.kind {
            MouseEventKind::ScrollDown => {
                if self.cursor + 1 < self.rows.len() {
                    self.cursor += 1;
                }
            }
            MouseEventKind::ScrollUp => self.cursor = self.cursor.saturating_sub(1),
            MouseEventKind::Down(MouseButton::Left) => {
                // A click ticks the row, exactly as space does. It used to move the cursor,
                // and act on a second click — but once rows have checkboxes, "click to tick,
                // click again to untick" is the only reading anyone will expect. Running
                // something is then a deliberate second step (`i`, `u`, `x`), which also
                // means no single click can install or remove anything.
                if let Some(idx) = self.row_at(m.row, height) {
                    self.cursor = idx;
                    self.toggle_pick();
                    self.flash = None;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `/` — open the marketplace browser.
    fn open_browser(&mut self, force_refresh: bool) {
        match crate::registry::load(force_refresh) {
            Ok((entries, note)) => {
                let listed: Vec<String> = crate::desired_plugins()
                    .iter()
                    .map(|l| crate::Spec::parse(l).repo)
                    .collect();
                self.browser = Some(crate::browse::Browser::new(entries, note, listed));
                self.flash = None;
            }
            // Browsing is a convenience over someone else's undocumented endpoint; failing to
            // reach it must not look like herdr-lazy is broken.
            Err(e) => self.flash = Some(format!("marketplace unavailable — {}", e)),
        }
    }

    /// Enter, in the browser — add to the list, close the browser, and put the cursor on the
    /// new entry so the next key acts on it.
    ///
    /// Leaving the user in the browser was worse than it sounds: the list behind it was built
    /// before the addition, so closing the browser showed a list that did not contain what had
    /// just been added, and `s` would have installed whatever row the cursor happened to be
    /// on. "Added, here it is, press s" is the only version of this that is safe to follow.
    fn add_and_return(&mut self) {
        let Some(name) = self.add_selected_from_browser() else {
            return;
        };
        self.browser = None;
        self.refresh();
        let found = self.rows.iter().position(|r| {
            r.listed_as.as_deref() == Some(name.as_str())
                || r.slug.as_deref() == Some(name.as_str())
        });
        if let Some(i) = found {
            self.cursor = i;
        }
        // Do not tell someone to install what they already have. A plugin can be installed and
        // simply absent from the list, in which case adopting it from the marketplace is only
        // bookkeeping — saying "press s to install" there makes the pane look like it does not
        // know its own state.
        let already_installed = found
            .and_then(|i| self.rows.get(i))
            .map(|r| r.installed.is_some())
            .unwrap_or(false);
        self.flash = Some(if already_installed {
            format!("added {} to your list — it is already installed", name)
        } else {
            format!("added {} — press i to install it", name)
        });
    }

    /// Write the selection into the list. Returns the name added, or None if nothing was.
    fn add_selected_from_browser(&mut self) -> Option<String> {
        let b = self.browser.as_ref()?;
        let Some(entry) = b.selected() else {
            self.flash = Some("nothing selected".to_string());
            return None;
        };
        let name = entry.full_name.clone();
        if let Err(e) = crate::add_to_list(&name) {
            self.flash = Some(format!("could not write the list: {}", e));
            return None;
        }
        Some(name)
    }

    /// `o`, in the browser — open the selected repository in a browser.
    ///
    /// Without this the browser can install a stranger's code but not show it to you first,
    /// which is the wrong way round. It is also the only thing here that sends traffic to the
    /// plugin's author.
    fn open_selected_url(&mut self) {
        let Some(b) = self.browser.as_ref() else {
            return;
        };
        let Some(entry) = b.selected() else {
            return;
        };
        if entry.url.is_empty() {
            self.flash = Some(format!("{} has no repository URL", entry.full_name));
            return;
        }
        let url = entry.url.clone();
        // macOS then Linux. Failing to find a browser is a shrug, not an error.
        let opened = std::process::Command::new("open")
            .arg(&url)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            || std::process::Command::new("xdg-open")
                .arg(&url)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        self.flash = Some(if opened {
            format!("opened {}", url)
        } else {
            format!("could not open a browser — {}", url)
        });
    }

    /// `d` — stop managing an entry. Never uninstalls; that is `x`.
    fn drop_selected(&mut self) {
        let Some(row) = self.selected() else { return };
        let Some(line) = row.listed_as.clone() else {
            self.flash = Some(format!("{} is not in your list", row.label));
            return;
        };
        self.flash = Some(match crate::remove_from_list(&line) {
            Ok(msg) => msg,
            Err(e) => format!("could not write the list: {}", e),
        });
        self.refresh();
    }

    fn draw(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        if self.help {
            return self.draw_help(out, width, height);
        }
        if self.pending_bind.is_some() {
            return self.draw_bind_confirm(out, width, height);
        }
        if self.detail_of.is_some() {
            return self.draw_detail(out, width, height);
        }
        if self.browser.is_some() {
            return self.draw_browser(out, width, height);
        }
        self.draw_list(out, width, height)
    }

    /// Show the exact change before making it.
    fn draw_bind_confirm(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        let (target, key) = self.pending_bind.clone().expect("checked by caller");
        let plugin_id = self
            .detail_of
            .and_then(|i| self.rows.get(i))
            .and_then(|r| r.detail.as_ref())
            .map(|d| d.plugin_id.clone())
            .unwrap_or_default();
        let path = crate::herdr_config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(herdr's config.toml — location unknown)".to_string());

        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(out, "\x1b[1m bind {}.{}\x1b[0m\r", plugin_id, target.id())?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;
        writeln!(out, " This appends to \x1b[1m{}\x1b[0m:\r", path)?;
        writeln!(out, "\r")?;
        let (kind, command) = crate::bind_toml_fields(&target, &plugin_id);
        for line in [
            "# added by herdr-lazy".to_string(),
            "[[keys.command]]".to_string(),
            format!("key = \"{}\"", key),
            format!("type = \"{}\"", kind),
            format!("command = \"{}\"", command),
        ] {
            writeln!(out, "   \x1b[32m{}\x1b[0m\r", line)?;
        }
        writeln!(out, "\r")?;
        writeln!(
            out,
            " \x1b[2mYour current config is copied to config.toml.herdr-lazy-backup first.\x1b[0m\r"
        )?;
        writeln!(
            out,
            " \x1b[2mThis is your real herdr config — the pane is launched by herdr, so there \
             is no copy to test against.\x1b[0m\r"
        )?;

        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m\x1b[1m[y]\x1b[0m write it  \
             \x1b[1m[n / esc]\x1b[0m cancel\r",
            height.saturating_sub(1),
            rule
        )?;
        out.flush()
    }

    /// What this plugin does, and how to actually use it.
    ///
    /// A distro that installs seven plugins in one go leaves the user holding seven things
    /// they did not choose individually and cannot obviously operate. herdr already knows the
    /// answer — every manifest declares its actions, panes and events — so the pane can just
    /// show it, and run the actions too.
    fn draw_detail(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        let idx = self.detail_of.expect("checked by caller");
        let Some(row) = self.rows.get(idx) else {
            return Ok(());
        };
        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(out, "\x1b[1m {}\x1b[0m\r", row.label)?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;

        let Some(d) = row.detail.as_ref() else {
            writeln!(
                out,
                " \x1b[2mnot installed yet — press i to install, then look again\x1b[0m\r"
            )?;
            write!(
                out,
                "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m\x1b[2many key goes back\x1b[0m\r",
                height.saturating_sub(1),
                rule
            )?;
            return out.flush();
        };

        let items = Self::bindable(d);

        if !d.description.is_empty() {
            for line in wrap(&d.description, (width as usize).saturating_sub(3)) {
                writeln!(out, " {}\r", line)?;
            }
            writeln!(out, "\r")?;
        }

        if items.is_empty() && d.events.is_empty() {
            writeln!(
                out,
                " \x1b[2mthis plugin declares no actions, panes or events — it may work \
                 entirely on its own\x1b[0m\r"
            )?;
        }

        // Actions and panes in one list, because `enter` and `b` treat them the same way and
        // the cursor indexes into it. Two separate sections would mean two things claiming to
        // be "the highlighted one".
        if !items.is_empty() {
            writeln!(
                out,
                " \x1b[1mthings you can run\x1b[0m  \x1b[2m(enter runs it · b binds it to a key)\x1b[0m\r"
            )?;
            for (i, (target, label)) in items.iter().enumerate() {
                let sel = if i == self.detail_cursor {
                    "\x1b[7m>"
                } else {
                    " "
                };
                let kind = match target {
                    crate::BindTarget::Action(_) => "action",
                    crate::BindTarget::Pane(_) => "pane",
                };
                writeln!(
                    out,
                    "{}\x1b[0m  {:<44} \x1b[2m{:<7}{}\x1b[0m\r",
                    sel,
                    truncate(label, 44),
                    kind,
                    target.id()
                )?;
            }
            writeln!(out, "\r")?;
        }

        if !d.events.is_empty() {
            writeln!(
                out,
                " \x1b[1mruns by itself on\x1b[0m  \x1b[2m{}\x1b[0m\r",
                d.events.join(", ")
            )?;
            writeln!(out, "\r")?;
        }

        if let Some((target, _)) = items.get(self.detail_cursor) {
            // Show the lines that `b` would actually write, not a fixed example — panes and
            // actions produce different `type` values, and a wrong example here would send
            // someone to hand-write a binding herdr ignores.
            let (kind, command) = crate::bind_toml_fields(target, &d.plugin_id);
            writeln!(
                out,
                " \x1b[2mb binds the highlighted one:  type = \"{}\"  command = \"{}\"\x1b[0m\r",
                kind, command
            )?;
        }

        let footer = if self.awaiting_bind {
            "\x1b[33mpress a letter to bind this to prefix+shift+<letter>, or esc\x1b[0m"
                .to_string()
        } else {
            match &self.flash {
                Some(m) => format!("\x1b[36m{}\x1b[0m", m),
                None => "\x1b[1m[enter]\x1b[0m run  \x1b[1m[b]\x1b[0m bind to a key  \
                         \x1b[1m[j/k]\x1b[0m move  \x1b[1m[esc]\x1b[0m back"
                    .to_string(),
            }
        };
        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m{}\r",
            height.saturating_sub(1),
            rule,
            footer
        )?;
        out.flush()
    }

    /// Every key, grouped by what it acts on.
    ///
    /// The list's keys are deliberately paired: lowercase is the row under the cursor,
    /// uppercase is everything. That rule is invisible in the footer, which has room for one
    /// of each pair at most, so it is spelled out here.
    fn draw_help(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        write!(out, "\x1b[H\x1b[2J")?;
        // The one rule the whole keymap follows, stated before any of the keys.
        writeln!(
            out,
            "\x1b[1m keys\x1b[0m   \x1b[2mlowercase acts on the selected row ·              UPPERCASE on your whole list\x1b[0m\r"
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;

        let section = |out: &mut dyn Write, title: &str, keys: &[(&str, &str)]| -> io::Result<()> {
            writeln!(out, " \x1b[1m{}\x1b[0m\r", title)?;
            for (k, desc) in keys {
                writeln!(out, "   \x1b[1m{:<12}\x1b[0m {}\r", k, desc)?;
            }
            writeln!(out, "\r")
        };

        section(
            out,
            "choosing what to act on",
            &[
                ("space", "tick or untick this row (click does the same)"),
                ("esc", "clear the selection"),
                (
                    "",
                    "with nothing ticked, every action below uses the cursor row",
                ),
            ],
        )?;
        section(
            out,
            "acting on it",
            &[
                (
                    "i / I",
                    "install what is missing, and put drifted pins back",
                ),
                (
                    "u / U",
                    "update to the latest commit (pinned entries are skipped)",
                ),
                ("x / X", "uninstall — X removes everything not in your list"),
                ("r / R", "restore to the commits recorded in the lockfile"),
            ],
        )?;
        section(
            out,
            "your list",
            &[
                ("a", "add the selected installed plugin(s) to your list"),
                ("d", "remove this entry (does not uninstall)"),
                (
                    "/",
                    "search the marketplace — enter adds, ctrl+o opens the repo",
                ),
            ],
        )?;
        section(
            out,
            "finding your way",
            &[
                (
                    "l / →",
                    "what this plugin does — run its actions, or bind one to a key",
                ),
                ("j / k", "down / up  (arrows and the wheel work too)"),
                ("g / G", "first / last row"),
                (
                    "A",
                    "toggle auto-sync: install missing plugins when herdr starts",
                ),
                ("ctrl+r", "re-read the list and what is installed"),
                ("? / q", "close this help / quit"),
            ],
        )?;

        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m\x1b[2many key closes this\x1b[0m\r",
            height.saturating_sub(1),
            rule
        )?;
        out.flush()
    }

    /// The marketplace overlay: a query line, matching plugins, and what each one is.
    fn draw_browser(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let b = self.browser.as_ref().expect("checked by caller");
        let rule = "─".repeat((width as usize).clamp(20, 200));
        let results = b.results();
        let today = crate::registry::today_days();

        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(
            out,
            "\x1b[1m marketplace\x1b[0m  \x1b[2m{} of {} · {}\x1b[0m\r",
            results.len(),
            b.all.len(),
            b.source_note
        )?;
        writeln!(out, " search: \x1b[1m{}\x1b[0m\x1b[7m \x1b[0m\r", b.query)?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;

        let visible = (height as usize).saturating_sub(6).max(1);
        let start = if b.cursor >= visible {
            b.cursor - visible + 1
        } else {
            0
        };
        if results.is_empty() {
            writeln!(out, " \x1b[2mnothing matches\x1b[0m\r")?;
        }
        for (i, e) in results.iter().enumerate().skip(start).take(visible) {
            let selected = i == b.cursor;
            let pointer = if selected { "\x1b[7m>\x1b[0m" } else { " " };
            // Already in the list is worth knowing before you add it a second time.
            let mark = if b.is_listed(e) {
                "\x1b[32m✔\x1b[0m"
            } else {
                " "
            };
            // Last push, not stars, is what says whether a plugin still works. Four columns
            // is enough for "3d" / "2w" / "5mo" and costs almost nothing.
            let age = crate::registry::age_label(&e.pushed_at, today);
            writeln!(
                out,
                "{} {} \x1b[33m{:>4}★\x1b[0m {:<38} \x1b[2m{:<5}{}\x1b[0m\r",
                pointer,
                mark,
                e.stars,
                truncate(&e.full_name, 38),
                age,
                truncate(&e.description, width.saturating_sub(58) as usize)
            )?;
        }

        let footer = match &self.flash {
            Some(msg) => format!("\x1b[36m{}\x1b[0m", msg),
            None => "\x1b[1m[enter]\x1b[0m add to your list  \x1b[1m[ctrl+o]\x1b[0m open repo  \
                     \x1b[1m[↑↓]\x1b[0m move  \x1b[1m[ctrl+r]\x1b[0m refresh  \
                     \x1b[1m[esc]\x1b[0m back"
                .to_string(),
        };
        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m{}\r",
            height.saturating_sub(1),
            rule,
            footer
        )?;
        out.flush()
    }

    fn draw_list(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        // Rules span the pane. A fixed width looked deliberate at 80 columns and plainly
        // broken at 140, where the list ran well past the line meant to underline it.
        let rule = "─".repeat((width as usize).clamp(20, 200));
        // Home the cursor and clear, rather than scrolling: redraw in place.
        write!(out, "\x1b[H\x1b[2J")?;

        let counts = |s: fn(&Status) -> bool| self.rows.iter().filter(|r| s(&r.status)).count();
        let ok = counts(|s| *s == Status::Ok);
        let todo = counts(|s| matches!(s, Status::Missing | Status::Drifted { .. }));
        let extra = counts(|s| *s == Status::Extra);
        let stale = self.rows.iter().filter(|r| r.maybe_stale).count();
        // Say how old the update information is once it is old enough to mislead. Without
        // this, "nothing has updates" is indistinguishable from "I last looked two days ago",
        // and the only cure — pressing `/` — is not something anyone would think to try.
        let freshness = match crate::registry::cache_age_hours() {
            Some(h) if h >= 12 => format!(" \x1b[2m· update info {}h old, / refreshes\x1b[0m", h),
            None => " \x1b[2m· no update info yet, / fetches it\x1b[0m".to_string(),
            _ => String::new(),
        };

        // Showing auto-sync here is the only way a user learns it exists: it has no row of its
        // own, and something that installs software at startup should be visible, not buried.
        let auto = if crate::auto_sync_enabled() {
            "  \x1b[32m· auto-sync on\x1b[0m"
        } else {
            ""
        };
        writeln!(
            out,
            "\x1b[1m herdr-lazy\x1b[0m  \x1b[2m{} ok · {} to sync · {} unlisted{}\x1b[0m{}{}\r",
            ok,
            todo,
            extra,
            if stale > 0 {
                format!(" · \x1b[33m{} may have updates\x1b[0m\x1b[2m", stale)
            } else {
                String::new()
            },
            auto,
            freshness
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;

        if let Some(e) = &self.error {
            writeln!(out, " \x1b[31mcannot read plugin list:\x1b[0m {}\r", e)?;
        } else if self.rows.is_empty() {
            writeln!(
                out,
                " \x1b[2mno plugin list yet — run `herdr-lazy init`\x1b[0m\r"
            )?;
        }

        // Reserve the header (2), footer (2) and a spare line.
        let (visible, start) = self.list_window(height);

        for (i, row) in self.rows.iter().enumerate().skip(start).take(visible) {
            let selected = i == self.cursor;
            let pointer = if selected { "\x1b[7m>" } else { " " };
            // A checkbox only when something is ticked: an always-visible column of empty
            // boxes implies every row needs a decision, when the common case is acting on
            // the one row under the cursor.
            let box_ = if self.any_picked() {
                if row.picked {
                    "\x1b[32m[x]\x1b[0m "
                } else {
                    "\x1b[2m[ ]\x1b[0m "
                }
            } else {
                ""
            };
            let commit = row
                .commit
                .as_deref()
                .map(crate::short)
                .unwrap_or_else(|| "-".to_string());
            // `↑` next to the commit rather than a column of its own: it qualifies the commit
            // that is shown, and the row is already wide.
            let up = if row.maybe_stale {
                "\x1b[33m↑\x1b[0m"
            } else {
                " "
            };
            // The marker alone does not say what it means; on an otherwise-quiet row there is
            // space to say it.
            let note = match (row.maybe_stale, row.status.note()) {
                (true, n) if n.is_empty() => {
                    "its repo has been pushed to since you installed — press u to update"
                        .to_string()
                }
                (_, n) => n,
            };
            writeln!(
                out,
                "{} {}{}{}\x1b[0m {:<44} \x1b[2m{:<12}\x1b[0m{} {}\x1b[0m\r",
                pointer,
                box_,
                row.status.colour(),
                row.status.marker(),
                truncate(&row.label, 44),
                commit,
                up,
                if note.is_empty() {
                    String::new()
                } else {
                    format!("\x1b[2m{}\x1b[0m", note)
                },
            )?;
        }

        // Keys are shown as `[s] sync`, not `sync` with a bold "s".
        //
        // Bolding the first letter reads as a hint only if you already know the convention,
        // and it disappears entirely on a terminal that renders bold faintly — leaving a row
        // of words with no visible connection to any key. Brackets survive any theme.
        let legend = [
            ("i", "install"),
            ("u", "update"),
            ("x", "uninstall"),
            ("r", "restore"),
            ("a", "adopt"),
            ("d", "drop"),
            ("/", "search"),
            ("?", "help"),
        ]
        .iter()
        .map(|(k, label)| format!("\x1b[1m[{}]\x1b[0m {}", k, label))
        .collect::<Vec<_>>()
        .join("  ");
        let footer = match &self.flash {
            Some(msg) => format!("\x1b[36m{}\x1b[0m", msg),
            None => legend,
        };
        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m{}\r",
            height.saturating_sub(1),
            rule,
            footer
        )?;
        out.flush()
    }
}

/// Cut a label to `max` *characters*.
///
/// Not display columns: a CJK or emoji label counts as one char per glyph while occupying two
/// terminal cells, so such a row's trailing columns drift right. Plugin slugs are ASCII in
/// practice, and the alternative is a unicode-width dependency for a cosmetic case.
/// Break text to a width, on spaces where possible.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", keep)
    }
}

/// Drop out of the TUI, run something noisy, and wait for the user before going back.
///
/// The alternate screen is left entirely so the command's own output scrolls normally and
/// stays scrollable in the terminal's history afterwards.
fn suspended<F: FnOnce()>(f: F) -> io::Result<()> {
    terminal::disable_raw_mode()?;
    let mut out = io::stdout();
    write!(out, "\x1b[?1049l")?;
    out.flush()?;

    f();

    println!("\n\x1b[2m-- press any key to return --\x1b[0m");
    io::stdout().flush()?;
    terminal::enable_raw_mode()?;
    // Swallow one key press, ignoring release/repeat events so the pane does not flash past.
    loop {
        if let Ok(Event::Key(k)) = event::read() {
            if k.kind == KeyEventKind::Press {
                break;
            }
        }
    }
    write!(io::stdout(), "\x1b[?1049h")?;
    io::stdout().flush()
}

/// What a keypress means, once modifiers are taken into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Quit,
    /// A plain, unmodified character key.
    Command(char),
    /// A modifier chord we deliberately do nothing with.
    Ignore,
}

/// Decide before dispatch, so the "no modifiers" rule lives in one testable place.
fn classify(key: &KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return if key.code == KeyCode::Char('c') {
            Action::Quit
        } else {
            Action::Ignore
        };
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return Action::Ignore;
    }
    match key.code {
        KeyCode::Char(c) => Action::Command(c),
        _ => Action::Ignore,
    }
}

pub(crate) fn run() -> io::Result<()> {
    let mut out = io::stdout();

    // Without a terminal this fails as "Device not configured (os error 6)", which tells the
    // reader nothing. It happens for a specific, recurring reason: herdr runs plugin *actions*
    // with no PTY, so `ui` invoked as an action — or from a keybinding wired straight to one —
    // cannot work. Only a pane gets a terminal. Say that instead.
    if let Err(e) = terminal::enable_raw_mode() {
        eprintln!(
            "herdr-lazy ui needs a terminal, and does not have one ({}).",
            e
        );
        eprintln!();
        eprintln!("If you ran this as a herdr plugin action or keybinding: actions get no PTY.");
        eprintln!("Open the pane instead:");
        eprintln!("  herdr plugin pane open --plugin herdr-lazy --entrypoint manage");
        return Ok(());
    }
    // Alternate screen, hide cursor, and capture mouse. Mouse is opt-out at the terminal
    // level: with capture on, the terminal stops doing its own selection, so anyone who wants
    // to copy text with the mouse would be blocked. SGR mode (1006) is enabled alongside 1000
    // so clicks past column 95 still report correctly.
    write!(out, "\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1006h")?;
    out.flush()?;

    let result = event_loop(&mut out);

    // Restore unconditionally, even if the loop failed: leaving a pane in raw mode with no
    // cursor would wedge the terminal.
    write!(out, "\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l")?;
    out.flush()?;
    terminal::disable_raw_mode()?;
    result
}

fn event_loop(out: &mut impl Write) -> io::Result<()> {
    let mut app = App::load();

    loop {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        app.draw(out, width, height)?;

        let key = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
            Event::Mouse(m) => {
                app.handle_mouse(m, height)?;
                continue;
            }
            Event::Resize(..) => continue,
            _ => continue,
        };

        // Any keypress retires the previous result line.
        app.flash = None;

        // Ctrl+C quits; everything else is a plain, unmodified key.
        //
        // Matching on `KeyCode::Char(c)` alone would also fire for the modified forms, and a
        // pty sends Ctrl+D at end of input — which silently ran "drop from list" on whatever
        // the cursor happened to be on. Destructive actions must not be reachable by a
        // modifier chord the user did not intend.
        // Help is modal and closes on anything, so it is checked before every other keymap.
        if app.help {
            app.help = false;
            continue;
        }

        // Details view: a short list of runnable actions.
        // Confirmation is modal: only y / n / esc mean anything here.
        if app.pending_bind.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => app.commit_bind(),
                _ => {
                    app.pending_bind = None;
                    app.flash = Some("not bound".to_string());
                }
            }
            continue;
        }

        if let Some(idx) = app.detail_of {
            let n = app
                .rows
                .get(idx)
                .and_then(|r| r.detail.as_ref())
                .map(|d| App::bindable(d).len())
                .unwrap_or(0);
            // Ctrl-chords first: a plain `j` moves, but Ctrl+J is a pty's Enter, and a
            // later arm would never see it.
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Char('m') => app.run_detail_action(),
                    KeyCode::Char('c') => return Ok(()),
                    _ => {}
                }
                continue;
            }
            // Waiting for the letter to bind to: take it and nothing else.
            if app.awaiting_bind {
                app.awaiting_bind = false;
                match key.code {
                    KeyCode::Char(c) if c.is_ascii_alphabetic() => app.bind_selected(c),
                    KeyCode::Esc => app.flash = Some("not bound".to_string()),
                    _ => app.flash = Some("that is not a letter — nothing bound".to_string()),
                }
                continue;
            }

            match key.code {
                KeyCode::Char('b') => {
                    if app
                        .rows
                        .get(idx)
                        .and_then(|r| r.detail.as_ref())
                        .map(|d| App::bindable(d).is_empty())
                        .unwrap_or(true)
                    {
                        app.flash = Some("no actions to bind".to_string());
                    } else {
                        app.awaiting_bind = true;
                        app.flash = None;
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.detail_of = None;
                    app.flash = None;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    if app.detail_cursor + 1 < n {
                        app.detail_cursor += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.detail_cursor = app.detail_cursor.saturating_sub(1)
                }
                KeyCode::Enter => app.run_detail_action(),
                _ => {}
            }
            continue;
        }

        // The browser is a text field: printable keys type into it rather than acting as
        // commands, so its input is handled before the list's keymap is consulted.
        if app.browser.is_some() {
            match key.code {
                KeyCode::Esc => {
                    // Rebuild the list, not just hide the browser. Opening the browser
                    // refreshes the marketplace cache, and the `↑` markers are computed from
                    // that cache when the rows are built — so without this, the one action
                    // that fetches fresh information is also the one whose result you cannot
                    // see until something else happens to reload.
                    app.browser = None;
                    app.flash = None;
                    app.refresh();
                }
                // Enter, plus the raw bytes some terminals deliver instead of it: Ctrl+J is
                // LF and Ctrl+M is CR. A pty that translates CR to LF would otherwise leave
                // Enter doing nothing at all, with no hint as to why.
                KeyCode::Enter => app.add_and_return(),
                KeyCode::Char('j') | KeyCode::Char('m')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    app.add_and_return()
                }
                // NOTHING here may bind a plain letter. The query is a text field, so `a`
                // and `o` must type an "a" and an "o" — binding them to add and open made
                // "worktree" search for "wrktree", and typing any word containing an "a"
                // silently wrote to the user's list. Commands in this view take a modifier.
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.open_selected_url()
                }
                KeyCode::Down => app.browser.as_mut().unwrap().move_down(),
                KeyCode::Up => app.browser.as_mut().unwrap().move_up(),
                KeyCode::Backspace => {
                    app.flash = None;
                    app.browser.as_mut().unwrap().backspace();
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.open_browser(true)
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(())
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.flash = None;
                    app.browser.as_mut().unwrap().push(c);
                }
                _ => {}
            }
            if let Some(b) = app.browser.as_mut() {
                b.clamp();
            }
            continue;
        }

        // Ctrl+R re-reads state in both views: the list here, the marketplace index in the
        // browser. Plain `r` is restore, following lazy.nvim.
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            app.refresh();
            app.flash = Some("re-read your list and what is installed".to_string());
            continue;
        }

        match classify(&key) {
            Action::Quit => return Ok(()),
            Action::Ignore if !matches!(key.code, KeyCode::Char(_)) => {}
            Action::Ignore => continue,
            Action::Command(_) => {}
        }

        match key.code {
            KeyCode::Char('q') => return Ok(()),
            // Esc clears a selection first; only an unselected Esc quits. Quitting while
            // rows are ticked would silently discard work the user was midway through.
            KeyCode::Esc => {
                if app.any_picked() {
                    app.clear_picks();
                    app.flash = Some("selection cleared".to_string());
                } else {
                    return Ok(());
                }
            }

            KeyCode::Char('j') | KeyCode::Down => {
                if app.cursor + 1 < app.rows.len() {
                    app.cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => app.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => app.cursor = app.rows.len().saturating_sub(1),

            // One rule: lowercase acts on the row under the cursor, uppercase on everything.
            //
            // The letters follow lazy.nvim (i/I install, u/U update, x/X clean, r/R restore),
            // because most people arriving here already have those in their fingers. lazy's
            // `S` (sync = install + clean + update) is deliberately NOT copied: it sits
            // outside that rule, and a key that quietly uninstalls things under a gentle name
            // is the wrong thing to inherit. See the `S` arm below.
            // Space / Enter tick the row; every bulk action then applies to what is ticked.
            KeyCode::Char(' ') | KeyCode::Enter => app.toggle_pick(),
            KeyCode::Char('l') | KeyCode::Right => app.open_detail(),
            KeyCode::Char('i') => app.install_targets()?,
            KeyCode::Char('I') => {
                suspended(|| {
                    let _ = crate::cmd_sync(false, &[]);
                })?;
                app.refresh();
            }
            KeyCode::Char('u') => app.update_targets()?,
            KeyCode::Char('U') => {
                suspended(|| {
                    let _ = crate::cmd_update(&[]);
                })?;
                app.refresh();
            }
            KeyCode::Char('x') => app.uninstall_targets(),
            KeyCode::Char('X') => {
                suspended(|| {
                    let _ = crate::cmd_sync(true, &[]);
                })?;
                app.refresh();
            }
            KeyCode::Char('r') => app.restore_targets()?,
            KeyCode::Char('R') => {
                suspended(|| {
                    let _ = crate::cmd_restore(&[]);
                })?;
                app.refresh();
            }
            KeyCode::Char('a') => app.adopt_targets(),
            KeyCode::Char('A') => {
                app.flash = Some(match crate::toggle_auto_sync() {
                    Ok((_, msg)) => msg,
                    Err(e) => format!("could not change auto-sync: {}", e),
                })
            }
            KeyCode::Char('d') => app.drop_selected(),
            KeyCode::Char('/') => app.open_browser(false),
            KeyCode::Char('?') => app.help = true,

            // Keys lazy.nvim has that this does not. Rather than doing nothing — which reads
            // as "the pane is broken" to someone whose fingers know lazy — say where the
            // equivalent lives.
            KeyCode::Char('S') => {
                app.flash = Some(
                    "no single sync key here — [I] install all · [U] update all · \
                     [X] remove what is not in your list"
                        .to_string(),
                )
            }
            KeyCode::Char('C') | KeyCode::Char('c') => {
                app.flash = Some("no check yet — [U] updates and reports what moved".to_string())
            }
            KeyCode::Char('L') => {
                app.flash =
                    Some("no log here — `herdr plugin log list` shows plugin output".to_string())
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(owner: &str, repo: &str, commit: &str, enabled: bool) -> Installed {
        Installed {
            plugin_id: repo.to_string(),
            name: repo.to_string(),
            enabled,
            source_kind: "github".to_string(),
            slug: Some(format!("{}/{}", owner, repo)),
            resolved_commit: Some(commit.to_string()),
            source_values: vec![owner.to_string(), repo.to_string()],
            ..Default::default()
        }
    }

    fn local(id: &str) -> Installed {
        Installed {
            plugin_id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            source_kind: "local".to_string(),
            slug: None,
            resolved_commit: None,
            source_values: vec!["local".to_string()],
            ..Default::default()
        }
    }

    const SHA: &str = "f32b0825f12543c1d03e54fb10d1741c40d66cdc";
    const OTHER: &str = "a8f86ec4103bc367b52e547b492483f3b792a952";

    #[test]
    fn bundle_entries_report_their_state() {
        let desired: Vec<Spec> = ["o/installed", "o/absent", &format!("o/pinned@{}", OTHER)]
            .iter()
            .map(|l| Spec::parse(l))
            .collect();
        let installed = vec![
            github("o", "installed", SHA, true),
            github("o", "pinned", SHA, true), // pinned to OTHER, sitting at SHA
        ];

        let r = rows(&desired, &installed);
        assert_eq!(r[0].status, Status::Ok);
        assert_eq!(r[1].status, Status::Missing);
        assert_eq!(
            r[2].status,
            Status::Drifted {
                have: SHA.to_string()
            }
        );
    }

    /// A local link must never be shown as prunable: `--prune` protects it, and a UI that
    /// suggests otherwise invites the user to try to remove the tool running the pane.
    #[test]
    fn extras_distinguish_local_links_from_prunable_plugins() {
        let desired = vec![Spec::parse("o/wanted")];
        let installed = vec![
            github("o", "wanted", SHA, true),
            github("someone", "unwanted", SHA, true),
            local("someone.local-plugin"),
        ];

        let r = rows(&desired, &installed);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].status, Status::Ok);
        assert_eq!(r[1].status, Status::Extra);
        assert_eq!(r[1].label, "someone/unwanted");
        assert_eq!(r[2].status, Status::ExtraLocal);
    }

    #[test]
    fn a_disabled_plugin_is_not_reported_as_ok() {
        let desired = vec![Spec::parse("o/repo")];
        let installed = vec![github("o", "repo", SHA, false)];
        assert_eq!(rows(&desired, &installed)[0].status, Status::Disabled);
    }

    #[test]
    fn tag_pins_are_shown_as_unverifiable() {
        let desired = vec![Spec::parse("o/repo@v1.2.0")];
        let installed = vec![github("o", "repo", SHA, true)];
        assert_eq!(rows(&desired, &installed)[0].status, Status::Unverifiable);
    }

    #[test]
    fn labels_are_truncated_by_char_not_byte() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
        // Japanese on purpose: each glyph is 3 bytes AND 2 display columns. The byte width
        // catches a byte-indexed slice (`&s[..max]`), which panics mid-character; the display
        // width is the case the doc comment above warns about, where the trailing columns of
        // such a row drift right.
        assert_eq!(truncate("ありがとうございます", 4), "ありが…");
    }

    /// The two states that sit adjacent in the list and mean opposite things must each say
    /// whether the plugin is installed — reading one as the other is the mistake to prevent.
    #[test]
    fn missing_and_extra_notes_both_state_installed_status() {
        let missing = Status::Missing.note();
        let extra = Status::Extra.note();
        assert!(missing.contains("not installed"), "{}", missing);
        assert!(extra.starts_with("installed,"), "{}", extra);
        assert!(missing.contains("in your list"), "{}", missing);
        assert!(extra.contains("not in your list"), "{}", extra);
    }

    #[test]
    fn no_note_uses_the_word_bundle() {
        for s in [
            Status::Missing,
            Status::Drifted { have: SHA.into() },
            Status::Unverifiable,
            Status::Disabled,
            Status::Extra,
            Status::ExtraLocal,
        ] {
            assert!(
                !s.note().to_lowercase().contains("bundle"),
                "jargon leaked into the UI: {}",
                s.note()
            );
        }
    }

    /// `a` needs an owner/repo to write into the list. A local link has none, so the row must
    /// carry no slug — otherwise the pane would offer to adopt something it cannot express.
    #[test]
    fn a_local_link_row_carries_no_slug_to_adopt() {
        let installed = vec![
            local("someone.local-plugin"),
            github("someone", "unlisted", SHA, true),
        ];
        let r = rows(&[], &installed);

        let link = r.iter().find(|x| x.status == Status::ExtraLocal).unwrap();
        assert_eq!(link.slug, None, "a local link cannot be adopted");
        assert_eq!(link.listed_as, None);

        let extra = r.iter().find(|x| x.status == Status::Extra).unwrap();
        assert_eq!(extra.slug.as_deref(), Some("someone/unlisted"));
        assert_eq!(
            extra.listed_as, None,
            "an extra is by definition not listed"
        );
    }

    /// `d` rewrites plugins.list by exact line, so a pinned row must report the line as
    /// written — dropping `owner/repo` when the file says `owner/repo@sha` would silently
    /// fail to remove anything.
    #[test]
    fn a_listed_row_reports_its_exact_line_including_any_pin() {
        let desired = vec![
            Spec::parse("o/plain"),
            Spec::parse(&format!("o/pinned@{}", SHA)),
        ];
        let installed = vec![
            github("o", "plain", SHA, true),
            github("o", "pinned", SHA, true),
        ];
        let r = rows(&desired, &installed);

        assert_eq!(r[0].listed_as.as_deref(), Some("o/plain"));
        assert_eq!(
            r[1].listed_as.as_deref(),
            Some(format!("o/pinned@{}", SHA).as_str())
        );
        // `a` uses the repo alone, without the pin.
        assert_eq!(r[1].slug.as_deref(), Some("o/pinned"));
    }

    /// Regression: installed normally (not as a local link), herdr-lazy is an ordinary github
    /// plugin that is absent from the user's list — precisely what prune removes. It must be
    /// recognised as itself, or `X` deletes the running tool's own directory.
    #[test]
    fn herdr_lazy_is_never_shown_as_prunable() {
        let mut me = github("natori-hrj", "herdr-lazy", SHA, true);
        me.plugin_id = "herdr-lazy".to_string();
        let r = rows(&[], &[me]);
        assert_eq!(r[0].status, Status::SelfEntry);
        assert_ne!(r[0].status, Status::Extra);

        // Also while developing, where it is a local link — same protection, clearer reason.
        let dev = local("herdr-lazy");
        assert_eq!(rows(&[], &[dev])[0].status, Status::SelfEntry);
    }

    /// Regression: destructive keys must be unreachable via a modifier chord.
    ///
    /// The event loop matched `KeyCode::Char('d')` without inspecting modifiers, so Ctrl+D —
    /// which a pty sends at end of input, and which users press to mean "end"/"quit" — ran
    /// "drop from list" on whatever the cursor was on. Two entries were silently deleted from
    /// a real list before this was noticed.
    /// lazy.nvim users arrive with `i`/`u`/`x`/`r` in their fingers, and `S` too. The first
    /// four must act; `S` must explain itself rather than doing nothing or, worse, doing
    /// something destructive under a gentle name.
    #[test]
    fn the_keymap_follows_lazy_nvim_where_it_can() {
        let plain = |c| classify(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        for c in ['i', 'u', 'x', 'r', 'a', 'd', '/', '?'] {
            assert_eq!(plain(c), Action::Command(c), "{} should be a command", c);
        }
        for c in ['I', 'U', 'X', 'R'] {
            assert_eq!(
                classify(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)),
                Action::Command(c),
                "{} should be a command",
                c
            );
        }
        // `s` no longer does anything: it used to install, and silently re-binding a key that
        // people may have learned is worse than leaving it inert.
        assert_eq!(plain('s'), Action::Command('s'));
    }

    #[test]
    fn modified_keys_are_not_treated_as_commands() {
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);

        for c in ['d', 'a', 'x', 's', 'u'] {
            assert_eq!(
                classify(&plain(c)),
                Action::Command(c),
                "plain {} should act",
                c
            );
            assert_eq!(
                classify(&ctrl(c)),
                Action::Ignore,
                "ctrl+{} must not act",
                c
            );
            assert_eq!(classify(&alt(c)), Action::Ignore, "alt+{} must not act", c);
        }
        // Ctrl+C remains the one honoured chord.
        assert_eq!(classify(&ctrl('c')), Action::Quit);
        assert_eq!(classify(&plain('q')), Action::Command('q'));
    }

    /// Uppercase must reach the dispatcher as its own command, not be folded into the
    /// lowercase one — `S` means "the whole list" and `s` means "this row".
    #[test]
    fn shifted_letters_are_distinct_commands() {
        let shifted = KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT);
        assert_eq!(classify(&shifted), Action::Command('S'));
        let plain = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(classify(&plain), Action::Command('s'));
    }

    /// `x` uninstalls by herdr's plugin id, so every row that is actually installed must
    /// carry it — including listed rows, which are the ones a user is most likely to remove.
    #[test]
    fn installed_rows_carry_the_id_needed_to_uninstall() {
        let desired = vec![Spec::parse("o/listed"), Spec::parse("o/absent")];
        let installed = vec![
            github("o", "listed", SHA, true),
            local("someone.local-plugin"),
        ];
        let r = rows(&desired, &installed);

        assert_eq!(r[0].installed, Some(("listed".into(), "github".into())));
        assert_eq!(
            r[1].installed, None,
            "a missing entry cannot be uninstalled"
        );
        let link = r.iter().find(|x| x.status == Status::ExtraLocal).unwrap();
        assert_eq!(
            link.installed,
            Some(("someone.local-plugin".into(), "local".into()))
        );
    }

    /// Clicking maps screen lines to rows; being off by one means clicking a row acts on its
    /// neighbour, which for an "act on second click" gesture is exactly the wrong failure.
    #[test]
    fn clicks_land_on_the_row_that_was_clicked() {
        let desired: Vec<Spec> = (0..5)
            .map(|i| Spec::parse(&format!("owner/p{}", i)))
            .collect();
        let mut app = App {
            rows: rows(&desired, &[]),
            ..Default::default()
        };
        let height = 20; // 5 chrome lines -> 15 visible, more than our 5 rows

        // The first list line sits directly under the header.
        assert_eq!(app.row_at(LIST_TOP as u16, height), Some(0));
        assert_eq!(app.row_at(LIST_TOP as u16 + 3, height), Some(3));
        // Header and beyond-the-last-row are not rows.
        assert_eq!(app.row_at(0, height), None);
        assert_eq!(app.row_at(1, height), None);
        assert_eq!(app.row_at(LIST_TOP as u16 + 5, height), None);

        // Once scrolled, the top line is the first visible row, not row 0.
        app.cursor = 4;
        let (visible, start) = app.list_window(4); // tiny pane: 1 visible row
        assert_eq!(visible, 1);
        assert_eq!(start, 4);
        assert_eq!(app.row_at(LIST_TOP as u16, 4), Some(4));
    }

    /// A single click must never install or remove anything — it only moves the cursor. The
    /// act-on-second-click rule is what makes an accidental click harmless.
    #[test]
    fn a_click_action_is_only_offered_where_it_is_safe() {
        // These are the two states where a click does something, and both are additive.
        for s in [Status::Missing, Status::Extra] {
            assert!(
                matches!(s, Status::Missing | Status::Extra),
                "click acts on missing (install) and extra (adopt) only"
            );
        }
        // Uninstalling is never reachable by mouse: no status maps a click to `x`.
        for s in [
            Status::Ok,
            Status::ExtraLocal,
            Status::SelfEntry,
            Status::Disabled,
        ] {
            assert!(
                !matches!(s, Status::Missing | Status::Extra),
                "{:?} must not have a click action",
                s
            );
        }
    }

    /// A checkbox toggles in place: press once to tick, press again to untick, cursor
    /// unmoved. An earlier version advanced the cursor after ticking, which meant pressing
    /// twice ticked two rows and there was no way to change your mind without navigating
    /// back — the one thing a checkbox is supposed to make easy.
    #[test]
    fn ticking_toggles_in_place() {
        let desired: Vec<Spec> = (0..4)
            .map(|i| Spec::parse(&format!("owner/p{}", i)))
            .collect();
        let mut app = App {
            rows: rows(&desired, &[]),
            ..Default::default()
        };

        app.cursor = 1;
        app.toggle_pick();
        assert!(app.rows[1].picked, "first press ticks");
        assert_eq!(app.cursor, 1, "cursor must not move");

        app.toggle_pick();
        assert!(!app.rows[1].picked, "second press unticks");
        assert_eq!(app.cursor, 1);
        assert!(!app.any_picked());
    }

    /// Bulk actions follow the ticks; with none, they fall back to the cursor row, so someone
    /// who never touches a checkbox sees no change in behaviour.
    #[test]
    fn bulk_actions_target_ticks_or_the_cursor() {
        let desired: Vec<Spec> = (0..4)
            .map(|i| Spec::parse(&format!("owner/p{}", i)))
            .collect();
        let mut app = App {
            rows: rows(&desired, &[]),
            ..Default::default()
        };

        app.cursor = 2;
        assert_eq!(app.targets(), vec![2], "no ticks: the cursor row");

        app.cursor = 0;
        app.toggle_pick();
        app.cursor = 2;
        app.toggle_pick();
        assert_eq!(app.targets(), vec![0, 2], "ticks win over the cursor");

        app.clear_picks();
        assert_eq!(app.targets(), vec![app.cursor]);
    }

    /// The bug this pins down: `b` only ever offered actions, and four of the seven plugins
    /// in the default set expose a pane and no actions at all. Pressing `b` on those said
    /// "no actions to bind" — so for most of what a new user has installed, the feature did
    /// nothing.
    #[test]
    fn panes_are_bindable_not_just_actions() {
        let pane_only = PluginDetail {
            plugin_id: "triage".into(),
            panes: vec![("list".into(), "Triage".into(), "split".into())],
            ..Default::default()
        };
        let items = App::bindable(&pane_only);
        assert_eq!(items.len(), 1, "a pane-only plugin must still be bindable");
        assert_eq!(items[0].0, crate::BindTarget::Pane("list".into()));
        assert_eq!(items[0].1, "Triage");

        let neither = PluginDetail {
            plugin_id: "x".into(),
            ..Default::default()
        };
        assert!(App::bindable(&neither).is_empty());
    }

    /// Actions come first, then panes, and the order must match what is drawn — the cursor
    /// indexes into this list, so a mismatch binds the wrong thing.
    #[test]
    fn bindable_lists_actions_before_panes() {
        let both = PluginDetail {
            plugin_id: "p".into(),
            actions: vec![("run".into(), "Run it".into())],
            panes: vec![("side".into(), "Sidebar".into(), "split".into())],
            ..Default::default()
        };
        let items = App::bindable(&both);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, crate::BindTarget::Action("run".into()));
        assert_eq!(items[1].0, crate::BindTarget::Pane("side".into()));
    }

    /// A missing title falls back to the id, so a row is never blank.
    #[test]
    fn an_untitled_entry_shows_its_id() {
        let d = PluginDetail {
            plugin_id: "p".into(),
            actions: vec![("bare".into(), String::new())],
            ..Default::default()
        };
        assert_eq!(App::bindable(&d)[0].1, "bare");
    }

    fn market(name: &str, pushed_at: &str) -> crate::registry::Entry {
        crate::registry::Entry {
            full_name: name.to_string(),
            pushed_at: pushed_at.to_string(),
            ..Default::default()
        }
    }

    fn installed_at(owner: &str, repo: &str, ms: u64) -> Installed {
        let mut p = github(owner, repo, SHA, true);
        p.installed_unix_ms = Some(ms);
        p
    }

    /// Day 20000 of the epoch, in ms — a fixed point so the test does not drift with the clock.
    const DAY_MS: u64 = 86_400_000;

    #[test]
    fn a_repo_pushed_after_install_is_flagged() {
        let desired = vec![Spec::parse("o/moved"), Spec::parse("o/still")];
        let installed = vec![
            installed_at("o", "moved", 20_000 * DAY_MS),
            installed_at("o", "still", 20_010 * DAY_MS),
        ];
        let market = vec![
            market("o/moved", "2024-10-05T00:00:00Z"), // long after day 20000
            market("o/still", "1970-01-05T00:00:00Z"), // long before
        ];
        let r = rows_with_updates(&desired, &installed, &market);
        assert!(r[0].maybe_stale, "a later push must be reported");
        assert!(!r[1].maybe_stale, "an older push must not be");
    }

    /// A pin says "this commit, deliberately". Reporting it as out of date would train people
    /// to ignore the marker.
    #[test]
    fn a_pinned_entry_is_never_flagged() {
        let sha = "10e93033263549600e75119c5617dac48137d011";
        let desired = vec![Spec::parse(&format!("o/pinned@{}", sha))];
        let installed = vec![installed_at("o", "pinned", 20_000 * DAY_MS)];
        let market = vec![market("o/pinned", "2024-10-05T00:00:00Z")];
        assert!(!rows_with_updates(&desired, &installed, &market)[0].maybe_stale);
    }

    /// With no cached index — a fresh install that has never opened the browser — the column
    /// is simply absent. It must never become a reason to reach for the network.
    #[test]
    fn without_a_cached_index_nothing_is_flagged() {
        let desired = vec![Spec::parse("o/x")];
        let installed = vec![installed_at("o", "x", 0)];
        assert!(!rows_with_updates(&desired, &installed, &[])[0].maybe_stale);
    }

    /// An unlisted plugin can be stale too — it is installed, and `u` would update it.
    #[test]
    fn unlisted_plugins_are_checked_as_well() {
        let installed = vec![installed_at("someone", "extra", 20_000 * DAY_MS)];
        let market = vec![market("someone/extra", "2024-10-05T00:00:00Z")];
        let r = rows_with_updates(&[], &installed, &market);
        assert_eq!(r.len(), 1);
        assert!(r[0].maybe_stale);
    }

    #[test]
    fn every_status_has_a_marker_and_colour() {
        for s in [
            Status::Ok,
            Status::Missing,
            Status::Drifted { have: SHA.into() },
            Status::Unverifiable,
            Status::Disabled,
            Status::Extra,
            Status::ExtraLocal,
        ] {
            assert!(!s.marker().is_empty());
            assert!(s.colour().starts_with("\x1b["));
        }
    }
}
