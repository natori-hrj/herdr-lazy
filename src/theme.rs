use std::fmt;

#[derive(Clone, Copy)]
pub(crate) enum Role {
    Danger,
    Success,
    Warning,
    Info,
    Local,
    Accent,
}

impl Role {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "danger" => Some(Self::Danger),
            "success" => Some(Self::Success),
            "warning" => Some(Self::Warning),
            "info" => Some(Self::Info),
            "local" => Some(Self::Local),
            "accent" => Some(Self::Accent),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Color {
    Plain,
    Ansi(u8),
    Rgb(u32),
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Plain => Ok(()),
            Self::Ansi(code) => write!(f, "\x1b[{code}m"),
            Self::Rgb(rgb) => write!(
                f,
                "\x1b[38;2;{};{};{}m",
                rgb >> 16,
                (rgb >> 8) & 0xff,
                rgb & 0xff
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Theme([Color; 6]);

impl Default for Theme {
    fn default() -> Self {
        Self([31, 32, 33, 34, 35, 36].map(Color::Ansi))
    }
}

impl Theme {
    pub(crate) fn configured() -> (Self, Option<&'static str>) {
        match std::env::var("HERDR_LAZY_THEME") {
            Ok(value) => match Self::parse(&value) {
                Ok(theme) => (theme, None),
                Err(error) => (Self::default(), Some(error)),
            },
            Err(std::env::VarError::NotPresent) => (Self::default(), None),
            Err(std::env::VarError::NotUnicode(_)) => {
                (Self::default(), Some("theme setting must be Unicode"))
            }
        }
    }

    pub(crate) fn color(self, role: Role) -> Color {
        self.0[role as usize]
    }

    fn parse(value: &str) -> Result<Self, &'static str> {
        let value = value.trim().to_ascii_lowercase();
        // Order follows Role. Preserve the terminal background and primary text in every theme.
        let preset = match value.as_str() {
            "" | "terminal" | "default" => return Ok(Self::default()),
            "catppuccin" | "catppuccin-mocha" => {
                [0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xcba6f7, 0x94e2d5]
            }
            "tokyo-night" => [0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff],
            "gruvbox" => [0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c],
            "nord" => [0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0],
            _ => return Self::custom(&value),
        };
        Ok(Self(preset.map(Color::Rgb)))
    }

    fn custom(value: &str) -> Result<Self, &'static str> {
        let mut theme = Self::default();
        let mut seen = [false; 6];
        for entry in value.split(',') {
            let (name, hex) = entry
                .split_once('=')
                .ok_or("expected a theme name or role=#RRGGBB")?;
            let role = Role::from_name(name.trim()).ok_or("unknown theme color role")?;
            let rgb = hex
                .trim()
                .strip_prefix('#')
                .filter(|hex| hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .ok_or("theme colors must use #RRGGBB")?;
            let rgb = u32::from_str_radix(rgb, 16).map_err(|_| "theme colors must use #RRGGBB")?;
            let index = role as usize;
            if seen[index] {
                return Err("theme color role appears twice");
            }
            seen[index] = true;
            theme.0[index] = Color::Rgb(rgb);
        }
        Ok(theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_default_preserves_ansi_and_presets_emit_truecolor() {
        assert_eq!(Theme::parse("terminal").unwrap(), Theme::default());
        assert_eq!(
            Theme::default().color(Role::Warning).to_string(),
            "\x1b[33m"
        );
        for name in ["catppuccin", "tokyo-night", "gruvbox", "nord"] {
            let theme = Theme::parse(name).unwrap();
            assert!(theme.0.iter().all(|color| matches!(color, Color::Rgb(_))));
        }
    }

    #[test]
    fn custom_roles_override_only_selected_colors() {
        let theme = Theme::parse("success=#Aa0102, accent = #0312ff").unwrap();
        assert_eq!(theme.color(Role::Success).to_string(), "\x1b[38;2;170;1;2m");
        assert_eq!(theme.color(Role::Accent), Color::Rgb(0x0312ff));
        assert_eq!(theme.color(Role::Info), Color::Ansi(34));
    }

    #[test]
    fn custom_palette_rejects_malformed_duplicate_and_control_values() {
        for value in [
            "unknown",
            "foo=#000001",
            "warning=#002ce",
            "warning=#1000000",
            "warning=#00000g",
            "accent=#000034,accent=#001234",
            "accent=#001234,",
            "warning=\x1b[33m",
        ] {
            assert!(Theme::parse(value).is_err(), "accepted {value:?}");
        }
    }
}
