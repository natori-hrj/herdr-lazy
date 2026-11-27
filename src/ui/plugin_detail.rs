use std::io::{self, Write};

use super::{
    footer_start_row, rule_line,
    terminal_text::{cell_width, clip},
    App, Color, Role,
};
use crate::github::Changes;

struct Line {
    text: String,
    color: Color,
}

#[derive(Default)]
struct Body {
    lines: Vec<Line>,
    selected_row: Option<usize>,
    selected_label: Option<String>,
}

impl Body {
    fn push(&mut self, text: impl Into<String>) {
        self.push_color(text, Color::Plain);
    }

    fn push_color(&mut self, text: impl Into<String>, color: Color) {
        self.lines.push(Line {
            text: text.into(),
            color,
        });
    }

    fn push_wrapped(&mut self, text: &str, width: usize) {
        self.push_wrapped_color(text, width, Color::Plain);
    }

    fn push_wrapped_color(&mut self, text: &str, width: usize, color: Color) {
        let mut line = String::new();
        let mut columns = 0;
        for ch in text.chars() {
            let ch = if ch.is_control() { ' ' } else { ch };
            let cells = cell_width(ch);
            if columns + cells > width && !line.is_empty() {
                self.push_color(std::mem::take(&mut line), color);
                columns = 0;
            }
            if cells > width {
                line.push('…');
                columns += 1;
            } else {
                line.push(ch);
                columns += cells;
            }
        }
        if !line.is_empty() {
            self.push_color(line, color);
        }
    }

    fn offset(&self, requested: usize, visible: usize) -> usize {
        requested.min(self.lines.len().saturating_sub(visible))
    }
}

fn body(app: &App, width: u16) -> Body {
    let mut body = Body::default();
    let Some(idx) = app.detail_of else {
        return body;
    };
    let Some(row) = app.rows.get(idx) else {
        return body;
    };

    let text_width = usize::from(width).saturating_sub(2).max(1);
    let (status, role) = match (row.installed.is_some(), row.enabled) {
        (false, _) => ("Not installed", Role::Warning),
        (true, Some(true)) => ("Installed · enabled", Role::Success),
        (true, Some(false)) => ("Installed · disabled", Role::Accent),
        (true, None) => ("Installed", Role::Success),
    };
    body.push_color(status, app.theme.color(role));
    if let Some(slug) = row.slug.as_deref() {
        body.push_wrapped(&format!("Repository · {slug}"), text_width);
    }
    if let Some(commit) = row.commit.as_deref() {
        body.push_wrapped(&format!("Resolved commit · {commit}"), text_width);
    }
    if let Some(reference) = row
        .listed_as
        .as_deref()
        .and_then(|line| crate::Spec::parse(line).reference)
    {
        body.push_wrapped(&format!("Listed ref · {reference}"), text_width);
    }
    if let Some(warning) = row.herdr_warning {
        body.push_wrapped_color(
            &format!(
                "Requires Herdr {} · running {}",
                warning.required, warning.running
            ),
            text_width,
            app.theme.color(Role::Warning),
        );
    }
    body.push("");

    let Some(detail) = row.detail.as_ref() else {
        body.push_wrapped(
            "Esc returns to the list. Press i there to install.",
            text_width,
        );
        return body;
    };

    if !detail.description.is_empty() {
        body.push_wrapped(&detail.description, text_width);
        body.push("");
    }

    if let Some(maintenance) = row.maintenance.as_ref() {
        if let Some(age) = maintenance.age() {
            let quiet = if maintenance.is_quiet() {
                " · quiet"
            } else {
                ""
            };
            body.push(format!("Maintenance · last pushed {age} ago{quiet}"));
        }
    }

    let items = App::bindable(detail);
    if items.is_empty() && detail.events.is_empty() {
        body.push("No actions, panes or events declared.");
    }

    if !items.is_empty() {
        body.push("Actions and panes");
        for (i, (target, label)) in items.iter().enumerate() {
            let selected = i == app.detail_cursor;
            let kind = match target {
                crate::BindTarget::Action(_) => "Action",
                crate::BindTarget::Pane(_) => "Pane",
            };
            let pointer = if selected { ">" } else { " " };
            if selected {
                body.selected_row = Some(body.lines.len());
                body.selected_label = Some(label.clone());
            }
            let short = if width < 28 {
                format!("{pointer} {label}")
            } else {
                format!("{pointer} {kind} · {label}")
            };
            let text = if width >= 70 && label != target.id() {
                let (short, _) = clip(&short, text_width.saturating_sub(25));
                format!("{short} · {}", target.id())
            } else {
                short
            };
            let color = if selected {
                app.theme.color(Role::Accent)
            } else {
                Color::Plain
            };
            body.push_color(text, color);
        }
        body.push("");
    }

    if !detail.events.is_empty() {
        body.push("Events");
        for event in &detail.events {
            body.push(format!("  {event}"));
        }
        body.push("");
    }

    if let Some((row_idx, changes)) = &app.detail_changes {
        if *row_idx == idx {
            let warning = app.theme.color(Role::Warning);
            match changes {
                Changes::Commits(subjects) if !subjects.is_empty() => {
                    body.push_color(format!("Changes · {} new commits", subjects.len()), warning);
                    for subject in subjects.iter().take(8) {
                        body.push(format!("  · {subject}"));
                    }
                    if subjects.len() > 8 {
                        body.push(format!("  … and {} more", subjects.len() - 8));
                    }
                    body.push("Esc returns to the list. Press u there to update.");
                }
                Changes::Commits(_) => {}
                Changes::ManyPlus(count) => {
                    body.push_color(format!("Changes · {count}+ commits behind"), warning);
                    body.push("Esc returns to the list. Press u there to update.");
                }
                Changes::Unknown(reason) => {
                    body.push(format!("Could not check for changes · {reason}"))
                }
            }
            body.push("");
        }
    }

    if let Some((target, _)) = items.get(app.detail_cursor) {
        let (kind, command) = crate::bind_toml_fields(target, &detail.plugin_id);
        body.push("Binding preview");
        body.push(format!("  type = \"{kind}\""));
        body.push_wrapped(&format!("  command = \"{command}\""), text_width);
    }
    body
}

fn footer_row(height: u16) -> usize {
    // As with the Manage list, Herdr may reserve the final two terminal rows.
    usize::from(footer_start_row(height)) + usize::from(height >= 3)
}

fn body_window(height: u16) -> (usize, usize) {
    let footer = footer_row(height);
    if height < 9 {
        (2, footer.saturating_sub(2))
    } else {
        (3, footer.saturating_sub(4))
    }
}

pub(super) fn confirm_selection_visible(app: &mut App, width: u16, height: u16) -> bool {
    let (_, visible) = body_window(height);
    if width < 6 || visible == 0 {
        app.flash = Some("Enlarge terminal to show the selected action".to_string());
        return false;
    }
    let body = body(app, width);
    let Some(selected) = body.selected_row else {
        app.flash = Some("No actions or panes available".to_string());
        return false;
    };
    let offset = body.offset(app.detail_scroll, visible);
    if (offset..offset + visible).contains(&selected) {
        return true;
    }
    app.detail_scroll = offset.clamp(selected.saturating_sub(visible - 1), selected);
    app.flash = Some("Selection revealed · press the key again".to_string());
    false
}

pub(super) fn reveal_selection(app: &mut App, width: u16, height: u16) {
    let (_, visible) = body_window(height);
    if visible == 0 {
        return;
    }
    let body = body(app, width);
    let Some(selected) = body.selected_row else {
        return;
    };
    let offset = body.offset(app.detail_scroll, visible);
    app.detail_scroll = offset.clamp(selected.saturating_sub(visible - 1), selected);
}

pub(super) fn scroll_page(app: &mut App, width: u16, height: u16, forward: bool) {
    let (_, visible) = body_window(height);
    let step = visible.saturating_sub(1).max(1) as isize;
    scroll_rows(app, width, height, if forward { step } else { -step });
}

pub(super) fn scroll_rows(app: &mut App, width: u16, height: u16, amount: isize) {
    let (_, visible) = body_window(height);
    let body = body(app, width);
    let offset = body.offset(app.detail_scroll, visible);
    app.detail_scroll = body.offset(offset.saturating_add_signed(amount), visible);
}

pub(super) fn draw(app: &App, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
    let Some(row) = app.detail_of.and_then(|idx| app.rows.get(idx)) else {
        return Ok(());
    };
    if width == 0 || height == 0 {
        return Ok(());
    }
    let columns = usize::from(width.saturating_sub(1)).max(1);
    let (start, visible) = body_window(height);
    let body = body(app, width);
    let offset = body.offset(app.detail_scroll, visible);
    let mut title = format!(" {}", row.label);
    if let Some((index, label)) = body.selected_row.zip(body.selected_label.as_ref()) {
        if !(offset..offset + visible).contains(&index) {
            title.push_str(&format!(" · Selected: {label}"));
        }
    }
    let (title, _) = clip(&title, columns);
    write!(out, "\x1b[H\x1b[2J\x1b[1m{title}\x1b[0m")?;
    for (i, line) in body.lines.iter().skip(offset).take(visible).enumerate() {
        let (text, _) = clip(&format!(" {}", line.text), columns);
        write!(out, "\x1b[{};1H{}{text}\x1b[0m", start + i, line.color)?;
    }

    let footer_row = footer_row(height);
    if height >= 9 {
        let rule = rule_line(width);
        write!(
            out,
            "\x1b[2;1H\x1b[2m{rule}\x1b[{};1H{rule}\x1b[0m",
            footer_row - 1
        )?;
    }
    if footer_row > 1 {
        let (footer, color) = if app.awaiting_bind {
            (
                "Letter binds to prefix+shift+letter · Esc cancels".to_string(),
                app.theme.color(Role::Warning),
            )
        } else if let Some(message) = app.flash.as_ref() {
            (message.clone(), app.theme.color(Role::Accent))
        } else {
            let mut hints = String::from("Esc Back");
            let compact = width < 75;
            if body.lines.len() > visible {
                hints.push_str(if compact {
                    " · Pg↑/↓"
                } else {
                    " · PgUp/PgDn Scroll"
                });
            }
            if body.selected_label.is_some() {
                hints.push_str(if compact {
                    " · ↑↓ Enter b"
                } else {
                    " · ↑/↓ Select · Enter Run · b Bind"
                });
            }
            if row.installed.is_some() {
                hints.push_str(if compact { " · L" } else { " · L Logs" });
            }
            (hints, Color::Plain)
        };
        let (footer, _) = clip(&footer, columns);
        write!(out, "\x1b[{footer_row};1H{color}{footer}\x1b[0m")?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Installed;

    fn fixture() -> App {
        let plugin = Installed {
            plugin_id: "example.plugin".into(),
            name: "Plugin 界".into(),
            description: "Navigation and workspace setup. ".repeat(3),
            actions: (0..12)
                .map(|i| (format!("example.plugin.action-{i}"), format!("Command {i}")))
                .collect(),
            events: vec!["workspace.created".into()],
            ..Default::default()
        };
        App {
            rows: super::super::rows(&[], &[plugin]),
            detail_of: Some(0),
            detail_changes: Some((0, Changes::Commits(vec!["Fix workspaces".into()]))),
            ..Default::default()
        }
    }

    fn render(app: &App, width: u16, height: u16) -> String {
        let mut output = Vec::new();
        draw(app, &mut output, width, height).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn compact_view_keeps_selection_and_scrolls_to_metadata() {
        let mut app = fixture();
        app.detail_cursor = 10;
        reveal_selection(&mut app, 40, 10);
        let output = render(&app, 40, 10);
        assert!(output.contains("> Action · Command 10"));
        assert!(output.contains("Pg↑/↓"));
        assert!(output.contains("Enter b"));
        assert!(output.contains("\x1b[8;1H"));
        assert!(!output.contains("\x1b[9;1H"));
        assert!(!output.contains("Command 0"));
        assert!(!output.contains('\n'));

        let mut scrolled = String::new();
        for _ in 0..5 {
            scroll_page(&mut app, 40, 10, true);
            scrolled.push_str(&render(&app, 40, 10));
        }
        assert!(scrolled.contains("Selected: Command 10"));
        assert!(scrolled.contains("Fix workspaces"));
        assert!(scrolled.contains("Binding preview"));
        assert_eq!(app.detail_cursor, 10);
    }

    #[test]
    fn offscreen_selection_requires_another_keypress_before_dispatch() {
        let mut app = fixture();
        app.detail_cursor = 10;
        assert!(!confirm_selection_visible(&mut app, 40, 10));
        assert!(render(&app, 40, 10).contains("> Action · Command 10"));
        assert!(confirm_selection_visible(&mut app, 40, 10));
        assert!(!confirm_selection_visible(&mut app, 40, 3));
    }

    #[test]
    fn detail_lists_only_available_local_metadata() {
        let mut app = fixture();
        let row = &mut app.rows[0];
        row.slug = Some("example/plugin".into());
        row.enabled = Some(false);
        row.commit = Some("0123456789abcdef0123456789abcdef01234567".into());
        row.listed_as = Some("example/plugin@stable".into());
        row.herdr_warning = Some(super::super::HerdrVersionWarning {
            required: crate::HerdrVersion::parse("0.10.0").unwrap(),
            running: crate::HerdrVersion::parse("0.9.5").unwrap(),
        });
        let wide = render(&app, 120, 24);
        for fact in [
            "Installed · disabled",
            "Repository · example/plugin",
            "Resolved commit · 0123456789abcdef0123456789abcdef01234567",
            "Listed ref · stable",
            "Requires Herdr 0.10.0 · running 0.9.5",
        ] {
            assert!(wide.contains(fact), "missing {fact}");
        }
        let narrow = body(&app, 25);
        let text: String = narrow.lines.into_iter().map(|line| line.text).collect();
        assert!(text.contains("0123456789abcdef0123456789abcdef01234567"));

        let spec = crate::Spec::parse("example/missing@v1");
        app.rows = super::super::rows(&[spec], &[]);
        let missing = render(&app, 80, 10);
        assert!(missing.contains("Not installed"));
        assert!(missing.contains("Repository · example/missing"));
        assert!(missing.contains("Listed ref · v1"));
        assert!(!missing.contains("Resolved commit"));
        assert!(!missing.contains("Requires Herdr"));
    }

    #[test]
    fn wide_view_keeps_the_action_id_visible() {
        let app = fixture();
        assert!(render(&app, 120, 24).contains("Command 0 · example.plugin.action-0"));
    }

    #[test]
    fn detail_display_does_not_emit_control_text_or_write_below_the_view() {
        let mut app = fixture();
        app.rows[0].label = "unsafe\x1b[2Jlabel".into();
        let output = render(&app, 30, 5);
        assert_eq!(output.matches("\x1b[2J").count(), 1);
        assert!(output.contains("\x1b[3;1H"));
        assert!(!output.contains("\x1b[4;1H"));
    }
}
