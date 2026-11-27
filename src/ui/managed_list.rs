use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

use super::{footer_start_row, rule_line, App, Role, Row, Status, Theme, LIST_TOP};

pub(super) fn list_top(height: u16) -> usize {
    if height < 7 {
        0
    } else {
        LIST_TOP
    }
}

pub(super) fn cell_width(ch: char) -> usize {
    // Count emoji presentation selectors conservatively for terminals that expand them.
    if ch == '\u{fe0f}' {
        1
    } else {
        ch.width().unwrap_or(0)
    }
}

pub(super) fn clip(text: &str, columns: usize) -> (String, usize) {
    let mut line = String::new();
    let mut width = 0;
    if columns == 0 {
        return (line, width);
    }
    for ch in text.chars() {
        // Keep text fields on their row and prevent them from emitting terminal commands.
        let ch = if ch.is_control() { ' ' } else { ch };
        let cells = cell_width(ch);
        if width + cells > columns {
            while width == columns {
                let removed = line.pop().expect("a full line contains a character");
                width -= cell_width(removed);
            }
            line.push('…');
            return (line, width + 1);
        }
        line.push(ch);
        width += cells;
    }
    (line, width)
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

fn row_line(
    out: &mut impl Write,
    row: &Row,
    focused: bool,
    picked: bool,
    columns: usize,
    theme: Theme,
) -> io::Result<()> {
    let pointer = if focused { "> " } else { "  " };
    if columns < 12 {
        let prefix = match (focused, row.picked) {
            (true, true) => "+",
            (true, false) => ">",
            (false, true) => "*",
            (false, false) => " ",
        };
        let (label, _) = clip(&row.label, columns.saturating_sub(1));
        return write!(out, "{prefix}{label}");
    }
    let compact = columns < 23;
    let checkbox = match (picked, row.picked) {
        (true, true) => "[x] ",
        (true, false) => "[ ] ",
        _ => "",
    };
    let status = status_label(&row.status, compact);
    let status_width = if compact { 3 } else { 10 };
    let warning = if row.herdr_warning.is_some() {
        '!'
    } else {
        ' '
    };
    let disabled = if row.enabled == Some(false) && row.status != Status::Disabled {
        '-'
    } else {
        ' '
    };
    let update = if row.maybe_stale { '^' } else { ' ' };
    let commit_width = if columns >= 64 { 16 } else { 0 };
    let available =
        columns.saturating_sub(pointer.len() + checkbox.len() + status_width + 4 + commit_width);
    let name_width = if columns >= 96 {
        available.min(40)
    } else {
        available
    };
    let (name, name_cells) = clip(&row.label, name_width);
    let emphasized = if focused { "\x1b[1m" } else { "" };
    write!(
        out,
        "{pointer}{checkbox}{}{status:<status_width$}\x1b[0m{warning}{disabled}{update} {emphasized}{name}\x1b[0m",
        theme.color(row.status.role())
    )?;

    if commit_width > 0 {
        let commit = row
            .commit
            .as_deref()
            .map(crate::short)
            .unwrap_or_else(|| "-".to_string());
        let (commit, cells) = clip(&commit, commit_width - 4);
        let pad = name_width - name_cells + 2;
        write!(out, "{:pad$}\x1b[2m{commit}\x1b[0m", "")?;
        let notes_width = available.saturating_sub(name_width);
        if notes_width > 0 {
            let (note, _) = clip(&row.trailing_text(), notes_width);
            let pad = commit_width - 2 - cells;
            write!(out, "{:pad$}\x1b[2m{note}\x1b[0m", "")?;
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
        let picked = app.any_picked();
        let (visible, start) = app.list_window(height);
        for (offset, row) in app.rows.iter().skip(start).take(visible).enumerate() {
            write!(out, "\x1b[{};1H", top + offset + 1)?;
            row_line(
                out,
                row,
                start + offset == app.cursor,
                picked,
                columns,
                app.theme,
            )?;
        }
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
    use super::*;

    #[test]
    fn clipping_uses_display_cells_and_flattens_control_characters() {
        assert_eq!(clip("日本語", 4), ("日…".into(), 3));
        assert_eq!(clip("a💡b", 3), ("a…".into(), 2));
        assert_eq!(clip("a\nb\tc\x1bd", 20), ("a b c d".into(), 7));
        assert_eq!(clip("name", 0), (String::new(), 0));
    }

    #[test]
    fn managed_rows_stay_inside_their_columns() {
        let mut row = super::super::rows(&[crate::Spec::parse("owner/japanese")], &[])
            .pop()
            .unwrap();
        row.label = "日本語のプラグイン💡名前の長い名前".repeat(5);
        row.commit = Some("af06d0ad3fd99dc06027de187f1369328bf2d455".into());
        row.picked = true;
        for columns in [1, 10, 16, 24, 39, 63, 64, 95, 96, 132] {
            let mut output = Vec::new();
            row_line(&mut output, &row, true, true, columns, Theme::default()).unwrap();
            let output = String::from_utf8(output).unwrap();
            let mut segments = output.split("\x1b[");
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
            assert_eq!(visible.contains("af06d0ad3fd9"), columns >= 64);
        }
    }
}
