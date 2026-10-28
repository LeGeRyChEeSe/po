//! Render profiles. The harness knows only three ways of displaying itself, and it can step
//! down from one to the next without ever climbing back up on its own during a session.

use std::env;
use std::fmt;

use crate::caps::{ColorDepth, TermCaps, UnicodeLevel};
use crate::env::{Platform, Transport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RenderProfile {
    /// No alternate screen: output is written as a stream, line by line, and survives a
    /// `| less`, a CI, a terminal that can do nothing. It is the last-resort mode, and the only
    /// one that cannot corrupt the display.
    Stream,
    /// Sober full screen: 16 colours, ASCII frames, no mouse, few bytes per frame. Targets slow
    /// SSH sessions, temperamental multiplexers and narrow screens.
    Plain,
    /// Full-featured full screen: true colour, mouse, hyperlinks, synchronized refresh.
    Rich,
}

impl fmt::Display for RenderProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Stream => "stream",
            Self::Plain => "plain",
            Self::Rich => "rich",
        };
        f.write_str(name)
    }
}

impl std::str::FromStr for RenderProfile {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stream" => Ok(Self::Stream),
            "plain" => Ok(Self::Plain),
            "rich" => Ok(Self::Rich),
            other => Err(po_i18n::t!("term.profile.unknown", value = other)),
        }
    }
}

/// Width below which even the block layout no longer holds. Blocks wrap to the width and the
/// settings panel goes full screen: a phone in portrait (40 to 50 columns under Termius) thus
/// keeps full screen and finger scrolling, which line mode cannot offer.
const NARROW_COLUMNS: u16 = 30;
/// Height below which full screen is pointless (soft keyboard open).
const SHORT_ROWS: u16 = 12;

impl RenderProfile {
    /// Profile chosen automatically, unless explicitly requested via `--render` or `PO_RENDER`.
    pub fn resolve(caps: &TermCaps, forced: Option<RenderProfile>) -> Self {
        if let Some(profile) = forced.or_else(env_override) {
            return profile;
        }
        Self::from_caps(caps)
    }

    /// Pure derivation from capabilities, with no environment reading: this is the testable rule.
    pub fn from_caps(caps: &TermCaps) -> Self {
        // No terminal, or a terminal that cannot switch screens: we stay in stream mode.
        if !caps.is_tty || !caps.alt_screen || caps.host.term_is_hostile {
            return Self::Stream;
        }

        let (cols, rows) = caps.size;
        if cols < NARROW_COLUMNS || rows < SHORT_ROWS {
            return Self::Plain;
        }

        // On a remote link, an unsynchronized full-screen refresh visibly tears. Without DECSET
        // 2026 to bracket it, we prefer sober rendering to one that flickers.
        if caps.host.transport == Transport::Ssh && !caps.sync_output {
            return Self::Plain;
        }

        // Phone screen: full screen works, but not the frills.
        if caps.host.platform == Platform::Termux {
            return Self::Plain;
        }

        if caps.color >= ColorDepth::Ansi256 && caps.unicode >= UnicodeLevel::Bmp {
            return Self::Rich;
        }
        Self::Plain
    }

    /// The notch below, used when the current rendering proves untenable in use.
    pub fn degrade(self) -> Self {
        match self {
            Self::Rich => Self::Plain,
            Self::Plain | Self::Stream => Self::Stream,
        }
    }

    pub fn uses_alt_screen(self) -> bool {
        self != Self::Stream
    }

    pub fn allows_mouse(self) -> bool {
        self == Self::Rich
    }

    /// Suitable frame characters: Unicode boxes only go through in rich rendering.
    pub fn box_chars(self, caps: &TermCaps) -> BoxChars {
        if self == Self::Rich && caps.unicode >= UnicodeLevel::Bmp {
            BoxChars::UNICODE
        } else {
            BoxChars::ASCII
        }
    }
}

fn env_override() -> Option<RenderProfile> {
    env::var("PO_RENDER").ok()?.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxChars {
    pub horizontal: char,
    pub vertical: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
}

impl BoxChars {
    pub const UNICODE: Self = Self {
        horizontal: '─',
        vertical: '│',
        top_left: '╭',
        top_right: '╮',
        bottom_left: '╰',
        bottom_right: '╯',
    };

    pub const ASCII: Self = Self {
        horizontal: '-',
        vertical: '|',
        top_left: '+',
        top_right: '+',
        bottom_left: '+',
        bottom_right: '+',
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{TermHost, Transport};

    fn caps(platform: Platform, transport: Transport, size: (u16, u16), sync: bool) -> TermCaps {
        TermCaps {
            host: TermHost {
                term: "xterm-256color".into(),
                multiplexer: None,
                transport,
                platform,
                emulator: None,
                term_is_hostile: false,
            },
            color: ColorDepth::TrueColor,
            unicode: UnicodeLevel::Full,
            alt_screen: true,
            mouse: true,
            kitty_keyboard: false,
            sync_output: sync,
            bracketed_paste: true,
            hyperlinks: false,
            size,
            is_tty: true,
            probed: true,
        }
    }

    #[test]
    fn a_comfortable_terminal_gives_rich() {
        let caps = caps(Platform::Linux, Transport::Local, (120, 40), true);
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Rich);
    }

    #[test]
    fn ssh_without_sync_avoids_tearing() {
        let caps = caps(Platform::Linux, Transport::Ssh, (120, 40), false);
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Plain);
    }

    #[test]
    fn ssh_with_sync_keeps_rich_rendering() {
        let caps = caps(Platform::Linux, Transport::Ssh, (120, 40), true);
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Rich);
    }

    #[test]
    fn a_narrow_or_short_screen_goes_plain() {
        assert_eq!(
            RenderProfile::from_caps(&caps(Platform::Linux, Transport::Local, (24, 40), true)),
            RenderProfile::Plain
        );
        // A phone in portrait keeps full screen: blocks wrap to the width.
        assert_eq!(
            RenderProfile::from_caps(&caps(Platform::Linux, Transport::Local, (40, 40), true)),
            RenderProfile::Rich
        );
        assert_eq!(
            RenderProfile::from_caps(&caps(Platform::Linux, Transport::Local, (120, 8), true)),
            RenderProfile::Plain
        );
    }

    #[test]
    fn local_macos_gets_full_rendering() {
        let caps = caps(Platform::MacOs, Transport::Local, (120, 40), true);
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Rich);
    }

    #[test]
    fn macos_over_ssh_follows_the_same_rule_as_linux() {
        // Connecting from a Mac to a remote machine must not change the rule: without
        // synchronized output, we avoid a torn full screen.
        let caps = caps(Platform::MacOs, Transport::Ssh, (120, 40), false);
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Plain);
    }

    #[test]
    fn termux_stays_sober() {
        let caps = caps(Platform::Termux, Transport::Local, (100, 30), true);
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Plain);
    }

    #[test]
    fn without_tty_we_write_a_stream() {
        let mut caps = caps(Platform::Linux, Transport::Local, (120, 40), true);
        caps.is_tty = false;
        caps.alt_screen = false;
        assert_eq!(RenderProfile::from_caps(&caps), RenderProfile::Stream);
    }

    #[test]
    fn degradation_is_monotonic() {
        assert_eq!(RenderProfile::Rich.degrade(), RenderProfile::Plain);
        assert_eq!(RenderProfile::Plain.degrade(), RenderProfile::Stream);
        assert_eq!(RenderProfile::Stream.degrade(), RenderProfile::Stream);
    }

    #[test]
    fn the_profile_parses_and_displays() {
        for profile in [RenderProfile::Rich, RenderProfile::Plain, RenderProfile::Stream] {
            assert_eq!(profile.to_string().parse::<RenderProfile>(), Ok(profile));
        }
        assert!("turbo".parse::<RenderProfile>().is_err());
    }
}
