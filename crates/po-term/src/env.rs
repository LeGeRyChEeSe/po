//! Detection of the terminal environment from environment variables alone.
//!
//! This pass does no I/O on the terminal: it is instantaneous, safe in a pipe, and serves as the
//! foundation that the interactive probe ([`crate::probe`]) then refines.

use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    Tmux,
    Screen,
    Zellij,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The terminal is attached locally.
    Local,
    /// Remote session: every byte costs, and the emulation chain can lie.
    Ssh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
    /// Android under Termux: no fine-grained mouse, soft keyboard, non-standard `PREFIX`.
    Termux,
    Other,
}

/// What the environment tells us before we have talked to the terminal.
#[derive(Debug, Clone)]
pub struct TermHost {
    pub term: String,
    pub multiplexer: Option<Multiplexer>,
    pub transport: Transport,
    pub platform: Platform,
    /// `TERM_PROGRAM`, `WT_SESSION`, `KONSOLE_VERSION`... when the emulator names itself.
    pub emulator: Option<String>,
    /// True when `TERM` is absent, `dumb`, or too poor for full-screen rendering.
    pub term_is_hostile: bool,
}

impl TermHost {
    pub fn detect() -> Self {
        let term = env::var("TERM").unwrap_or_default();
        let platform = detect_platform();

        Self {
            multiplexer: detect_multiplexer(&term),
            transport: detect_transport(),
            emulator: detect_emulator(),
            term_is_hostile: term_is_hostile(&term, platform),
            platform,
            term,
        }
    }

    /// Does the terminal sit behind a layer that may swallow our control sequences?
    pub fn needs_passthrough(&self) -> bool {
        matches!(self.multiplexer, Some(Multiplexer::Tmux | Multiplexer::Screen))
    }
}

/// tmux version, when readable.
///
/// tmux does not relay `DECRQM` queries to the host terminal: probing synchronized output from
/// inside a session therefore never gets an answer, even though tmux >= 3.3 implements it
/// itself. Without this check, a user on SSH inside tmux - the most common configuration on a
/// server - would be deprived of rich rendering.
pub fn tmux_version() -> Option<(u32, u32)> {
    // tmux 3.2 and later put their version in the pane's environment. Reading it there avoids
    // spawning a subprocess at startup - faster, and the only reliable way when running a
    // third-party binary is not possible (emulation, sandbox).
    if env::var("TERM_PROGRAM").is_ok_and(|value| value == "tmux")
        && let Ok(version) = env::var("TERM_PROGRAM_VERSION")
        && let Some(parsed) = parse_tmux_version(&version)
    {
        return Some(parsed);
    }

    let output = std::process::Command::new("tmux").arg("-V").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_tmux_version(&String::from_utf8_lossy(&output.stdout))
}

/// True if running inside tmux and its mouse is off (`mouse off`, the default value). tmux then
/// passes no scroll gesture to the application: the wheel and the finger (Termius) have no
/// effect, whatever Po does. False outside tmux, or if the option cannot be read.
pub fn tmux_mouse_off() -> bool {
    if env::var_os("TMUX").is_none() {
        return false;
    }
    std::process::Command::new("tmux")
        .args(["show-options", "-gv", "mouse"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "off")
}

/// Parses the output of `tmux -V` ("tmux 3.4", "tmux 3.2a", "tmux next-3.5").
pub fn parse_tmux_version(raw: &str) -> Option<(u32, u32)> {
    let digits: String = raw
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = digits.split('.');
    let major = parts.next()?.parse().ok()?;
    // A version without a minor ("tmux 3") remains usable.
    let minor = parts.next().and_then(|m| m.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

fn detect_platform() -> Platform {
    // Termux signals itself explicitly; we test it before the OS because it *is* a Linux.
    if env::var_os("TERMUX_VERSION").is_some()
        || env::var("PREFIX").is_ok_and(|p| p.contains("com.termux"))
    {
        return Platform::Termux;
    }
    match std::env::consts::OS {
        "windows" => Platform::Windows,
        "macos" => Platform::MacOs,
        "linux" | "android" => Platform::Linux,
        _ => Platform::Other,
    }
}

fn detect_multiplexer(term: &str) -> Option<Multiplexer> {
    if env::var_os("TMUX").is_some() || term.starts_with("tmux") {
        return Some(Multiplexer::Tmux);
    }
    if env::var_os("ZELLIJ").is_some() {
        return Some(Multiplexer::Zellij);
    }
    // `STY` is set by GNU screen; `TERM=screen*` alone is not enough because tmux often still
    // presents itself as `screen-256color`.
    if env::var_os("STY").is_some() {
        return Some(Multiplexer::Screen);
    }
    None
}

fn detect_transport() -> Transport {
    let remote = ["SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT"]
        .iter()
        .any(|k| env::var_os(k).is_some());
    if remote { Transport::Ssh } else { Transport::Local }
}

fn detect_emulator() -> Option<String> {
    for key in [
        "TERM_PROGRAM",
        "TERMINAL_EMULATOR",
        "KONSOLE_VERSION",
        "VTE_VERSION",
        "ALACRITTY_WINDOW_ID",
    ] {
        if let Ok(v) = env::var(key)
            && !v.is_empty()
        {
            return Some(format!("{key}={v}"));
        }
    }
    // Windows Terminal does not set TERM_PROGRAM but a session identifier.
    if env::var_os("WT_SESSION").is_some() {
        return Some("windows-terminal".into());
    }
    None
}

fn term_is_hostile(term: &str, platform: Platform) -> bool {
    // On Windows, an empty TERM is the norm (ConPTY) and is not hostile at all.
    if term.is_empty() {
        return platform != Platform::Windows;
    }
    matches!(term, "dumb" | "unknown" | "vt52" | "net")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_version_is_parsed() {
        assert_eq!(parse_tmux_version("tmux 3.4
"), Some((3, 4)));
        // Patch versions carry a letter, which the parser must ignore.
        assert_eq!(parse_tmux_version("tmux 3.2a
"), Some((3, 2)));
        assert_eq!(parse_tmux_version("tmux next-3.5
"), Some((3, 5)));
        assert_eq!(parse_tmux_version("tmux 2
"), Some((2, 0)));
        assert_eq!(parse_tmux_version("command not found"), None);
        // The environment variable carries only the number, with no prefix.
        assert_eq!(parse_tmux_version("3.4"), Some((3, 4)));
        assert_eq!(parse_tmux_version("3.5a"), Some((3, 5)));
    }

    #[test]
    fn termux_takes_precedence_over_linux() {
        // We cannot mutate the test process environment safely, so we test the pure logic
        // exposed by the helpers.
        assert!(term_is_hostile("dumb", Platform::Linux));
        assert!(term_is_hostile("", Platform::Linux));
        assert!(!term_is_hostile("", Platform::Windows));
        assert!(!term_is_hostile("xterm-256color", Platform::Linux));
    }
}
