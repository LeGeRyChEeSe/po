//! Color themes of the terminal interface (`/theme`).
//!
//! Every color of the interface goes through [`crate::view::Palette`], by one of a few named
//! roles (the accent is cyan, a warning yellow, success green, an error red, a remark dark
//! gray…). A theme gives each of those its own color; "Po" keeps the terminal's own, which
//! follow the terminal's palette. On a terminal without true color, a theme falls back to the
//! named colors nearest to its own.
//!
//! The phone's themes are its own (in its settings): the two never touch each other.

use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// The terminal's own colors.
    #[default]
    Po,
    /// Blues and turquoise.
    Ocean,
    /// Greens and wood.
    Forest,
    /// Pinks and violets, an evening sky.
    Dusk,
    /// Warm sand and terracotta.
    Desert,
    /// Strong colors, for a bright room or tired eyes.
    Contrast,
    /// Grays only: what matters stands out by its brightness.
    Mono,
}

/// The theme in force; `u8::MAX` until read from the preferences.
static THEME: AtomicU8 = AtomicU8::new(u8::MAX);

impl Theme {
    pub const ALL: [Self; 7] = [Self::Po, Self::Ocean, Self::Forest, Self::Dusk, Self::Desert, Self::Contrast, Self::Mono];

    /// Stored name, the same in every language.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Po => "po",
            Self::Ocean => "ocean",
            Self::Forest => "forest",
            Self::Dusk => "dusk",
            Self::Desert => "desert",
            Self::Contrast => "contrast",
            Self::Mono => "mono",
        }
    }

    /// Accepts the stored names and their French equivalents.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "po" | "defaut" | "défaut" | "default" => Some(Self::Po),
            "ocean" | "océan" => Some(Self::Ocean),
            "forest" | "foret" | "forêt" => Some(Self::Forest),
            "dusk" | "crepuscule" | "crépuscule" => Some(Self::Dusk),
            "desert" | "désert" => Some(Self::Desert),
            "contrast" | "contraste" => Some(Self::Contrast),
            "mono" | "monochrome" => Some(Self::Mono),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Po => po_i18n::t!("view.theme.po"),
            Self::Ocean => po_i18n::t!("view.theme.ocean"),
            Self::Forest => po_i18n::t!("view.theme.forest"),
            Self::Dusk => po_i18n::t!("view.theme.dusk"),
            Self::Desert => po_i18n::t!("view.theme.desert"),
            Self::Contrast => po_i18n::t!("view.theme.contrast"),
            Self::Mono => po_i18n::t!("view.theme.mono"),
        }
    }

    pub fn current() -> Self {
        let stored = THEME.load(Ordering::Relaxed);
        if let Some(theme) = Self::ALL.get(stored as usize) {
            return *theme;
        }
        let theme = po_i18n::prefs::get_str("theme").and_then(|v| Self::parse(&v)).unwrap_or_default();
        THEME.store(theme as u8, Ordering::Relaxed);
        theme
    }

    /// Applies the theme right away (the next frame is drawn with it) and keeps it.
    pub fn set(self) -> std::io::Result<()> {
        THEME.store(self as u8, Ordering::Relaxed);
        po_i18n::prefs::set_str("theme", self.as_str())
    }

    /// The color a role takes in this theme: true color when the terminal has it, else the
    /// nearest named color.
    pub fn color(self, role: Color, truecolor: bool) -> Color {
        let Some((rgb, named)) = self.pick(role) else { return role };
        if truecolor { Color::Rgb(rgb.0, rgb.1, rgb.2) } else { named }
    }

    /// For each role the interface uses, this theme's color and its named fallback.
    fn pick(self, role: Color) -> Option<((u8, u8, u8), Color)> {
        use Color as C;
        let table: &[(Color, (u8, u8, u8), Color)] = match self {
            Self::Po => return None,
            Self::Ocean => &[
                (C::Cyan, (0x4f, 0xc3, 0xf7), C::LightCyan),
                (C::Yellow, (0xff, 0xd5, 0x4f), C::Yellow),
                (C::Green, (0x4d, 0xd0, 0xc1), C::Cyan),
                (C::Red, (0xff, 0x8a, 0x80), C::LightRed),
                (C::LightRed, (0xff, 0xab, 0xa4), C::LightRed),
                (C::Magenta, (0x9f, 0xa8, 0xff), C::LightBlue),
                (C::Blue, (0x64, 0x9c, 0xff), C::Blue),
                (C::DarkGray, (0x6b, 0x84, 0x9c), C::DarkGray),
                (C::Gray, (0xa9, 0xc1, 0xd6), C::Gray),
            ],
            Self::Forest => &[
                (C::Cyan, (0x8b, 0xc3, 0x4a), C::LightGreen),
                (C::Yellow, (0xe6, 0xc0, 0x6b), C::Yellow),
                (C::Green, (0x66, 0xbb, 0x6a), C::Green),
                (C::Red, (0xe5, 0x73, 0x73), C::Red),
                (C::LightRed, (0xef, 0x9a, 0x9a), C::LightRed),
                (C::Magenta, (0xc5, 0xa0, 0x6e), C::Yellow),
                (C::Blue, (0x7c, 0xb3, 0x8f), C::Green),
                (C::DarkGray, (0x7a, 0x86, 0x6c), C::DarkGray),
                (C::Gray, (0xb8, 0xc2, 0xa8), C::Gray),
            ],
            Self::Dusk => &[
                (C::Cyan, (0xf4, 0x8f, 0xb1), C::LightMagenta),
                (C::Yellow, (0xff, 0xb7, 0x4d), C::Yellow),
                (C::Green, (0xb3, 0x9d, 0xdb), C::LightBlue),
                (C::Red, (0xff, 0x6e, 0x6e), C::LightRed),
                (C::LightRed, (0xff, 0x9e, 0x9e), C::LightRed),
                (C::Magenta, (0xce, 0x93, 0xd8), C::Magenta),
                (C::Blue, (0x90, 0x8c, 0xe8), C::Blue),
                (C::DarkGray, (0x8a, 0x7a, 0x96), C::DarkGray),
                (C::Gray, (0xc8, 0xb8, 0xd4), C::Gray),
            ],
            Self::Desert => &[
                (C::Cyan, (0xe0, 0x8e, 0x5a), C::LightRed),
                (C::Yellow, (0xf2, 0xc1, 0x4e), C::Yellow),
                (C::Green, (0xa8, 0xb5, 0x6b), C::Green),
                (C::Red, (0xd9, 0x5d, 0x4a), C::Red),
                (C::LightRed, (0xe8, 0x8b, 0x7d), C::LightRed),
                (C::Magenta, (0xc9, 0x86, 0x7a), C::Magenta),
                (C::Blue, (0x8f, 0xa9, 0xb8), C::Blue),
                (C::DarkGray, (0x94, 0x82, 0x6e), C::DarkGray),
                (C::Gray, (0xd4, 0xc4, 0xa8), C::Gray),
            ],
            Self::Contrast => &[
                (C::Cyan, (0x00, 0xe5, 0xff), C::LightCyan),
                (C::Yellow, (0xff, 0xea, 0x00), C::LightYellow),
                (C::Green, (0x00, 0xe6, 0x76), C::LightGreen),
                (C::Red, (0xff, 0x17, 0x44), C::LightRed),
                (C::LightRed, (0xff, 0x52, 0x52), C::LightRed),
                (C::Magenta, (0xff, 0x40, 0xff), C::LightMagenta),
                (C::Blue, (0x44, 0x8a, 0xff), C::LightBlue),
                (C::DarkGray, (0xb0, 0xb0, 0xb0), C::Gray),
                (C::Gray, (0xe0, 0xe0, 0xe0), C::White),
            ],
            Self::Mono => &[
                (C::Cyan, (0xf0, 0xf0, 0xf0), C::White),
                (C::Yellow, (0xd8, 0xd8, 0xd8), C::White),
                (C::Green, (0xc8, 0xc8, 0xc8), C::Gray),
                (C::Red, (0xff, 0xff, 0xff), C::White),
                (C::LightRed, (0xe8, 0xe8, 0xe8), C::White),
                (C::Magenta, (0xc0, 0xc0, 0xc0), C::Gray),
                (C::Blue, (0xb8, 0xb8, 0xb8), C::Gray),
                (C::DarkGray, (0x80, 0x80, 0x80), C::DarkGray),
                (C::Gray, (0xb0, 0xb0, 0xb0), C::Gray),
            ],
        };
        table.iter().find(|(r, ..)| *r == role).map(|(_, rgb, named)| (*rgb, *named))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_recolors_the_roles_and_po_keeps_the_terminals_own() {
        assert_eq!(Theme::Po.color(Color::Cyan, true), Color::Cyan);
        assert_eq!(Theme::Ocean.color(Color::Cyan, true), Color::Rgb(0x4f, 0xc3, 0xf7));
        assert_eq!(Theme::Ocean.color(Color::Cyan, false), Color::LightCyan, "without true color, the nearest named color");
        // A color no role uses is left alone (the mascot's own, a Reset).
        assert_eq!(Theme::Mono.color(Color::Reset, true), Color::Reset);
        for theme in Theme::ALL {
            assert_eq!(Theme::parse(theme.as_str()), Some(theme));
        }
        assert_eq!(Theme::parse("Forêt"), Some(Theme::Forest));
    }
}
