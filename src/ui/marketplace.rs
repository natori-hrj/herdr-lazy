use std::io::{self, Write};

use crate::browse::Browser;
use crate::registry::{age_label, today_days, Entry};
use crate::theme::{Color, Role, Theme};

use super::terminal_text::{cell_width, clip};
use super::{footer_start_row, rule_line, App};

pub(super) struct Window {
    top: usize,
    visible: usize,
    start: usize,
}

impl Window {
    pub(super) fn new(cursor: usize, height: u16) -> Self {
        let (top, end) = match height {
            0..=3 => (0, usize::from(height > 0)),
            4..=5 => (1, usize::from(height - 2)),
            6..=7 => (1, usize::from(footer_start_row(height)) - 1),
            _ => (3, usize::from(footer_start_row(height)) - 1),
        };
        let visible = end - top;
        Self {
            top,
            visible,
            start: cursor.saturating_sub(visible.saturating_sub(1)),
        }
    }

    pub(super) fn index_at(&self, screen_row: u16) -> Option<usize> {
        let offset = usize::from(screen_row).checked_sub(self.top)?;
        (offset < self.visible).then(|| self.start + offset)
    }
}

fn query_tail(query: &str, columns: usize) -> String {
    let mut width = 0;
    for (index, ch) in query.char_indices().rev() {
        let cells = if ch.is_control() { 1 } else { cell_width(ch) };
        if width + cells > columns {
            if columns <= 1 {
                return "…".chars().take(columns).collect();
            }
            let rest = &query[index + ch.len_utf8()..];
            // Make room to indicate that the beginning of the query is hidden.
            let trim = usize::from(width == columns);
            let mut suffix = rest.chars();
            if trim > 0 {
                while suffix.next().is_some_and(|first| cell_width(first) == 0) {}
            }
            let (visible, _) = clip(suffix.as_str(), columns - 1);
            return format!("…{visible}");
        }
        width += cells;
    }
    clip(query, columns).0
}

fn search_line(out: &mut impl Write, browser: &Browser, columns: usize) -> io::Result<()> {
    if columns < 2 {
        return write!(out, "\x1b[7m \x1b[0m");
    }
    let prefix = if columns >= 32 {
        "Search "
    } else if columns >= 8 {
        "/ "
    } else {
        "/"
    };
    let category = format!("  [Tab] {}", browser.category_label());
    let compact_category = format!("  {}", browser.category_label());
    let available = columns - prefix.len() - 1;
    let category = if category.len() + 6 <= available {
        category.as_str()
    } else if compact_category.len() + 4 <= available {
        compact_category.as_str()
    } else {
        ""
    };
    let query = query_tail(&browser.query, available - category.len());
    write!(
        out,
        "\x1b[1m{prefix}\x1b[0m\x1b[1m{query}\x1b[0m\x1b[7m \x1b[0m\x1b[2m{category}\x1b[0m"
    )
}

struct Presence {
    installed: bool,
    listed: bool,
}

fn result_line(
    out: &mut impl Write,
    entry: &Entry,
    focused: bool,
    presence: Presence,
    columns: usize,
    theme: Theme,
    today: i64,
) -> io::Result<()> {
    let point = if focused { ">" } else { " " };
    let emphasize = if focused { "\x1b[1m" } else { "" };
    if columns == 1 {
        return write!(out, "{emphasize}{point}\x1b[0m");
    }
    let status_width = if columns >= 32 {
        16
    } else if columns >= 16 {
        10
    } else {
        0
    };
    let status = match (presence.installed, presence.listed) {
        (true, true) if status_width >= 16 => "Installed+Listed",
        (true, true) => "Inst.+List",
        (true, false) => "Installed",
        (false, true) => "Listed",
        (false, false) => "",
    };
    let metrics_width = if columns >= 64 { 16 } else { 0 };
    let available = columns - 2 - metrics_width - status_width - usize::from(status_width > 0) * 2;
    let name_width = if columns >= 96 {
        available.min(36)
    } else {
        available
    };
    let (name, cells) = clip(&entry.full_name, name_width);
    write!(out, "{emphasize}{point} {name}\x1b[0m")?;
    if status_width > 0 {
        let pad = name_width - cells + 2;
        let color = if status.is_empty() {
            Color::Plain
        } else {
            theme.color(Role::Success)
        };
        write!(out, "{:pad$}{color}{status:<status_width$}\x1b[0m", "")?;
    }
    if metrics_width > 0 {
        let metrics = format!("{} · {}★", age_label(&entry.pushed_at, today), entry.stars);
        let (metrics, cells) = clip(&metrics, metrics_width - 4);
        write!(out, "  \x1b[2m{metrics}\x1b[0m")?;
        let description_width = available - name_width;
        if description_width > 0 {
            let pad = metrics_width - 2 - cells;
            let (description, _) = clip(&entry.description, description_width);
            write!(out, "{:pad$}\x1b[2m{description}\x1b[0m", "")?;
        }
    }
    Ok(())
}

pub(super) fn draw(
    app: &App,
    out: &mut impl Write,
    width: u16,
    height: u16,
    refreshing: bool,
) -> io::Result<()> {
    write!(out, "\x1b[H\x1b[2J")?;
    let columns = usize::from(width.saturating_sub(1));
    let browser = app.browser.as_ref().expect("checked by caller");
    let window = Window::new(browser.cursor, height);
    if columns == 0 || window.visible == 0 {
        return out.flush();
    }
    let results = browser.results();
    let loading = refreshing && browser.all.is_empty();
    let failed = !refreshing && browser.source_note.contains("refresh failed:");
    if window.top == 3 {
        let title = if (refreshing || failed) && columns < 34 {
            "Marketplace".to_string()
        } else {
            format!("Marketplace  {}/{}", results.len(), browser.all.len())
        };
        let (title, title_cells) = clip(&title, columns);
        let cached_failure_note =
            browser
                .source_note
                .split_once(" (refresh failed: ")
                .map(|(cache, error)| {
                    format!("refresh failed: {} · {cache}", error.trim_end_matches(')'))
                });
        let note = if refreshing {
            if loading {
                "Loading index…"
            } else {
                "Refreshing…"
            }
        } else {
            cached_failure_note
                .as_deref()
                .unwrap_or(&browser.source_note)
        };
        let (note, _) = clip(note, columns.saturating_sub(title_cells + 2));
        let color = if failed {
            app.theme.color(Role::Danger)
        } else {
            Color::Plain
        };
        write!(out, "\x1b[1m{title}\x1b[0m")?;
        if !note.is_empty() {
            let dim = if failed || refreshing { "" } else { "\x1b[2m" };
            write!(out, "  {dim}{color}{note}\x1b[0m")?;
        }
        write!(out, "\x1b[2;1H")?;
    }
    if window.top > 0 {
        search_line(out, browser, columns)?;
    }
    if window.top == 3 {
        write!(out, "\x1b[3;1H\x1b[2m{}\x1b[0m", rule_line(width))?;
    }

    if results.is_empty() {
        let message = if loading {
            "Loading marketplace index…"
        } else if failed && browser.all.is_empty() {
            "Marketplace unavailable · Ctrl+R retries"
        } else if browser.all.is_empty() {
            "No plugins in the index"
        } else {
            "No matches · edit search or change category"
        };
        let (message, _) = clip(message, columns);
        write!(out, "\x1b[{};1H{message}", window.top + 1)?;
    } else {
        let today = today_days();
        for (offset, entry) in results
            .iter()
            .skip(window.start)
            .take(window.visible)
            .enumerate()
        {
            let focused = window.start + offset == browser.cursor;
            let installed = app.rows.iter().any(|row| {
                row.installed.is_some()
                    && row
                        .slug
                        .as_deref()
                        .is_some_and(|slug| slug.eq_ignore_ascii_case(&entry.full_name))
            });
            write!(out, "\x1b[{};1H", window.top + offset + 1)?;
            result_line(
                out,
                entry,
                focused,
                Presence {
                    installed,
                    listed: browser.is_listed(entry),
                },
                columns,
                app.theme,
                today,
            )?;
        }
    }

    if height >= 6 {
        let divider = footer_start_row(height);
        let footer = divider + 1;
        write!(out, "\x1b[{divider};1H\x1b[2m{}\x1b[0m", rule_line(width))?;
        let primary = "[Enter] Add";
        let mut hint_line = String::new();
        for hint in [
            primary,
            "[Esc] Back",
            "[Tab] Category",
            "[Ctrl+O] Repo",
            "[Ctrl+R] Refresh",
        ] {
            let gap = if hint_line.is_empty() { "" } else { "  " };
            if hint_line.len() + gap.len() + hint.len() > columns {
                break;
            }
            hint_line.push_str(gap);
            hint_line.push_str(hint);
        }
        if hint_line.is_empty() {
            hint_line = clip("[Esc] Back", columns).0;
        }
        write!(out, "\x1b[{footer};1H")?;
        if let Some(flash) = &app.flash {
            let (label, _) = clip(flash, columns);
            let accent = app.theme.color(Role::Accent);
            write!(out, "{accent}{label}\x1b[0m")?;
        } else if let Some(rest) = hint_line
            .strip_prefix(primary)
            .filter(|_| !results.is_empty())
        {
            write!(out, "\x1b[1m{primary}\x1b[0m{rest}")?;
        } else {
            write!(out, "{hint_line}")?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App {
            browser: Some(Browser::new(
                vec![Entry {
                    full_name: "owner/worktree".to_string(),
                    description: "複数のワークツリーを管理する💡[2J".to_string(),
                    stars: 180,
                    pushed_at: "2026-11-20T00:00:00Z".to_string(),
                    ..Entry::default()
                }],
                "cached 2h ago".to_string(),
                vec!["owner/worktree".to_string()],
            )),
            ..App::default()
        }
    }

    fn frame(app: &App, width: u16, height: u16, refreshing: bool) -> String {
        let mut output = Vec::new();
        draw(app, &mut output, width, height, refreshing).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn marketplace_visual_hierarchy_emphasizes_search_focus_and_add() {
        let app = app();
        let mut output = Vec::new();
        app.draw(&mut output, 80, 24).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b[1mSearch \x1b[0m"));
        assert!(output.contains("\x1b[1m> owner/worktree\x1b[0m"));
        assert!(output.contains("\x1b[1m[Enter] Add\x1b[0m"));
        assert!(output.contains("\x1b[2mcached 2h ago\x1b[0m"));
    }

    #[test]
    fn marketplace_rows_and_search_fit_terminal_cells() {
        let mut app = app();
        app.browser.as_mut().unwrap().category = Some("notifications");
        app.browser.as_mut().unwrap().query = "現行の検索プラグイン💡xyz".to_string();
        assert_eq!(query_tail("現行の検索プラグイン💡xyz", 5), "…xyz");
        for width in [1, 2, 10, 24, 40, 64, 80, 120] {
            for height in [2, 4, 5, 6, 8, 14] {
                let frames = [
                    frame(&app, width, height, true),
                    frame(&self::app(), width, height, false),
                ];
                for frame in frames {
                    let mut cells = 0;
                    for sequence in frame.split("\x1b[").skip(1) {
                        let (index, final_char) = sequence
                            .char_indices()
                            .find(|(_, ch)| ('@'..='~').contains(ch))
                            .unwrap();
                        if final_char == 'H' {
                            cells = 0;
                            if let Some((row, _)) = sequence[..index].split_once(';') {
                                assert!(row.parse::<u16>().unwrap() <= height);
                            }
                        }
                        let text = &sequence[index + 1..];
                        assert!(!text.chars().any(char::is_control));
                        cells += text.chars().map(cell_width).sum::<usize>();
                        assert!(cells < usize::from(width), "{width}x{height}: {frame:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn hit_testing_follows_the_results_as_the_viewport_changes() {
        let tall = Window::new(6, 14);
        assert_eq!(tall.index_at(2), None);
        assert_eq!(tall.index_at(3), Some(0));
        assert_eq!(tall.index_at(9), Some(6));
        assert_eq!(tall.index_at(10), None);

        let compact = Window::new(6, 6);
        assert_eq!(compact.index_at(0), None);
        assert_eq!(compact.index_at(1), Some(6));
        assert_eq!(compact.index_at(2), None);

        let tiny = Window::new(6, 2);
        assert_eq!(tiny.index_at(0), Some(6));
        assert_eq!(tiny.index_at(1), None);
    }

    #[test]
    fn refresh_status_and_empty_search_are_distinct() {
        let mut app = app();
        assert!(frame(&app, 80, 14, true).contains("Refreshing…"));
        assert!(frame(&app, 80, 14, true).contains("owner/worktree"));

        app.browser.as_mut().unwrap().source_note =
            "cached 5h ago (refresh failed: offline)".to_string();
        let cached_failure = frame(&app, 40, 11, false);
        assert!(cached_failure.contains("refresh failed"));
        assert!(cached_failure.contains("owner/worktree"));

        app.browser.as_mut().unwrap().query = "unmatched".to_string();
        assert!(frame(&app, 80, 14, false).contains("No matches"));

        let browser = app.browser.as_mut().unwrap();
        browser.all.clear();
        browser.source_note = "refresh failed: timeout".to_string();
        assert!(frame(&app, 80, 14, true).contains("Loading marketplace index"));
        assert!(frame(&app, 80, 14, false).contains("Marketplace unavailable"));
    }
}
