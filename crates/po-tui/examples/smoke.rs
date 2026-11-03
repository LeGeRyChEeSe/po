//! Smoke test of the full-screen rendering, meant to be run in a real PTY.
//!
//! There is no agent here: we check that the TUI takes over the terminal, draws without
//! panicking on content that stresses the rendering - accents, emoji, ideographs, very long
//! lines - then hands back a clean terminal. That last part is what the `terminal-matrix.sh`
//! bench checks by comparing `stty -g` before and after.
//!
//! Usage: `cargo run -p po-tui --example smoke -- [milliseconds]`

use po_term::{RenderProfile, Terminal};
use po_tui::{App, Line, LineKind, view};

fn main() -> std::io::Result<()> {
    let hold_ms: u64 = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(400);

    // The rich profile is forced: the point of the test is precisely to exercise the TUI path,
    // even on a PTY that detection would classify as `plain`.
    let mut terminal = Terminal::detect(Some(RenderProfile::Rich));
    // On a terminal that refuses raw mode - partial emulation, missing `ioctl` - we degrade
    // instead of giving up: that is the behavior the harness itself must have.
    let (guard, profile) = terminal.enter_best_effort();
    if guard.is_none() {
        println!("tui-smoke: {}", po_i18n::t!("tui.smoke.unavailable", profile = profile));
        return Ok(());
    }

    let mut app = App::new();
    app.status.model = "test-model".into();
    app.status.mode = Some(po_router::BudgetMode::Balanced);
    app.status.tokens = 12_345;
    app.status.cost = 0.0421;
    app.status.local_share = 0.62;
    app.status.vram_free_mib = Some(4096);

    app.transcript.push(Line::new(LineKind::User, "Summarize the project, please"));
    app.transcript.push(Line::new(LineKind::Assistant, "Here is a summary with accents: à, é, ï, ç, œ."));
    app.transcript.push(Line::new(LineKind::Assistant, "Ideographs: 日本語のテキスト"));
    app.transcript.push(Line::new(LineKind::Assistant, "Emoji: 🚀 ✅ 👨‍👩‍👧 - double width"));
    app.transcript.push(Line::new(LineKind::Tool, "glob **/*.rs - 34 files"));
    app.transcript.push(Line::new(LineKind::Delegation, "subagent qwen3-27b · search"));
    app.transcript.push(Line::new(LineKind::Error, "a tool failed"));
    app.transcript.push(Line::new(
        LineKind::Assistant,
        "A deliberately very long line, meant to check that wrapping works without overflowing \
         the area or corrupting the input frame, including on a narrow terminal where it will \
         take several physical rows.",
    ));
    app.transcript.push(Line::new(LineKind::Summary, "1,234 tokens · $0.0000 · 100% local"));

    for c in "input in progress…".chars() {
        app.input.insert(c);
    }

    let mut backend = view::Backend::new(&terminal)?;
    backend.draw(&app, &terminal)?;
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));

    // The help and the authorization dialog are drawn on top: they have their own centering
    // logic, which a narrow screen can break.
    app.show_help = true;
    backend.draw(&app, &terminal)?;
    std::thread::sleep(std::time::Duration::from_millis(hold_ms));

    drop(guard);
    println!("tui-smoke: {}", po_i18n::t!("tui.smoke.done"));
    Ok(())
}
