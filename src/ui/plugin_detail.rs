use std::io::{self, Write};

use super::{
    footer_start_row, rule_line,
    terminal_text::{clip, one_line, wrap_line},
    App, Color, Role,
};
use crate::github::Changes;

struct Line {
    text: String,
    color: Color,
    bold: bool,
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
        self.push_styled(text, color, false);
    }

    fn push_styled(&mut self, text: impl Into<String>, color: Color, bold: bool) {
        self.lines.push(Line {
            text: text.into(),
            color,
            bold,
        });
    }

    fn push_wrapped(&mut self, text: &str, width: usize) {
        self.push_wrapped_color(text, width, Color::Plain);
    }

    fn push_wrapped_color(&mut self, text: &str, width: usize, color: Color) {
        if text.is_empty() {
            return;
        }
        for line in wrap_line(&one_line(text), width) {
            self.push_color(line, color);
        }
    }

    fn offset(&self, requested: usize, visible: usize) -> usize {
        requested.min(self.lines.len().saturating_sub(visible))
    }
}

fn push_trust(body: &mut Body, report: &crate::plugin_trust::Report, width: usize) {
    body.push("");
    body.push_styled("Plugin provenance", Color::Plain, true);
    for fact in report.lines() {
        body.push_wrapped(&fact, width);
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
    if row.trust.hide_install_details {
        body.push_color("Installation · Unknown", app.theme.color(Role::Warning));
        body.push_wrapped(
            "The installed record cannot be attributed to this entry. Manifest, actions, compatibility, and logs are unavailable here.",
            text_width,
        );
        push_trust(&mut body, &row.trust, text_width);
        return body;
    }
    let (status, role) = match (row.installed.is_some(), row.enabled) {
        (false, _) => ("Not installed", Role::Warning),
        (true, Some(true)) => ("Installed · enabled", Role::Success),
        (true, Some(false)) => ("Installed · disabled", Role::Accent),
        (true, None) => ("Installed", Role::Success),
    };
    body.push_color(status, app.theme.color(role));
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

    let Some(detail) = row.plugin_detail() else {
        body.push_wrapped(
            "Esc returns to the list. Press i there to install.",
            text_width,
        );
        push_trust(&mut body, &row.trust, text_width);
        return body;
    };

    if !detail.description.is_empty() {
        body.push_wrapped(&detail.description, text_width);
        body.push("");
    }

    let items = App::bindable(detail);
    if items.is_empty() && detail.events.is_empty() {
        body.push("No actions, panes or events declared.");
    }

    if !items.is_empty() {
        body.push_styled("Actions and panes", Color::Plain, true);
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
            body.push_styled(text, color, selected);
        }
        body.push("");
    }

    if !detail.events.is_empty() {
        body.push_styled("Events", Color::Plain, true);
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
                    body.push_styled(
                        format!("Changes · {} new commits", subjects.len()),
                        warning,
                        true,
                    );
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
                    body.push_styled(format!("Changes · {count}+ commits behind"), warning, true);
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
        body.push_styled("Binding preview", Color::Plain, true);
        body.push(format!("  type = \"{kind}\""));
        body.push_wrapped(&format!("  command = \"{command}\""), text_width);
    }
    push_trust(&mut body, &row.trust, text_width);
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
    let selection_visible = body
        .selected_row
        .is_some_and(|index| (offset..offset + visible).contains(&index));
    let title = format!(" {}", row.label);
    let (_, title_cells) = clip(&title, columns);
    let summary = if let Some(label) = body.selected_label.as_ref().filter(|_| !selection_visible) {
        let budget = (columns * 2 / 3).max(columns - title_cells);
        clip(&format!(" · Selected: {label}"), budget)
    } else {
        (String::new(), 0)
    };
    let (summary, summary_cells) = summary;
    let (title, _) = clip(&title, columns - summary_cells);
    write!(out, "\x1b[H\x1b[2J\x1b[1m{title}\x1b[0m{summary}")?;
    for (i, line) in body.lines.iter().skip(offset).take(visible).enumerate() {
        let (text, _) = clip(&format!(" {}", line.text), columns);
        let bold = if line.bold { "\x1b[1m" } else { "" };
        write!(
            out,
            "\x1b[{};1H{}{bold}{text}\x1b[0m",
            start + i,
            line.color
        )?;
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
        write!(out, "\x1b[{footer_row};1H")?;
        let status = if app.awaiting_bind {
            Some((
                "Letter binds to prefix+shift+letter · Esc cancels",
                app.theme.color(Role::Warning),
            ))
        } else {
            app.flash
                .as_deref()
                .map(|message| (message, app.theme.color(Role::Accent)))
        };
        if let Some((message, color)) = status {
            let (footer, _) = clip(message, columns);
            write!(out, "{color}{footer}\x1b[0m")?;
        } else {
            let mut hints = String::from("Esc Back");
            let compact = width < 75;
            hints.push_str(
                match (offset > 0, offset + visible < body.lines.len(), compact) {
                    (true, true, true) => " · Pg↑/↓",
                    (true, false, true) => " · Pg↑",
                    (false, true, true) => " · Pg↓",
                    (true, true, false) => " · PgUp/PgDn Scroll",
                    (true, false, false) => " · PgUp Scroll",
                    (false, true, false) => " · PgDn Scroll",
                    (false, false, _) => "",
                },
            );
            if body.selected_label.is_some() {
                hints.push_str(if compact {
                    " · ↑↓ Enter b"
                } else {
                    " · ↑/↓ Select · Enter Run · b Bind"
                });
            }
            if row.installed.is_some() && !row.trust.hide_install_details {
                hints.push_str(if compact { " · L" } else { " · L Logs" });
            }
            let (footer, _) = clip(&hints, columns);
            let primary = if compact { "Enter" } else { "Enter Run" };
            if let Some((before, after)) = footer.split_once(primary).filter(|_| selection_visible)
            {
                write!(out, "{before}\x1b[1m{primary}\x1b[0m{after}")?;
            } else {
                write!(out, "{footer}")?;
            }
        }
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
        app.draw(&mut output, width, height).unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn detail_description_wraps_whole_graphemes() {
        let mut app = fixture();
        let detail = app.rows[0].detail.as_mut().unwrap();
        detail.description = "x👩‍💻⌚\u{fe0e}☀\u{fe0f}e\u{301}界🇯🇵\tZ".into();
        let frame = render(&app, 5, 24);
        for line in [" x👩‍💻", " ⌚\u{fe0e}☀\u{fe0f}", " e\u{301}界", " 🇯🇵 ", " Z"]
        {
            assert!(frame.contains(&format!(";1H{line}\x1b[0m")), "{frame:?}");
        }
        assert!(!frame.contains('\t'));
        let frame = render(&app, 3, 24);
        assert!(frame.contains(";1H …\x1b[0m"));
        assert!(!frame.contains('💻'));
    }

    #[test]
    fn detail_visual_hierarchy_tracks_the_viewport() {
        let mut app = fixture();
        let initial = render(&app, 40, 9);
        assert!(initial.contains("Pg↓"));
        assert!(!initial.contains("Pg↑"));
        assert!(!initial.contains("\x1b[1mEnter\x1b[0m"));

        app.detail_cursor = 1;
        reveal_selection(&mut app, 40, 9);
        let selected = render(&app, 40, 9);
        assert!(selected.contains("\x1b[1m Actions and panes\x1b[0m"));
        assert!(selected.contains("\x1b[1m > Action · Command 1\x1b[0m"));
        assert!(selected.contains("Pg↑/↓"));
        assert!(selected.contains("\x1b[7;1HEsc Back · Pg↑/↓ · ↑↓ \x1b[1mEnter\x1b[0m b · L"));

        scroll_rows(&mut app, 40, 11, isize::MAX);
        app.rows[0].label = "owner/long-plugin-name".into();
        let end = render(&app, 40, 11);
        assert!(end.contains("Selected: Command 1"));
        assert!(end.contains("Lock comparison does not authenticate"));
        assert!(end.contains("Pg↑"));
        assert!(!end.contains("Pg↓"));
        assert!(!end.contains("\x1b[1mEnter\x1b[0m"));
    }

    #[test]
    fn compact_view_keeps_selection_and_scrolls_to_metadata() {
        let mut app = fixture();
        app.detail_cursor = 10;
        reveal_selection(&mut app, 40, 10);
        let output = render(&app, 40, 10);
        assert!(output.contains("> Action · Command 10"));
        assert!(output.contains("Pg↑/↓"));
        assert!(output.contains("Enter\x1b[0m b"));
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
    fn detail_shows_source_labelled_evidence_and_unknowns_for_missing_rows() {
        let mut app = fixture();
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let plugin = Installed {
            slug: Some("Example/Plugin".into()),
            source_kind: "github".into(),
            resolved_commit: Some(sha.into()),
            ..Installed::default()
        };
        let spec = crate::Spec::parse("example/plugin@stable");
        app.rows = super::super::rows(&[spec], &[plugin]);
        app.rows[0]
            .trust
            .set_lock(Some(&format!("example/plugin@{sha}")));
        app.rows[0].herdr_warning = Some(super::super::HerdrVersionWarning {
            required: crate::HerdrVersion::parse("0.10.0").unwrap(),
            running: crate::HerdrVersion::parse("0.9.5").unwrap(),
        });
        app.detail_changes = None;
        let wide = render(&app, 120, 24);
        for fact in [
            "Installed · disabled",
            "Requires Herdr 0.10.0 · running 0.9.5",
            "Plugin provenance",
            "Repository · example/plugin (bundle list)",
            "Resolved revision · 0123456789abcdef0123456789abcdef01234567 (installed registry)",
            "Requested ref · stable (bundle list)",
            "Registry revision matches lock",
        ] {
            assert!(wide.contains(fact), "missing {fact}: {wide:?}");
        }
        let narrow = body(&app, 25);
        let text: String = narrow.lines.into_iter().map(|line| line.text).collect();
        assert!(text.contains(sha));

        let spec = crate::Spec::parse("example/missing@v1");
        app.rows = super::super::rows(&[spec], &[]);
        let missing = render(&app, 80, 24);
        assert!(missing.contains("Not installed"));
        assert!(missing.contains("Repository · example/missing (bundle list)"));
        assert!(missing.contains("Requested ref · v1 (bundle list)"));
        assert!(missing.contains("Resolved revision · Unknown"));
        assert!(!missing.contains("Requires Herdr"));
    }

    #[test]
    fn detail_does_not_attribute_a_case_distinct_subdirectory_to_the_bundle_entry() {
        let plugin = Installed {
            plugin_id: "fixture-other-subdir".into(),
            enabled: true,
            source_kind: "github".into(),
            slug: Some("example/plugin/tools".into()),
            min_herdr_version: crate::HerdrVersion::parse("0.10.0"),
            description: "Other manifest description".into(),
            actions: vec![("other.action".into(), "Other action".into())],
            events: vec!["other.event".into()],
            ..Installed::default()
        };
        let mut app = App {
            rows: super::super::rows_with_herdr_version(
                &[crate::Spec::parse("example/plugin/Tools")],
                &[plugin],
                &[],
                crate::HerdrVersion::parse("0.9.5"),
            ),
            detail_of: Some(0),
            detail_changes: Some((0, Changes::Commits(vec!["Other plugin changes".into()]))),
            ..App::default()
        };
        assert_eq!(app.rows[0].enabled, Some(true));
        assert!(app.rows[0].herdr_warning.is_some());
        assert!(app.rows[0].detail.is_some());
        assert!(app.rows[0].plugin_detail().is_none());

        let frame = render(&app, 120, 24);
        assert!(frame.contains("Installation · Unknown"));
        assert!(frame.contains("Repository · example/plugin/Tools (bundle list)"));
        for hidden in [
            "Installed ·",
            "Other manifest description",
            "Other action",
            "other.event",
            "Requires Herdr",
            "Other plugin changes",
            "Binding preview",
            "L Logs",
        ] {
            assert!(!frame.contains(hidden), "unexpected {hidden}: {frame:?}");
        }
        assert!(!confirm_selection_visible(&mut app, 120, 24));
        app.run_detail_action();
        app.bind_selected('t');
        assert!(app.pending_bind.is_none());
        app.open_detail_logs(0);
        assert!(app.log_view.is_none());
        assert_eq!(
            app.flash.as_deref(),
            Some("Logs unavailable · plugin identity is uncertain")
        );
    }

    #[test]
    fn ordinary_weak_local_detail_still_shows_actions() {
        let plugin = Installed {
            name: "plugin".into(),
            enabled: true,
            source_kind: "local".into(),
            actions: vec![("local.action".into(), "Local action".into())],
            ..Installed::default()
        };
        let mut app = App {
            rows: super::super::rows(&[crate::Spec::parse("example/plugin")], &[plugin]),
            detail_of: Some(0),
            ..App::default()
        };
        let frame = render(&app, 120, 24);
        assert!(frame.contains("Installed · enabled"));
        assert!(frame.contains("Local action"));
        assert!(frame.contains("L Logs"));
        assert!(confirm_selection_visible(&mut app, 120, 24));
    }

    #[test]
    fn detail_scroll_stays_local_and_log_modal_keeps_precedence() {
        let mut app = fixture();
        let cursor = app.detail_cursor;
        scroll_rows(&mut app, 80, 9, isize::MAX);
        assert!(render(&app, 80, 9).contains("Lock comparison does not authenticate"));
        let offset = app.detail_scroll;
        app.log_view = Some(super::super::LogView::from_result(
            "example.plugin".into(),
            Ok("example log".into()),
        ));
        let modal = render(&app, 80, 24);
        assert!(modal.contains("example log"));
        assert!(!modal.contains("Lock comparison"));
        app.log_view = None;
        assert_eq!(app.detail_scroll, offset);
        assert_eq!(app.detail_cursor, cursor);
        assert!(render(&app, 80, 9).contains("Lock comparison does not authenticate"));
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
