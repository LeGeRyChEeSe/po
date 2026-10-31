//! Line input with keyboard shortcuts active in the middle of a conversation.
//!
//! The system's `read_line` is enough to read text, but it swallows the whole line before
//! handing control back: impossible to intercept a key combination. Yet changing the budget
//! mode or swapping the local model must happen **without leaving the conversation** and
//! without typing a command — that is the difference between a setting people use and a setting
//! people forget.
//!
//! Two design rules:
//! - raw mode is only armed during reading, never during display; a session interrupted at the
//!   wrong moment therefore cannot leave the terminal in raw mode;
//! - outside an interactive terminal (pipe, file, `TERM=dumb`, CI), we fall back to
//!   `read_line`. Shortcuts would make no sense there, and control sequences would soil the
//!   output.

use std::io::{self, BufRead, Write};

use crate::caps::UnicodeLevel;
use crate::width::display_width;

/// What a read hands back to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The user submitted a line.
    Line(String),
    /// A `Ctrl+<letter>` combination declared by the caller was pressed.
    /// The letter is returned in lowercase.
    Shortcut(char),
    /// End of input: Ctrl+D on an empty line, or exhausted stream.
    Eof,
    /// Ctrl+C: the current line is abandoned, the session continues.
    Interrupt,
    /// Shift+Tab: switch working mode, without touching the current line.
    BackTab,
}

/// Editing state of a line. Kept apart from any input/output to be testable: cursor and history
/// logic is exactly the kind that breaks silently.
#[derive(Debug, Default)]
pub struct LineEditor {
    buffer: String,
    /// Cursor position, in bytes, always on a character boundary.
    cursor: usize,
    history: Vec<String>,
    /// Index into the history during navigation; `None` when editing the current line.
    browsing: Option<usize>,
    /// The line being edited, set aside while browsing the history.
    stashed: String,
    /// Escape sequences being reassembled.
    ///
    /// This state lives in the editor, not in the read loop: a shortcut returns control between
    /// a key press and its release, and a reset on every call made the release count as a second
    /// press — a single `Ctrl+G` then cycled through two budget modes.
    decoder: VtDecoder,
    /// Last key honoured, to ignore the release that repeats it.
    previous_key: Option<Key>,
}

/// Result of absorbing a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The line changed: it must be redrawn.
    Redraw,
    /// Nothing moved (unknown key, bound reached).
    Ignored,
    /// The read is over.
    Done(Input),
}

/// Normalised key, independent of crossterm so that the logic stays testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Start of a sequence, never an editing action in itself.
    Escape,
    Char(char),
    Ctrl(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    /// Shift+Tab (`ESC [ Z`).
    BackTab,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Puts a text into the line, cursor at the end.
    ///
    /// This is what lets a shortcut *prepare* a command rather than run it: swapping a 9 GB
    /// model by mistake would be far worse than having to confirm.
    pub fn prefill(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        self.cursor = self.buffer.len();
        self.browsing = None;
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.browsing = None;
    }

    /// Records a submitted line in the history, without consecutive duplicates or empty lines.
    pub fn remember(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.history.last().map(String::as_str) == Some(line) {
            return;
        }
        self.history.push(line.to_owned());
    }

    /// Absorbs a key. `shortcuts` lists the letters reserved by the caller.
    pub fn apply(&mut self, key: Key, shortcuts: &[char]) -> Step {
        match key {
            Key::Enter => {
                let line = std::mem::take(&mut self.buffer);
                self.cursor = 0;
                self.browsing = None;
                Step::Done(Input::Line(line))
            }
            Key::Ctrl('c') => {
                self.clear();
                Step::Done(Input::Interrupt)
            }
            // Ctrl+D only quits on an empty line: otherwise it deletes forward, as everywhere
            // else. Quitting in the middle of a sentence would lose work.
            Key::Ctrl('d') if self.buffer.is_empty() => Step::Done(Input::Eof),
            Key::Ctrl('d') | Key::Delete => {
                if self.cursor >= self.buffer.len() {
                    return Step::Ignored;
                }
                let next = self.next_boundary();
                self.buffer.replace_range(self.cursor..next, "");
                Step::Redraw
            }
            Key::Ctrl('a') | Key::Home => self.move_to(0),
            Key::Ctrl('e') | Key::End => {
                let end = self.buffer.len();
                self.move_to(end)
            }
            Key::Ctrl('u') => {
                if self.cursor == 0 {
                    return Step::Ignored;
                }
                self.buffer.replace_range(0..self.cursor, "");
                self.cursor = 0;
                Step::Redraw
            }
            Key::Ctrl('k') => {
                if self.cursor >= self.buffer.len() {
                    return Step::Ignored;
                }
                self.buffer.truncate(self.cursor);
                Step::Redraw
            }
            Key::Ctrl('w') => {
                let start = self.previous_word();
                if start == self.cursor {
                    return Step::Ignored;
                }
                self.buffer.replace_range(start..self.cursor, "");
                self.cursor = start;
                Step::Redraw
            }
            Key::Ctrl(letter) if shortcuts.contains(&letter) => Step::Done(Input::Shortcut(letter)),
            Key::BackTab => Step::Done(Input::BackTab),
            Key::Ctrl(_) => Step::Ignored,
            Key::Backspace => {
                if self.cursor == 0 {
                    return Step::Ignored;
                }
                let previous = self.previous_boundary();
                self.buffer.replace_range(previous..self.cursor, "");
                self.cursor = previous;
                Step::Redraw
            }
            Key::Left => {
                if self.cursor == 0 {
                    return Step::Ignored;
                }
                let previous = self.previous_boundary();
                self.move_to(previous)
            }
            Key::Right => {
                if self.cursor >= self.buffer.len() {
                    return Step::Ignored;
                }
                let next = self.next_boundary();
                self.move_to(next)
            }
            Key::Up => self.browse(-1),
            Key::Down => self.browse(1),
            Key::Char(c) => {
                self.buffer.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                Step::Redraw
            }
            // Escape is only a sequence marker: the decoder already consumed it when it
            // introduced one, and only the isolated press remains here, with no effect on the
            // line.
            Key::Escape => Step::Ignored,
        }
    }

    fn move_to(&mut self, position: usize) -> Step {
        if position == self.cursor {
            return Step::Ignored;
        }
        self.cursor = position;
        Step::Redraw
    }

    /// Character boundary preceding the cursor: indispensable to avoid cutting an accented
    /// character or an ideograph in two.
    fn previous_boundary(&self) -> usize {
        self.buffer[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn next_boundary(&self) -> usize {
        self.buffer[self.cursor..]
            .chars()
            .next()
            .map(|c| self.cursor + c.len_utf8())
            .unwrap_or(self.cursor)
    }

    /// Start of the previous word: we skip the spaces first, then the word itself.
    fn previous_word(&self) -> usize {
        let head = &self.buffer[..self.cursor];
        let trimmed = head.trim_end();
        match trimmed.rfind(char::is_whitespace) {
            Some(index) => index + 1,
            None => 0,
        }
    }

    /// Walks the history. `direction` is -1 to go back, 1 to go forward.
    fn browse(&mut self, direction: i32) -> Step {
        if self.history.is_empty() {
            return Step::Ignored;
        }
        let position = match (self.browsing, direction) {
            // First step back: the current line is set aside, not lost.
            (None, -1) => {
                self.stashed = self.buffer.clone();
                self.history.len() - 1
            }
            (None, _) => return Step::Ignored,
            (Some(0), -1) => return Step::Ignored,
            (Some(index), -1) => index - 1,
            (Some(index), _) if index + 1 >= self.history.len() => {
                // Going down past the last element returns the line that was being written.
                self.browsing = None;
                self.buffer = std::mem::take(&mut self.stashed);
                self.cursor = self.buffer.len();
                return Step::Redraw;
            }
            (Some(index), _) => index + 1,
        };
        self.browsing = Some(position);
        self.buffer = self.history[position].clone();
        self.cursor = self.buffer.len();
        Step::Redraw
    }
}

/// Reads a line by arming raw mode, or falls back to `read_line` outside an interactive
/// terminal.
///
/// `prompt` is written as is: the caller puts its colours in it. `prompt_width` is its
/// *visible* width, colour sequences excluded — without it, the cursor would be put back in the
/// wrong place as soon as there is colour.
pub fn read_line(
    editor: &mut LineEditor,
    prompt: &str,
    prompt_width: usize,
    interactive: bool,
    shortcuts: &[char],
    level: UnicodeLevel,
) -> Input {
    if !interactive {
        return read_line_plain(prompt);
    }
    match read_line_raw(editor, prompt, prompt_width, shortcuts, level) {
        Ok(input) => input,
        // Raw mode can fail (terminal without the capability, descriptor redirected after the
        // fact): better input without shortcuts than an unusable session.
        Err(_) => read_line_plain(prompt),
    }
}

/// VT input on the Windows console.
///
/// `crossterm`'s raw mode is not enough here. Measured from Termius to `sshd` on Windows: a
/// left arrow arrives as **two** events, `[` then `D`, the escape that introduces them having
/// vanished on the way. Nothing then distinguishes the arrow from the text "[D", and the input
/// line fills with debris at every cursor movement.
///
/// `ENABLE_VIRTUAL_TERMINAL_INPUT` asks the console to pass the input through as the terminal
/// sends it, escape included. The previous mode is restored on drop: what Po borrows from the
/// terminal, Po gives back.
#[cfg(windows)]
mod vt_input {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
        SetConsoleMode,
    };

    pub struct Guard {
        handle: HANDLE,
        previous_mode: u32,
    }

    impl Guard {
        pub fn enter() -> Option<Self> {
            // SAFETY: `GetStdHandle` only returns a descriptor already held by the process; the
            // next two calls only write into `mode`.
            unsafe {
                let handle = GetStdHandle(STD_INPUT_HANDLE);
                if handle.is_null() {
                    return None;
                }
                let mut mode = 0u32;
                if GetConsoleMode(handle, &mut mode) == 0 {
                    return None;
                }
                if mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
                    return None; // already in the right mode: nothing to give back
                }
                if SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) == 0 {
                    return None;
                }
                Some(Self { handle, previous_mode: mode })
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: we restore exactly the mode read on entry, on the same descriptor.
            unsafe { SetConsoleMode(self.handle, self.previous_mode) };
        }
    }
}

#[cfg(not(windows))]
mod vt_input {
    pub struct Guard;

    impl Guard {
        /// On Unix systems, the terminal already delivers its sequences directly.
        pub fn enter() -> Option<Self> {
            None
        }
    }
}

/// An input event as the terminal sends it, before any interpretation.
///
/// Two fields suffice: the character received, or the named key when the system supplies one.
/// Keeping them separate lets the decoder reassemble escape sequences without depending on what
/// the platform was kind enough to recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawEvent {
    character: Option<char>,
    named: Option<Key>,
    ctrl: bool,
    /// True for a key release. Windows emits them, and some only come **as** releases:
    /// discarding them all would remove submission in an SSH session.
    release: bool,
}

impl RawEvent {
    fn character(c: char, ctrl: bool, release: bool) -> Self {
        Self { character: Some(c), named: None, ctrl, release }
    }

    fn named(key: Key, ctrl: bool, release: bool) -> Self {
        Self { character: None, named: Some(key), ctrl, release }
    }
}

/// Source of input events.
///
/// On Unix, `crossterm` does the job very well. On Windows, it emits **nothing** for the escape
/// byte in an SSH session — measured: a left arrow only arrives as `[` then `D`, indistinguishable
/// from typed text. The console records, however, carry it. So we read them ourselves, exactly
/// as Po already reads `/dev/tty` on Unix.
#[cfg(windows)]
mod source {
    use std::io;

    use windows_sys::Win32::System::Console::{
        GetStdHandle, INPUT_RECORD, KEY_EVENT, LEFT_CTRL_PRESSED, ReadConsoleInputW,
        RIGHT_CTRL_PRESSED, STD_INPUT_HANDLE, WINDOW_BUFFER_SIZE_EVENT,
    };

    use super::{RawEvent, Key};

    /// Next event, or `None` for a resize (the caller redraws).
    pub fn next() -> io::Result<Option<RawEvent>> {
        loop {
            // SAFETY: `GetStdHandle` returns a descriptor already held by the process, and
            // `ReadConsoleInputW` only writes into `record` and `read`, both local.
            let record: INPUT_RECORD = unsafe {
                let handle = GetStdHandle(STD_INPUT_HANDLE);
                let mut record: INPUT_RECORD = std::mem::zeroed();
                let mut read_count = 0u32;
                if ReadConsoleInputW(handle, &mut record, 1, &mut read_count) == 0 || read_count == 0 {
                    return Err(io::Error::last_os_error());
                }
                record
            };

            if record.EventType == WINDOW_BUFFER_SIZE_EVENT as u16 {
                return Ok(None);
            }
            if record.EventType != KEY_EVENT as u16 {
                continue;
            }
            // SAFETY: the event type has just been checked.
            let key_event = unsafe { record.Event.KeyEvent };
            let release = key_event.bKeyDown == 0;
            let ctrl = key_event.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;

            // SAFETY: the union only carries two representations of the same 16-bit word.
            let unit = unsafe { key_event.uChar.UnicodeChar };
            if unit != 0 {
                if let Some(c) = char::from_u32(unit as u32) {
                    return Ok(Some(RawEvent::character(c, ctrl, release)));
                }
                continue;
            }
            if let Some(key) = named_key(key_event.wVirtualKeyCode) {
                return Ok(Some(RawEvent::named(key, ctrl, release)));
            }
            // A lone modifier key (Shift, Ctrl...): nothing to do with it.
        }
    }

    /// Keys that have no associated character.
    fn named_key(code: u16) -> Option<Key> {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_RIGHT, VK_UP,
        };
        Some(match code {
            c if c == VK_LEFT => Key::Left,
            c if c == VK_RIGHT => Key::Right,
            c if c == VK_UP => Key::Up,
            c if c == VK_DOWN => Key::Down,
            c if c == VK_HOME => Key::Home,
            c if c == VK_END => Key::End,
            c if c == VK_DELETE => Key::Delete,
            _ => return None,
        })
    }
}

#[cfg(not(windows))]
mod source {
    use std::io;

    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

    use super::{RawEvent, Key};

    pub fn next() -> io::Result<Option<RawEvent>> {
        loop {
            let Event::Key(key_event) = event::read()? else {
                return Ok(None);
            };
            // AltGr arrives on Windows as Ctrl+Alt: it is typing (`#`, `@`, `{` on an AZERTY
            // keyboard), not a shortcut.
            let ctrl = crate::input::is_shortcut_ctrl(key_event.modifiers);
            let release = key_event.kind == KeyEventKind::Release;
            let raw = match key_event.code {
                KeyCode::Char(c) => RawEvent::character(c, ctrl, release),
                KeyCode::Esc => RawEvent::named(Key::Escape, ctrl, release),
                KeyCode::Enter => RawEvent::named(Key::Enter, ctrl, release),
                KeyCode::Backspace => RawEvent::named(Key::Backspace, ctrl, release),
                KeyCode::Delete => RawEvent::named(Key::Delete, ctrl, release),
                KeyCode::Left => RawEvent::named(Key::Left, ctrl, release),
                KeyCode::Right => RawEvent::named(Key::Right, ctrl, release),
                KeyCode::Home => RawEvent::named(Key::Home, ctrl, release),
                KeyCode::End => RawEvent::named(Key::End, ctrl, release),
                KeyCode::Up => RawEvent::named(Key::Up, ctrl, release),
                KeyCode::Down => RawEvent::named(Key::Down, ctrl, release),
                KeyCode::BackTab => RawEvent::named(Key::BackTab, ctrl, release),
                _ => continue,
            };
            return Ok(Some(raw));
        }
    }
}

fn read_line_plain(prompt: &str) -> Input {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => Input::Eof,
        Ok(_) => Input::Line(line.trim_end_matches(['\r', '\n']).to_owned()),
    }
}

fn read_line_raw(
    editor: &mut LineEditor,
    prompt: &str,
    prompt_width: usize,
    shortcuts: &[char],
    level: UnicodeLevel,
) -> io::Result<Input> {
    crossterm::terminal::enable_raw_mode()?;
    // Whatever happens next — error, panic, early return — raw mode is given back.
    let _raw = RawGuard;
    let _vt = vt_input::Guard::enter();

    let mut rows = (0, 0);
    draw(prompt, prompt_width, editor, level, &mut rows)?;
    loop {
        let raw = match source::next()? {
            Some(raw) => raw,
            // A resize shifts everything: we redraw rather than leave remnants.
            None => {
                draw(prompt, prompt_width, editor, level, &mut rows)?;
                continue;
            }
        };
        let release = raw.release;
        let Some(key) = editor.decoder.feed(raw) else { continue };
        // Windows emits a press **and** a release for the same key: keeping only one avoids
        // doubled characters. But in an SSH session, Enter only arrives as a release — measured
        // from Termius. Discarding every release made submission impossible remotely. So we only
        // discard a release that repeats the effect of the previous event, which covers both
        // cases in one go.
        if release && Some(key) == editor.previous_key {
            editor.previous_key = None;
            continue;
        }
        editor.previous_key = Some(key);
        match editor.apply(key, shortcuts) {
            Step::Ignored => {}
            Step::Redraw => draw(prompt, prompt_width, editor, level, &mut rows)?,
            Step::Done(input) => {
                // End the line on screen before handing control back: what follows is written in
                // normal mode, and must start from a clean column, below the whole line.
                leave(rows)?;
                return Ok(input);
            }
        }
    }
}

/// Rebuilds the editing keys from what the terminal actually sends.
///
/// Not every terminal presents named keys. Measured from Termius (Android) to `sshd` on
/// Windows, the console translates nothing at all: Enter arrives as `0x0D`, Backspace as
/// `0x7F`, and Left arrow as **three** events — `ESC`, `[`, `D`. Without this decoder, the left
/// arrow writes "[D" into the line instead of moving the cursor, and Enter never submits
/// anything. The harness would be unusable from a phone.
#[derive(Debug, Default)]
struct VtDecoder {
    /// Body of the sequence in progress, `ESC` excluded. Empty outside a sequence.
    pending: Option<String>,
}

impl VtDecoder {
    fn feed(&mut self, raw: RawEvent) -> Option<Key> {
        if let Some(body) = self.pending.as_mut() {
            let next_char = match raw.character {
                Some(c) => c,
                // A sequence interrupted by something else: we abandon it rather than insert its
                // debris into the line.
                None => {
                    self.pending = None;
                    return translate(raw);
                }
            };
            // An `ESC` only introduces a sequence if it is **immediately** followed by an
            // introducer. Without this condition, a lone Escape swallowed the next key: the
            // letter became the body of a sequence, `decode_sequence` recognised nothing in it,
            // and the character vanished without a trace. Measured from a psmux session — the
            // user opens the inventory, closes it with Escape, and the first letter is not
            // written. The price is `Alt+letter`, which nothing uses here.
            if body.is_empty() && !matches!(next_char, '[' | 'O') {
                self.pending = None;
                // A second escape is not the body of the first: it opens another one. The Windows
                // console emits press **and** release, hence two in a row.
                if next_char == '\u{1b}' {
                    self.pending = Some(String::new());
                    return None;
                }
                return translate(raw);
            }
            body.push(next_char);
            // Introducer, parameters and separators end nothing.
            if matches!(next_char, '[' | 'O' | '?' | ';' | '0'..='9') && body.len() < 16 {
                return None;
            }
            let sequence = std::mem::take(body);
            self.pending = None;
            return decode_sequence(&sequence);
        }

        // Escape comes in two forms depending on the terminal: named key or bare byte.
        // Recognising only one let "[D" be written into the line at every left arrow.
        if raw.named == Some(Key::Escape) || raw.character == Some('\u{1b}') {
            self.pending = Some(String::new());
            return None;
        }
        translate(raw)
    }
}

/// Translates the body of a sequence (without `ESC`) into an editing key.
///
/// Both forms are in use: `ESC [ D` in normal cursor mode, `ESC O D` in application mode — a
/// terminal can switch from one to the other during a session.
fn decode_sequence(body: &str) -> Option<Key> {
    let stripped = body.trim_start_matches(['[', 'O', '?']);
    Some(match stripped {
        "A" => Key::Up,
        "B" => Key::Down,
        "C" => Key::Right,
        "D" => Key::Left,
        "H" | "1~" | "7~" => Key::Home,
        "F" | "4~" | "8~" => Key::End,
        "3~" => Key::Delete,
        "Z" => Key::BackTab,
        // The rest — cursor position, function keys, wheel — makes no sense here. We discard it
        // silently: better a key with no effect than a stray character.
        _ => return None,
    })
}

fn translate(raw: RawEvent) -> Option<Key> {
    if let Some(named) = raw.named {
        return (named != Key::Escape).then_some(named);
    }
    let c = raw.character?;
    Some(match c {
        // Historical control characters are worth their key: `Ctrl+M` **is** the carriage
        // return, `Ctrl+H` the erase. A terminal that interprets nothing sends them in this
        // form, and the user did press Enter.
        '\r' | '\n' => Key::Enter,
        '\u{7f}' | '\u{8}' => Key::Backspace,
        // 0x01..0x1A: the Ctrl combinations, as a terminal sends them.
        '\u{1}'..='\u{1a}' => Key::Ctrl((b'a' + c as u8 - 1) as char),
        other if raw.ctrl => Key::Ctrl(other.to_ascii_lowercase()),
        other => Key::Char(other),
    })
}

/// Where a line longer than the screen puts things, in rows counted from its first one: the
/// cursor's row and column, the row the writing ends on, and whether that end needs a line
/// break to exist (text that exactly fills its last row leaves the cursor pending on it).
fn layout(prompt_width: usize, before_cursor: usize, total: usize, cols: usize) -> (usize, usize, usize, bool) {
    let cols = cols.max(1);
    let total = prompt_width + total;
    let cursor = prompt_width + before_cursor;
    let full = total > 0 && total.is_multiple_of(cols);
    (cursor / cols, cursor % cols, total / cols, full)
}

/// Rewrites the line from its first row: a line longer than the screen spans several, and
/// rewriting only the cursor's row left the first one behind at each key (observed: the start of
/// a long message repeated down the whole screen). `row` is the cursor's row, kept from the
/// previous draw, with the last row of the line.
fn draw(
    prompt: &str,
    prompt_width: usize,
    editor: &LineEditor,
    level: UnicodeLevel,
    rows: &mut (usize, usize),
) -> io::Result<()> {
    let mut out = io::stdout();
    let cols = crossterm::terminal::size().map_or(80, |(c, _)| usize::from(c));
    if rows.0 > 0 {
        write!(out, "\x1b[{}A", rows.0)?;
    }
    // Back to the start, and everything below erased: a shorter line leaves nothing behind.
    out.write_all(b"\r\x1b[J")?;
    write!(out, "{prompt}{}", editor.buffer())?;
    let before = display_width(&editor.buffer()[..editor.cursor()], level);
    let (cursor_row, cursor_col, end_row, full) = layout(prompt_width, before, display_width(editor.buffer(), level), cols);
    if full {
        out.write_all(b"\r\n")?;
    }
    if end_row > cursor_row {
        write!(out, "\x1b[{}A", end_row - cursor_row)?;
    }
    // Columns are 1-based in the sequence, hence the +1.
    write!(out, "\r\x1b[{}G", cursor_col + 1)?;
    *rows = (cursor_row, end_row);
    out.flush()
}

/// Puts the cursor below the line, wherever it was in it, before handing control back.
fn leave(rows: (usize, usize)) -> io::Result<()> {
    let mut out = io::stdout();
    let (cursor_row, end_row) = rows;
    if end_row > cursor_row {
        write!(out, "\x1b[{}B", end_row - cursor_row)?;
    }
    out.write_all(b"\r\n")?;
    out.flush()
}

/// Gives raw mode back on drop, error paths included.
struct RawGuard;

impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Ctrl in the "shortcut" sense. On Windows, AltGr is reported as Ctrl+Alt: on an AZERTY
/// keyboard, `#`, `@`, `{`, `}`, `[`, `]`, `|`, `\`, `~` and `€` are obtained this way. Treating
/// them as shortcuts made them impossible to type (observed: `# Bench` became ` Bench`).
pub fn is_shortcut_ctrl(modifiers: crossterm::event::KeyModifiers) -> bool {
    use crossterm::event::KeyModifiers;
    modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHORTCUTS: [char; 4] = ['b', 'o', 't', 'g'];

    fn type_text(editor: &mut LineEditor, text: &str) {
        for c in text.chars() {
            editor.apply(Key::Char(c), &SHORTCUTS);
        }
    }

    #[test]
    fn a_line_longer_than_the_screen_is_laid_out_over_its_rows() {
        // 2 of prompt + 8 of text on 10 columns: exactly one full row, the end needs a break.
        assert_eq!(layout(2, 8, 8, 10), (1, 0, 1, true));
        // 2 + 15 on 10 columns, cursor after 3 characters: first row, column 5; end on row 1.
        assert_eq!(layout(2, 3, 15, 10), (0, 5, 1, false));
        // Cursor at the end of 2 + 25 on 10 columns: row 2, column 7.
        assert_eq!(layout(2, 25, 25, 10), (2, 7, 2, false));
        assert_eq!(layout(2, 0, 0, 0), (2, 0, 2, true), "a zero width does not divide by zero");
    }

    /// Event as delivered by a console that translates nothing: a bare character.
    fn raw(c: char) -> RawEvent {
        RawEvent::character(c, false, false)
    }

    fn esc() -> RawEvent {
        RawEvent::named(Key::Escape, false, false)
    }

    fn named(key: Key) -> RawEvent {
        RawEvent::named(key, false, false)
    }

    #[test]
    fn enter_and_erase_sometimes_arrive_as_bare_bytes() {
        // Measured from Termius to sshd on Windows: `key=None char=0x0D` for Enter, `char=0x7F`
        // for Backspace. Without translation, one submits nothing and the other inserts an
        // invisible character.
        let mut decoder = VtDecoder::default();
        assert_eq!(decoder.feed(raw('\r')), Some(Key::Enter));
        assert_eq!(decoder.feed(raw('\n')), Some(Key::Enter));
        assert_eq!(decoder.feed(raw('\u{7f}')), Some(Key::Backspace));
        assert_eq!(decoder.feed(raw('\u{8}')), Some(Key::Backspace));
    }

    #[test]
    fn an_arrow_arrives_as_three_events() {
        // `ESC` `[` `D`: the same left arrow, chopped up by the console. The first two events
        // must produce nothing, the third the whole key.
        let mut decoder = VtDecoder::default();
        assert_eq!(decoder.feed(esc()), None);
        assert_eq!(decoder.feed(raw('[')), None);
        assert_eq!(decoder.feed(raw('D')), Some(Key::Left));

        for (final_byte, expected) in
            [('A', Key::Up), ('B', Key::Down), ('C', Key::Right), ('D', Key::Left)]
        {
            let mut decoder = VtDecoder::default();
            decoder.feed(esc());
            decoder.feed(raw('['));
            assert_eq!(decoder.feed(raw(final_byte)), Some(expected));
        }
    }

    #[test]
    fn application_mode_is_recognised_too() {
        // `ESC O D`: the same arrow, when the terminal has switched to application cursor mode.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        decoder.feed(raw('O'));
        assert_eq!(decoder.feed(raw('D')), Some(Key::Left));
    }

    #[test]
    fn sequences_with_parameters_are_read_whole() {
        // `ESC [ 3 ~`: Delete. The parameter does not end the sequence.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        decoder.feed(raw('['));
        assert_eq!(decoder.feed(raw('3')), None);
        assert_eq!(decoder.feed(raw('~')), Some(Key::Delete));
    }

    #[test]
    fn an_unknown_sequence_leaves_no_debris() {
        // A function key makes no sense in an input line; what matters is that it does not
        // write "[15~" instead.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        for c in ['[', '1', '5', '~'] {
            let output = decoder.feed(raw(c));
            assert!(output.is_none(), "'{c}' must produce nothing");
        }
        // The decoder is available again for the next key.
        assert_eq!(decoder.feed(raw('a')), Some(Key::Char('a')));
    }

    #[test]
    fn a_lone_escape_does_not_eat_the_next_key() {
        // Defect measured from a psmux session: after an Escape, the next character vanished.
        // The decoder was waiting for the rest of a sequence, and took the letter for its body.
        // An Escape only introduces a sequence when followed by an introducer.
        let mut decoder = VtDecoder::default();
        assert_eq!(decoder.feed(esc()), None);
        assert_eq!(decoder.feed(raw('Z')), Some(Key::Char('Z')));
        assert_eq!(decoder.feed(raw('Y')), Some(Key::Char('Y')));
    }

    #[test]
    fn a_lone_escape_still_lets_a_shortcut_through() {
        // Ctrl+G survived the defect — which is what made it confusing: the keyboard looked
        // alive. It must keep going through, now for the right reason.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        assert_eq!(decoder.feed(raw('\u{7}')), Some(Key::Ctrl('g')));
    }

    #[test]
    fn two_escapes_in_a_row_eat_nothing_either() {
        // On the Windows console, Escape arrives as press **and** release: two escape bytes in a
        // row. The second must not become the body of the first.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        assert_eq!(decoder.feed(esc()), None);
        assert_eq!(decoder.feed(raw('a')), Some(Key::Char('a')));
    }

    #[test]
    fn an_arrow_still_works_after_a_lone_escape() {
        // The fix must not cost the sequences: ESC [ D remains a left arrow.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        decoder.feed(raw('x'));
        decoder.feed(esc());
        assert_eq!(decoder.feed(raw('[')), None);
        assert_eq!(decoder.feed(raw('D')), Some(Key::Left));
    }

    #[test]
    fn a_truncated_sequence_does_not_block_input() {
        // The user presses Escape, changes their mind, and types. The character must go through.
        let mut decoder = VtDecoder::default();
        decoder.feed(esc());
        assert_eq!(decoder.feed(raw('[')), None);
        // A named event interrupts the sequence: we honour it rather than lose it.
        assert_eq!(decoder.feed(named(Key::Enter)), Some(Key::Enter));
    }

    #[test]
    fn a_line_is_typed_and_submitted() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "hello");
        assert_eq!(
            editor.apply(Key::Enter, &SHORTCUTS),
            Step::Done(Input::Line("hello".into()))
        );
        assert_eq!(editor.buffer(), "", "the submitted line must not remain");
    }

    #[test]
    fn the_cursor_does_not_cut_an_accented_character() {
        // The classic trap: indexing a UTF-8 string by bytes. An "é" takes two bytes, an
        // ideograph three, an emoji four — a single missed boundary and the program panics in
        // the middle of typing.
        let mut editor = LineEditor::new();
        type_text(&mut editor, "café日本🚀");
        editor.apply(Key::Backspace, &SHORTCUTS);
        assert_eq!(editor.buffer(), "café日本");
        editor.apply(Key::Backspace, &SHORTCUTS);
        assert_eq!(editor.buffer(), "café日");
        editor.apply(Key::Left, &SHORTCUTS);
        editor.apply(Key::Left, &SHORTCUTS);
        assert_eq!(editor.cursor(), 3, "two characters back from \"café日\"");
        editor.apply(Key::Backspace, &SHORTCUTS);
        assert_eq!(editor.buffer(), "caé日");
    }

    #[test]
    fn a_declared_shortcut_interrupts_input_without_losing_it() {
        // Changing mode mid-sentence must not erase what is already typed: it is precisely the
        // gesture one wants to make *while* writing.
        let mut editor = LineEditor::new();
        type_text(&mut editor, "analyse this");
        assert_eq!(
            editor.apply(Key::Ctrl('b'), &SHORTCUTS),
            Step::Done(Input::Shortcut('b'))
        );
        assert_eq!(editor.buffer(), "analyse this", "the current line is preserved");
    }

    #[test]
    fn an_undeclared_control_is_ignored() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "x");
        assert_eq!(editor.apply(Key::Ctrl('z'), &SHORTCUTS), Step::Ignored);
        assert_eq!(editor.buffer(), "x");
    }

    #[test]
    fn ctrl_d_only_quits_on_an_empty_line() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "abc");
        editor.apply(Key::Home, &SHORTCUTS);
        assert_eq!(editor.apply(Key::Ctrl('d'), &SHORTCUTS), Step::Redraw);
        assert_eq!(editor.buffer(), "bc", "in the middle, it deletes forward");
        editor.clear();
        assert_eq!(editor.apply(Key::Ctrl('d'), &SHORTCUTS), Step::Done(Input::Eof));
    }

    #[test]
    fn ctrl_c_abandons_the_line_without_ending_the_session() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "a regretted sentence");
        assert_eq!(
            editor.apply(Key::Ctrl('c'), &SHORTCUTS),
            Step::Done(Input::Interrupt)
        );
        assert_eq!(editor.buffer(), "");
    }

    #[test]
    fn history_goes_up_and_down_without_losing_the_current_line() {
        let mut editor = LineEditor::new();
        editor.remember("first");
        editor.remember("second");
        type_text(&mut editor, "draft");

        editor.apply(Key::Up, &SHORTCUTS);
        assert_eq!(editor.buffer(), "second");
        editor.apply(Key::Up, &SHORTCUTS);
        assert_eq!(editor.buffer(), "first");
        editor.apply(Key::Down, &SHORTCUTS);
        assert_eq!(editor.buffer(), "second");
        editor.apply(Key::Down, &SHORTCUTS);
        assert_eq!(editor.buffer(), "draft", "the line set aside comes back intact");
    }

    #[test]
    fn history_refuses_duplicates_and_empty_lines() {
        let mut editor = LineEditor::new();
        editor.remember("same");
        editor.remember("same");
        editor.remember("   ");
        editor.apply(Key::Up, &SHORTCUTS);
        assert_eq!(editor.buffer(), "same");
        editor.apply(Key::Up, &SHORTCUTS);
        assert_eq!(editor.buffer(), "same", "a single element in the history");
    }

    #[test]
    fn ctrl_w_erases_the_previous_word() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "po local starts  ");
        editor.apply(Key::Ctrl('w'), &SHORTCUTS);
        assert_eq!(editor.buffer(), "po local ");
        editor.apply(Key::Ctrl('w'), &SHORTCUTS);
        assert_eq!(editor.buffer(), "po ");
    }

    #[test]
    fn ctrl_u_and_ctrl_k_cut_on_either_side_of_the_cursor() {
        let mut editor = LineEditor::new();
        type_text(&mut editor, "left|right");
        for _ in 0.."right".len() {
            editor.apply(Key::Left, &SHORTCUTS);
        }
        editor.apply(Key::Ctrl('k'), &SHORTCUTS);
        assert_eq!(editor.buffer(), "left|");
        editor.apply(Key::Ctrl('u'), &SHORTCUTS);
        assert_eq!(editor.buffer(), "");
    }

    #[test]
    fn a_shortcut_can_prepare_a_command_without_running_it() {
        // Swapping a multi-gigabyte model with a single key would be dangerous: the shortcut
        // pre-fills, the user confirms.
        let mut editor = LineEditor::new();
        editor.prefill("/local ");
        assert_eq!(editor.buffer(), "/local ");
        assert_eq!(editor.cursor(), "/local ".len());
        type_text(&mut editor, "2");
        assert_eq!(
            editor.apply(Key::Enter, &SHORTCUTS),
            Step::Done(Input::Line("/local 2".into()))
        );
    }

    #[test]
    fn bounds_do_not_panic() {
        // Every movement key on an empty line: it is the first gesture of a user discovering
        // the prompt.
        let mut editor = LineEditor::new();
        for key in [
            Key::Left,
            Key::Right,
            Key::Home,
            Key::End,
            Key::Up,
            Key::Down,
            Key::Backspace,
            Key::Delete,
            Key::Ctrl('w'),
            Key::Ctrl('u'),
            Key::Ctrl('k'),
        ] {
            assert_eq!(editor.apply(key, &SHORTCUTS), Step::Ignored, "{key:?}");
        }
        assert_eq!(editor.buffer(), "");
    }
}
