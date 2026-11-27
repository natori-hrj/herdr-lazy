use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

pub(crate) const ENV_KEY: &str = "HERDR_LAZY_THEME";
pub(crate) const INPUT_LIMIT: usize = 512;

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
        Self::ALL
            .iter()
            .find_map(|(role, id)| (*id == name).then_some(*role))
    }

    const ALL: [(Self, &'static str); 6] = [
        (Self::Danger, "danger"),
        (Self::Success, "success"),
        (Self::Warning, "warning"),
        (Self::Info, "info"),
        (Self::Local, "local"),
        (Self::Accent, "accent"),
    ];
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

#[derive(Clone, Copy)]
pub(crate) enum Selection {
    Ansi(u8),
    Rgb(u32),
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Ansi(code) => write!(f, "\x1b[{code}m"),
            Self::Rgb(rgb) => write!(
                f,
                "\x1b[48;2;{};{};{}m",
                rgb >> 16,
                (rgb >> 8) & 0xff,
                rgb & 0xff
            ),
        }
    }
}

impl Selection {
    pub(crate) fn foreground(self) -> Color {
        match self {
            Self::Ansi(_) => Color::Ansi(39),
            Self::Rgb(rgb) if luminance(rgb) >= 0.179 => Color::Rgb(0x000000),
            Self::Rgb(_) => Color::Rgb(0xffffff),
        }
    }

    pub(crate) fn status_color(self, color: Color) -> Color {
        if let (Self::Rgb(background), Color::Rgb(foreground)) = (self, color) {
            let background = luminance(background);
            let foreground = luminance(foreground);
            let contrast =
                (background.max(foreground) + 0.05) / (background.min(foreground) + 0.05);
            if contrast < 4.5 {
                return self.foreground();
            }
        }
        color
    }
}

fn luminance(rgb: u32) -> f64 {
    let linear = |shift| {
        let value = f64::from((rgb >> shift) & 0xff_u32) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Theme {
    colors: [Color; 6],
    selection_background: Option<u32>,
    custom: bool,
}

impl Default for Theme {
    fn default() -> Self {
        PRESETS[0].palette
    }
}

pub(crate) struct Preset {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) palette: Theme,
}

const fn rgb(colors: [u32; 6], selection_background: u32) -> Theme {
    let [danger, success, warning, info, local, accent] = colors;
    Theme {
        colors: [
            Color::Rgb(danger),
            Color::Rgb(success),
            Color::Rgb(warning),
            Color::Rgb(info),
            Color::Rgb(local),
            Color::Rgb(accent),
        ],
        selection_background: Some(selection_background),
        custom: false,
    }
}

pub(crate) const PRESETS: [Preset; 5] = [
    Preset {
        id: "terminal",
        label: "Terminal default",
        palette: Theme {
            colors: [
                Color::Ansi(31),
                Color::Ansi(32),
                Color::Ansi(33),
                Color::Ansi(34),
                Color::Ansi(35),
                Color::Ansi(36),
            ],
            selection_background: None,
            custom: false,
        },
    },
    Preset {
        id: "catppuccin",
        label: "Catppuccin Mocha",
        palette: rgb(
            [0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xcba6f7, 0x94e2d5],
            0x313244,
        ),
    },
    Preset {
        id: "tokyo-night",
        label: "Tokyo Night",
        palette: rgb(
            [0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff],
            0x292e42,
        ),
    },
    Preset {
        id: "gruvbox",
        label: "Gruvbox",
        palette: rgb(
            [0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c],
            0x3c3836,
        ),
    },
    Preset {
        id: "nord",
        label: "Nord",
        palette: rgb(
            [0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0],
            0x242932,
        ),
    },
];

impl Theme {
    pub(crate) fn configured() -> (Self, Option<&'static str>) {
        resolve(std::env::var(ENV_KEY), || {
            load_saved(&crate::config_dir().join("theme"))
        })
    }

    pub(crate) fn color(self, role: Role) -> Color {
        self.colors[role as usize]
    }

    pub(crate) fn selection(self) -> Selection {
        if let Some(background) = self.selection_background {
            return Selection::Rgb(background);
        }
        let colors = std::env::var("COLORFGBG").unwrap_or_default();
        match (self.custom, colors.rsplit_once(';')) {
            (true, Some((_, "7" | "15"))) => Selection::Rgb(0xeeeeee),
            (true, _) => Selection::Rgb(0x303030),
            (false, Some((_, "7"))) => Selection::Ansi(107),
            (false, Some((_, "15"))) => Selection::Ansi(47),
            (false, _) => Selection::Ansi(100),
        }
    }

    pub(crate) fn is_custom(self) -> bool {
        self.custom
    }

    pub(crate) fn custom_value(self) -> String {
        Role::ALL
            .iter()
            .filter_map(|(role, name)| match self.color(*role) {
                Color::Rgb(color) => Some(format!("{name}=#{color:06x}")),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    fn parse(value: &str) -> Result<Self, &'static str> {
        let value = value.trim().to_ascii_lowercase();
        let id = match value.as_str() {
            "" | "default" => "terminal",
            "catppuccin-mocha" => "catppuccin",
            name => name,
        };
        match PRESETS.iter().find(|preset| preset.id == id) {
            Some(preset) => Ok(preset.palette),
            None => Self::custom(id),
        }
    }

    pub(crate) fn parse_custom(value: &str) -> Result<Self, &'static str> {
        Self::custom(&value.trim().to_ascii_lowercase())
    }

    fn custom(value: &str) -> Result<Self, &'static str> {
        let mut theme = Self {
            custom: true,
            ..Self::default()
        };
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
            theme.colors[index] = Color::Rgb(rgb);
        }
        Ok(theme)
    }
}

fn resolve(
    environment: Result<String, std::env::VarError>,
    saved: impl FnOnce() -> Result<Theme, &'static str>,
) -> (Theme, Option<&'static str>) {
    let result = match environment {
        Ok(value) => Theme::parse(&value),
        Err(std::env::VarError::NotPresent) => saved(),
        Err(std::env::VarError::NotUnicode(_)) => Err("theme setting must be Unicode"),
    };
    match result {
        Ok(theme) => (theme, None),
        Err(error) => (Theme::default(), Some(error)),
    }
}

fn load_saved(path: &Path) -> Result<Theme, &'static str> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Err("theme preference is not a regular file"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Theme::default()),
        Err(_) => return Err("could not read the saved theme"),
    }
    let file = File::open(path).map_err(|_| "could not read the saved theme")?;
    let mut value = String::new();
    file.take(INPUT_LIMIT as u64 + 2)
        .read_to_string(&mut value)
        .map_err(|_| "could not read the saved theme")?;
    if value.len() > INPUT_LIMIT + 1 {
        return Err("saved theme exceeds the size limit");
    }
    Theme::parse(&value)
}

pub(crate) fn save(value: &str) -> io::Result<()> {
    save_to(&crate::config_dir().join("theme"), value)
}

fn save_to(path: &Path, value: &str) -> io::Result<()> {
    if value.len() > INPUT_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "theme exceeds the size limit",
        ));
    }
    Theme::parse(value).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    crate::ensure_parent(path)?;
    crate::write_bytes_atomically(path, format!("{}\n", value.trim()).as_bytes())
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
            assert!(theme
                .colors
                .iter()
                .all(|color| matches!(color, Color::Rgb(_))));
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
    fn saved_theme_loads_only_without_an_environment_setting() {
        let saved = || Ok(PRESETS[4].palette);
        assert_eq!(
            resolve(Err(std::env::VarError::NotPresent), saved),
            (PRESETS[4].palette, None)
        );
        assert_eq!(
            resolve(Ok("gruvbox".to_string()), || panic!(
                "environment takes precedence"
            )),
            (PRESETS[3].palette, None)
        );
        let (fallback, error) = resolve(Ok("bad value".to_string()), || {
            panic!("environment takes precedence")
        });
        assert_eq!(fallback, Theme::default());
        assert!(error.is_some());
    }

    #[test]
    fn a_valid_custom_setting_round_trips_and_invalid_saves_leave_it_intact() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("herdr-lazy-theme-{}-{unique}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("theme");
        assert_eq!(load_saved(&path), Ok(Theme::default()));

        let theme = Theme::parse_custom("success=#80c7a2 , warning=#FFFFFF").unwrap();
        assert!(theme.is_custom());
        save_to(&path, &theme.custom_value()).unwrap();
        assert_eq!(load_saved(&path), Ok(theme));
        assert!(save_to(&path, "warning=invalid").is_err());
        assert_eq!(load_saved(&path), Ok(theme));

        save_to(&path, "nord").unwrap();
        assert_eq!(load_saved(&path), Ok(PRESETS[4].palette));
        fs::write(&path, " ".repeat(INPUT_LIMIT + 2)).unwrap();
        assert_eq!(load_saved(&path), Err("saved theme exceeds the size limit"));
        assert_eq!(
            load_saved(&directory),
            Err("theme preference is not a regular file")
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
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
