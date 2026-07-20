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

    fn note(&self) -> String {
        match self {
            Status::Ok => String::new(),
            Status::Missing => "not installed — press s".to_string(),
            Status::Drifted { have } => {
                format!("at {} — press s to restore the pin", crate::short(have))
            }
            Status::Unverifiable => "pinned to a tag/branch — cannot verify locally".to_string(),
            Status::Disabled => "disabled — herdr will not run it".to_string(),
            Status::Extra => "not in bundle — press x to prune".to_string(),
            Status::ExtraLocal => "local link — prune will not touch it".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    label: String,
    commit: Option<String>,
    status: Status,
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
        });
    }

    rows
}

struct App {
    rows: Vec<Row>,
    cursor: usize,
    /// Set when a refresh fails, so the pane explains itself instead of showing an empty list.
    error: Option<String>,
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
            },
            Err(e) => App {
                rows: Vec::new(),
                cursor: 0,
                error: Some(e),
            },
        }
    }

    fn refresh(&mut self) {
        let cursor = self.cursor;
        *self = App::load();
        self.cursor = cursor.min(self.rows.len().saturating_sub(1));
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
            "\x1b[1m herdr-lazy\x1b[0m  \x1b[2m{} ok · {} to sync · {} extra\x1b[0m\r",
            ok, todo, extra
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", "─".repeat(64))?;

        if let Some(e) = &self.error {
            writeln!(out, " \x1b[31mcannot read plugin list:\x1b[0m {}\r", e)?;
        } else if self.rows.is_empty() {
            writeln!(
                out,
                " \x1b[2mno bundle yet — run `herdr-lazy init`\x1b[0m\r"
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

        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m\x1b[1ms\x1b[0m sync  \x1b[1mu\x1b[0m update  \
             \x1b[1mx\x1b[0m prune  \x1b[1mr\x1b[0m refresh  \x1b[1mq\x1b[0m quit\r",
            height.saturating_sub(1),
            "─".repeat(64)
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

        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => return Ok(()),
            KeyEvent {
                code: KeyCode::Char('q') | KeyCode::Esc,
                ..
            } => return Ok(()),

            KeyEvent {
                code: KeyCode::Char('j') | KeyCode::Down,
                ..
            } => {
                if app.cursor + 1 < app.rows.len() {
                    app.cursor += 1;
                }
            }
            KeyEvent {
                code: KeyCode::Char('k') | KeyCode::Up,
                ..
            } => {
                app.cursor = app.cursor.saturating_sub(1);
            }
            KeyEvent {
                code: KeyCode::Char('g') | KeyCode::Home,
                ..
            } => app.cursor = 0,
            KeyEvent {
                code: KeyCode::Char('G') | KeyCode::End,
                ..
            } => app.cursor = app.rows.len().saturating_sub(1),

            KeyEvent {
                code: KeyCode::Char('s'),
                ..
            } => {
                suspended(|| {
                    let _ = crate::cmd_sync(false);
                })?;
                app.refresh();
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                ..
            } => {
                suspended(|| {
                    let _ = crate::cmd_update(&[]);
                })?;
                app.refresh();
            }
            KeyEvent {
                code: KeyCode::Char('x'),
                ..
            } => {
                suspended(|| {
                    let _ = crate::cmd_sync(true);
                })?;
                app.refresh();
            }
            KeyEvent {
                code: KeyCode::Char('r'),
                ..
            } => app.refresh(),
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
            local("natori.lazy"),
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
