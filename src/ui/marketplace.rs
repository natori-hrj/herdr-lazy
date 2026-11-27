use std::io::{self, Write};

use crate::browse::Browser;
use crate::registry::{age_label, today_days, Entry};
use crate::theme::{Color, Role, Selection};

use super::list_columns::Columns;
use super::terminal_text::{clip, clip_tail, finish_selection, start_selection};
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

fn search_line(
    out: &mut impl Write,
    browser: &Browser,
    columns: usize,
    has_divider: bool,
) -> io::Result<Option<String>> {
    if columns < 2 {
        write!(out, "\x1b[7m \x1b[0m")?;
        return Ok(None);
    }
    let prefix = if columns >= 32 {
        "Search "
    } else if columns >= 8 {
        "/ "
    } else {
        "/"
    };
    let (category, installed, sort) = (
        browser.category_label(),
        browser.install_filter_label(),
        browser.sort_label(),
    );
    let controls = format!("  [Tab] {category}  [Ctrl+F] {installed}  [Ctrl+N] {sort}");
    let compact_install = if installed == "not installed" {
        "not inst."
    } else {
        installed
    };
    let available = columns - prefix.len() - 1;
    // The existing divider can carry the shortcuts without taking cells from the query.
    let divider_controls = (has_divider && controls.len() + 6 > available)
        .then(|| format!("Tab {category} ^F {compact_install} ^N {sort}"))
        .filter(|label| label.len() <= columns);
    let compact = format!("  {category} / {installed} / {sort}");
    let category_only = format!("  {category}");
    let controls = if divider_controls.is_some() {
        ""
    } else {
        [controls.as_str(), &compact, &category_only]
            .into_iter()
            .find(|label| label.len() + 6 <= available)
            .unwrap_or("")
    };
    let query = clip_tail(&browser.query, available - controls.len());
    write!(
        out,
        "\x1b[1m{prefix}\x1b[0m\x1b[1m{query}\x1b[0m\x1b[7m \x1b[0m{controls}"
    )?;
    Ok(divider_controls)
}

fn status(browser: &Browser, entry: &Entry, columns: usize) -> &'static str {
    if columns < 16 {
        return "";
    }
    match (browser.is_installed(entry), browser.is_listed(entry)) {
        (true, true) if columns >= 40 => "Installed+Listed",
        (true, true) => "Inst.+List",
        (true, false) => "Installed",
        (false, true) => "Listed",
        (false, false) => "",
    }
}

fn draw_results(
    app: &App,
    out: &mut impl Write,
    browser: &Browser,
    results: &[&Entry],
    window: &Window,
    columns: usize,
) -> io::Result<usize> {
    let today = today_days();
    let rows: Vec<_> = results
        .iter()
        .skip(window.start)
        .take(window.visible)
        .map(|&entry| {
            let status = status(browser, entry, columns);
            let metrics = format!("{} · {}★", age_label(&entry.pushed_at, today), entry.stars);
            (entry, status, metrics)
        })
        .collect();
    let status_width = rows
        .iter()
        .map(|(_, status, _)| status.len())
        .max()
        .unwrap_or(0);
    let status_cost = if status_width > 0 {
        status_width + 2
    } else {
        0
    };
    let layout = Columns::new(
        columns.saturating_sub(2 + status_cost),
        rows.iter().map(|(entry, _, metrics)| {
            (
                entry.full_name.as_str(),
                metrics.as_str(),
                entry.description.as_str(),
            )
        }),
    );
    for (offset, (entry, status, metrics)) in rows.iter().enumerate() {
        let focused = window.start + offset == browser.cursor;
        let selection = focused.then(|| app.theme.selection());
        write!(out, "\x1b[{};1H", window.top + offset + 1)?;
        if let Some(selection) = selection {
            start_selection(out, selection, columns)?;
        }
        let point = if focused { ">" } else { " " };
        write!(out, "{point}")?;
        if columns > 1 {
            let (name, cells) = clip(&entry.full_name, layout.name);
            let emphasize = if focused { "\x1b[1m" } else { "" };
            write!(out, " {emphasize}{name}\x1b[22m")?;
            let mut padding = layout.name - cells;
            if status_width > 0 {
                let gap = padding + 2;
                let color = if status.is_empty() {
                    Color::Plain
                } else {
                    let color = app.theme.color(Role::Success);
                    selection.map_or(color, |selection| selection.status_color(color))
                };
                let foreground = selection.map_or(Color::Ansi(39), Selection::foreground);
                write!(out, "{:gap$}{color}{status:<status_width$}{foreground}", "")?;
                padding = 0;
            }
            layout.write_tail(out, padding, metrics, &entry.description)?;
        }
        if focused {
            finish_selection(out)?;
        }
    }
    Ok(layout.trailing)
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
    let inventory_unavailable = !browser.all.is_empty() && browser.inventory_unavailable();
    if window.top == 3 {
        let title = if inventory_unavailable || ((refreshing || failed) && columns < 34) {
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
        let inventory_note = (!refreshing && inventory_unavailable)
            .then(|| format!("Install state unavailable · {note}"));
        let note = inventory_note.as_deref().unwrap_or(note);
        let (note, _) = clip(note, columns.saturating_sub(title_cells + 2));
        let color = if failed || inventory_note.is_some() {
            app.theme.color(Role::Danger)
        } else {
            Color::Plain
        };
        write!(out, "\x1b[1m{title}\x1b[0m")?;
        if !note.is_empty() {
            write!(out, "  {color}{note}\x1b[0m")?;
        }
        write!(out, "\x1b[2;1H")?;
    }
    let divider_controls = if window.top > 0 {
        search_line(out, browser, columns, window.top == 3)?
    } else {
        None
    };
    if window.top == 3 {
        if let Some(controls) = divider_controls {
            write!(out, "\x1b[3;1H{controls}")?;
        } else {
            write!(out, "\x1b[3;1H\x1b[2m{}\x1b[0m", rule_line(width))?;
        }
    }

    let description_width = if results.is_empty() {
        let message = if loading {
            "Loading marketplace index…"
        } else if failed && browser.all.is_empty() {
            "Marketplace unavailable · Ctrl+R retries"
        } else if browser.all.is_empty() {
            "No plugins in the index"
        } else if browser.install_filter_unavailable() {
            "Install state unavailable · Ctrl+R"
        } else {
            "No matches · Ctrl+L resets search"
        };
        let (message, _) = clip(message, columns);
        write!(out, "\x1b[{};1H{message}", window.top + 1)?;
        0
    } else {
        draw_results(app, out, browser, &results, &window, columns)?
    };

    if height >= 6 {
        let divider = footer_start_row(height);
        let footer = divider + 1;
        // The divider can show more of the selected description without taking a result row.
        let preview = results.get(browser.cursor).and_then(|entry| {
            let (text, cells) = clip(&entry.description, columns);
            (cells > description_width && cells > 1).then_some(text)
        });
        if let Some(description) = preview {
            write!(out, "\x1b[{divider};1H{description}")?;
        } else {
            write!(out, "\x1b[{divider};1H\x1b[2m{}\x1b[0m", rule_line(width))?;
        }
        let primary = "[Enter] Add";
        let mut hint_line = String::new();
        for hint in [
            primary,
            "[Esc] Back",
            "[Ctrl+L] Reset",
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
            write!(out, "{hint_line}\x1b[0m")?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::super::terminal_text::display_width;
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
                Some(&[]),
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
    fn marketplace_search_keeps_whole_graphemes_before_the_caret() {
        let mut app = app();
        for (query, width, expected) in [
            ("xy👩‍💻", 6, "…👩‍💻"),
            ("xy👩‍💻", 5, "…"),
            ("xy👩‍💻", 7, "xy👩‍💻"),
            ("xy⌚\u{fe0e}", 5, "…⌚\u{fe0e}"),
            ("xy☀\u{fe0f}", 6, "…☀\u{fe0f}"),
            ("xy\u{301}", 4, "…"),
            ("xy\u{301}", 5, "xy\u{301}"),
            ("xy界", 6, "…界"),
            ("xy🇯🇵🇺", 7, "…🇯🇵🇺"),
            ("xy🇯🇵🇺", 6, "…🇺"),
            ("x\r\ny", 6, "… y"),
            ("x", 3, ""),
        ] {
            app.browser.as_mut().unwrap().query = query.into();
            let rendered = frame(&app, width, 8, false);
            let search = format!("/\x1b[0m\x1b[1m{expected}\x1b[0m\x1b[7m \x1b[0m");
            assert!(
                rendered.contains(&search),
                "{query:?}, {width}: {rendered:?}"
            );
        }
    }

    #[test]
    fn marketplace_visual_hierarchy_emphasizes_search_and_available_add() {
        let mut app = app();
        let mut output = Vec::new();
        app.draw(&mut output, 80, 24).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b[1mSearch \x1b[0m"));
        assert!(output.contains("\x1b[1m[Enter] Add\x1b[0m"));
        assert!(output.contains("\x1b[0m  cached 2h ago\x1b[0m"));

        let browser = app.browser.as_mut().unwrap();
        browser.source_note = "cached 2h ago (refresh failed: offline)".into();
        browser.query = "unmatched".into();
        let empty = frame(&app, 80, 24, false);
        let danger = app.theme.color(Role::Danger);
        assert!(empty.contains("\x1b[1mSearch \x1b[0m\x1b[1munmatched\x1b[0m\x1b[7m "));
        assert!(empty.contains(&format!("  {danger}refresh failed: offline")));
        assert!(empty.contains("\x1b[22;1H[Enter] Add"));
    }

    #[test]
    fn marketplace_supporting_information_uses_normal_intensity() {
        let mut app = app();
        let browser = app.browser.as_mut().unwrap();
        browser.all[0].description = "Workspace shortcuts".into();
        browser.all.push(Entry {
            full_name: "owner/other".into(),
            description: "Navigate between workspaces".into(),
            ..Entry::default()
        });
        browser.cursor = 1;
        for preset in crate::theme::PRESETS {
            app.theme = preset.palette;
            let wide = frame(&app, 120, 24, false);
            for (position, next, labels) in [
                ("4;1H", "5;1H", ["180★", "Workspace shortcuts"]),
                ("5;1H", "21;1H", ["0★", "Navigate between workspaces"]),
                ("2;1H", "3;1H", ["[Tab]", "[Ctrl+N]"]),
            ] {
                let (_, line) = wide.split_once(&format!("\x1b[{position}")).unwrap();
                let (line, _) = line.split_once(&format!("\x1b[{next}")).unwrap();
                assert!(labels.iter().all(|label| line.contains(label)), "{line:?}");
                assert!(!line.contains("\x1b[2m"), "{line:?}");
            }
            assert!(wide.contains("\x1b[3;1H\x1b[2m─"));
            assert!(wide
                .contains("\x1b[0m  [Esc] Back  [Ctrl+L] Reset  [Ctrl+O] Repo  [Ctrl+R] Refresh"));

            let narrow = frame(&app, 40, 24, false);
            assert!(narrow.contains("\x1b[3;1HTab "));
            assert!(narrow.contains("\x1b[21;1HNavigate between workspaces\x1b[22;1H"));
        }
    }

    #[test]
    fn search_view_retains_a_forward_indic_grapheme() {
        let mut app = app();
        app.browser.as_mut().unwrap().query = "xxकक्क".into();
        let rendered = frame(&app, 6, 8, false);
        assert!(
            rendered.contains("/\x1b[0m\x1b[1m…क्क\x1b[0m\x1b[7m "),
            "{rendered:?}"
        );
    }

    #[test]
    fn search_view_fits_a_ligature_in_one_cell() {
        let mut app = app();
        for (query, width, expected) in [("لا", 4, "لا"), ("xxلا", 5, "…لا")] {
            app.browser.as_mut().unwrap().query = query.into();
            let rendered = frame(&app, width, 8, false);
            let search = format!("/\x1b[0m\x1b[1m{expected}\x1b[0m\x1b[7m ");
            assert!(rendered.contains(&search), "{rendered:?}");
        }
    }

    #[test]
    fn marketplace_badges_follow_the_browser_inventory() {
        let installed = crate::parse_plugin_list(
            r#"{"result":{"plugins":[
                {"name":"worktree","source":{"url":"https://github.com/OWNER/worktree.git"}}
            ]}}"#,
        )
        .unwrap();
        let mut app = app();
        let browser = app.browser.as_mut().unwrap();
        *browser = Browser::new(
            browser.all.clone(),
            browser.source_note.clone(),
            browser.listed.clone(),
            Some(&installed),
        );

        let available = frame(&app, 80, 14, false);
        assert!(available.contains("Installed+Listed"), "{available:?}");

        // The Manage view still reflects its older snapshot.
        app.rows = super::super::rows(&[crate::Spec::parse("owner/worktree")], &installed);
        assert_eq!(app.rows[0].slug.as_deref(), Some("owner/worktree"));
        assert!(app.rows[0].installed.is_some());
        let browser = app.browser.as_mut().unwrap();
        browser.replace_entries(browser.all.clone(), browser.source_note.clone(), None);

        let unavailable = frame(&app, 80, 14, false);
        assert!(unavailable.contains("Install state unavailable"));
        assert!(unavailable.contains("Listed"));
        assert!(!unavailable.contains("Installed"), "{unavailable:?}");
    }

    #[test]
    fn adaptive_marketplace_shows_descriptions_and_keeps_click_targets() {
        let mut app = app();
        let browser = app.browser.as_mut().unwrap();
        browser.all[0].description = "Workspace shortcuts".into();
        browser.all.push(Entry {
            full_name: "owner/a-very-long-plugin-name-that-could-take-the-entire-terminal".into(),
            description: "Navigate between workspaces".into(),
            ..Entry::default()
        });
        browser.cursor = 1;

        for (width, height) in [(40, 9), (40, 11), (80, 24), (100, 30), (160, 48)] {
            let output = frame(&app, width, height, false);
            let (_, first) = output.split_once("\x1b[4;1H").unwrap();
            let (first, rest) = first.split_once("\x1b[5;1H").unwrap();
            let (second, _) = rest.split_once(";1H").unwrap();
            assert!(first.contains("Listed") && first.contains("owner/worktree"));
            assert!(first.contains("Workspace"), "{width}x{height}: {first:?}");
            assert!(second.contains("owner/a-very") && second.contains("Navigate"));
            assert!(
                output.contains("Navigate between workspaces"),
                "{width}x{height}: {output:?}"
            );

            let window = Window::new(1, height);
            assert_eq!(window.index_at(3), Some(0));
            assert_eq!(window.index_at(4), Some(1));
            assert_eq!(window.index_at(footer_start_row(height) - 1), None);
        }
    }

    #[test]
    fn offscreen_marketplace_names_do_not_take_description_space() {
        let mut app = app();
        let browser = app.browser.as_mut().unwrap();
        browser.all[0].description = "Workspace shortcuts".into();
        browser.all.extend([
            Entry {
                full_name: "owner/brief".into(),
                ..Entry::default()
            },
            Entry {
                full_name: format!("owner/{}", "long-name".repeat(20)),
                ..Entry::default()
            },
        ]);
        let output = frame(&app, 64, 9, false);
        let (_, rest) = output.split_once("\x1b[4;1H").unwrap();
        let (first, _) = rest.split_once("\x1b[5;1H").unwrap();
        assert!(first.contains("Listed"));
        assert!(first.contains("180★"));
        assert!(first.contains("Workspace shortcuts"), "{first:?}");
        assert!(!output.contains("long-name"));
    }

    #[test]
    fn marketplace_rows_and_search_fit_terminal_cells() {
        let mut app = app();
        app.browser.as_mut().unwrap().category = Some("notifications");
        app.browser.as_mut().unwrap().query = "現行の検索プラグイン💡xyz".to_string();
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
                        if matches!(final_char, 'H' | 'G') {
                            cells = 0;
                            if let Some((row, _)) = sequence[..index].split_once(';') {
                                assert!(row.parse::<u16>().unwrap() <= height);
                            }
                        }
                        let text = &sequence[index + 1..];
                        assert!(!text.chars().any(char::is_control));
                        cells += display_width(text);
                        assert!(cells < usize::from(width), "{width}x{height}: {frame:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn marketplace_compact_controls_remain_visible_at_40_columns() {
        let mut app = app();
        let browser = app.browser.as_mut().unwrap();
        browser.category = Some("notifications");
        browser.cycle_install_filter();
        browser.cycle_install_filter();
        browser.query = "長い worktree query".to_string();

        let compact = frame(&app, 40, 12, false);
        assert!(compact.contains("Tab notifications"), "{compact:?}");
        assert!(compact.contains("^F not inst."), "{compact:?}");
        assert!(compact.contains("^N stars"), "{compact:?}");
        assert!(compact.contains("長い worktree query"), "{compact:?}");
        assert!(compact.contains("\x1b[4;1HNo matches"), "{compact:?}");
        assert!(compact.contains("\x1b[10;1H[Enter] Add"), "{compact:?}");
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
