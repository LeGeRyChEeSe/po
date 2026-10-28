//! Terminal state guard: whatever is switched on will be switched off, whatever happens.
//!
//! There are three exit paths, and all three must restore the terminal: normal exit (`Drop`),
//! panic (panic hook) and fatal signal (`SIGINT`/`SIGTERM`/`SIGHUP`). A harness that covers
//! only the first leaves the user's shell in a corrupted state — the exact defect reported on
//! SSH sessions.

use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::caps::TermCaps;
use crate::profile::RenderProfile;

/// What is currently active on the terminal, as flags.
/// Read by paths that are not allowed to allocate (signal handler), hence the atomic.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

const RAW: u8 = 1 << 0;
const ALT_SCREEN: u8 = 1 << 1;
const MOUSE: u8 = 1 << 2;
const KITTY: u8 = 1 << 3;
const BRACKETED_PASTE: u8 = 1 << 4;

// Restoration sequences, written as is from any context.
const SEQ_SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const SEQ_LEAVE_ALT: &[u8] = b"\x1b[?1049l";
const SEQ_MOUSE_OFF: &[u8] = b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";
const SEQ_KITTY_POP: &[u8] = b"\x1b[<u";
const SEQ_PASTE_OFF: &[u8] = b"\x1b[?2004l";
const SEQ_RESET_SGR: &[u8] = b"\x1b[0m";

pub struct TerminalGuard {
    profile: RenderProfile,
}

impl TerminalGuard {
    /// Sets up the terminal state matching the profile, and arms the safety nets.
    pub fn enter(caps: &TermCaps, profile: RenderProfile) -> io::Result<Self> {
        install_safety_nets();

        let mut flags = 0u8;
        let mut out = io::stdout();

        if profile.uses_alt_screen() {
            crossterm::terminal::enable_raw_mode()?;
            flags |= RAW;

            out.write_all(b"\x1b[?1049h")?;
            flags |= ALT_SCREEN;

            if caps.bracketed_paste {
                out.write_all(b"\x1b[?2004h")?;
                flags |= BRACKETED_PASTE;
            }
            if caps.mouse && profile.allows_mouse() {
                out.write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h")?;
                flags |= MOUSE;
            }
            if caps.kitty_keyboard {
                // Flag 1: disambiguates keys, which makes Ctrl+C reliable — the blocking point
                // reported under Termux.
                out.write_all(b"\x1b[>1u")?;
                flags |= KITTY;
            }
            out.flush()?;
        }

        ACTIVE.fetch_or(flags, Ordering::SeqCst);
        Ok(Self { profile })
    }

    pub fn profile(&self) -> RenderProfile {
        self.profile
    }

    /// Suspends the terminal state while running something that owns the screen (an `$EDITOR`,
    /// an interactive command), then restores it.
    pub fn suspended<T>(&mut self, caps: &TermCaps, f: impl FnOnce() -> T) -> io::Result<T> {
        let profile = self.profile;
        restore_now();
        let result = f();
        let restored = Self::enter(caps, profile)?;
        // The new guard must not restore a second time when dropped.
        std::mem::forget(restored);
        Ok(result)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_now();
    }
}

/// Switches off everything that is on. Idempotent: callable several times, from anywhere.
pub fn restore_now() {
    let flags = ACTIVE.swap(0, Ordering::SeqCst);
    if flags == 0 {
        return;
    }

    let mut seq: Vec<&[u8]> = Vec::with_capacity(8);
    // Reverse order of activation: input modes first, the screen last.
    if flags & KITTY != 0 {
        seq.push(SEQ_KITTY_POP);
    }
    if flags & MOUSE != 0 {
        seq.push(SEQ_MOUSE_OFF);
    }
    if flags & BRACKETED_PASTE != 0 {
        seq.push(SEQ_PASTE_OFF);
    }
    seq.push(SEQ_RESET_SGR);
    seq.push(SEQ_SHOW_CURSOR);
    if flags & ALT_SCREEN != 0 {
        seq.push(SEQ_LEAVE_ALT);
    }

    let mut out = io::stdout();
    for bytes in seq {
        let _ = out.write_all(bytes);
    }
    let _ = out.flush();

    if flags & RAW != 0 {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

fn install_safety_nets() {
    use std::sync::Once;
    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // Restore *before* printing, otherwise the message is written into the alternate
            // screen that disappears right after, and the user never sees the cause.
            restore_now();
            previous(info);
        }));

        install_signal_handlers();
    });
}

#[cfg(unix)]
fn install_signal_handlers() {
    // SAFETY: we install a handler on signals whose default semantics is process termination;
    // the handler only calls functions that are safe in signal context.
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            libc::signal(signal, handle_signal as *const () as libc::sighandler_t);
        }
    }
}

#[cfg(unix)]
extern "C" fn handle_signal(signal: libc::c_int) {
    // Signal context: no allocation, no lock. We write the restoration sequence directly to the
    // output descriptor, then restore the default behaviour and re-raise the signal so that the
    // exit code stays correct.
    const RESET: &[u8] = b"\x1b[<u\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[0m\x1b[?25h\x1b[?1049l";
    // SAFETY: `write` is safe in signal context; the buffer is static and alive.
    unsafe {
        libc::write(1, RESET.as_ptr().cast(), RESET.len());
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {
    // On Windows, Ctrl+C arrives as an input event in raw mode and is handled by the event
    // loop; abruptly closing the console destroys the screen buffer anyway, so there is nothing
    // to restore.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_with_nothing_active_does_nothing() {
        ACTIVE.store(0, Ordering::SeqCst);
        restore_now();
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn restoration_is_idempotent() {
        ACTIVE.store(ALT_SCREEN | MOUSE, Ordering::SeqCst);
        restore_now();
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 0, "the flags must be consumed");
        restore_now();
        assert_eq!(ACTIVE.load(Ordering::SeqCst), 0);
    }
}
