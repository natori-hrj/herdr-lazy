use std::io::{self, Write};

use crossterm::event::KeyCode;

use crate::theme::{Role, Theme, INPUT_LIMIT, PRESETS};

use super::terminal_text::clip;

const CUSTOM: usize = PRESETS.len();

fn save_visible(width: u16, height: u16) -> bool {
    // Give the focused option, a preview, and the save hint one visible row each.
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
        if columns == 0 || height == 0 {
            return out.flush();
        }
        let last = usize::from(height.saturating_sub(2).max(1));
        let title_visible = last >= 4;
        if title_visible {
            let title = if self.editing {
                "Custom theme"
            } else {
                "Theme preview"
            };
            write!(out, "\x1b[1m{}\x1b[0m", clip(title, columns).0)?;
        }
        if self.editing {
            let input_row = if title_visible { 2 } else { 1 };
            self.draw_input(out, input_row, columns)?;
        } else {
            let top = usize::from(title_visible);
            let visible = (last - top).saturating_sub(2).max(1);
            let start = self.cursor.saturating_sub(visible - 1);
            for (offset, index) in (start..=CUSTOM).take(visible).enumerate() {
                let row = top + offset + 1;
                let label = PRESETS.get(index).map_or("Custom", |preset| preset.label);
                let marker = if index == self.cursor { ">" } else { " " };
                let current = if index == self.current {
                    "  [Current]"
                } else {
                    ""
                };
                let (label, _) = clip(&format!("{marker} {label}{current}"), columns);
                write!(out, "\x1b[{row};1H{label}")?;
            }
        }
        if last >= 2 {
            let preview = if last >= 3 { last - 1 } else { last };
            self.draw_preview(out, preview, columns)?;
        }
        if last >= 3 {
            let hint = if self.environment_override {
                "Environment override · [Esc] Cancel"
            } else if !save_visible(width, height) {
                "Enlarge pane to save"
            } else if self.editing {
                "[Enter] Save · [Esc] Cancel · [Del] Clear"
            } else if self.cursor == CUSTOM {
                "[Enter] Edit · [Esc] Cancel"
            } else {
                "[Enter] Save · [Esc] Cancel"
            };
            let row = last;
            write!(out, "\x1b[{row};1H{}", clip(hint, columns).0)?;
        }
        out.flush()
    }

    fn draw_input(&self, out: &mut impl Write, row: usize, columns: usize) -> io::Result<()> {
        // Keep the edit caret visible after the input outgrows its line. Custom palette tokens
        // are ASCII. An unsupported character can occupy two cells until the user removes it.
        let reversed: String = self.custom.chars().rev().collect();
        let tail: String = clip(&reversed, columns.saturating_sub(1))
            .0
            .chars()
            .rev()
            .collect();
        write!(out, "\x1b[{row};1H{tail}\x1b[7m \x1b[0m")
    }

    fn draw_preview(&self, out: &mut impl Write, row: usize, columns: usize) -> io::Result<()> {
        write!(out, "\x1b[{row};1H")?;
        if let Some(error) = &self.error {
            let danger = Theme::default().color(Role::Danger);
            return write!(out, "{danger}{}\x1b[0m", clip(error, columns).0);
        }
        let theme = match self.preview() {
            Ok(theme) => theme,
            Err(error) if self.editing && !self.custom.is_empty() => {
                return write!(out, "{}", clip(error, columns).0);
            }
            Err(_) => return write!(out, "{}", clip("Example: success=#80c7a2", columns).0),
        };
        let samples = [
            (Role::Success, "Installed", "OK"),
            (Role::Warning, "Missing", "Warn"),
            (Role::Danger, "Error", "Err"),
            (Role::Accent, "Notice", "Note"),
            (Role::Info, "Manager", "Info"),
            (Role::Local, "Local link", "Local"),
        ];
        let mut used = 0;
        for (role, label, short) in samples {
            let label = if columns < 54 { short } else { label };
            let gap = if used == 0 { "" } else { "  " };
            if used + gap.len() + label.len() > columns {
                break;
            }
            let color = theme.color(role);
            write!(out, "{gap}{color}{label}\x1b[0m")?;
            used += gap.len() + label.len();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
