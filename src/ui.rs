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
            Status::Missing => "in your list, not installed — press s to install".to_string(),
            Status::Drifted { have } => format!(
                "installed at {}, pinned elsewhere — press s to restore the pin",
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
}

/// Build the view: every bundle entry, then anything installed that the bundle does not name.
fn rows(desired: &[Spec], installed: &[Installed]) -> Vec<Row> {
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
            status: if p.source_kind == "local" {
                Status::ExtraLocal
            } else {
                Status::Extra
            },
            listed_as: None,
            slug: p.slug.clone(),
        });
    }

    rows
}

struct App {
    rows: Vec<Row>,
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
        match installed_plugins() {
            Ok(installed) => App {
                rows: rows(&desired, &installed),
                cursor: 0,
                error: None,
                flash: None,
            },
            Err(e) => App {
                rows: Vec::new(),
                cursor: 0,
                error: Some(e),
                flash: None,
            },
        }
    }

    fn refresh(&mut self) {
        let (cursor, flash) = (self.cursor, self.flash.take());
        *self = App::load();
        self.cursor = cursor.min(self.rows.len().saturating_sub(1));
        self.flash = flash;
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    /// `a` — bring an installed-but-unlisted plugin under management.
    fn adopt_selected(&mut self) {
        let Some(row) = self.selected() else { return };
        if row.listed_as.is_some() {
            self.flash = Some(format!("{} is already in your list", row.label));
            return;
        }
        let Some(slug) = row.slug.clone() else {
            self.flash = Some(format!(
                "{} is a local link — it has no owner/repo to record",
                row.label
            ));
            return;
        };
        self.flash = Some(match crate::add_to_list(&slug) {
            Ok(msg) => msg,
            Err(e) => format!("could not write the list: {}", e),
        });
        self.refresh();
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

    fn draw(&self, out: &mut impl Write, height: u16) -> io::Result<()> {
        // Home the cursor and clear, rather than scrolling: redraw in place.
        write!(out, "\x1b[H\x1b[2J")?;

        let counts = |s: fn(&Status) -> bool| self.rows.iter().filter(|r| s(&r.status)).count();
        let ok = counts(|s| *s == Status::Ok);
        let todo = counts(|s| matches!(s, Status::Missing | Status::Drifted { .. }));
        let extra = counts(|s| *s == Status::Extra);

        writeln!(
            out,
            "\x1b[1m herdr-lazy\x1b[0m  \x1b[2m{} ok · {} to sync · {} unlisted\x1b[0m\r",
            ok, todo, extra
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", "─".repeat(64))?;

        if let Some(e) = &self.error {
            writeln!(out, " \x1b[31mcannot read plugin list:\x1b[0m {}\r", e)?;
        } else if self.rows.is_empty() {
            writeln!(
                out,
                " \x1b[2mno plugin list yet — run `herdr-lazy init`\x1b[0m\r"
            )?;
        }

        // Reserve the header (2), footer (2) and a spare line.
        let visible = (height as usize).saturating_sub(5).max(1);
        let start = if self.cursor >= visible {
            self.cursor - visible + 1
        } else {
            0
        };

        for (i, row) in self.rows.iter().enumerate().skip(start).take(visible) {
            let selected = i == self.cursor;
            let pointer = if selected { "\x1b[7m>" } else { " " };
            let commit = row
                .commit
                .as_deref()
                .map(crate::short)
                .unwrap_or_else(|| "-".to_string());
            let note = row.status.note();
            writeln!(
                out,
                "{} {}{}\x1b[0m {:<44} \x1b[2m{:<12}\x1b[0m {}\x1b[0m\r",
                pointer,
                row.status.colour(),
                row.status.marker(),
                truncate(&row.label, 44),
                commit,
                if note.is_empty() {
                    String::new()
                } else {
                    format!("\x1b[2m{}\x1b[0m", note)
                },
            )?;
        }

        let legend = "\x1b[1ms\x1b[0m sync  \x1b[1mu\x1b[0m update  \x1b[1ma\x1b[0m adopt                        \x1b[1md\x1b[0m drop  \x1b[1mx\x1b[0m uninstall extras                        \x1b[1mr\x1b[0m refresh  \x1b[1mq\x1b[0m quit";
        let footer = match &self.flash {
            Some(msg) => format!("\x1b[36m{}\x1b[0m", msg),
            None => legend.to_string(),
        };
        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m{}\r",
            height.saturating_sub(1),
            "─".repeat(64),
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
    terminal::enable_raw_mode()?;
    write!(out, "\x1b[?1049h\x1b[?25l")?; // alternate screen, hide cursor
    out.flush()?;

    let result = event_loop(&mut out);

    // Restore unconditionally, even if the loop failed: leaving a pane in raw mode with no
    // cursor would wedge the terminal.
    write!(out, "\x1b[?25h\x1b[?1049l")?;
    out.flush()?;
    terminal::disable_raw_mode()?;
    result
}

fn event_loop(out: &mut impl Write) -> io::Result<()> {
    let mut app = App::load();

    loop {
        let (_, height) = terminal::size().unwrap_or((80, 24));
        app.draw(out, height)?;

        let key = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => k,
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
        match classify(&key) {
            Action::Quit => return Ok(()),
            Action::Ignore if !matches!(key.code, KeyCode::Char(_)) => {}
            Action::Ignore => continue,
            Action::Command(_) => {}
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),

            KeyCode::Char('j') | KeyCode::Down => {
                if app.cursor + 1 < app.rows.len() {
                    app.cursor += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => app.cursor = app.cursor.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => app.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => app.cursor = app.rows.len().saturating_sub(1),

            KeyCode::Char('s') => {
                suspended(|| {
                    let _ = crate::cmd_sync(false);
                })?;
                app.refresh();
            }
            KeyCode::Char('u') => {
                suspended(|| {
                    let _ = crate::cmd_update(&[]);
                })?;
                app.refresh();
            }
            KeyCode::Char('x') => {
                suspended(|| {
                    let _ = crate::cmd_sync(true);
                })?;
                app.refresh();
            }
            KeyCode::Char('a') => app.adopt_selected(),
            KeyCode::Char('d') => app.drop_selected(),
            KeyCode::Char('r') => app.refresh(),
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
            local("herdr-lazy"),
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
            local("herdr-lazy"),
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

    /// Regression: destructive keys must be unreachable via a modifier chord.
    ///
    /// The event loop matched `KeyCode::Char('d')` without inspecting modifiers, so Ctrl+D —
    /// which a pty sends at end of input, and which users press to mean "end"/"quit" — ran
    /// "drop from list" on whatever the cursor was on. Two entries were silently deleted from
    /// a real list before this was noticed.
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
