use std::io::{self, Write};

use crossterm::event::KeyCode;

use crate::theme::{Role, Theme, INPUT_LIMIT, PRESETS};

use super::terminal_text::{clip, clip_tail, display_width};

const CUSTOM: usize = PRESETS.len();
const CURRENT_SUFFIX: &str = "  [Current]";

fn theme_label(index: usize) -> &'static str {
    PRESETS.get(index).map_or("Custom", |preset| preset.label)
}

fn save_visible(width: u16, height: u16) -> bool {
    // Leave room for the theme selection and its save/cancel hint.
    width >= 20 && height >= 5
}

pub(super) enum Action {
    Continue,
    Cancel,
    Apply { value: String, theme: Theme },
}

pub(super) struct Picker {
    current: usize,
    cursor: usize,
    custom: String,
    editing: bool,
    environment_override: bool,
    error: Option<String>,
}

impl Picker {
    pub(super) fn new(theme: Theme, environment_override: bool) -> Self {
        let current = if theme.is_custom() {
            CUSTOM
        } else {
            PRESETS
                .iter()
                .position(|preset| preset.palette == theme)
                .unwrap_or(0)
        };
        Self {
            current,
            cursor: current,
            custom: theme.custom_value(),
            editing: false,
            environment_override,
            error: None,
        }
    }

    pub(super) fn move_to(&mut self, down: bool) {
        if !self.editing {
            self.cursor = if down {
                (self.cursor + 1).min(CUSTOM)
            } else {
                self.cursor.saturating_sub(1)
            };
            self.error = None;
        }
    }

    pub(super) fn save_error(&mut self, error: io::Error) {
        self.error = Some(format!("Could not save theme · {error}"));
    }

    fn preview(&self) -> Result<Theme, &'static str> {
        match PRESETS.get(self.cursor) {
            Some(preset) => Ok(preset.palette),
            None => Theme::parse_custom(&self.custom),
        }
    }

    pub(super) fn key(&mut self, key: KeyCode, width: u16, height: u16) -> Action {
        if key == KeyCode::Esc {
            return Action::Cancel;
        }
        let can_save = !self.environment_override && save_visible(width, height);
        if self.editing {
            match key {
                KeyCode::Char(ch)
                    if !ch.is_control() && self.custom.len() + ch.len_utf8() <= INPUT_LIMIT =>
                {
                    self.custom.push(ch);
                    self.error = None;
                }
                KeyCode::Delete => {
                    self.custom.clear();
                    self.error = None;
                }
                KeyCode::Backspace => {
                    self.custom.pop();
                    self.error = None;
                }
                KeyCode::Enter if can_save => match self.preview() {
                    Ok(theme) => {
                        return Action::Apply {
                            value: theme.custom_value(),
                            theme,
                        }
                    }
                    Err(error) => self.error = Some(error.to_string()),
                },
                _ => {}
            }
            return Action::Continue;
        }
        match key {
            KeyCode::Down | KeyCode::Char('j') => self.move_to(true),
            KeyCode::Up | KeyCode::Char('k') => self.move_to(false),
            KeyCode::Home | KeyCode::Char('g') => {
                self.cursor = 0;
                self.error = None;
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.cursor = CUSTOM;
                self.error = None;
            }
            KeyCode::Enter if self.cursor == CUSTOM => self.editing = true,
            KeyCode::Enter if can_save => {
                let preset = &PRESETS[self.cursor];
                return Action::Apply {
                    value: preset.id.to_string(),
                    theme: preset.palette,
                };
            }
            _ => {}
        }
        Action::Continue
    }

    pub(super) fn draw(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        write!(out, "\x1b[H\x1b[2J")?;
        let columns = usize::from(width.saturating_sub(1));
        let last = usize::from(height.saturating_sub(2));
        if columns == 0 || last == 0 {
            return out.flush();
        }
        let current = theme_label(self.current);
        let current_suffix_fits = 2 + current.len() + CURRENT_SUFFIX.len() <= columns;
        let compact = last == 3;
        let compact_choice = (compact && !self.editing && self.cursor != self.current)
            .then(|| format!("> {} · Current: {current}", theme_label(self.cursor)));
        // If both themes need their own row, use the preview's row for the focused option.
        let split_choice = compact
            && !self.editing
            && compact_choice
                .as_ref()
                .map_or(!current_suffix_fits, |label| {
                    label.chars().count() > columns
                });
        let title_visible = last >= 4 || split_choice;
        let preview = self.preview();
        let show_statuses = last >= 6 && columns >= 19 && preview.is_ok() && self.error.is_none();
        let preview_rows = usize::from(last >= 2 && !split_choice) + usize::from(show_statuses);
        let hint_visible = last >= 3;
        let top = usize::from(title_visible);
        let visible = (last - top - preview_rows - usize::from(hint_visible)).max(1);
        let start = self.cursor.saturating_sub(visible - 1);
        let current_in_title = title_visible
            && (split_choice
                || self.current < start
                || self.current >= start + visible
                || !current_suffix_fits);
        if title_visible {
            let title = if self.editing {
                let title = format!("Custom theme · Current: {current}");
                if title.chars().count() <= columns {
                    title
                } else {
                    format!("Current: {current}")
                }
            } else if current_in_title {
                let label = format!("Current: {current}");
                if split_choice && label.len() > columns {
                    format!("On {current}")
                } else {
                    label
                }
            } else {
                "Theme preview".to_string()
            };
            write!(out, "\x1b[1m{}\x1b[0m", clip(&title, columns).0)?;
        }
        let mut row = top + 1;
        if self.editing {
            let prefix = if compact && save_visible(width, height) {
                let prefix = format!("On {current} · ");
                if display_width(&prefix) + 8 <= columns {
                    prefix
                } else {
                    format!("[{}] ", clip(current, columns - 11).0)
                }
            } else {
                String::new()
            };
            self.draw_input(out, row, columns, &prefix)?;
            row += 1;
            if preview_rows > 0 {
                self.draw_preview(out, row, columns, preview, show_statuses)?;
                row += preview_rows;
            }
        } else {
            for index in (start..=CUSTOM).take(visible) {
                let name = theme_label(index);
                let focused = index == self.cursor;
                let marker = if focused { "> " } else { "  " };
                let label = if let Some(label) = compact_choice.as_ref().filter(|_| !split_choice) {
                    label.clone()
                } else if index != self.current || current_in_title {
                    format!("{marker}{name}")
                } else if current_suffix_fits {
                    format!("{marker}{name}{CURRENT_SUFFIX}")
                } else {
                    format!("{marker}[Current] {name}")
                };
                write!(out, "\x1b[{row};1H{}", clip(&label, columns).0)?;
                row += 1;
                if focused && preview_rows > 0 {
                    self.draw_preview(out, row, columns, preview, show_statuses)?;
                    row += preview_rows;
                }
            }
        }
        if split_choice && self.error.is_some() {
            self.draw_preview(out, row, columns, preview, false)?;
        } else if hint_visible {
            let edit_hint = "[Enter] Save · [Esc] Cancel · [Del] Clear";
            let full_save_hint = "[Enter] Save · [Esc] Cancel";
            let full_hint_fits = full_save_hint.chars().count() <= columns;
            let save_hint = if full_hint_fits {
                full_save_hint
            } else {
                "↵ Save · Esc Cancel"
            };
            let hint = if self.environment_override {
                if columns >= 35 {
                    "Environment override · [Esc] Cancel"
                } else {
                    "Locked · Esc Cancel"
                }
            } else if !save_visible(width, height) {
                "Enlarge pane to save"
            } else if self.editing {
                if edit_hint.chars().count() <= columns {
                    edit_hint
                } else {
                    save_hint
                }
            } else if self.cursor == CUSTOM {
                if full_hint_fits {
                    "[Enter] Edit · [Esc] Cancel"
                } else {
                    "↵ Edit · Esc Cancel"
                }
            } else {
                save_hint
            };
            write!(out, "\x1b[{row};1H{}", clip(hint, columns).0)?;
        }
        out.flush()
    }

    fn draw_input(
        &self,
        out: &mut impl Write,
        row: usize,
        columns: usize,
        prefix: &str,
    ) -> io::Result<()> {
        // Reserve a cell for the caret after the visible end of the input.
        let tail = clip_tail(&self.custom, columns - display_width(prefix) - 1);
        write!(out, "\x1b[{row};1H{prefix}{tail}\x1b[7m \x1b[0m")
    }

    fn draw_preview(
        &self,
        out: &mut impl Write,
        row: usize,
        columns: usize,
        preview: Result<Theme, &'static str>,
        show_statuses: bool,
    ) -> io::Result<()> {
        write!(out, "\x1b[{row};1H")?;
        if let Some(error) = &self.error {
            let danger = Theme::default().color(Role::Danger);
            return write!(out, "{danger}{}\x1b[0m", clip(error, columns).0);
        }
        let theme = match preview {
            Ok(theme) => theme,
            Err(error) if self.editing && !self.custom.is_empty() => {
                return write!(out, "{}", clip(error, columns).0);
            }
            Err(_) => return write!(out, "{}", clip("Example: success=#80c7a2", columns).0),
        };
        // Match the Manage list's focus pointer, colored status, bold name, and dim commit.
        let prefix = if columns >= 19 { "    > " } else { "> " };
        let (prefix, prefix_cells) = clip(prefix, columns);
        let status = if columns >= 26 { "Installed" } else { "OK" };
        let (status, status_cells) = clip(status, columns - prefix_cells);
        let name_columns = columns - prefix_cells - status_cells;
        let success = theme.color(Role::Success);
        write!(out, "{prefix}{success}{status}\x1b[0m")?;
        if name_columns > 1 {
            let (name, _) = clip("pane-tools", name_columns - 1);
            write!(out, " \x1b[1m{name}\x1b[0m")?;
        }
        if columns >= 35 {
            write!(out, "  \x1b[2ma1b2c3d\x1b[0m")?;
        }
        if show_statuses {
            let wide = columns >= 51;
            let indent = if wide {
                "      "
            } else if columns >= 21 {
                "  "
            } else {
                ""
            };
            write!(out, "\x1b[{};1H{indent}", row + 1)?;
            let statuses = [
                (Role::Warning, "Missing", "MIS"),
                (Role::Danger, "Error", "ERR"),
                (Role::Accent, "Disabled", "OFF"),
                (Role::Info, "Manager", "MGR"),
                (Role::Local, "Local link", "LNK"),
            ];
            for (index, (role, label, short)) in statuses.into_iter().enumerate() {
                let gap = if index == 0 {
                    ""
                } else if wide {
                    "  "
                } else {
                    " "
                };
                let color = theme.color(role);
                let status = if wide { label } else { short };
                write!(out, "{gap}{color}{status}\x1b[0m")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_input_draws_whole_graphemes_before_the_caret() {
        let mut picker = Picker::new(Theme::default(), false);
        picker.key(KeyCode::End, 32, 5);
        picker.key(KeyCode::Enter, 32, 5);
        for ch in "abcdefge\u{301}👩‍💻".chars() {
            picker.key(KeyCode::Char(ch), 32, 5);
        }
        for (width, height, expected) in [
            (6, 8, "\x1b[2;1H…e\u{301}👩‍💻"),
            (32, 5, "\x1b[1;1HOn Terminal default · …defge\u{301}👩‍💻"),
        ] {
            let mut out = Vec::new();
            picker.draw(&mut out, width, height).unwrap();
            let frame = String::from_utf8(out).unwrap();
            assert!(
                frame.contains(&format!("{expected}\x1b[7m \x1b[0m")),
                "{frame:?}"
            );
        }
    }

    #[test]
    fn focused_preview_sits_below_its_option_and_keeps_current_visible() {
        let mut picker = Picker::new(Theme::default(), false);
        picker.move_to(true);

        for (width, height) in [(28, 9), (100, 28)] {
            let mut output = Vec::new();
            picker.draw(&mut output, width, height).unwrap();
            let output = String::from_utf8(output).unwrap();
            let focused = output.find("> Catppuccin Mocha").unwrap();
            assert!(output.contains("[Current]") || output.contains("Current: Terminal"));
            let next_row = output[focused..].split(";1H").nth(1).unwrap();
            assert!(next_row.starts_with("    > \x1b[38;2;166;227;161m"));
            assert!(next_row.contains("\x1b[1mpane-tools\x1b[0m"));
        }
    }

    #[test]
    fn five_row_picker_shows_applied_theme_while_previewing_another() {
        let nord = PRESETS.iter().find(|preset| preset.id == "nord").unwrap();
        let mut picker = Picker::new(nord.palette, false);
        assert!(matches!(
            picker.key(KeyCode::Char('g'), 40, 5),
            Action::Continue
        ));

        let mut output = Vec::new();
        picker.draw(&mut output, 40, 5).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("> Terminal default"));
        assert!(output.contains("[Enter] Save · [Esc] Cancel"));
        assert!(output.contains("Current: Nord"));
        assert!(output.contains("\x1b[32mInstalled"));
        for row in output
            .split("\x1b[")
            .filter_map(|escape| escape.split_once(";1H"))
            .map(|(row, _)| row.parse::<usize>().unwrap())
        {
            assert!((1..=3).contains(&row), "picker wrote to row {row}");
        }
        assert!(matches!(picker.key(KeyCode::Esc, 40, 5), Action::Cancel));
        assert!(matches!(
            picker.key(KeyCode::Enter, 40, 5),
            Action::Apply { value, theme } if value == "terminal" && theme == Theme::default()
        ));
    }

    #[test]
    fn compact_save_error_keeps_both_theme_names_visible() {
        let mut picker = Picker::new(Theme::default(), false);
        picker.move_to(true);
        picker.save_error(io::Error::other("blocked"));

        let mut output = Vec::new();
        picker.draw(&mut output, 40, 5).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("Current: Terminal default"));
        assert!(output.contains("\x1b[2;1H> Catppuccin Mocha"));
        assert!(output.contains("\x1b[3;1H\x1b[31mCould not save theme · blocked\x1b[0m"));
        assert!(!output.contains("\x1b[4;1H"));
        assert!(!output.contains("\x1b[5;1H"));
    }

    #[test]
    fn previews_move_without_changing_the_current_choice() {
        let mut picker = Picker::new(Theme::default(), false);
        assert!(matches!(
            picker.key(KeyCode::Down, 80, 14),
            Action::Continue
        ));
        assert_eq!(picker.current, 0);
        assert_eq!(picker.preview(), Ok(PRESETS[1].palette));
        assert!(matches!(picker.key(KeyCode::Esc, 80, 14), Action::Cancel));
        assert!(matches!(
            picker.key(KeyCode::Enter, 80, 14),
            Action::Apply { value, theme, .. }
                if value == "catppuccin" && theme == PRESETS[1].palette
        ));

        for (width, height) in [(0, 0), (12, 14), (80, 4)] {
            assert!(matches!(
                picker.key(KeyCode::Enter, width, height),
                Action::Continue
            ));
        }

        let mut locked = Picker::new(Theme::default(), true);
        locked.move_to(true);
        assert!(matches!(
            locked.key(KeyCode::Enter, 80, 14),
            Action::Continue
        ));
    }

    #[test]
    fn a_custom_candidate_is_validated_and_restored_for_editing() {
        let existing = Theme::parse_custom("success=#abcdef").unwrap();
        let mut picker = Picker::new(existing, false);
        assert_eq!(picker.current, CUSTOM);
        assert_eq!(picker.preview(), Ok(existing));
        assert!(matches!(
            picker.key(KeyCode::Enter, 80, 14),
            Action::Continue
        ));
        picker.key(KeyCode::Delete, 80, 14);
        assert!(matches!(
            picker.key(KeyCode::Enter, 80, 14),
            Action::Continue
        ));
        assert!(picker.error.is_some());
        for ch in "warning=#a11b22".chars() {
            picker.key(KeyCode::Char(ch), 80, 14);
        }
        assert!(matches!(
            picker.key(KeyCode::Enter, 80, 14),
            Action::Apply { value, theme }
                if value == "warning=#a11b22" && theme == Theme::parse_custom(&value).unwrap()
        ));
    }
}
