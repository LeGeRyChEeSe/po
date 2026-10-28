//! Interactive probe: we *ask* the terminal for its capabilities instead of assuming them.
//!
//! Method: we emit the wanted queries followed by a `DA1` (`CSI c`), which every terminal
//! answers. We then read until we see the `DA1` reply — a sentinel guaranteeing that the
//! previous replies have already arrived — or until the maximum delay. A terminal that ignores
//! a query therefore does not waste time: it is the absence of a reply in the stream, not a
//! timeout, that indicates it.
//!
//! On Windows, the local console goes through ConPTY, which answers some queries itself and does
//! not relay others: we abstain there and the environment heuristics are reliable
//! (`WT_SESSION` designates Windows Terminal unambiguously). In an incoming **SSH session**,
//! however, the other end is a real emulator, and measurement confirms it: from Termius on
//! Android to `sshd` + `pwsh`, the burst comes back complete — `CSI ?2026;0$y` followed by
//! `CSI ?61;6;7;21;22;23;24;28;32;42 c`. Giving up on asking amounted to condemning every SSH
//! client of a Windows machine to plain rendering, however capable it might be.

use std::time::Duration;

/// Raw replies from the terminal to a burst of queries.
#[derive(Debug, Default, Clone)]
pub struct ProbeReply {
    /// Bytes received before the `DA1` reply, uninterpreted.
    pub raw: Vec<u8>,
    /// False if the terminal never answered (timeout reached): everything is then undetermined.
    pub answered: bool,
}

impl ProbeReply {
    /// Did the terminal emit a reply containing this pattern?
    pub fn contains(&self, needle: &[u8]) -> bool {
        self.raw
            .windows(needle.len())
            .any(|window| window == needle)
    }

    /// Reply to the Kitty keyboard protocol: `CSI ? <flags> u`.
    pub fn has_kitty_keyboard(&self) -> bool {
        find_csi(&self.raw, b'u')
            .is_some_and(|body| body.first() == Some(&b'?'))
    }

    /// Reply to `DECRQM 2026`: `CSI ? 2026 ; <state> $ y`, state 1 or 2 = supported.
    pub fn has_sync_output(&self) -> bool {
        let Some(body) = find_csi(&self.raw, b'y') else {
            return false;
        };
        let text = String::from_utf8_lossy(body);
        let Some(rest) = text.strip_prefix("?2026;") else {
            return false;
        };
        matches!(rest.trim_end_matches('$').trim(), "1" | "2")
    }

    /// `OSC 4` reply (palette colour): proof that the terminal really serves 256 colours.
    pub fn has_palette_query(&self) -> bool {
        self.contains(b"rgb:")
    }
}

/// Extracts the body of the first `CSI ... <final>` sequence found.
fn find_csi(haystack: &[u8], final_byte: u8) -> Option<&[u8]> {
    let mut idx = 0;
    while idx + 1 < haystack.len() {
        if haystack[idx] == 0x1b && haystack[idx + 1] == b'[' {
            let body_start = idx + 2;
            if let Some(offset) = haystack[body_start..].iter().position(|&b| b == final_byte) {
                return Some(&haystack[body_start..body_start + offset]);
            }
        }
        idx += 1;
    }
    None
}

/// Queries emitted in one burst, in this order, before the `DA1` sentinel.
#[cfg_attr(not(unix), allow(dead_code))]
const QUERIES: &[&[u8]] = &[
    b"\x1b[?u",          // Kitty keyboard protocol
    b"\x1b[?2026$p",     // DECRQM: synchronised output
    b"\x1b]4;1;?\x07",   // OSC 4: colour at index 1
];

#[cfg_attr(not(unix), allow(dead_code))]
const SENTINEL: &[u8] = b"\x1b[c"; // DA1

/// Queries the terminal. Returns `None` if the probe is not practicable here (no TTY,
/// unsupported platform, terminal declared hostile).
///
/// `remote` says the session arrives over SSH. On Unix the probe talks to `/dev/tty` and the
/// parameter changes nothing; on Windows, it is what distinguishes the local console — where
/// ConPTY interposes itself — from the stream relayed to a remote emulator, where the question
/// makes sense.
pub fn query(timeout: Duration, remote: bool) -> Option<ProbeReply> {
    imp::query(timeout, remote)
}

#[cfg(unix)]
mod imp {
    use super::{ProbeReply, QUERIES, SENTINEL};
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::{Duration, Instant};

    pub fn query(timeout: Duration, _remote: bool) -> Option<ProbeReply> {
        let mut tty = OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
        let fd = tty.as_raw_fd();

        // SAFETY: `fd` is a valid terminal descriptor, alive for as long as the guard.
        let _raw = RawMode::enter(fd)?;

        for query in QUERIES {
            tty.write_all(query).ok()?;
        }
        tty.write_all(SENTINEL).ok()?;
        tty.flush().ok()?;

        let reply = read_until_sentinel(&mut tty, fd, timeout);

        // A terminal that does not understand one of these queries may display pieces of them
        // (`$p`, `?u`...) instead of consuming them. We erase the current line so that this
        // debris does not end up above the first line of output.
        let _ = tty.write_all(b"\r\x1b[2K");
        let _ = tty.flush();

        Some(reply)
    }

    fn read_until_sentinel(tty: &mut File, fd: i32, timeout: Duration) -> ProbeReply {
        let deadline = Instant::now() + timeout;
        let mut reply = ProbeReply::default();
        let mut buf = [0u8; 256];

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !wait_readable(fd, remaining) {
                break;
            }
            match tty.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    reply.raw.extend_from_slice(&buf[..n]);
                    reply.answered = true;
                    // The DA1 reply (`CSI ? ... c`) closes the burst.
                    if super::find_csi(&reply.raw, b'c').is_some() {
                        break;
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        reply
    }

    fn wait_readable(fd: i32, timeout: Duration) -> bool {
        let mut poll_fd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: `poll_fd` points to a valid local structure, `nfds` is 1.
        unsafe { libc::poll(&mut poll_fd, 1, millis) > 0 }
    }

    /// Puts the terminal in raw mode and restores it on drop, panics included.
    struct RawMode {
        fd: i32,
        saved: libc::termios,
    }

    impl RawMode {
        fn enter(fd: i32) -> Option<Self> {
            // SAFETY: structure filled in by `tcgetattr` before any of its fields is read.
            let mut saved: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
                return None;
            }
            let mut raw = saved;
            // SAFETY: `cfmakeraw` only writes into the structure provided.
            unsafe { libc::cfmakeraw(&mut raw) };
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                return None;
            }
            Some(Self { fd, saved })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: we restore exactly the state read on entry, on the same descriptor.
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved) };
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{ProbeReply, QUERIES, SENTINEL};
    use std::io::Write;
    use std::time::{Duration, Instant};

    use crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use crossterm::terminal;

    /// On the local console, ConPTY interposes itself: it answers part of the queries itself and
    /// reading the console in parallel with input would steal keystrokes. We ask nothing there.
    /// In an incoming SSH session, the burst goes through and comes back: measured, not assumed.
    pub fn query(timeout: Duration, remote: bool) -> Option<ProbeReply> {
        if !remote {
            return None;
        }
        let _raw = RawMode::enter()?;

        let mut out = std::io::stdout();
        for query in QUERIES {
            out.write_all(query).ok()?;
        }
        out.write_all(SENTINEL).ok()?;
        out.flush().ok()?;

        let reply = read_until_sentinel(timeout);

        // Same precaution as on Unix: a terminal that does not understand a query may spit
        // pieces of it back onto the current line.
        let _ = out.write_all(b"\r\x1b[2K");
        let _ = out.flush();

        Some(reply)
    }

    /// Rebuilds the reply bytes from keyboard events.
    ///
    /// Windows does not deliver a byte stream but console records: each character of the reply
    /// arrives as a distinct key, `ESC` included. We reassemble them in the order received,
    /// which gives back exactly the sequence emitted by the remote terminal.
    fn read_until_sentinel(timeout: Duration) -> ProbeReply {
        let deadline = Instant::now() + timeout;
        let mut reply = ProbeReply::default();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !matches!(event::poll(remaining), Ok(true)) {
                break;
            }
            let Ok(Event::Key(key)) = event::read() else {
                continue;
            };
            // Windows also emits a release per key: counting it would double the reply.
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Esc => reply.raw.push(0x1b),
                KeyCode::Char(c) => {
                    let mut buf = [0u8; 4];
                    reply.raw.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                _ => continue,
            }
            reply.answered = true;
            if super::find_csi(&reply.raw, b'c').is_some() {
                break;
            }
        }
        reply
    }

    /// Raw mode for the duration of the probe, restored on drop, panics included.
    struct RawMode;

    impl RawMode {
        fn enter() -> Option<Self> {
            terminal::enable_raw_mode().ok()?;
            Some(Self)
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = terminal::disable_raw_mode();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(bytes: &[u8]) -> ProbeReply {
        ProbeReply { raw: bytes.to_vec(), answered: true }
    }

    #[test]
    fn detects_the_kitty_keyboard() {
        assert!(reply(b"\x1b[?1u\x1b[?62;c").has_kitty_keyboard());
        // A terminal without support answers the DA1 alone.
        assert!(!reply(b"\x1b[?62;1;6c").has_kitty_keyboard());
    }

    #[test]
    fn detects_synchronised_output() {
        assert!(reply(b"\x1b[?2026;2$y").has_sync_output());
        assert!(reply(b"\x1b[?2026;1$y").has_sync_output());
        // State 0 = not recognised, 4 = read-only permanently reserved, not supported.
        assert!(!reply(b"\x1b[?2026;0$y").has_sync_output());
    }

    #[test]
    fn no_reply_does_not_lie() {
        let empty = ProbeReply::default();
        assert!(!empty.answered);
        assert!(!empty.has_kitty_keyboard());
        assert!(!empty.has_sync_output());
        assert!(!empty.has_palette_query());
    }

    #[test]
    fn extracts_the_csi_body() {
        assert_eq!(find_csi(b"\x1b[?2026;2$y", b'y'), Some(&b"?2026;2$"[..]));
        assert_eq!(find_csi(b"noise\x1b[?1u", b'u'), Some(&b"?1"[..]));
        assert_eq!(find_csi(b"no csi here", b'u'), None);
    }
}
