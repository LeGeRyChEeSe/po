//! Watchdog: Ctrl+C keeps working even when the interface loop is blocked.
//!
//! The keyboard is read by the interface loop. If some work blocks it, nothing is read or
//! redrawn anymore: seen with a file walk started over a whole home folder, Po stayed frozen
//! with no way out but killing the process. The watchdog runs on a separate thread: when the
//! loop has shown no sign of life for a few seconds, it announces it at the bottom of the
//! screen and reads the keyboard itself; Ctrl+C then restores the terminal and exits.

use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// Silence beyond which the loop is considered blocked. It beats at least four times a
/// second: two seconds of silence can only mean a blockage.
const STALL: Duration = Duration::from_secs(2);

static ORIGIN: OnceLock<Instant> = OnceLock::new();
static LAST_BEAT: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The watchdog wrote on the screen: the interface must redraw it entirely.
static SCRIBBLED: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// Starts the watchdog (once per process) and arms it.
pub fn start() {
    beat();
    if ACTIVE.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new().name("po-watchdog".into()).spawn(|| {
        let mut announced = false;
        while ACTIVE.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            let silent = now_ms().saturating_sub(LAST_BEAT.load(Ordering::Relaxed));
            if silent < STALL.as_millis() as u64 {
                announced = false;
                continue;
            }
            if !announced {
                announce();
                SCRIBBLED.store(true, Ordering::Relaxed);
                announced = true;
            }
            // The loop no longer reads the keyboard: the watchdog does it in its place.
            if let Ok(true) = crossterm::event::poll(Duration::from_millis(100))
                && let Ok(Event::Key(key)) = crossterm::event::read()
                && key.kind != KeyEventKind::Release
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                emergency_exit();
            }
        }
    });
}

/// Disarms the watchdog (normal exit of the interface).
pub fn stop() {
    ACTIVE.store(false, Ordering::SeqCst);
}

/// Sign of life from the interface loop.
pub fn beat() {
    LAST_BEAT.store(now_ms(), Ordering::Relaxed);
}

/// True once if the watchdog wrote on the screen since the last call.
pub fn take_scribbled() -> bool {
    SCRIBBLED.swap(false, Ordering::Relaxed)
}

/// Banner at the bottom of the screen. The loop being blocked, nobody else writes meanwhile.
fn announce() {
    let rows = crossterm::terminal::size().map(|(_, rows)| rows).unwrap_or(24);
    let mut out = std::io::stdout();
    let banner = po_i18n::t!("tui.watchdog.stalled");
    let _ = write!(out, "\x1b7\x1b[{rows};1H\x1b[2K\x1b[7m {banner} \x1b[0m\x1b8");
    let _ = out.flush();
}

/// Restores the terminal to the state it was found in, then exits.
fn emergency_exit() -> ! {
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    po_term::restore_now();
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!("\n{}", po_i18n::t!("tui.watchdog.exit"));
    std::process::exit(130);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_beat_resets_the_counter() {
        beat();
        let silent = now_ms().saturating_sub(LAST_BEAT.load(Ordering::Relaxed));
        assert!(silent < STALL.as_millis() as u64);
        assert!(!take_scribbled());
    }
}
