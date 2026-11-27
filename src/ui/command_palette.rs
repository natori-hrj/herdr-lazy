use std::io::{self, Write};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{footer_start_row, managed_list::clip, App};
use crate::theme::{Role, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManageCommand {
    Browse,
    Detail,
    Logs,
    Extras,
    Recommendations,
    Profile,
    Machines,
    SharedBundle,
    Starter,
    Refresh,
    Help,
}

struct Item {
    command: ManageCommand,
    label: &'static str,
    key: char,
    hint: &'static str,
    keywords: &'static str,
}

const ITEMS: &[Item] = &[
    Item {
        command: ManageCommand::Browse,
        label: "Browse Marketplace",
        key: '/',
        hint: "/",
        keywords: "search discover plugins",
    },
    Item {
        command: ManageCommand::Detail,
        label: "Inspect selected plugin",
        key: 'l',
        hint: "l",
        keywords: "detail actions metadata",
    },
    Item {
        command: ManageCommand::Logs,
        label: "View selected plugin logs",
        key: 'L',
        hint: "L",
        keywords: "diagnose output errors",
    },
    Item {
        command: ManageCommand::Extras,
        label: "Browse optional bundles",
        key: 'e',
        hint: "e",
        keywords: "extras",
    },
    Item {
        command: ManageCommand::Recommendations,
        label: "View recommendations",
        key: 'n',
        hint: "n",
        keywords: "suggestions agent",
    },
    Item {
        command: ManageCommand::Profile,
        label: "Open workspace profile",
        key: 'p',
        hint: "p",
        keywords: "project lock profile",
    },
    Item {
        command: ManageCommand::Machines,
        label: "Inspect machine health",
        key: 'm',
        hint: "m",
        keywords: "local remote ssh status",
    },
    Item {
        command: ManageCommand::SharedBundle,
        label: "Browse a shared bundle",
        key: 'f',
        hint: "f",
        keywords: "adopt import",
    },
    Item {
        command: ManageCommand::Starter,
        label: "Open Workspace Starter",
        key: 'w',
        hint: "w",
        keywords: "launch setup",
    },
    Item {
        command: ManageCommand::Refresh,
        label: "Reload Manage list",
        key: 'r',
        hint: "Ctrl+R",
        keywords: "refresh reread",
    },
    Item {
        command: ManageCommand::Help,
        label: "Show keyboard help",
        key: '?',
        hint: "?",
        keywords: "shortcuts keybindings",
    },
];

impl ManageCommand {
    pub(super) fn for_key(key: KeyEvent) -> Option<Self> {
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Self::Refresh);
        }
        if !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return None;
        }
        if key.code == KeyCode::Right {
            return Some(Self::Detail);
        }
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        ITEMS
            .iter()
            .find(|item| item.key == c && item.command != Self::Refresh)
            .map(|item| item.command)
    }
}

pub(super) fn is_open_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('p') && key.modifiers == KeyModifiers::CONTROL
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum PaletteAction {
    Continue,
    Close,
    Quit,
    Run(ManageCommand),
}

#[derive(Default)]
pub(super) struct Palette {
    query: String,
    cursor: usize,
    has_selected: bool,
    has_logs: bool,
    has_profile: bool,
    has_recommendations: bool,
}

impl Palette {
    pub(super) fn new(app: &App) -> Self {
        let row = app.selected();
        Self {
            has_selected: row.is_some(),
            has_logs: row.is_some_and(|row| row.installed.is_some()),
            has_profile: app.workspace_profile.is_some() || app.profile_error.is_some(),
            has_recommendations: app.recommendation_info().is_some(),
            ..Self::default()
        }
    }

    fn results(&self) -> Vec<&'static Item> {
        let query = self.query.to_ascii_lowercase();
        let mut matches: Vec<_> = ITEMS
            .iter()
            .filter(|item| match item.command {
                ManageCommand::Detail => self.has_selected,
                ManageCommand::Logs => self.has_logs,
                ManageCommand::Profile => self.has_profile,
                ManageCommand::Recommendations => self.has_recommendations,
                _ => true,
            })
            .filter_map(|item| {
                let label = item.label.to_ascii_lowercase();
                let score = query.split_whitespace().try_fold(0, |score, term| {
                    rank(term, &label)
                        .or_else(|| rank(term, item.keywords).map(|rank| rank + 1))
                        .map(|rank| score + rank)
                })?;
                Some((score, item))
            })
            .collect();
        matches.sort_by_key(|(score, _)| *score);
        matches.into_iter().map(|(_, item)| item).collect()
    }

    pub(super) fn move_cursor(&mut self, forward: bool) {
        self.cursor = if forward {
            self.cursor
                .saturating_add(1)
                .min(self.results().len().saturating_sub(1))
        } else {
            self.cursor.saturating_sub(1)
        };
    }

    pub(super) fn handle_key(&mut self, key: KeyEvent, width: u16, height: u16) -> PaletteAction {
        if is_open_key(key) {
            return PaletteAction::Close;
        }
        if key.modifiers == KeyModifiers::CONTROL {
            return match key.code {
                KeyCode::Char('c') => PaletteAction::Quit,
                KeyCode::Char('j' | 'm') => self.run_selected(width, height),
                _ => PaletteAction::Continue,
            };
        }
        if !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return PaletteAction::Continue;
        }
        match key.code {
            KeyCode::Esc => return PaletteAction::Close,
            KeyCode::Enter => return self.run_selected(width, height),
            KeyCode::Down => self.move_cursor(true),
            KeyCode::Up => self.move_cursor(false),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.results().len().saturating_sub(1),
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.query.push(c);
                self.cursor = 0;
            }
            _ => {}
        }
        PaletteAction::Continue
    }

    fn run_selected(&self, width: u16, height: u16) -> PaletteAction {
        if width < 6 || visible_rows(height) == 0 {
            return PaletteAction::Continue;
        }
        self.results()
            .get(self.cursor)
            .map(|item| PaletteAction::Run(item.command))
            .unwrap_or(PaletteAction::Continue)
    }

    pub(super) fn draw(
        &self,
        out: &mut impl Write,
        width: u16,
        height: u16,
        theme: Theme,
    ) -> io::Result<()> {
        write!(out, "\x1b[H\x1b[2J")?;
        let columns = usize::from(width.saturating_sub(1));
        let footer = footer_row(height);
        if columns == 0 || height == 0 {
            return out.flush();
        }
        let (title, _) = clip(" Commands", columns);
        write!(out, "\x1b[1m{title}\x1b[0m")?;
        if footer < 3 {
            return out.flush();
        }
        let (query, _) = clip(&format!(" > {}", self.query), columns.saturating_sub(1));
        write!(out, "\x1b[2;1H{query}\x1b[7m \x1b[0m")?;

        let results = self.results();
        let visible = visible_rows(height);
        let start = self.cursor.saturating_sub(visible.saturating_sub(1));
        if visible > 0 && results.is_empty() {
            let (empty, _) = clip(" No matching commands", columns);
            write!(out, "\x1b[3;1H{empty}")?;
        }
        for (i, item) in results.iter().enumerate().skip(start).take(visible) {
            let selected = i == self.cursor;
            let pointer = if selected { " > " } else { "   " };
            let line = if columns >= 44 {
                let (name, _) = clip(&format!("{pointer}{}", item.label), columns - 10);
                format!("{name}  [{}]", item.hint)
            } else {
                format!("{pointer}{}", item.label)
            };
            let (text, _) = clip(&line, columns);
            let color = if selected {
                theme.color(Role::Accent)
            } else {
                crate::theme::Color::Plain
            };
            write!(out, "\x1b[{};1H{color}{text}\x1b[0m", 3 + i - start)?;
        }
        let (hint, _) = clip(" ↑↓ Select · Enter Open · Esc Back", columns);
        write!(out, "\x1b[{footer};1H\x1b[2m{hint}\x1b[0m")?;
        out.flush()
    }
}

fn footer_row(height: u16) -> usize {
    usize::from(footer_start_row(height)) + usize::from(height >= 3)
}

fn visible_rows(height: u16) -> usize {
    footer_row(height).saturating_sub(3)
}

fn rank(needle: &str, text: &str) -> Option<usize> {
    if text.starts_with(needle) {
        return Some(0);
    }
    if text.contains(needle) {
        return Some(1);
    }
    let mut text = text.chars();
    needle
        .chars()
        .all(|ch| text.by_ref().any(|candidate| candidate == ch))
        .then_some(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn context(has_selected: bool, has_logs: bool) -> Palette {
        Palette {
            has_selected,
            has_logs,
            ..Default::default()
        }
    }

    fn type_query(palette: &mut Palette, text: &str) {
        for ch in text.chars() {
            assert_eq!(
                palette.handle_key(key(KeyCode::Char(ch)), 40, 10),
                PaletteAction::Continue
            );
        }
    }

    #[test]
    fn fuzzy_search_uses_all_terms_and_favors_direct_matches() {
        let mut palette = context(true, true);
        type_query(&mut palette, "Brws MKT");
        assert_eq!(
            palette.handle_key(key(KeyCode::Enter), 40, 10),
            PaletteAction::Run(ManageCommand::Browse)
        );
        assert_eq!(rank("browse", "browse marketplace"), Some(0));
        assert_eq!(rank("market", "browse marketplace"), Some(1));
        assert_eq!(rank("mkt", "browse marketplace"), Some(2));
        assert_eq!(rank("tmk", "browse marketplace"), None);
    }

    #[test]
    fn selected_row_commands_are_only_offered_when_available() {
        let commands = |palette: Palette| {
            palette
                .results()
                .iter()
                .map(|item| item.command)
                .collect::<Vec<_>>()
        };
        assert!(!commands(Palette::default()).contains(&ManageCommand::Detail));
        assert!(!commands(context(true, false)).contains(&ManageCommand::Logs));
        assert!(commands(context(true, true)).contains(&ManageCommand::Logs));
    }

    #[test]
    fn workspace_entries_reflect_available_context() {
        let mut app = App::default();
        let commands = |palette: Palette| {
            palette
                .results()
                .into_iter()
                .map(|item| item.command)
                .collect::<Vec<_>>()
        };
        assert!(!commands(Palette::new(&app)).contains(&ManageCommand::Profile));
        assert!(!commands(Palette::new(&app)).contains(&ManageCommand::Recommendations));
        app.profile_error = Some("unreadable workspace profile".to_string());
        assert!(commands(Palette::new(&app)).contains(&ManageCommand::Profile));
    }

    #[test]
    fn palette_consumes_input_and_never_runs_a_hidden_or_missing_result() {
        let mut palette = Palette::default();
        type_query(&mut palette, "xx");
        assert_eq!(
            palette.handle_key(key(KeyCode::Enter), 40, 10),
            PaletteAction::Continue
        );
        assert_eq!(
            palette.handle_key(key(KeyCode::Esc), 40, 10),
            PaletteAction::Close
        );
        let mut palette = Palette::default();
        assert_eq!(
            palette.handle_key(key(KeyCode::Enter), 40, 4),
            PaletteAction::Continue
        );
        assert_eq!(
            palette.handle_key(key(KeyCode::Enter), 1, 10),
            PaletteAction::Continue
        );
        let open = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(is_open_key(open));
        assert_eq!(palette.handle_key(open, 40, 10), PaletteAction::Close);
        assert!(!is_open_key(key(KeyCode::Char('p'))));
    }

    #[test]
    fn original_shortcuts_and_command_dispatch_share_types() {
        assert_eq!(
            ManageCommand::for_key(key(KeyCode::Char('/'))),
            Some(ManageCommand::Browse)
        );
        assert_eq!(
            ManageCommand::for_key(key(KeyCode::Right)),
            Some(ManageCommand::Detail)
        );
        assert_eq!(
            ManageCommand::for_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            Some(ManageCommand::Logs)
        );
        assert_eq!(
            ManageCommand::for_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(ManageCommand::Refresh)
        );
        assert_eq!(ManageCommand::for_key(key(KeyCode::Char('r'))), None);
        assert_eq!(
            ManageCommand::for_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT)),
            None
        );
        assert_eq!(ManageCommand::for_key(key(KeyCode::Char('x'))), None);
    }

    #[test]
    fn compact_draw_reveals_selected_row_and_keeps_host_rows_clear() {
        let mut palette = context(true, true);
        palette.handle_key(key(KeyCode::End), 40, 8);
        let mut bytes = Vec::new();
        palette.draw(&mut bytes, 32, 8, Theme::default()).unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("> Show keyboard help"));
        assert!(output.contains("\x1b[6;1H"));
        assert!(!output.contains("\x1b[7;1H"));
        assert!(!output.contains("\r\n"));
    }
}
