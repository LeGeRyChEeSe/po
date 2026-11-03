//! Effective terminal capabilities: a merge of environment hints and, when possible, of the
//! terminal's actual replies. A capability is only enabled if it is **observed** or guaranteed
//! by the environment; doubt always resolves downward.

use std::env;
use std::time::Duration;

use crate::env::{Multiplexer, Platform, TermHost, Transport};
use crate::probe::{self, ProbeReply};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnicodeLevel {
    /// ASCII only: frames and icons in plain characters.
    Ascii,
    /// Unicode without emoji: box-drawing characters go through.
    Bmp,
    /// Emoji and composed graphemes rendered at the right width.
    Full,
}

/// Maximum time granted to the terminal to answer the probe.
/// Deliberately short: beyond that, a slow remote session would penalize startup.
const PROBE_TIMEOUT: Duration = Duration::from_millis(120);
const PROBE_TIMEOUT_SSH: Duration = Duration::from_millis(350);

#[derive(Debug, Clone)]
pub struct TermCaps {
    pub host: TermHost,
    pub color: ColorDepth,
    pub unicode: UnicodeLevel,
    pub alt_screen: bool,
    pub mouse: bool,
    pub kitty_keyboard: bool,
    /// DECSET 2026: brackets a refresh so the terminal displays it in one piece.
    /// It is the direct fix for the text overlaps observed under tmux.
    pub sync_output: bool,
    pub bracketed_paste: bool,
    pub hyperlinks: bool,
    pub size: (u16, u16),
    /// True if the output is a real terminal; false in a pipe, a file, a CI.
    pub is_tty: bool,
    /// True if the capabilities come from an actual reply from the terminal.
    pub probed: bool,
}

impl TermCaps {
    /// Detects everything detectable, probe included if the context allows it.
    pub fn detect() -> Self {
        Self::detect_with(false)
    }

    /// Like [`TermCaps::detect`], but `assume_tty` makes it analyse the terminal **attached to
    /// the process** even if the output is redirected.
    ///
    /// This is essential for a diagnostic script: it necessarily captures the output in a
    /// variable or a pipe, which would leave `doctor` mute about the real capabilities. The
    /// render profile of a real session, for its part, keeps depending on the effective output.
    pub fn detect_with(assume_tty: bool) -> Self {
        let host = TermHost::detect();
        let real_tty = std::io::IsTerminal::is_terminal(&std::io::stdout())
            && std::io::IsTerminal::is_terminal(&std::io::stdin());
        // `assume_tty` is an explicit assertion by the caller, not a heuristic: under Termux,
        // neither `/dev/tty` nor `stderr` allows checking for a terminal when the output is
        // captured. We therefore trust it, and the probe will report the absence of a reply on
        // its own if the bet was wrong.
        let is_tty = real_tty || assume_tty;

        let reply = if is_tty && !host.term_is_hostile && !no_probe_requested() {
            let timeout = match host.transport {
                Transport::Ssh => PROBE_TIMEOUT_SSH,
                Transport::Local => PROBE_TIMEOUT,
            };
            probe::query(timeout, host.transport == Transport::Ssh)
        } else {
            None
        };

        Self::from_parts(host, is_tty, reply.as_ref())
    }

    /// Construction without I/O **on the terminal**: it carries the decision logic, hence it is
    /// the one that is tested.
    ///
    /// It does, however, consult the process environment (`COLORTERM`, `NO_COLOR`, `LANG`, and
    /// the `PO_*` overrides), which is not a parameter. A test about colour depth or the Unicode
    /// level therefore depends on the machine running it - deterministic tests must stick to
    /// structural decisions.
    pub fn from_parts(host: TermHost, is_tty: bool, reply: Option<&ProbeReply>) -> Self {
        let probed = reply.is_some_and(|r| r.answered);
        let size = terminal_size();

        let color = if !is_tty || host.term_is_hostile {
            ColorDepth::None
        } else {
            color_depth(&host, reply)
        };

        // Without a TTY, no full-screen mode makes sense: the output goes into a pipe.
        let interactive = is_tty && !host.term_is_hostile;

        Self {
            unicode: unicode_level(&host),
            alt_screen: interactive,
            mouse: interactive && mouse_supported(&host),
            // An advanced keyboard capability is not invented: without proof, we stay legacy.
            kitty_keyboard: interactive && reply.is_some_and(ProbeReply::has_kitty_keyboard),
            sync_output: interactive && sync_output_supported(&host, reply),
            bracketed_paste: interactive,
            hyperlinks: interactive && hyperlinks_supported(&host),
            color,
            size,
            is_tty,
            probed,
            host,
        }
    }

    /// Degraded view: what we keep when rich rendering proves untenable.
    pub fn degraded(&self) -> Self {
        Self {
            color: self.color.min(ColorDepth::Ansi16),
            unicode: self.unicode.min(UnicodeLevel::Bmp),
            mouse: false,
            kitty_keyboard: false,
            hyperlinks: false,
            ..self.clone()
        }
    }
}

fn no_probe_requested() -> bool {
    env::var_os("PO_NO_PROBE").is_some() || env::var_os("CI").is_some()
}

fn color_depth(host: &TermHost, reply: Option<&ProbeReply>) -> ColorDepth {
    if let Ok(force) = env::var("PO_COLOR") {
        match force.as_str() {
            "0" | "none" => return ColorDepth::None,
            "16" => return ColorDepth::Ansi16,
            "256" => return ColorDepth::Ansi256,
            "truecolor" | "24bit" => return ColorDepth::TrueColor,
            _ => {}
        }
    }
    // As no-color.org says: present *and* not empty. An empty `NO_COLOR=` cleared colour, and
    // the full-screen interface with it.
    if env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return ColorDepth::None;
    }

    let colorterm = env::var("COLORTERM").unwrap_or_default();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        // tmux only relays 24-bit colour if configured to; without `Tc`/`RGB` declared in its
        // terminfo, colours are crushed. We stay cautious.
        if host.multiplexer == Some(Multiplexer::Tmux) && !tmux_declares_truecolor(host) {
            return ColorDepth::Ansi256;
        }
        return ColorDepth::TrueColor;
    }

    // Windows Terminal and modern ConPTY handle 24-bit colour without announcing it via
    // COLORTERM.
    if host.platform == Platform::Windows && host.emulator.is_some() {
        return ColorDepth::TrueColor;
    }

    if host.term.contains("256") || reply.is_some_and(ProbeReply::has_palette_query) {
        return ColorDepth::Ansi256;
    }
    if host.term.is_empty() {
        return if host.platform == Platform::Windows { ColorDepth::Ansi256 } else { ColorDepth::None };
    }
    ColorDepth::Ansi16
}

fn tmux_declares_truecolor(host: &TermHost) -> bool {
    host.term.contains("direct") || env::var_os("TMUX_TRUECOLOR").is_some()
}

fn unicode_level(host: &TermHost) -> UnicodeLevel {
    if let Ok(force) = env::var("PO_UNICODE") {
        match force.as_str() {
            "ascii" => return UnicodeLevel::Ascii,
            "bmp" => return UnicodeLevel::Bmp,
            "full" => return UnicodeLevel::Full,
            _ => {}
        }
    }
    if host.term_is_hostile {
        return UnicodeLevel::Ascii;
    }

    let utf8 = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| env::var(k).ok())
        .any(|v| v.to_ascii_uppercase().contains("UTF-8") || v.to_ascii_uppercase().contains("UTF8"));

    match host.platform {
        // ConPTY is UTF-16 internally and renders emoji, but the width reported by the console
        // stays inconsistent depending on the font: we avoid betting the layout on it.
        Platform::Windows => UnicodeLevel::Bmp,
        // Termux is UTF-8 by default and renders emoji correctly.
        Platform::Termux => UnicodeLevel::Full,
        _ if utf8 => UnicodeLevel::Full,
        _ => UnicodeLevel::Ascii,
    }
}

fn mouse_supported(host: &TermHost) -> bool {
    match host.platform {
        // Touch screen: mouse tracking produces spurious events while scrolling.
        Platform::Termux => false,
        _ => match host.multiplexer {
            // tmux swallows mouse events if `set -g mouse on` is not active; we therefore only
            // enable it when it announces it.
            Some(Multiplexer::Tmux) => env::var("TMUX_MOUSE").is_ok_and(|v| v == "1"),
            Some(Multiplexer::Screen) => false,
            _ => true,
        },
    }
}

fn sync_output_supported(host: &TermHost, reply: Option<&ProbeReply>) -> bool {
    if reply.is_some_and(ProbeReply::has_sync_output) {
        return true;
    }
    // tmux does not forward the query to the host terminal, so the probe stays unanswered even
    // when the capability exists. tmux 3.3 introduced its own handling of synchronized output:
    // we recognise it by version rather than give up rich rendering for every remote tmux user.
    if host.multiplexer == Some(Multiplexer::Tmux) {
        return matches!(crate::env::tmux_version(), Some((major, minor)) if (major, minor) >= (3, 3));
    }
    // With no probe possible - local Windows console, probe disabled - we rely on known
    // implementations. A probe that answered without announcing 2026 is authoritative: that is
    // the case of Termius, which answers `CSI ?2026;0$y`, i.e. "I do not know this mode".
    if reply.is_none() && host.platform == Platform::Windows {
        return host.emulator.as_deref() == Some("windows-terminal");
    }
    false
}

fn hyperlinks_supported(host: &TermHost) -> bool {
    // OSC 8 crosses multiplexers poorly and is not clickable on mobile.
    if host.multiplexer.is_some() || host.platform == Platform::Termux {
        return false;
    }
    host.emulator.is_some()
}

/// Size of the terminal, without asking it anything.
///
/// Distinct from [`TermCaps::detect`] on purpose: full detection queries the terminal and reads
/// its reply. Calling it mid-session just for the width would steal keystrokes from the user.
/// Everything that only formats output therefore goes through here.
pub fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((80, 24))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(platform: Platform, term: &str) -> TermHost {
        TermHost {
            term: term.into(),
            multiplexer: None,
            transport: Transport::Local,
            platform,
            emulator: None,
            term_is_hostile: matches!(term, "dumb" | ""),
        }
    }

    #[test]
    fn diagnostic_mode_analyses_the_terminal_despite_redirection() {
        // Without this, a diagnostic script - which necessarily captures the output - would only
        // ever see "this is not a terminal", which is accurate but useless.
        let caps = TermCaps::detect_with(true);
        assert!(caps.is_tty);
        assert!(caps.alt_screen || caps.host.term_is_hostile);
    }

    #[test]
    fn without_tty_everything_is_disabled() {
        let caps = TermCaps::from_parts(host(Platform::Linux, "xterm-256color"), false, None);
        assert_eq!(caps.color, ColorDepth::None);
        assert!(!caps.alt_screen);
        assert!(!caps.mouse);
        assert!(!caps.sync_output);
    }

    #[test]
    fn hostile_terminal_stays_ascii() {
        let caps = TermCaps::from_parts(host(Platform::Linux, "dumb"), true, None);
        assert_eq!(caps.unicode, UnicodeLevel::Ascii);
        assert_eq!(caps.color, ColorDepth::None);
        assert!(!caps.alt_screen);
    }

    #[test]
    fn kitty_requires_proof() {
        let caps = TermCaps::from_parts(host(Platform::Linux, "xterm-256color"), true, None);
        assert!(!caps.kitty_keyboard, "no probe must mean no promise");
    }

    #[test]
    fn macos_behaves_like_a_full_unix() {
        // No Apple machine is available to run Po; this test at least pins down the expected
        // behaviour, so that a regression on this path is visible from any platform.
        let mut host = host(Platform::MacOs, "xterm-256color");
        host.emulator = Some("TERM_PROGRAM=iTerm.app".into());
        let caps = TermCaps::from_parts(host, true, None);

        assert!(caps.alt_screen);
        assert!(caps.mouse, "a desktop terminal must keep the mouse");
        assert!(caps.bracketed_paste);
        assert!(caps.hyperlinks, "iTerm and Terminal.app handle OSC 8");
        // Colour depth depends on the test process environment, not on the platform: we only
        // check that some colour is available.
        assert!(caps.color >= ColorDepth::Ansi256);
    }

    #[test]
    fn macos_under_tmux_loses_hyperlinks_but_keeps_the_rest() {
        let mut host = host(Platform::MacOs, "screen-256color");
        host.multiplexer = Some(Multiplexer::Tmux);
        let caps = TermCaps::from_parts(host, true, None);
        assert!(!caps.hyperlinks, "OSC 8 crosses multiplexers poorly");
        assert!(caps.alt_screen);
    }

    #[test]
    fn termux_disables_the_mouse() {
        let caps = TermCaps::from_parts(host(Platform::Termux, "xterm-256color"), true, None);
        assert!(!caps.mouse);
        assert_eq!(caps.unicode, UnicodeLevel::Full);
    }

    #[test]
    fn degradation_never_goes_back_up() {
        let caps = TermCaps::from_parts(host(Platform::Linux, "xterm-256color"), true, None);
        let low = caps.degraded();
        assert!(low.color <= caps.color);
        assert!(low.unicode <= caps.unicode);
        assert!(!low.mouse);
    }
}
