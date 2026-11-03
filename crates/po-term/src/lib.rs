//! `po-term` - the layer that makes Po usable anywhere there is a terminal.
//!
//! Guiding principle: **assume nothing**. Each capability is either observed through a reply
//! from the terminal, guaranteed by a reliable environment hint, or considered absent. Doubt
//! always resolves toward the poorest rendering, never the other way round.
//!
//! ```no_run
//! use po_term::{Terminal, RenderProfile};
//!
//! let terminal = Terminal::detect(None);
//! println!("{}", terminal.report());
//! if terminal.profile() == RenderProfile::Stream {
//!     // stream output: no alternate screen, the output stays usable in a pipe
//! }
//! ```

pub mod caps;
pub mod env;
pub mod guard;
pub mod input;
pub mod probe;
pub mod profile;
pub mod width;
pub mod wrap;

pub use caps::{ColorDepth, TermCaps, UnicodeLevel, terminal_size};
pub use env::{Multiplexer, Platform, TermHost, Transport};
pub use guard::{TerminalGuard, restore_now};
pub use input::{Input, Key, LineEditor};
pub use profile::{BoxChars, RenderProfile};

/// The terminal as Po sees it: its capabilities and the render profile that follows.
#[derive(Debug, Clone)]
pub struct Terminal {
    caps: TermCaps,
    profile: RenderProfile,
}

impl Terminal {
    /// Detects the environment, probes the terminal if practicable, and deduces the profile.
    /// `forced` short-circuits the deduction (the `--render` option).
    pub fn detect(forced: Option<RenderProfile>) -> Self {
        let caps = TermCaps::detect();
        let profile = RenderProfile::resolve(&caps, forced);
        Self { caps, profile }
    }

    /// Diagnostic detection: analyses the terminal attached to the process even if the output
    /// is redirected. Reserved for `po doctor --tty`, never used for a real session.
    pub fn detect_attached(forced: Option<RenderProfile>) -> Self {
        let caps = TermCaps::detect_with(true);
        let profile = RenderProfile::resolve(&caps, forced);
        Self { caps, profile }
    }

    pub fn caps(&self) -> &TermCaps {
        &self.caps
    }

    pub fn profile(&self) -> RenderProfile {
        self.profile
    }

    /// Steps down one rendering notch and aligns the capabilities accordingly.
    /// Called when the current rendering proves untenable in use, never the reverse.
    pub fn degrade(&mut self) -> RenderProfile {
        self.profile = self.profile.degrade();
        self.caps = self.caps.degraded();
        self.profile
    }

    /// Takes control of the terminal for the duration of the session.
    pub fn enter(&self) -> std::io::Result<TerminalGuard> {
        TerminalGuard::enter(&self.caps, self.profile)
    }

    /// Takes control, or degrades until a profile that works is reached.
    ///
    /// Some environments refuse raw mode even though they present a terminal: partial emulation,
    /// missing `ioctl`, restricted container, exotic `pty`. Failing there would be the worst
    /// choice - the user did nothing wrong and ends up with no tool. So we step down a notch at
    /// a time, down to the stream, which demands nothing of the terminal.
    ///
    /// Returns the guard if full-screen mode could be established, and the profile actually
    /// obtained.
    pub fn enter_best_effort(&mut self) -> (Option<TerminalGuard>, RenderProfile) {
        loop {
            if self.profile == RenderProfile::Stream {
                return (None, RenderProfile::Stream);
            }
            match self.enter() {
                Ok(guard) => return (Some(guard), self.profile),
                Err(_) => {
                    self.degrade();
                }
            }
        }
    }

    /// Readable account of what was detected - the `po doctor` command relies on it, and it is
    /// what we will ask a user whose terminal misbehaves for.
    pub fn report(&self) -> String {
        use po_i18n::t;
        let caps = &self.caps;
        let host = &caps.host;
        let mux = match host.multiplexer {
            Some(Multiplexer::Tmux) => "tmux",
            Some(Multiplexer::Screen) => "screen",
            Some(Multiplexer::Zellij) => "zellij",
            None => t!("term.report.none"),
        };
        let term = if host.term.is_empty() { t!("term.report.empty") } else { &host.term };
        let probe = if caps.probed {
            t!("term.report.probe_answered")
        } else if caps.is_tty {
            t!("term.report.probe_silent")
        } else {
            t!("term.report.probe_not_applicable")
        };
        let emulator = host.emulator.as_deref().unwrap_or(t!("term.report.unknown"));
        let rows: Vec<(&str, String)> = vec![
            (t!("term.report.render_profile"), self.profile.to_string()),
            ("TERM", term.to_owned()),
            (t!("term.report.platform"), format!("{:?}", host.platform)),
            (t!("term.report.transport"), format!("{:?}", host.transport)),
            (t!("term.report.multiplexer"), mux.to_owned()),
            (t!("term.report.emulator"), emulator.to_owned()),
            (t!("term.report.size"), format!("{}x{}", caps.size.0, caps.size.1)),
            (t!("term.report.colors"), format!("{:?}", caps.color)),
            (t!("term.report.unicode"), format!("{:?}", caps.unicode)),
            (t!("term.report.alt_screen"), yes_no(caps.alt_screen).to_owned()),
            (t!("term.report.mouse"), yes_no(caps.mouse).to_owned()),
            (t!("term.report.kitty_keyboard"), yes_no(caps.kitty_keyboard).to_owned()),
            (t!("term.report.sync_output"), yes_no(caps.sync_output).to_owned()),
            (t!("term.report.bracketed_paste"), yes_no(caps.bracketed_paste).to_owned()),
            (t!("term.report.hyperlinks"), yes_no(caps.hyperlinks).to_owned()),
            (t!("term.report.probe"), probe.to_owned()),
        ];
        let mut lines: Vec<String> =
            rows.into_iter().map(|(label, value)| format!("{label:<17} : {value}")).collect();
        if !caps.is_tty {
            lines.push(format!("{:<17} : {}", t!("term.report.note"), t!("term.report.not_a_terminal")));
        }
        lines.join("\n")
    }
}

/// Restores the default behaviour of `SIGPIPE`.
///
/// Rust ignores it at startup, so writing to a closed pipe - `po doctor | head -3`, the most
/// ordinary case there is - does not end the program but makes `println!` fail, which panics.
/// A command-line tool must exit silently in that case, as `ls` or `cat` do. To be called once,
/// at the very start of `main`.
pub fn restore_default_sigpipe() {
    #[cfg(unix)]
    // SAFETY: we restore the default disposition of a signal whose default action is process
    // termination; no resource is involved.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { po_i18n::t!("common.yes") } else { po_i18n::t!("common.no") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_covers_all_capabilities() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let terminal = Terminal::detect(Some(RenderProfile::Stream));
        let report = terminal.report();
        for expected in ["render profile", "colors", "unicode", "mouse", "probe"] {
            assert!(report.contains(expected), "incomplete report: {expected} missing");
        }
    }

    #[test]
    fn taking_the_terminal_best_effort_never_blocks() {
        // Outside a terminal, taking control fails: we must get stream mode, not an error.
        let mut terminal = Terminal::detect(Some(RenderProfile::Rich));
        let (guard, profile) = terminal.enter_best_effort();
        if guard.is_none() {
            assert_eq!(profile, RenderProfile::Stream);
        }
        // In every case, the function returns with a usable profile.
        assert!(profile <= RenderProfile::Rich);
    }

    #[test]
    fn restoring_sigpipe_is_harmless_and_idempotent() {
        // On platforms without signals, the call must simply do nothing.
        restore_default_sigpipe();
        restore_default_sigpipe();
    }

    #[test]
    fn degrading_lowers_capabilities_and_profile() {
        let mut terminal = Terminal::detect(Some(RenderProfile::Rich));
        assert_eq!(terminal.profile(), RenderProfile::Rich);
        assert_eq!(terminal.degrade(), RenderProfile::Plain);
        assert!(!terminal.caps().mouse);
        assert_eq!(terminal.degrade(), RenderProfile::Stream);
        assert_eq!(terminal.degrade(), RenderProfile::Stream, "the floor is stable");
    }
}
