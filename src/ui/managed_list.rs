use std::io::{self, Write};

use crate::theme::{Color, Selection};

use super::list_columns::Columns;
use super::terminal_text::{clip, finish_selection, start_selection};
use super::{footer_start_row, rule_line, App, Role, Row, Status, LIST_TOP};

pub(super) fn list_top(height: u16) -> usize {
    if height < 7 {
        0
    } else {
        LIST_TOP
    }
}

fn status_label(status: &Status, compact: bool) -> &'static str {
    let (label, short) = match status {
        Status::Ok => ("Installed", "OK"),
        Status::Missing => ("Missing", "MIS"),
        Status::Drifted { .. } => ("Drifted", "DRF"),
        Status::Unverifiable => ("Unverified", "UNV"),
        Status::Disabled => ("Disabled", "OFF"),
        Status::Extra => ("Unlisted", "EXT"),
        Status::ExtraLocal => ("Local link", "LNK"),
        Status::SelfEntry => ("Manager", "MGR"),
    };
    if compact {
        short
    } else {
        label
    }
}

fn markers(row: &Row) -> [char; 3] {
    [
        if row.herdr_warning.is_some() {
            '!'
        } else {
            ' '
        },
        if row.enabled == Some(false) && row.status != Status::Disabled {
            '-'
        } else {
            ' '
        },
        if row.maybe_stale { '^' } else { ' ' },
    ]
}

fn compact_pointer(focused: bool, picked: bool) -> &'static str {
    match (focused, picked) {
        (true, true) => "+",
        (true, false) => ">",
        (false, true) => "*",
        (false, false) => " ",
    }
}

fn draw_rows(app: &App, out: &mut impl Write, columns: usize, height: u16) -> io::Result<()> {
    let picked = app.any_picked();
    let (visible, start) = app.list_window(height);
    let rows = app.rows.get(start..).unwrap_or_default();
    let rows = &rows[..visible.min(rows.len())];
    let top = list_top(height);
    let compact = columns < 23;
    let status_width = rows
        .iter()
        .map(|row| status_label(&row.status, compact).len())
        .max()
        .unwrap_or(0);
    let show_markers = rows.iter().any(|row| markers(row) != [' '; 3]);
    let selector_width = 2 + usize::from(picked) * 4;
    let state_width = status_width + usize::from(show_markers) * 3;
    let narrow = columns < 12 || columns < selector_width + state_width + 1;
    let prefix = if narrow { 1 } else { selector_width } + state_width;
    let details: Vec<_> = rows
        .iter()
        .map(|row| {
            if narrow {
                return (String::new(), String::new());
            }
            let commit = row.commit.as_deref().map(crate::short).unwrap_or_default();
            (commit, row.trailing_text())
        })
        .collect();
    let layout = Columns::new(
        columns.saturating_sub(prefix + 1),
        rows.iter()
            .zip(&details)
            .map(|(row, (commit, note))| (row.label.as_str(), commit.as_str(), note.as_str())),
    );
    for (offset, (row, (commit, note))) in rows.iter().zip(&details).enumerate() {
        let focused = start + offset == app.cursor;
        let selection = focused.then(|| app.theme.selection());
        write!(out, "\x1b[{};1H", top + offset + 1)?;
        if let Some(selection) = selection {
            start_selection(out, selection, columns)?;
        }
        let pointer = if narrow {
            compact_pointer(focused, row.picked)
        } else if focused {
            "> "
        } else {
            "  "
        };
        if columns < prefix {
            let (label, _) = clip(&row.label, columns - 1);
            write!(out, "{pointer}{label}")?;
        } else {
            let checkbox = match (picked && !narrow, row.picked) {
                (true, true) => "[x] ",
                (true, false) => "[ ] ",
                _ => "",
            };
            let status = status_label(&row.status, compact);
            let color = app.theme.color(row.status.role());
            let color = selection.map_or(color, |selection| selection.status_color(color));
            let foreground = selection.map_or(Color::Ansi(39), Selection::foreground);
            write!(
                out,
                "{pointer}{checkbox}{color}{status:<status_width$}{foreground}"
            )?;
            if show_markers {
                let [warning, disabled, update] = markers(row);
                write!(out, "{warning}{disabled}{update}")?;
            }
            if columns > prefix {
                let (name, cells) = clip(&row.label, layout.name);
                let emphasized = if focused { "\x1b[1m" } else { "" };
                write!(out, " {emphasized}{name}\x1b[22m")?;
                layout.write_tail(
                    out,
                    layout.name - cells,
                    if commit.is_empty() { "-" } else { commit },
                    note,
                    focused,
                )?;
            }
        }
        if focused {
            finish_selection(out)?;
        }
    }
    Ok(())
}

pub(super) fn draw(app: &App, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
    write!(out, "\x1b[H\x1b[2J")?;
    let columns = usize::from(width.saturating_sub(1));
    if columns == 0 || height == 0 {
        return out.flush();
    }

    let top = list_top(height);
    if top > 0 {
        let installed = app
            .rows
            .iter()
            .filter(|r| r.status != Status::Missing)
            .count();
        let to_sync = app
            .rows
            .iter()
            .filter(|r| matches!(r.status, Status::Missing | Status::Drifted { .. }))
            .count();
        let updates = app.rows.iter().filter(|r| r.maybe_stale).count();
        let counts = if columns >= 50 {
            format!("  {installed} installed · {to_sync} to sync · {updates} updates")
        } else if columns >= 32 {
            format!("  {installed} installed · {to_sync} to sync")
        } else {
            format!("  {installed} installed")
        };
        let (title, cells) = clip("Plugins", columns);
        let (counts, _) = clip(&counts, columns.saturating_sub(cells));
        write!(
            out,
            "\x1b[1m{title}\x1b[0m\x1b[2m{counts}\x1b[0m\x1b[2;1H\x1b[2m{}\x1b[0m",
            rule_line(width)
        )?;
    }

    if let Some(error) = &app.error {
        let (message, _) = clip(&format!("Cannot read plugin list · {error}"), columns);
        let danger = app.theme.color(Role::Danger);
        write!(out, "\x1b[{};1H{danger}{message}\x1b[0m", top + 1)?;
    } else if app.rows.is_empty() {
        let (message, _) = clip(
            "No plugins yet · [/] Browse or run herdr-lazy init",
            columns,
        );
        write!(out, "\x1b[{};1H{message}", top + 1)?;
    } else {
        draw_rows(app, out, columns, height)?;
    }

    if top > 0 {
        let divider = footer_start_row(height);
        let footer = divider + 1;
        write!(out, "\x1b[{divider};1H\x1b[2m{}\x1b[0m", rule_line(width))?;
        if let Some(flash) = &app.flash {
            let (message, _) = clip(flash, columns);
            let accent = app.theme.color(Role::Accent);
            write!(out, "\x1b[{footer};1H{accent}{message}\x1b[0m")?;
        } else {
            let mut legend = String::new();
            for hint in [
                "[/] Browse",
                "[?] Help",
                "[Ctrl+P] Commands",
                "[l] Detail",
                "[i] Install",
                "[u] Update",
                "[x] Remove",
            ] {
                let gap = if legend.is_empty() { "" } else { "  " };
                if legend.len() + gap.len() + hint.len() > columns {
                    break;
                }
                legend.push_str(gap);
                legend.push_str(hint);
            }
            if legend.is_empty() {
                let (fallback, _) = clip("[?] Help", columns);
                legend = fallback;
            }
            if let Some(row) = app.selected() {
                for (shown, text) in [
                    (row.herdr_warning.is_some(), "! Requires newer Herdr"),
                    (row.maybe_stale, "^ Update available"),
                    (
                        row.enabled == Some(false) && row.status != Status::Disabled,
                        "- Disabled",
                    ),
                ] {
                    if shown && legend.len() + 2 + text.len() <= columns {
                        legend.push_str("  ");
                        legend.push_str(text);
                    }
                }
            }
            write!(out, "\x1b[{footer};1H{legend}")?;
        }
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::super::terminal_text::cell_width;
    use super::*;

    #[test]
    fn adaptive_manage_shows_notes_alongside_short_and_long_names() {
        let app = App {
            rows: super::super::rows(
                &[
                    crate::Spec::parse("owner/a"),
                    crate::Spec::parse(
                        "owner/a-very-long-plugin-name-that-could-take-the-entire-terminal",
                    ),
                ],
                &[],
            ),
            cursor: 1,
            ..App::default()
        };
        for (width, height) in [(40, 9), (40, 11), (80, 24), (100, 30), (160, 48)] {
            let mut output = Vec::new();
            app.draw(&mut output, width, height).unwrap();
            let frame = String::from_utf8(output).unwrap();
            for (index, name) in ["owner/a", "owner/a-very"].into_iter().enumerate() {
                let position = format!("\x1b[{};1H", 3 + index);
                let line = frame
                    .split_once(&position)
                    .unwrap()
                    .1
                    .split(";1H")
                    .next()
                    .unwrap();
                assert!(
                    line.contains("Missing") && line.contains(name),
                    "{width}x{height}: {line:?}"
                );
                assert!(line.contains("in your"), "{width}x{height}: {line:?}");
                assert_eq!(app.row_at(2 + index as u16, height), Some(index));
            }
        }
    }

    #[test]
    fn picked_manage_row_keeps_status_and_markers_at_narrow_widths() {
        let mut rows = super::super::rows(&[crate::Spec::parse("owner/plugin")], &[]);
        let row = &mut rows[0];
        row.status = Status::Drifted {
            have: "abc123".into(),
        };
        row.picked = true;
        row.enabled = Some(false);
        row.maybe_stale = true;
        row.herdr_warning = Some(super::super::HerdrVersionWarning {
            required: crate::HerdrVersion::parse("0.8.0").unwrap(),
            running: crate::HerdrVersion::parse("0.7.4").unwrap(),
        });
        let app = App {
            rows,
            ..App::default()
        };

        for (width, expected) in [(14, "> [x] DRF!-^"), (13, "+DRF!-^ own"), (8, "+DRF!-^")] {
            let mut frame = Vec::new();
            app.draw(&mut frame, width, 2).unwrap();
            let frame = String::from_utf8(frame).unwrap();
            let (_, line) = frame.split_once("\x1b[1;1H").unwrap();
            let mut segments = line.rsplit("\x1b[1G").next().unwrap().split("\x1b[");
            let mut visible = segments.next().unwrap().to_string();
            for segment in segments {
                visible.push_str(segment.split_once('m').unwrap().1);
            }
            assert!(
                visible.starts_with(expected),
                "{width} columns: {visible:?}"
            );
            assert!(visible.chars().map(cell_width).sum::<usize>() < usize::from(width));
        }
    }

    #[test]
    fn managed_rows_stay_inside_their_columns() {
        let mut row = super::super::rows(&[crate::Spec::parse("owner/japanese")], &[])
            .pop()
            .unwrap();
        row.label = "日本語のプラグイン💡名前の長い名前".repeat(5);
        row.commit = Some("af06d0ad3fd99dc06027de187f1369328bf2d455".into());
        row.picked = true;
        let app = App {
            rows: vec![row],
            ..App::default()
        };
        for columns in [1, 10, 16, 24, 39, 63, 64, 95, 96, 132] {
            let mut output = Vec::new();
            draw_rows(&app, &mut output, columns, 2).unwrap();
            let output = String::from_utf8(output).unwrap();
            let line = output.strip_prefix("\x1b[1;1H").unwrap();
            let mut segments = line.rsplit("\x1b[1G").next().unwrap().split("\x1b[");
            let mut visible = segments.next().unwrap().to_string();
            for segment in segments {
                visible.push_str(segment.split_once('m').unwrap().1);
            }
            let width = visible.chars().map(cell_width).sum::<usize>();
            assert!(width <= columns, "{columns} columns: {visible}");
            if columns >= 24 {
                assert!(visible.contains("Missing"));
                assert!(visible.contains("> [x]"));
            }
        }
    }
}
