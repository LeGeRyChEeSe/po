//! Log of raw keyboard events, as `crossterm` delivers them.
//!
//! Used to answer a question that no reading of the code settles: what does this particular
//! terminal actually send when an arrow key is pressed? The answer changes with the terminal,
//! the transport and the platform.
//!
//! `cargo run -p po-term --example keys` — Ctrl+C to exit.

use std::io::Write;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};

fn main() -> std::io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = std::io::stdout();
    out.write_all(format!("{}\r\n", po_i18n::t!("term.example.press_keys")).as_bytes())?;
    out.flush()?;

    loop {
        if !event::poll(Duration::from_secs(30))? {
            break;
        }
        let event = event::read()?;
        let Event::Key(key) = event else {
            out.write_all(format!("{event:?}\r\n").as_bytes())?;
            out.flush()?;
            continue;
        };
        out.write_all(
            format!("code={:?} mods={:?} kind={:?}\r\n", key.code, key.modifiers, key.kind)
                .as_bytes(),
        )?;
        out.flush()?;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }
    }
    crossterm::terminal::disable_raw_mode()?;
    Ok(())
}
