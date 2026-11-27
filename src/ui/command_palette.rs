use std::io::{self, Write};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    footer_start_row,
    terminal_text::{clip, clip_tail, display_width},
    App,
};
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
        let last_row = last_content_row(height);
        if columns == 0 || last_row == 0 {
            return out.flush();
        }
        let (title, _) = clip(" Commands", columns);
        write!(out, "\x1b[1m{title}\x1b[0m")?;
        if last_row < 2 {
            return out.flush();
        }
        let query = query_line(&self.query, columns.saturating_sub(1));
        write!(out, "\x1b[2;1H{query}\x1b[7m \x1b[0m")?;
        if last_row < 3 {
            return out.flush();
        }

        let results = self.results();
        let visible = visible_rows(height);
        let start = self.cursor.saturating_sub(visible.saturating_sub(1));
        let mut row = 3;
        if visible > 0 && results.is_empty() {
            let (empty, _) = clip(" No matching commands", columns);
            write!(out, "\x1b[{row};1H{empty}")?;
            row += 1;
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
            write!(out, "\x1b[{row};1H")?;
            if selected {
                write!(out, "\x1b[1m{}", theme.color(Role::Accent))?;
            }
            write!(out, "{text}\x1b[0m")?;
            row += 1;
        }
        let hints: &[&str] = match (visible > 0 && width >= 6, results.len()) {
            (false, _) | (_, 0) => &[" Esc Back"],
            (true, 1) => &[" Enter Open · Esc Back", " Enter · Esc", "Enter Esc"],
            _ => &[
                " ↑↓ Select · Enter Open · Esc Back",
                " ↑↓ · Enter · Esc",
                "Enter Esc",
            ],
        };
        let hint = hints
            .iter()
            .copied()
            .find(|hint| display_width(hint) <= columns)
            .unwrap_or("Esc");
        let (hint, _) = clip(hint, columns);
        write!(out, "\x1b[{row};1H\x1b[2m{hint}\x1b[0m")?;
        out.flush()
    }
}

fn query_line(query: &str, columns: usize) -> String {
    const PROMPT: &str = " > ";
    let available = columns.saturating_sub(PROMPT.len());
    if available == 0 {
        return clip(&format!("{PROMPT}{query}"), columns).0;
    }
    format!("{PROMPT}{}", clip_tail(query, available))
}

fn last_content_row(height: u16) -> usize {
    usize::from(footer_start_row(height)) + usize::from(height >= 3)
}

fn visible_rows(height: u16) -> usize {
    last_content_row(height).saturating_sub(3)
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

    fn render(palette: &Palette, width: u16, height: u16) -> String {
        let mut bytes = Vec::new();
        palette
            .draw(&mut bytes, width, height, Theme::default())
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn hints_follow_the_visible_results_at_short_and_tall_heights() {
        let mut palette = Palette::default();
        let initial = render(&palette, 80, 24);
        assert!(initial.contains("> Browse Marketplace"));
        assert!(initial.contains("\x1b[10;1H\x1b[2m ↑↓ Select · Enter Open · Esc Back"));
        let short = render(&palette, 80, 6);
        assert!(short.contains("> Browse Marketplace"));
        assert!(!short.contains("Browse optional bundles"));
        assert!(short.contains("\x1b[4;1H\x1b[2m ↑↓ Select · Enter Open · Esc Back"));

        type_query(&mut palette, "Brws MKT");
        for height in [6, 24] {
            let filtered = render(&palette, 80, height);
            assert!(filtered.contains(" > Brws MKT"));
            assert!(filtered.contains("> Browse Marketplace"));
            assert!(!filtered.contains("Show keyboard help"));
            assert!(filtered.contains("\x1b[4;1H\x1b[2m Enter Open · Esc Back"));
        }

        type_query(&mut palette, " zzzz");
        for height in [6, 24] {
            let empty = render(&palette, 80, height);
            assert!(empty.contains("\x1b[3;1H No matching commands"));
            assert!(empty.contains("\x1b[4;1H\x1b[2m Esc Back"));
        }
    }

    #[test]
    fn narrow_draw_keeps_the_open_and_back_hints_visible() {
        let output = render(&Palette::default(), 24, 8);
        assert!(output.contains(" ↑↓ · Enter · Esc"));
    }

    #[test]
    fn compact_query_draws_with_only_two_content_rows() {
        let mut palette = Palette::default();
        type_query(&mut palette, "browse");
        let output = render(&palette, 40, 4);
        assert!(output.contains(" Commands"));
        assert!(output.contains("\x1b[2;1H > browse\x1b[7m \x1b[0m"));
        assert!(!output.contains("\x1b[3;1H"));
        assert!(!output.contains("\x1b[4;1H"));
        assert_eq!(
            palette.handle_key(key(KeyCode::Enter), 40, 4),
            PaletteAction::Continue
        );
    }

    #[test]
    fn compact_query_keeps_its_editing_end_visible() {
        let query = "unmatched-query-with-a-long-prefix-followed-by-the-tail";
        for (width, height) in [(24, 8), (40, 9)] {
            let mut palette = Palette::default();
            type_query(&mut palette, query);
            let tail_cells = usize::from(width) - 6;
            for end in [query.len(), query.len() - 1] {
                let tail = &query[end - tail_cells..end];
                let output = render(&palette, width, height);
                assert!(
                    output.contains(&format!("\x1b[2;1H > …{tail}\x1b[7m \x1b[0m")),
                    "{width}x{height} query row: {output:?}"
                );
                assert!(output.contains("No matching commands"));
                palette.handle_key(key(KeyCode::Backspace), width, height);
            }
        }
    }

    #[test]
    fn palette_query_keeps_graphemes_and_contextual_width_before_the_caret() {
        for (query, width, expected) in [
            ("xy👩‍💻", 8, "…👩‍💻"),
            ("xy👩‍💻", 7, "…"),
            ("xye\u{301}", 7, "…e\u{301}"),
            ("xxकक्क", 8, "…क्क"),
            ("لا", 6, "لا"),
        ] {
            let mut palette = Palette::default();
            type_query(&mut palette, query);
            let frame = render(&palette, width, 8);
            assert!(
                frame.contains(&format!("\x1b[2;1H > {expected}\x1b[7m \x1b[0m")),
                "{query:?}, {width}: {frame:?}"
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
        let output = render(&palette, 32, 8);
        assert!(output.contains("> Show keyboard help"));
        assert!(output.contains("\x1b[6;1H"));
        assert!(!output.contains("\x1b[7;1H"));
        assert!(!output.contains("\r\n"));
    }
}
