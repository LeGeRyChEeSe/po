//! Displayable state of a session.
//!
//! Kept apart from rendering so it can be tested without a terminal: this module decides
//! *what* to show, `view` decides *how*. The decisions that matter - which line is a fold, what
//! the status bar carries, how a long exchange folds - can therefore be checked with ordinary
//! tests.

use po_core::Ledger;
use po_router::BudgetMode;
use po_term::UnicodeLevel;
use po_term::width::{display_width, truncate, truncate_with_ellipsis};

/// Kind of a line, which determines its color and prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// User input.
    User,
    /// Model answer.
    Assistant,
    /// Finished tool call.
    Tool,
    /// Tool call in progress: the line is rewritten in place when it ends.
    ToolRunning,
    /// Delegation to a subagent.
    Delegation,
    /// Harness information: compaction, routing fallback, server started.
    Notice,
    Error,
    /// End-of-turn recap.
    Summary,
    /// Model reasoning: folded by default, expanded with Ctrl+O.
    Thinking,
    /// Header of a reasoning block: duration and volume, always visible.
    ThinkingSummary,
    /// Tool call that ended in failure.
    ToolError,
    /// Title of a card: boxed report of a command.
    CardTitle,
    /// Line of a card.
    Card,
    /// Line of a card reporting a problem.
    CardWarn,
    /// Subheading inside a card.
    CardSection,
    /// List item: its name (`text`) and its description (`detail`).
    CardItem,
    /// Field: a key (`text`) and its value (`detail`), aligned in columns.
    CardField,
    /// Discreet remark at the foot of a card.
    CardDim,
    /// An image of the conversation (`text`: its full path): a thumbnail and a line to open it.
    Image,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
    /// Associated duration (tool call), for the totals of a folded group.
    pub secs: Option<f64>,
    /// Folded content (text of a reasoning block received in one piece), shown with Ctrl+O.
    pub detail: Option<String>,
    /// Progress of a subagent, rewritten while it works ("I am reading the sources").
    pub progress: Option<String>,
}

impl Line {
    pub fn new(kind: LineKind, text: impl Into<String>) -> Self {
        Self { kind, text: text.into(), secs: None, detail: None, progress: None }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_progress(mut self, progress: impl Into<String>) -> Self {
        self.progress = Some(progress.into());
        self
    }

    pub fn with_secs(mut self, secs: f64) -> Self {
        self.secs = Some(secs);
        self
    }

    /// Displayed prefix. Plain ASCII to stay readable on a terminal without Unicode; the view
    /// substitutes richer symbols when the terminal renders them correctly.
    pub fn marker(&self) -> &'static str {
        match self.kind {
            LineKind::User => ">",
            LineKind::Assistant => "",
            LineKind::Tool => "-",
            LineKind::ToolRunning => "~",
            LineKind::Delegation => "->",
            LineKind::Notice => "i",
            LineKind::Image => "#",
            LineKind::Error => "!",
            LineKind::Summary => "=",
            LineKind::Thinking | LineKind::ThinkingSummary => "~",
            LineKind::ToolError => "x",
            LineKind::CardTitle
            | LineKind::Card
            | LineKind::CardWarn
            | LineKind::CardSection
            | LineKind::CardItem
            | LineKind::CardField
            | LineKind::CardDim => "|",
        }
    }
}

/// What the status bar carries. It permanently answers three questions: who is answering, in
/// which mode, and how much it costs.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub model: String,
    pub mode: Option<BudgetMode>,
    pub local_server: Option<String>,
    /// The session's local model is in memory (`Some(false)`: unloaded, it loads at the next
    /// request); `None` for a cloud model or before the first look.
    pub model_loaded: Option<bool>,
    pub vram_free_mib: Option<u64>,
    pub tokens: u64,
    pub cost: f64,
    pub local_share: f64,
    /// True while a turn is in progress: input is then refused.
    pub busy: bool,
    /// Tokens sent with the next request, and the session model's window.
    pub context: Option<(u32, u32)>,
}

/// Fill gauge: "▰▰▱▱▱ 23 %", or in ASCII "[##---] 23%".
pub fn gauge(used: u32, window: u32, unicode: bool) -> String {
    if window == 0 {
        return String::new();
    }
    let ratio = (used as f64 / window as f64).clamp(0.0, 1.0);
    let filled = (ratio * 5.0).ceil() as usize;
    let percent = (ratio * 100.0).round() as u32;
    if unicode {
        format!("{}{} {percent} %", "▰".repeat(filled), "▱".repeat(5 - filled))
    } else {
        format!("[{}{}] {percent}%", "#".repeat(filled), "-".repeat(5 - filled))
    }
}

impl Status {
    pub fn update_from_ledger(&mut self, ledger: &Ledger) {
        self.tokens = ledger.total_tokens();
        self.cost = ledger.total_cost();
        self.local_share = ledger.local_share();
    }

    /// Bar segments, from most to least important. The view drops them from the end when width
    /// is short, rather than truncating in the middle of a number.
    pub fn segments(&self) -> Vec<String> {
        let mut segments = Vec::new();
        if !self.model.is_empty() {
            segments.push(self.model.clone());
            if self.model_loaded == Some(false) {
                segments.push(po_i18n::t!("tui.status.unloaded").to_owned());
            }
        }
        if let Some(mode) = self.mode {
            segments.push(mode.label().to_owned());
        }
        if self.tokens > 0 {
            segments.push(format!("{} tok", self.tokens));
            if self.cost > 0.0 {
                segments.push(format!("${:.4}", self.cost));
            }
            segments.push(format!("{:.0}% local", self.local_share * 100.0));
        }
        // The local server often names the very model shown first: said once is enough.
        if let Some(server) = self.local_server.as_ref().filter(|s| !self.model.contains(s.as_str())) {
            segments.push(server.clone());
        }
        if let Some(vram) = self.vram_free_mib {
            segments.push(po_i18n::t!("tui.status.vram", mib = vram));
        }
        segments
    }

    /// Joins the segments that fit in `width` columns.
    pub fn render(&self, width: usize) -> String {
        let segments = self.segments();
        let mut out = String::new();
        for segment in segments {
            let candidate_len = out.chars().count() + segment.chars().count() + 3;
            if !out.is_empty() && candidate_len > width {
                break;
            }
            if !out.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(&segment);
        }
        out
    }
}

/// Neutralizes what would break the layout or repaint the screen.
/// The rule is shared with the streaming output, hence its place in `po-term`.
fn sanitize(text: &str) -> String {
    po_term::width::sanitize_line(text)
}

/// Number of lines kept. Beyond that, the oldest drop out: a long session must not grow memory
/// indefinitely, and the full history lives in the session.
const MAX_LINES: usize = 5000;

#[derive(Debug, Default)]
pub struct Transcript {
    lines: Vec<Line>,
    /// Lines removed from the top since the start: converts a stable absolute position into a
    /// current index.
    dropped: usize,
    /// Scroll offset from the bottom. Zero means "stuck to the last message".
    scroll: std::cell::Cell<usize>,
}

impl Transcript {
    /// Clears all lines and resets scrolling to the bottom.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped = 0;
        self.scroll.set(0);
    }

    /// Adds a line. A text containing line breaks is split: rendering assumes a `Line` takes one
    /// logical line, and a break slipping in - exotic model name, tool output, message from a
    /// third-party server - would shift the whole display from there on.
    pub fn push(&mut self, line: Line) {
        if line.text.contains('\n') || line.text.contains('\r') {
            let kind = line.kind;
            for part in line.text.split(['\n', '\r']).filter(|p| !p.is_empty()) {
                self.push_single(Line {
                    kind,
                    text: sanitize(part),
                    secs: line.secs,
                    detail: line.detail.clone(),
                    progress: line.progress.clone(),
                });
            }
            return;
        }
        self.push_single(Line { text: sanitize(&line.text), ..line });
    }

    fn push_single(&mut self, line: Line) {
        self.lines.push(line);
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(..excess);
            self.dropped += excess;
        }
    }

    /// Absolute position of the next line, valid even after old lines have dropped off the top.
    pub fn next_position(&self) -> usize {
        self.dropped + self.lines.len()
    }

    /// Rewrites the line at an absolute position, if it is still there. A rewritten line does
    /// not move: the eye finds it where it left it.
    pub fn replace_at(&mut self, position: usize, line: Line) -> bool {
        let Some(index) = position.checked_sub(self.dropped) else {
            return false;
        };
        match self.lines.get_mut(index) {
            Some(slot) => {
                *slot = Line {
                    kind: line.kind,
                    text: sanitize(&line.text.replace(['\n', '\r'], " ")),
                    secs: line.secs,
                    detail: line.detail,
                    progress: line.progress,
                };
                true
            }
            None => false,
        }
    }

    /// Line at an absolute position, if it is still there.
    pub fn get(&self, position: usize) -> Option<&Line> {
        self.lines.get(position.checked_sub(self.dropped)?)
    }

    /// Adds a multi-line text as as many lines of the same kind.
    pub fn push_text(&mut self, kind: LineKind, text: &str) {
        if text.is_empty() {
            self.push(Line::new(kind, ""));
            return;
        }
        for line in text.lines() {
            self.push(Line::new(kind, line));
        }
    }

    /// Lines dropped off the top: added to an index, gives an absolute position.
    pub fn offset(&self) -> usize {
        self.dropped
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// Replaces the text of the last line. Used to rewrite the current line while an answer
    /// arrives in fragments.
    pub fn replace_last(&mut self, text: String) {
        if let Some(last) = self.lines.last_mut() {
            last.text = sanitize(&text);
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn scroll(&self) -> usize {
        self.scroll.get()
    }

    /// Sets the scroll from the view: when a block expands under the finger, the view knows how
    /// far to shift so the touched line stays in place.
    pub fn set_scroll(&self, rows: usize) {
        self.scroll.set(rows);
    }

    pub fn is_at_bottom(&self) -> bool {
        self.scroll.get() == 0
    }

    /// Scrolls up by `amount` lines, without going past the start.
    pub fn scroll_up(&mut self, amount: usize, viewport: usize) {
        let max = self.lines.len().saturating_sub(viewport);
        self.scroll.set((self.scroll.get() + amount).min(max));
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll.set(self.scroll.get().saturating_sub(amount));
    }

    /// Scroll counted in screen rows, bounded by what the view measured.
    pub fn scroll_rows(&mut self, delta: isize, max: usize) {
        let current = self.scroll.get();
        self.scroll.set(if delta < 0 { current.saturating_sub(delta.unsigned_abs()) } else { (current + delta as usize).min(max) });
    }

    /// Brings the scroll back within bounds after a change of content or size.
    pub fn clamp_scroll(&mut self, max: usize) {
        self.scroll.set(self.scroll.get().min(max));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll.set(0);
    }

    /// Visible window of `height` lines, given the scroll.
    pub fn visible(&self, height: usize) -> &[Line] {
        if self.lines.len() <= height {
            return &self.lines;
        }
        let end = self.lines.len() - self.scroll.get();
        let start = end.saturating_sub(height);
        &self.lines[start..end]
    }
}

/// Input area: a buffer with a cursor, and a recallable history.
#[derive(Debug, Default)]
pub struct Input {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    /// Position in the history; `None` means a fresh input is being edited.
    history_index: Option<usize>,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.buffer
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.trim().is_empty()
    }

    pub fn insert(&mut self, c: char) {
        // The cursor counts characters, not bytes: inserting after an accent must land in the
        // right place.
        let byte = self.byte_offset(self.cursor);
        self.buffer.insert(byte, c);
        self.cursor += 1;
    }

    /// Inserts pasted text. Windows line endings are reduced to `\n`, a tab becomes four
    /// spaces, and any other control character is dropped: a paste must not be able to drive
    /// the terminal through the input area.
    pub fn insert_str(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in normalized.chars() {
            match c {
                '\n' => self.insert('\n'),
                '\t' => (0..4).for_each(|_| self.insert(' ')),
                c if c.is_control() => {}
                c => self.insert(c),
            }
        }
    }

    /// Ctrl+U: deletes from the start of the current line to the cursor.
    pub fn kill_to_start(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let start = chars[..self.cursor].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
        self.buffer = chars[..start].iter().chain(&chars[self.cursor..]).collect();
        self.cursor = start;
    }

    /// Ctrl+K: deletes from the cursor to the end of the current line.
    pub fn kill_to_end(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let end = chars[self.cursor..].iter().position(|c| *c == '\n').map(|i| self.cursor + i).unwrap_or(chars.len());
        self.buffer = chars[..self.cursor].iter().chain(&chars[end..]).collect();
    }

    /// Ctrl+W: deletes the word before the cursor.
    pub fn delete_word_back(&mut self) {
        let chars: Vec<char> = self.buffer.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        self.buffer = chars[..start].iter().chain(&chars[self.cursor..]).collect();
        self.cursor = start;
    }

    /// Replaces the whole input, cursor included (completion).
    pub fn replace(&mut self, text: String, cursor: usize) {
        self.cursor = cursor.min(text.chars().count());
        self.buffer = text;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte = self.byte_offset(self.cursor - 1);
        self.buffer.remove(byte);
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.buffer.chars().count() {
            return;
        }
        let byte = self.byte_offset(self.cursor);
        self.buffer.remove(byte);
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buffer.chars().count());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.buffer.chars().count();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    /// Submits the input: returns it, stores it in the history and empties the buffer.
    pub fn submit(&mut self) -> String {
        let text = self.buffer.trim().to_owned();
        if !text.is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.clear();
        text
    }

    /// Recalls the previous input.
    pub fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(current) => current - 1,
        };
        self.history_index = Some(index);
        self.buffer = self.history[index].clone();
        self.cursor = self.buffer.chars().count();
    }

    /// Goes back down the history, until an empty input is reached.
    pub fn history_next(&mut self) {
        let Some(current) = self.history_index else {
            return;
        };
        if current + 1 >= self.history.len() {
            self.history_index = None;
            self.buffer.clear();
            self.cursor = 0;
            return;
        }
        self.history_index = Some(current + 1);
        self.buffer = self.history[current + 1].clone();
        self.cursor = self.buffer.chars().count();
    }

    fn byte_offset(&self, char_index: usize) -> usize {
        self.buffer.char_indices().nth(char_index).map(|(offset, _)| offset).unwrap_or(self.buffer.len())
    }
}

/// A local model as the picker displays it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModel {
    /// File name, without the path or extension.
    pub label: String,
    /// Size and launch plan, computed on the machine's actual hardware.
    pub detail: String,
    /// Is this the model the server is serving right now?
    pub loaded: bool,
    /// False when the launch plan is not viable here: it is still displayed, but the user is
    /// not allowed to pick it believing it will work.
    pub viable: bool,
}

/// Local model picker, displayed as an overlay.
///
/// The requirement is to swap models "just by selecting them". Typing a number is already
/// better than composing a `llama-server` command line, but it is not *selecting*: you must
/// read the list, memorize a digit, then type it. The arrows and Enter remove those three
/// steps.
#[derive(Debug, Clone, Default)]
pub struct Picker {
    pub entries: Vec<LocalModel>,
    selected: usize,
    /// Server state at the time of opening, shown at the foot of the picker.
    pub server: String,
}

impl Picker {
    pub fn new(entries: Vec<LocalModel>, server: impl Into<String>) -> Self {
        // Open on the already loaded model rather than the first of the list: it is where the
        // user starts from, and seeing it highlighted immediately shows where things stand.
        let selected = entries.iter().position(|e| e.loaded).unwrap_or(0);
        Self { entries, selected, server: server.into() }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn current(&self) -> Option<&LocalModel> {
        self.entries.get(self.selected)
    }

    /// Moves the selection. The list **does not wrap**: on a phone screen where only three
    /// lines fit, a silent wrap-around makes you lose track of the position.
    pub fn move_by(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(d.unsigned_abs() as usize),
            d => (self.selected + d as usize).min(last),
        };
    }
}

/// Width reserved for the model name in the picker, markers included.
const LABEL_COLS: usize = 38;

impl Picker {
    /// Composes a picker row, bounded to the width actually available.
    ///
    /// Quantized model names are long - `Hermes3.6-35B-A3B-Uncensored-Genesis-V6-APEX-Compact`
    /// is 52 columns - and a name left free pushes the rest out of the frame. The cut is made
    /// in **display width**, not in bytes or characters: an ideograph takes two columns, and
    /// miscounting shifts the whole border.
    pub fn row(&self, index: usize, width: usize, level: UnicodeLevel, cursor: &str) -> (String, String) {
        let Some(entry) = self.entries.get(index) else {
            return (String::new(), String::new());
        };
        let mark = if index == self.selected { cursor } else { " " };
        // What is already running is marked: without it, the model gets relaunched in place.
        let state = if entry.loaded { "● " } else { "  " };
        let prefix = format!(" {mark} {state}");
        let remaining = LABEL_COLS.saturating_sub(display_width(&prefix, level));
        let name = truncate(&entry.label, remaining, level);
        let padding = remaining.saturating_sub(display_width(name, level));
        let left = format!("{prefix}{name}{}", " ".repeat(padding));

        // The right column takes what is left, one space of margin included. The cut is
        // marked: without an indicator, "32768 to" reads as a display bug rather than a row that
        // is too narrow. Seen in a 74-column window.
        let available = width.saturating_sub(LABEL_COLS + 1);
        let right = truncate_with_ellipsis(&entry.detail, available, level);
        (left, right)
    }

    /// The rows of one entry: the compact row when its name and detail fit in their columns,
    /// else the name whole (over as many rows as it needs) and the detail on its own row under
    /// it - a long model name is never cut.
    pub fn rows(&self, index: usize, width: usize, level: UnicodeLevel, cursor: &str) -> Vec<(String, String)> {
        let Some(entry) = self.entries.get(index) else { return Vec::new() };
        let mark = if index == self.selected { cursor } else { " " };
        let state = if entry.loaded { "● " } else { "  " };
        let prefix = format!(" {mark} {state}");
        let name_room = LABEL_COLS.saturating_sub(display_width(&prefix, level));
        let detail_room = width.saturating_sub(LABEL_COLS + 1);
        if display_width(&entry.label, level) <= name_room && display_width(&entry.detail, level) <= detail_room {
            return vec![self.row(index, width, level, cursor)];
        }
        let indent = display_width(&prefix, level);
        let room = width.saturating_sub(indent).max(4);
        let mut rows = Vec::new();
        let mut line = String::new();
        let mut used = 0;
        for c in entry.label.chars() {
            let w = display_width(c.encode_utf8(&mut [0; 4]), level);
            if used + w > room && !line.is_empty() {
                rows.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(c);
            used += w;
        }
        rows.push(line);
        let pad = " ".repeat(indent);
        let mut out: Vec<(String, String)> = rows.into_iter().enumerate().map(|(i, piece)| (format!("{}{piece}", if i == 0 { &prefix } else { &pad }), String::new())).collect();
        if !entry.detail.is_empty() {
            out.push((pad, truncate_with_ellipsis(&entry.detail, room, level)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_too_long_name_does_not_overflow_the_frame() {
        // Real name read on the machine: 52 columns, against 38 for the name column.
        let picker = Picker::new(
            vec![LocalModel {
                label: "Hermes3.6-35B-A3B-Uncensored-Genesis-V6-APEX-Compact".into(),
                detail: "16587 Mio · CPU · 32768 tok".into(),
                loaded: false,
                viable: true,
            }],
            "ready",
        );
        let (left, right) = picker.row(0, 72, UnicodeLevel::Full, "›");
        assert_eq!(display_width(&left, UnicodeLevel::Full), 38);
        assert!(
            display_width(&left, UnicodeLevel::Full) + display_width(&right, UnicodeLevel::Full) <= 72,
            "the whole row must fit in the given width"
        );
    }

    #[test]
    fn a_truncated_right_column_shows_it() {
        // 50 columns: the modal has only 11 columns left for the detail. Without an indicator,
        // "32768 to" reads as a display bug. Seen in a 74-column psmux window, where the detail
        // stopped in the middle of a word.
        let picker = Picker::new(
            vec![LocalModel {
                label: "gemma-4-E4B-it-Q4_K_M".into(),
                detail: "4746 Mio · GPU · 32768 tok".into(),
                loaded: false,
                viable: true,
            }],
            "ready",
        );
        let (_, right) = picker.row(0, 50, UnicodeLevel::Full, "›");
        assert!(right.ends_with('…'), "the cut must be visible: \"{right}\"");
        assert!(display_width(&right, UnicodeLevel::Full) <= 11);
    }

    #[test]
    fn a_short_name_is_padded_to_align_the_column() {
        let picker = Picker::new(
            vec![
                LocalModel { label: "a".into(), detail: "1 Mio".into(), loaded: false, viable: true },
                LocalModel {
                    label: "gemma-4-E4B-it-Q4_K_M".into(),
                    detail: "4746 Mio".into(),
                    loaded: false,
                    viable: true,
                },
            ],
            "ready",
        );
        let a = picker.row(0, 72, UnicodeLevel::Full, "›").0;
        let b = picker.row(1, 72, UnicodeLevel::Full, "›").0;
        assert_eq!(
            display_width(&a, UnicodeLevel::Full),
            display_width(&b, UnicodeLevel::Full),
            "details must start at the same column"
        );
    }

    #[test]
    fn the_picker_fits_a_phone_screen() {
        // 40 columns: the width of a Termux terminal in portrait. The right column almost
        // disappears, but nothing overflows.
        let picker = Picker::new(
            vec![LocalModel {
                label: "Qwen3.8-27B-UD-IQ2_XXS".into(),
                detail: "8592 Mio · GPU 65/65 · 32768 tok".into(),
                loaded: true,
                viable: true,
            }],
            "ready",
        );
        let (left, right) = picker.row(0, 40, UnicodeLevel::Full, "›");
        let total = display_width(&left, UnicodeLevel::Full) + display_width(&right, UnicodeLevel::Full);
        assert!(total <= 40, "{total} columns for 40 available");
        assert!(left.contains('●'), "the loaded model stays flagged");
    }

    #[test]
    fn a_missing_row_does_not_panic() {
        let picker = Picker::new(Vec::new(), "stopped");
        assert_eq!(picker.row(0, 72, UnicodeLevel::Full, "›"), (String::new(), String::new()));
    }

    #[test]
    fn the_picker_opens_on_the_already_loaded_model() {
        // Opening on the first of the list would make the user think they are going to swap
        // while they are looking at something other than what is running.
        let picker = Picker::new(vec![model("a", false), model("b", true), model("c", false)], "ready");
        assert_eq!(picker.selected(), 1);
        assert_eq!(picker.current().unwrap().label, "b");
    }

    #[test]
    fn without_a_loaded_model_the_picker_starts_from_the_first() {
        let picker = Picker::new(vec![model("a", false), model("b", false)], "stopped");
        assert_eq!(picker.selected(), 0);
    }

    #[test]
    fn the_selection_neither_wraps_nor_overflows() {
        let mut picker = Picker::new(vec![model("a", false), model("b", false), model("c", false)], "stopped");
        picker.move_by(-1);
        assert_eq!(picker.selected(), 0, "cannot go above the first");
        picker.move_by(1);
        picker.move_by(1);
        picker.move_by(1);
        picker.move_by(1);
        assert_eq!(picker.selected(), 2, "nor below the last");
    }

    #[test]
    fn an_empty_picker_does_not_panic() {
        // A machine without any GGUF: the case of a fresh setup, and the first one the user will
        // meet if they try the shortcut before downloading anything.
        let mut picker = Picker::new(Vec::new(), "stopped");
        picker.move_by(1);
        picker.move_by(-1);
        assert!(picker.is_empty());
        assert!(picker.current().is_none());
    }

    fn model(name: &str, loaded: bool) -> LocalModel {
        LocalModel { label: name.into(), detail: "8592 Mio · GPU 65/65".into(), loaded, viable: true }
    }

    #[test]
    fn the_status_bar_prioritizes_the_essentials() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let status = Status {
            model: "claude-opus-5".into(),
            mode: Some(BudgetMode::SaveMaxTokens),
            tokens: 12_345,
            cost: 0.1234,
            local_share: 0.75,
            vram_free_mib: Some(4096),
            ..Default::default()
        };
        // Wide: everything fits.
        let wide = status.render(200);
        assert!(wide.contains("claude-opus-5"));
        assert!(wide.contains(BudgetMode::SaveMaxTokens.label()));
        assert!(wide.contains("75% local"));

        // Narrow: the model and the mode survive, the VRAM drops.
        let narrow = status.render(30);
        assert!(narrow.contains("claude-opus-5"));
        assert!(!narrow.contains("VRAM"));
        assert!(narrow.chars().count() <= 30);
    }

    #[test]
    fn an_empty_bar_produces_nothing() {
        assert_eq!(Status::default().render(80), "");
    }

    #[test]
    fn a_segment_wider_than_the_bar_is_still_shown() {
        // Otherwise the bar would be empty on a very narrow terminal, which is worse.
        let status = Status { model: "a-very-long-model-name".into(), ..Default::default() };
        assert!(!status.render(10).is_empty());
    }

    #[test]
    fn the_transcript_bounds_its_memory() {
        let mut transcript = Transcript::default();
        for i in 0..MAX_LINES + 500 {
            transcript.push(Line::new(LineKind::Assistant, format!("line {i}")));
        }
        assert_eq!(transcript.len(), MAX_LINES);
        // The oldest ones go.
        assert!(transcript.lines()[0].text.contains("500"));
    }

    #[test]
    fn a_line_containing_a_break_is_split() {
        // A model name or a tool output can contain a line break; rendering assumes a line is
        // one line.
        let mut transcript = Transcript::default();
        transcript.push(Line::new(LineKind::Assistant, "one\ntwo\nthree"));
        assert_eq!(transcript.len(), 3);
        assert!(transcript.lines().iter().all(|l| !l.text.contains('\n')));
    }

    #[test]
    fn escape_sequences_are_neutralized() {
        // Without this, a model could make the screen repaint by emitting control sequences in
        // its answer.
        let mut transcript = Transcript::default();
        transcript.push(Line::new(LineKind::Assistant, "before\u{1b}[31mred\u{7}"));
        let text = &transcript.lines()[0].text;
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\u{7}'));
        assert!(text.contains("red"), "the visible text is kept");
    }

    #[test]
    fn tabs_become_spaces() {
        let mut transcript = Transcript::default();
        transcript.push(Line::new(LineKind::Tool, "column\tvalue"));
        assert_eq!(transcript.lines()[0].text, "column value");
    }

    #[test]
    fn replacing_the_last_line_rewrites_in_place() {
        let mut transcript = Transcript::default();
        transcript.push(Line::new(LineKind::Assistant, "Hel"));
        transcript.replace_last("Hello".into());
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript.lines()[0].text, "Hello");
    }

    #[test]
    fn replacing_on_an_empty_transcript_does_not_panic() {
        Transcript::default().replace_last("x".into());
    }

    #[test]
    fn multiline_text_becomes_several_lines() {
        let mut transcript = Transcript::default();
        transcript.push_text(LineKind::Assistant, "one\ntwo\nthree");
        assert_eq!(transcript.len(), 3);
    }

    #[test]
    fn the_visible_window_follows_the_scroll() {
        let mut transcript = Transcript::default();
        for i in 0..100 {
            transcript.push(Line::new(LineKind::Assistant, i.to_string()));
        }
        // Stuck to the bottom: the last ones are seen.
        assert_eq!(transcript.visible(10).last().unwrap().text, "99");

        transcript.scroll_up(5, 10);
        assert_eq!(transcript.visible(10).last().unwrap().text, "94");
        assert!(!transcript.is_at_bottom());

        transcript.scroll_to_bottom();
        assert_eq!(transcript.visible(10).last().unwrap().text, "99");
    }

    #[test]
    fn scrolling_stays_within_bounds() {
        let mut transcript = Transcript::default();
        for i in 0..20 {
            transcript.push(Line::new(LineKind::Assistant, i.to_string()));
        }
        transcript.scroll_up(1000, 10);
        assert_eq!(transcript.scroll(), 10, "cannot scroll past the start");
        transcript.scroll_down(1000);
        assert!(transcript.is_at_bottom());
    }

    #[test]
    fn a_short_transcript_is_displayed_entirely() {
        let mut transcript = Transcript::default();
        transcript.push(Line::new(LineKind::User, "a"));
        assert_eq!(transcript.visible(50).len(), 1);
    }

    #[test]
    fn input_handles_accented_characters() {
        let mut input = Input::default();
        for c in "café".chars() {
            input.insert(c);
        }
        assert_eq!(input.text(), "café");
        assert_eq!(input.cursor(), 4);

        input.backspace();
        assert_eq!(input.text(), "caf", "deleting a multi-byte character must not panic");
    }

    #[test]
    fn insertion_happens_at_the_cursor() {
        let mut input = Input::default();
        for c in "ac".chars() {
            input.insert(c);
        }
        input.move_left();
        input.insert('b');
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn the_cursor_stays_within_bounds() {
        let mut input = Input::default();
        input.move_left();
        assert_eq!(input.cursor(), 0);
        input.insert('a');
        input.move_right();
        input.move_right();
        assert_eq!(input.cursor(), 1);
        input.backspace();
        input.backspace();
        assert!(input.text().is_empty());
    }

    #[test]
    fn submitting_stores_in_history_and_empties_the_input() {
        let mut input = Input::default();
        for c in "hello".chars() {
            input.insert(c);
        }
        assert_eq!(input.submit(), "hello");
        assert!(input.is_empty());

        input.history_previous();
        assert_eq!(input.text(), "hello");
    }

    #[test]
    fn history_does_not_keep_consecutive_duplicates() {
        let mut input = Input::default();
        for _ in 0..2 {
            for c in "same".chars() {
                input.insert(c);
            }
            input.submit();
        }
        input.history_previous();
        let first = input.text().to_owned();
        input.history_previous();
        assert_eq!(input.text(), first);
        assert_eq!(input.history.len(), 1);
    }

    #[test]
    fn going_back_down_the_history_returns_to_an_empty_input() {
        let mut input = Input::default();
        for c in "a".chars() {
            input.insert(c);
        }
        input.submit();
        input.history_previous();
        assert_eq!(input.text(), "a");
        input.history_next();
        assert!(input.text().is_empty(), "back to a fresh input");
    }

    #[test]
    fn an_empty_input_does_not_enter_the_history() {
        let mut input = Input::default();
        input.insert(' ');
        assert_eq!(input.submit(), "");
        input.history_previous();
        assert!(input.text().is_empty());
    }

    #[test]
    fn a_paste_keeps_its_lines_and_loses_its_controls() {
        let mut input = Input::default();
        input.insert_str("one\r\ntwo\tend\x1b[2J");
        assert_eq!(input.text(), "one\ntwo    end[2J");
        assert_eq!(input.cursor(), input.text().chars().count());
    }

    #[test]
    fn line_editing_shortcuts_act_on_the_current_line() {
        let mut input = Input::default();
        input.insert_str("one two three");
        input.delete_word_back();
        assert_eq!(input.text(), "one two ");
        input.move_home();
        input.kill_to_end();
        assert_eq!(input.text(), "");
        input.insert_str(
            "a
bc",
        );
        input.kill_to_start();
        assert_eq!(
            input.text(),
            "a
",
            "only the current line is erased"
        );
    }
}
