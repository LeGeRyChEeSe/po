//! `po-tui` — full-screen interface.
//!
//! It does not replace the streaming output: `po-term` decides which of the two suits the
//! terminal, and the TUI is only offered in the `rich` profile. What is visible here is also
//! visible in streaming mode; only the presentation changes.
//!
//! The agent turn runs *inside* the event loop rather than in a detached task: the agent thus
//! does not need to be shared between threads, and the interface stays responsive thanks to a
//! `select!` that keeps handling keys while the model works.

pub mod approval;
pub mod monitor;
pub mod question;
pub mod render;
pub mod settings;
pub mod state;
pub mod view;
mod watchdog;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures_util::StreamExt;
use po_agent::{Agent, AgentEvent};
use po_term::{Terminal, TerminalGuard};
use tokio::sync::mpsc;

pub mod complete;
pub mod voice;
pub mod control;

pub use approval::{ApprovalRequest, ChannelApprover};
pub use state::{Input, Line, LineKind, Status, Transcript};

/// What the loop expects from the caller between two turns.
pub enum Outcome {
    /// The user has quit.
    Quit,
    /// A command to handle outside the TUI (local model management, for example).
    Command(String),
}

/// What the model is doing at this moment, as the user should read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Request sent, nothing received yet: prompt prefill, loading, waiting queue.
    Waiting,
    Thinking,
    Writing,
    /// A tool is running.
    Tool,
    /// The local server is serving another client; the request waits its turn.
    Queued,
}

/// Progress of the current turn, so the screen never stays still.
#[derive(Debug, Clone)]
pub struct Progress {
    pub started: std::time::Instant,
    pub phase: Phase,
    pub thinking_chars: usize,
    pub text_chars: usize,
    /// Tool being run, if any.
    pub tool: Option<String>,
    /// Model call in progress: start, first fragment received, volume produced since.
    pub call_started: std::time::Instant,
    pub first_token: Option<std::time::Instant>,
    pub call_chars: usize,
}

impl Progress {
    pub fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            phase: Phase::Waiting,
            thinking_chars: 0,
            text_chars: 0,
            tool: None,
            call_started: now,
            first_token: None,
            call_chars: 0,
        }
    }

    /// A fragment of the current call has arrived.
    fn produced(&mut self, chars: usize) {
        self.first_token.get_or_insert_with(std::time::Instant::now);
        self.call_chars += chars;
    }

    /// Generation rate of the current call, in tokens per second (estimated at four characters
    /// per token), as soon as there is enough to measure.
    pub fn live_generation(&self) -> Option<f64> {
        let first = self.first_token?;
        let secs = first.elapsed().as_secs_f64();
        (secs >= 0.5 && self.call_chars >= 40).then(|| self.call_chars as f64 / 4.0 / secs)
    }

    /// Tokens produced, estimated at four characters each. It is an estimate displayed as such
    /// (`~`): the exact count only arrives at the end of the answer.
    pub fn estimated_tokens(&self) -> usize {
        (self.thinking_chars + self.text_chars).div_ceil(4)
    }

    /// Label of the current state, without animation.
    pub fn label(&self) -> String {
        match (self.phase, &self.tool) {
            (Phase::Waiting, _) => po_i18n::t!("tui.progress.waiting").to_owned(),
            (Phase::Queued, _) => po_i18n::t!("tui.progress.queued").to_owned(),
            (Phase::Thinking, _) => po_i18n::t!("tui.progress.thinking").to_owned(),
            (Phase::Writing, _) => po_i18n::t!("tui.progress.writing").to_owned(),
            (Phase::Tool, Some(name)) => po_i18n::t!("tui.progress.tool_named", name = name),
            (Phase::Tool, None) => po_i18n::t!("tui.progress.tool").to_owned(),
        }
    }

    /// Full line: animation frame, state, duration, volume produced.
    pub fn line(&self, frame: usize, unicode: bool) -> String {
        let glyphs: &[&str] = if unicode {
            &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        } else {
            &["|", "/", "-", "\\"]
        };
        let glyph = glyphs[frame % glyphs.len()];
        let seconds = self.started.elapsed().as_secs();
        let mut line = format!("{glyph} {} · {seconds} s", self.label());
        if self.estimated_tokens() > 0 {
            line.push_str(&format!(" · ~{} tok", self.estimated_tokens()));
        }
        line
    }
}

impl Default for Progress {
    fn default() -> Self {
        Self::new()
    }
}

pub struct App {
    pub transcript: Transcript,
    pub input: Input,
    pub status: Status,
    /// Pending approval request, displayed as an overlay.
    pub pending_approval: Option<ApprovalRequest>,
    /// Help displayed over the conversation.
    pub show_help: bool,
    /// Local model picker, opened with Ctrl+O.
    pub picker: Option<state::Picker>,
    /// Progress of the current turn; `None` between two turns.
    pub progress: Option<Progress>,
    /// Animation frame counter, advanced at each beat.
    pub frame: usize,
    /// Timing of the last call, shown in the turn summary.
    last_stats: Option<po_agent::CallStats>,
    /// Tools in progress: name, position of their line, start. The line is rewritten at the end.
    running_tools: Vec<(String, usize, std::time::Instant)>,
    /// Last routing announcement: repeating it at every call drowns the conversation.
    last_route: Option<String>,
    /// True when fragments are being displayed for the current turn.
    streaming: bool,
    /// Line being built: a fragment rarely ends on a line end.
    partial: String,
    /// Known commands, for completion (`/…`).
    pub commands: Vec<(String, String)>,
    /// Workspace root, for path completion (`@…`).
    pub workspace: Option<std::path::PathBuf>,
    /// Messages submitted during a turn, sent in order once the turn is over.
    pub queued: std::collections::VecDeque<String>,
    /// The last turn was interrupted: the queue is then not flushed automatically.
    pub last_interrupted: bool,
    /// Reasoning being displayed, and its unfinished line.
    thinking_open: bool,
    thinking_partial: String,
    /// The reasoning of the current call was streamed line by line.
    thinking_streamed: bool,
    /// Start and end of the current call's reasoning, and position of its header.
    thinking_started: Option<std::time::Instant>,
    thinking_ended: Option<std::time::Instant>,
    thinking_header: Option<usize>,
    /// Ctrl+O: reasoning and tool groups expanded.
    pub expanded: bool,
    /// The wheel scrolls the conversation; `/mouse off` gives selection back to the terminal.
    pub mouse: bool,
    /// Brief message of the status bar, and the time it appeared.
    pub toast: Option<(String, std::time::Instant)>,
    /// Settings panel (F2), modal.
    pub panel: Option<settings::Panel>,
    /// Search palette (Ctrl+K), modal.
    pub finder: Option<settings::Finder>,
    /// Model question waiting for an answer.
    pub pending_question: Option<question::Pending>,
    /// Work mode shared with the agent (Shift+Tab).
    pub work_mode: Option<po_agent::work_mode::WorkModeHandle>,
    /// Session title, displayed at the top center.
    pub title: Option<String>,
    /// A title is being written by the model.
    title_pending: bool,
    /// Subagents tracked in their frame.
    pub monitor: monitor::Monitor,
    /// Blocks toggled one by one, by touch or click (absolute positions).
    pub toggled: std::collections::HashSet<usize>,
    /// Last measured speeds, kept for the information row.
    pub speeds: Option<po_agent::CallStats>,
    /// Block that has just been touched, and its screen row: at the next draw, the view shifts
    /// the scroll so it stays under the finger once expanded.
    pub anchor: std::cell::Cell<Option<(usize, u16)>>,
    /// Subagents' big card open.
    pub monitor_open: bool,
    /// First Esc pressed: a second one, within the delay, stops the agent and its subagents.
    stop_armed: Option<std::time::Instant>,
    /// Subagent shown in the big card.
    pub monitor_selected: usize,
    /// Row of each subagent in the conversation (absolute position), to write its progress
    /// there; and started subagents whose row has not arrived yet.
    delegations: std::collections::HashMap<String, usize>,
    unlinked: Vec<(String, String)>,
    /// Walk through the completion list with the arrows.
    completion: Option<CompletionNav>,
    /// Settings tabs read at the start of the turn: during the turn, the agent is busy and the
    /// panel opens on them.
    tabs_cache: Vec<settings::Tab>,
    /// Setting commands given during a turn, applied at the next safe point (after tool
    /// results), otherwise at the end of the turn. The boolean: typed on the keyboard (its
    /// report goes into the conversation) rather than chosen in the panel.
    deferred: Vec<(String, bool)>,
    /// Submitted message (keyboard or remote control) that goes out on the loop's next pass.
    pending_submit: Option<String>,
    /// Restart requested: the loop ends at the end of the current turn.
    pub restart: bool,
    /// MCP address of this session's remote control.
    pub control_url: Option<String>,
    /// Voice dictation: enabled (button visible), recording in progress (the means to stop it,
    /// and since when), transcription in progress, and where to send the transcribed text.
    pub voice_enabled: bool,
    voice_stop: Option<tokio::sync::oneshot::Sender<()>>,
    pub voice_started: Option<std::time::Instant>,
    pub voice_busy: bool,
    voice_tx: Option<mpsc::UnboundedSender<Result<String, String>>>,
    /// Card change being animated: start, card left, direction.
    card_anim: Option<(std::time::Instant, usize, i8)>,
    /// Press on a card, not yet released: a drag makes it a swipe, a release in place a touch.
    press: Option<Press>,
    /// `/mouse test`: every mouse event (and special key) is displayed.
    pub mouse_debug: bool,
    /// Po Labs enabled (indicator of the information row).
    pub labs: bool,
    /// `/…` command to run on the loop's next pass (touch on an indicator).
    pending_command: Option<String>,
}

/// Duration of the page turning when changing card.
pub const CARD_TURN: Duration = Duration::from_millis(280);
/// Columns to travel for a drag to count as a swipe.
const SWIPE_COLUMNS: i32 = 3;
/// A press the terminal never releases (some do not send the release) counts as a touch after
/// this delay.
const PRESS_AS_TAP: Duration = Duration::from_millis(450);

/// Press on a subagent card.
#[derive(Debug, Clone, Copy)]
struct Press {
    column: u16,
    row: u16,
    at: std::time::Instant,
    hit: Hit,
    block: Option<(usize, u16)>,
}

/// Restart requested by the last full-screen session: the caller relaunches Po.
static RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True if the session that has just ended asked to be relaunched (once).
pub fn take_restart() -> bool {
    RESTART.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Where the walk through the completion list stands: what had been typed, the chosen item, and
/// the text it put in the input. Any other keystroke ends it.
#[derive(Debug, Clone)]
struct CompletionNav {
    query: String,
    cursor: usize,
    selected: usize,
    filled: String,
}

/// Delay to confirm a stop with a second Esc.
const STOP_CONFIRM: Duration = Duration::from_secs(4);

/// Display duration of a brief message.
const TOAST: Duration = Duration::from_secs(5);

fn seconds_label(secs: f64) -> String {
    if secs < 10.0 {
        let text = format!("{secs:.1}");
        format!("{} s", if po_i18n::lang() == po_i18n::Lang::Fr { text.replace('.', ",") } else { text })
    } else {
        format!("{secs:.0} s")
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            transcript: Transcript::default(),
            input: Input::default(),
            status: Status::default(),
            pending_approval: None,
            show_help: false,
            picker: None,
            progress: None,
            frame: 0,
            last_stats: None,
            running_tools: Vec::new(),
            last_route: None,
            streaming: false,
            partial: String::new(),
            commands: Vec::new(),
            workspace: None,
            queued: std::collections::VecDeque::new(),
            last_interrupted: false,
            thinking_open: false,
            thinking_partial: String::new(),
            thinking_streamed: false,
            thinking_started: None,
            thinking_ended: None,
            thinking_header: None,
            expanded: false,
            mouse: true,
            toast: None,
            panel: None,
            finder: None,
            pending_question: None,
            work_mode: None,
            title: None,
            title_pending: false,
            monitor: monitor::Monitor::default(),
            toggled: std::collections::HashSet::new(),
            speeds: None,
            anchor: std::cell::Cell::new(None),
            monitor_open: false,
            stop_armed: None,
            monitor_selected: 0,
            delegations: std::collections::HashMap::new(),
            unlinked: Vec::new(),
            completion: None,
            tabs_cache: Vec::new(),
            deferred: Vec::new(),
            pending_submit: None,
            restart: false,
            control_url: None,
            voice_enabled: false,
            voice_stop: None,
            voice_started: None,
            voice_busy: false,
            voice_tx: None,
            card_anim: None,
            press: None,
            mouse_debug: false,
            labs: po_i18n::prefs::get_bool("labs").unwrap_or(false),
            pending_command: None,
        }
    }

    /// Redisplays a resumed conversation (`--resume`, in-place restart): messages, folded
    /// reasoning, tool calls. Tool results stay in the session.
    pub fn replay(&mut self, messages: &[po_core::Message]) {
        use po_core::{ContentBlock, Role};
        for message in messages {
            match message.role {
                // A tool-results message is not user input.
                Role::User if message.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })) => {}
                Role::User => {
                    for block in &message.content {
                        if let ContentBlock::Text { text } = block {
                            self.transcript.push_text(LineKind::User, text);
                        }
                    }
                }
                Role::Assistant => {
                    for block in &message.content {
                        match block {
                            ContentBlock::Thinking { text, .. } if !text.is_empty() => {
                                let label = po_i18n::t!("tui.thinking.tokens", tokens = text.chars().count().div_ceil(4));
                                self.transcript.push(Line::new(LineKind::ThinkingSummary, label).with_detail(text.clone()));
                            }
                            ContentBlock::Text { text } if !text.is_empty() => {
                                self.transcript.push_text(LineKind::Assistant, text);
                            }
                            ContentBlock::ToolUse { name, input, .. } if name == "task" => {
                                let agent = input["agent"].as_str().unwrap_or(po_i18n::t!("tui.delegation.default_agent"));
                                let task = input["task"].as_str().unwrap_or("");
                                self.transcript.push(Line::new(LineKind::Delegation, agent).with_detail(task));
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                let summary = po_agent::summarize_call(name, input);
                                self.transcript.push(Line::new(LineKind::Tool, format!("{name} {summary}")));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        if !messages.is_empty() {
            self.note(po_i18n::t!("tui.note.resumed"));
            self.transcript.scroll_to_bottom();
        }
    }

    /// Subagents of the small deck, in order of arrival: those still on screen.
    pub fn visible_deck(&self) -> Vec<usize> {
        let now = std::time::Instant::now();
        self.monitor.views.iter().enumerate().filter(|(_, v)| v.visible(now)).map(|(i, _)| i).collect()
    }

    /// Front card: the chosen one if it is still there, otherwise the most recent.
    pub fn front_card(&self) -> usize {
        let deck = self.visible_deck();
        if deck.contains(&self.monitor_selected) {
            self.monitor_selected
        } else {
            deck.last().copied().unwrap_or(self.monitor_selected)
        }
    }

    /// Card change animation: progress (0 to 1), card left, direction.
    pub fn card_turn(&self) -> Option<(f32, usize, i8)> {
        let (started, from, dir) = self.card_anim?;
        let progress = started.elapsed().as_secs_f32() / CARD_TURN.as_secs_f32();
        (progress < 1.0).then_some((progress, from, dir))
    }

    /// An animation asks for closely spaced frames.
    pub fn animating(&self) -> bool {
        self.card_turn().is_some()
    }

    /// Delay before the next animation frame: short for a turning card, slower for a color
    /// wave, which needs no more to look smooth.
    pub fn animation_delay(&self, wave: bool) -> Option<Duration> {
        if self.animating() {
            Some(Duration::from_millis(16))
        } else if wave {
            Some(Duration::from_millis(90))
        } else {
            None
        }
    }

    /// Switches to the next card (`dir` = 1) or the previous one (-1): within the deck, or among
    /// all the session's subagents when the big card is open.
    pub fn switch_agent(&mut self, dir: i8) {
        let order: Vec<usize> =
            if self.monitor_open { (0..self.monitor.views.len()).collect() } else { self.visible_deck() };
        if order.len() < 2 {
            return;
        }
        let current = if self.monitor_open { self.monitor_selected } else { self.front_card() };
        let position = order.iter().position(|i| *i == current).unwrap_or(0) as i32;
        let next = order[(position + i32::from(dir)).rem_euclid(order.len() as i32) as usize];
        self.card_anim = Some((std::time::Instant::now(), current, dir));
        self.monitor_selected = next;
    }

    /// A press left without release or drag becomes a touch: some terminals do not send the
    /// release. Returns true if something happened.
    pub fn expire_press(&mut self) -> bool {
        match self.press {
            Some(press) if press.at.elapsed() >= PRESS_AS_TAP => {
                self.press = None;
                self.tap(press)
            }
            _ => false,
        }
    }

    /// Touch on a card: the deck opens the big card, the big card expands a block.
    fn tap(&mut self, press: Press) -> bool {
        match press.hit {
            Hit::MonitorPanel(index) => {
                self.monitor_open = true;
                self.monitor_selected = index;
                true
            }
            Hit::MonitorCard => {
                let Some((key, row)) = press.block else { return false };
                let Some(view) = self.monitor.views.get_mut(self.monitor_selected) else { return false };
                if !view.toggled.remove(&key) {
                    view.toggled.insert(key);
                }
                view.anchor.set(Some((key, row)));
                true
            }
            _ => false,
        }
    }

    /// "Dictate" button or F3: starts the recording, or stops it and starts the transcription.
    /// The text arrives later through the dictation channel and is inserted into the input.
    pub fn toggle_voice(&mut self) {
        if let Some(stop) = self.voice_stop.take() {
            let _ = stop.send(());
            self.voice_started = None;
            self.voice_busy = true;
            return;
        }
        if self.voice_busy {
            return;
        }
        let settings = voice::Settings::load();
        self.voice_enabled = settings.enabled;
        if !settings.enabled {
            self.toast(po_i18n::t!("tui.toast.voice_off"));
            return;
        }
        if let Some(missing) = settings.missing() {
            self.toast(missing);
            return;
        }
        let Some(results) = self.voice_tx.clone() else { return };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        self.voice_stop = Some(stop);
        self.voice_started = Some(std::time::Instant::now());
        tokio::spawn(async move {
            let recording = match voice::start(&settings).await {
                Ok(recording) => recording,
                Err(err) => {
                    let _ = results.send(Err(err));
                    return;
                }
            };
            // A forgotten dictation stops by itself after two minutes.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(120), stopped).await;
            let _ = results.send(recording.finish(&settings).await);
        });
    }

    /// Transcribed text (or failure): it joins the input, without being sent.
    pub fn voice_result(&mut self, result: Result<String, String>) {
        self.voice_busy = false;
        self.voice_stop = None;
        self.voice_started = None;
        match result {
            Ok(text) if text.trim().is_empty() => self.toast(po_i18n::t!("tui.toast.voice_nothing")),
            Ok(text) => {
                if !self.input.is_empty() && !self.input.text().ends_with(char::is_whitespace) {
                    self.input.insert(' ');
                }
                self.input.insert_str(text.trim());
            }
            Err(err) => self.toast(po_i18n::t!("tui.toast.voice_error", error = err)),
        }
    }

    /// Applies a subagent update: its frame, and its row in the conversation, which shows what
    /// it says it is doing.
    pub fn apply_subagent(&mut self, update: po_agent::SubagentUpdate) {
        let id = monitor::update_id(&update).to_owned();
        if let po_agent::SubagentUpdate::Started { agent, .. } = &update
            && !self.link_delegation(&id, agent)
        {
            self.unlinked.push((id.clone(), agent.clone()));
        }
        self.monitor.apply(update);
        self.refresh_delegation(&id);
    }

    /// Links a subagent to the last still-free delegation row bearing its name.
    fn link_delegation(&mut self, id: &str, agent: &str) -> bool {
        let taken: std::collections::HashSet<usize> = self.delegations.values().copied().collect();
        let offset = self.transcript.offset();
        let found = self.transcript.lines().iter().enumerate().rev().find(|(i, l)| {
            l.kind == LineKind::Delegation
                && l.text.split(" · ").next() == Some(agent)
                && !taken.contains(&(offset + i))
        });
        match found {
            Some((i, _)) => {
                self.delegations.insert(id.to_owned(), offset + i);
                true
            }
            None => false,
        }
    }

    fn refresh_delegation(&mut self, id: &str) {
        let Some(&position) = self.delegations.get(id) else { return };
        let Some(view) = self.monitor.views.iter().find(|v| v.id == id) else { return };
        let status = view.status_line();
        if let Some(line) = self.transcript.get(position)
            && line.progress.as_deref() != Some(status.as_str())
        {
            let line = line.clone().with_progress(status);
            self.transcript.replace_at(position, line);
        }
    }

    /// Highlighted item in the completion list.
    pub fn completion_selected(&self) -> usize {
        self.completion.as_ref().filter(|nav| nav.filled == self.input.text()).map(|nav| nav.selected).unwrap_or(0)
    }

    /// Arrows and Enter in the completion list. The arrows choose and fill the input without
    /// sending it; Enter on a half-typed command picks the highlighted item; Enter once the
    /// command is chosen sends it. Returns true if the key was taken here.
    pub fn completion_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release || !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return false;
        }
        let list = self.suggestions();
        if list.is_empty() {
            self.completion = None;
            return false;
        }
        let active = self.completion.clone().filter(|nav| nav.filled == self.input.text());
        let (query, cursor) = match &active {
            Some(nav) => (nav.query.clone(), nav.cursor),
            None => (self.input.text().to_owned(), self.input.cursor()),
        };
        let len = list.len();
        let selected = match (key.code, &active) {
            (KeyCode::Down, Some(nav)) => (nav.selected + 1) % len,
            (KeyCode::Up, Some(nav)) => (nav.selected + len - 1) % len,
            (KeyCode::Down, None) => 0,
            (KeyCode::Up, None) => len - 1,
            // A half-typed command: Enter picks the highlighted item, without sending.
            (KeyCode::Enter, None) if query.starts_with('/') && !query.contains(char::is_whitespace) => 0,
            _ => return false,
        };
        let (text, new_cursor) = complete::apply(&query, cursor, &list[selected]);
        self.input.replace(text.clone(), new_cursor);
        self.completion = Some(CompletionNav { query, cursor, selected, filled: text });
        true
    }

    /// Brief message in the status bar: what has no place in the conversation (mode switch,
    /// retry, router choice).
    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), std::time::Instant::now()));
    }

    /// A title is being written.
    pub fn title_pending_display(&self) -> bool {
        self.title_pending
    }

    /// Brief message still displayable.
    pub fn current_toast(&self) -> Option<&str> {
        self.toast.as_ref().filter(|(_, at)| at.elapsed() < TOAST).map(|(t, _)| t.as_str())
    }

    /// Command report, put in a titled card.
    pub fn card(&mut self, title: &str, lines: Vec<(bool, String)>) {
        self.transcript.push(Line::new(LineKind::CardTitle, title));
        self.card_lines(lines);
    }

    fn card_lines(&mut self, lines: Vec<(bool, String)>) {
        for (warn, text) in lines {
            self.transcript.push(Line::new(if warn { LineKind::CardWarn } else { LineKind::Card }, text));
        }
    }

    /// Header of the current call's reasoning, placed at the first fragment: it thus stays above
    /// the answer, even when the reasoning text only arrives at the end.
    fn ensure_thinking_header(&mut self) {
        if self.thinking_header.is_none() {
            self.thinking_started.get_or_insert_with(std::time::Instant::now);
            self.thinking_header = Some(self.transcript.next_position());
            self.transcript.push(Line::new(LineKind::ThinkingSummary, po_i18n::t!("tui.thinking.ellipsis")));
        }
    }

    /// Closes the call's reasoning: duration, volume, and folded text if it was not streamed.
    fn finish_thinking(&mut self, text: Option<&str>) {
        let Some(position) = self.thinking_header.take() else {
            return;
        };
        let started = self.thinking_started.take();
        let ended = self.thinking_ended.take().unwrap_or_else(std::time::Instant::now);
        let mut label = po_i18n::t!("tui.progress.thinking").to_owned();
        if let Some(started) = started {
            label.push_str(&format!(" · {}", seconds_label(ended.duration_since(started).as_secs_f64())));
        }
        if let Some(text) = text {
            label.push_str(&format!(" · ~{} tokens", text.chars().count().div_ceil(4)));
        }
        let mut line = Line::new(LineKind::ThinkingSummary, label);
        if let Some(text) = text.filter(|_| !self.thinking_streamed) {
            line = line.with_detail(text);
        }
        self.transcript.replace_at(position, line);
        self.thinking_open = false;
        self.thinking_streamed = false;
    }

    /// Adds a streamed fragment. Fragments arrive cut anywhere — sometimes in the middle of a
    /// word — so a line is only published once it is finished, and the last partial line is
    /// rewritten at each fragment to stay visible.
    fn append_streamed(&mut self, fragment: &str) {
        self.thinking_open = false;
        if !self.streaming {
            self.streaming = true;
            self.transcript.push(Line::new(LineKind::Assistant, String::new()));
        }
        for c in fragment.chars() {
            if c == '\n' {
                self.transcript.replace_last(self.partial.clone());
                self.transcript.push(Line::new(LineKind::Assistant, String::new()));
                self.partial.clear();
            } else {
                self.partial.push(c);
            }
        }
        self.transcript.replace_last(self.partial.clone());
    }

    /// Reasoning displayed as it flows, in dimmed lines. It precedes the answer and does not mix
    /// with it: the first answer line opens its own row.
    fn append_thinking(&mut self, fragment: &str) {
        self.ensure_thinking_header();
        self.thinking_streamed = true;
        if !self.thinking_open {
            self.thinking_open = true;
            self.transcript.push(Line::new(LineKind::Thinking, String::new()));
            self.thinking_partial.clear();
        }
        for c in fragment.chars() {
            if c == '\n' {
                self.transcript.replace_last(self.thinking_partial.clone());
                self.transcript.push(Line::new(LineKind::Thinking, String::new()));
                self.thinking_partial.clear();
            } else {
                self.thinking_partial.push(c);
            }
        }
        self.transcript.replace_last(self.thinking_partial.clone());
    }

    /// Closes the streaming in progress.
    fn finish_streaming(&mut self) {
        if !self.partial.is_empty() {
            self.transcript.replace_last(self.partial.clone());
            self.partial.clear();
        }
        self.streaming = false;
    }

    /// Completion suggestions for the current input. During an arrow walk, the list stays that
    /// of what had been typed, not of the choice put in the input.
    pub fn suggestions(&self) -> Vec<complete::Suggestion> {
        match self.completion.as_ref().filter(|nav| nav.filled == self.input.text()) {
            Some(nav) => complete::suggestions(&nav.query, nav.cursor, &self.commands, self.workspace.as_deref())
                .into_iter()
                .filter(|s| s.replacement.trim_end() != nav.query)
                .collect(),
            None => {
                complete::suggestions(self.input.text(), self.input.cursor(), &self.commands, self.workspace.as_deref())
            }
        }
    }

    /// Tab: applies the highlighted suggestion, if there is one.
    pub fn complete(&mut self) -> bool {
        let selected = self.completion_selected();
        let (query, cursor) = match self.completion.take().filter(|nav| nav.filled == self.input.text()) {
            Some(nav) => (nav.query, nav.cursor),
            None => (self.input.text().to_owned(), self.input.cursor()),
        };
        let Some(chosen) =
            complete::suggestions(&query, cursor, &self.commands, self.workspace.as_deref()).into_iter().nth(selected)
        else {
            return false;
        };
        let (text, cursor) = complete::apply(&query, cursor, &chosen);
        self.input.replace(text, cursor);
        true
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.transcript.push(Line::new(LineKind::Notice, text));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.transcript.push(Line::new(LineKind::Error, text));
    }

    /// Turns an agent event into displayable lines.
    pub fn absorb(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ModelSelected { model, class, reason } => {
                self.status.model = model.clone();
                if reason.is_noteworthy() {
                    let announcement = format!("{model} · {} — {}", class.label(), reason.explain());
                    if self.last_route.as_deref() != Some(announcement.as_str()) {
                        self.toast(announcement.clone());
                        self.last_route = Some(announcement);
                    }
                }
            }
            // The reasoning is folded under its header: Ctrl+O expands it.
            AgentEvent::Thinking(text) => self.finish_thinking(Some(&text)),
            AgentEvent::ThinkingText(fragment) => self.append_thinking(&fragment),
            AgentEvent::CallStats(stats) => {
                self.finish_thinking(None);
                self.speeds = Some(stats);
                if let Some(progress) = self.progress.as_mut() {
                    progress.call_started = std::time::Instant::now();
                    progress.first_token = None;
                    progress.call_chars = 0;
                }
                self.last_stats = Some(stats)
            }
            AgentEvent::Queued => {
                if let Some(progress) = self.progress.as_mut() {
                    progress.phase = Phase::Queued;
                }
            }
            AgentEvent::ThinkingDelta(chars) => {
                self.ensure_thinking_header();
                if let Some(progress) = self.progress.as_mut() {
                    progress.phase = Phase::Thinking;
                    progress.produced(chars);
                    progress.thinking_chars += chars;
                }
            }
            AgentEvent::TextDelta(fragment) => {
                if self.thinking_started.is_some() && self.thinking_ended.is_none() {
                    self.thinking_ended = Some(std::time::Instant::now());
                }
                if let Some(progress) = self.progress.as_mut() {
                    progress.phase = Phase::Writing;
                    progress.produced(fragment.chars().count());
                    progress.text_chars += fragment.chars().count();
                }
                self.append_streamed(&fragment)
            }
            AgentEvent::Text(text) => {
                // The full text follows the fragments: if they have already displayed everything,
                // rewriting it would double the answer on screen.
                if self.streaming {
                    self.finish_streaming();
                } else {
                    self.transcript.push_text(LineKind::Assistant, &text);
                }
            }
            AgentEvent::ToolStarted { name, summary } => {
                if let Some(progress) = self.progress.as_mut() {
                    progress.phase = Phase::Tool;
                    progress.tool = Some(name.clone());
                }
                let position = self.transcript.next_position();
                self.transcript.push(Line::new(LineKind::ToolRunning, format!("{name} {summary}")));
                self.running_tools.push((name, position, std::time::Instant::now()));
            }
            AgentEvent::ToolFinished { name, summary, is_error } => {
                // The tool is done, the model takes over again: we go back to waiting.
                if let Some(progress) = self.progress.as_mut() {
                    progress.phase = Phase::Waiting;
                    progress.tool = None;
                }
                let kind = if is_error { LineKind::ToolError } else { LineKind::Tool };
                // The launch row becomes the result row, in the same place: a call reads as a whole,
                // and the conversation does not double in length.
                let running = self.running_tools.iter().position(|(n, ..)| *n == name);
                let (position, elapsed) = match running {
                    Some(i) => {
                        let (_, position, started) = self.running_tools.remove(i);
                        (Some(position), started.elapsed().as_secs_f64())
                    }
                    None => (None, 0.0),
                };
                // A Po Labs request has its own card: what was noted is seen right away.
                if name == "po_labs" && !is_error {
                    let request = position
                        .and_then(|p| self.transcript.get(p))
                        .map(|l| l.text.trim_start_matches("po_labs").trim().to_owned())
                        .unwrap_or_default();
                    let line = Line::new(LineKind::CardTitle, po_i18n::t!("labs.card_title"));
                    if !position.is_some_and(|p| self.transcript.replace_at(p, line.clone())) {
                        self.transcript.push(line);
                    }
                    self.transcript.push(Line::new(LineKind::Card, request));
                    self.transcript.push(Line::new(LineKind::CardDim, summary));
                    return;
                }
                let line = Line::new(kind, format!("{name} {summary}")).with_secs(elapsed);
                let replaced = position.is_some_and(|p| self.transcript.replace_at(p, line.clone()));
                if !replaced {
                    self.transcript.push(line);
                }
            }
            AgentEvent::ToolDenied { name, summary } => {
                self.transcript.push(Line::new(LineKind::ToolError, po_i18n::t!("tui.tool.denied", name = name, summary = summary)));
            }
            AgentEvent::Delegated { model, agent, task, background } => {
                // The main agent is waiting for a subagent: the bar says so, rather than showing an
                // endless "thinking".
                if !background && let Some(progress) = self.progress.as_mut() {
                    progress.phase = Phase::Tool;
                    progress.tool = Some(po_i18n::t!("tui.progress.subagent", agent = agent));
                }
                // The whole brief is kept folded: the goal is displayed, the rest expands on touch.
                self.transcript.push(
                    Line::new(
                        LineKind::Delegation,
                        format!("{agent} · {model}{}", if background { po_i18n::t!("tui.delegation.background") } else { "" }),
                    )
                    .with_detail(task)
                    .with_progress(po_i18n::t!("tui.monitor.preparing")),
                );
                // The subagent may have started before this row arrived (two channels).
                if let Some(i) = self.unlinked.iter().position(|(_, a)| *a == agent) {
                    let (id, _) = self.unlinked.remove(i);
                    self.delegations.insert(id.clone(), self.transcript.next_position() - 1);
                    self.refresh_delegation(&id);
                }
            }
            AgentEvent::Retrying { model, attempt, error } => {
                self.toast(po_i18n::t!("tui.toast.retry", model = model, attempt = attempt, error = error));
            }
            AgentEvent::ModelDisabled { model, reason } => {
                // Discarding a model changes the behavior of the rest of the session: it is a visible
                // error, not a discreet note.
                self.transcript
                    .push(Line::new(LineKind::Error, po_i18n::t!("tui.model.disabled", model = model, reason = reason)));
            }
            AgentEvent::Notice(text) => self.note(text),
            AgentEvent::Asked { question, answer } => {
                self.card(po_i18n::t!("tui.card.question"), vec![(false, question), (false, format!("→ {answer}"))]);
            }
            AgentEvent::TurnCompleted { usage, cost, local_share, local_seconds } => {
                // A discreet end-of-turn row: speed, volume, cost if there is one.
                let mut parts = Vec::new();
                if let Some(stats) = self.last_stats.take().filter(|s| s.generation_tok_s > 0.0) {
                    parts.push(format!("{:.0} tok/s", stats.generation_tok_s));
                }
                parts.push(format!("{} tokens", usage.total()));
                if cost > 0.0 {
                    parts.push(format!("${cost:.4}"));
                }
                if local_seconds >= 0.05 && local_share < 0.999 {
                    parts.push(po_i18n::t!("tui.summary.local", pct = format!("{:.0}", local_share * 100.0)));
                }
                self.transcript.push(Line::new(LineKind::Summary, parts.join(" · ")));
            }
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

/// Shortcuts, listed once so the help and the handling do not diverge, in the current
/// language.
pub fn shortcuts() -> Vec<(&'static str, &'static str)> {
    vec![
        (po_i18n::t!("tui.shortcut.key_enter"), po_i18n::t!("tui.shortcut.send")),
        (po_i18n::t!("tui.shortcut.key_shift_tab"), po_i18n::t!("tui.shortcut.mode")),
        ("Ctrl+O", po_i18n::t!("tui.shortcut.expand")),
        ("F2", po_i18n::t!("tui.shortcut.settings")),
        ("Ctrl+K", po_i18n::t!("tui.shortcut.palette")),
        ("F3", po_i18n::t!("tui.shortcut.dictate")),
        ("Ctrl+G", po_i18n::t!("tui.shortcut.budget")),
        ("Ctrl+L", po_i18n::t!("tui.shortcut.local")),
        ("Ctrl+P", po_i18n::t!("tui.shortcut.pin")),
        ("Ctrl+R", po_i18n::t!("tui.shortcut.stop_server")),
        ("↑ ↓", po_i18n::t!("tui.shortcut.history")),
        ("PgUp PgDn", po_i18n::t!("tui.shortcut.scroll")),
        ("Ctrl+C", po_i18n::t!("tui.shortcut.interrupt")),
        ("F1", po_i18n::t!("tui.shortcut.help")),
    ]
}

/// Action derived from a key.
#[derive(Debug, PartialEq)]
pub enum KeyAction {
    None,
    Submit(String),
    Quit,
    Interrupt,
    CycleBudget,
    ShowLocal,
    PinModel,
    StopServer,
    ToggleHelp,
    ScrollUp,
    ScrollDown,
    Approve(bool),
    /// Message submitted during a turn: it will go out at the end of it.
    Queue(String),
    /// Tab: complete the current command or path.
    Complete,
    /// Ctrl+O: expand or fold reasoning and tools.
    ToggleExpand,
    /// F2: settings panel.
    OpenSettings,
    /// Ctrl+K: search palette.
    OpenPalette,
    /// Shift+Tab: plan → builder → bypass.
    CycleWorkMode,
    /// Esc: close what is open, or request a stop (confirmed by a second Esc).
    Escape,
    /// F3: dictate (start or finish).
    Dictate,
}

/// What a key does when the model picker is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    Move(i32),
    /// Load the highlighted model.
    Choose,
    /// Give the VRAM back without loading anything else.
    Stop,
    Close,
    None,
}

/// The picker captures **the whole** keyboard while it is open.
///
/// Letting unknown keys through to the input would produce the worst case: you think you are
/// navigating a list, and you are actually writing into the conversation.
pub fn map_picker_key(key: KeyEvent) -> PickerAction {
    use crossterm::event::KeyCode;

    if key.kind == KeyEventKind::Release {
        return PickerAction::None;
    }
    let ctrl = po_term::input::is_shortcut_ctrl(key.modifiers);
    match (key.code, ctrl) {
        (KeyCode::Up, _) | (KeyCode::Char('k'), false) => PickerAction::Move(-1),
        (KeyCode::Down, _) | (KeyCode::Char('j'), false) => PickerAction::Move(1),
        (KeyCode::PageUp, _) => PickerAction::Move(-5),
        (KeyCode::PageDown, _) => PickerAction::Move(5),
        (KeyCode::Enter, _) => PickerAction::Choose,
        // "s" as in stop, and Ctrl+R which does the same from the conversation: two paths to a
        // gesture one wants to be able to do without thinking.
        (KeyCode::Char('s'), false) | (KeyCode::Char('r'), true) => PickerAction::Stop,
        (KeyCode::Esc, _) | (KeyCode::Char('q'), false) | (KeyCode::Char('c'), true) => PickerAction::Close,
        // Ctrl+L closes what it opened: a toggling shortcut is easier to remember than one that
        // only opens in one direction.
        (KeyCode::Char('l'), true) => PickerAction::Close,
        _ => PickerAction::None,
    }
}

/// Turns a key into an action. Pure function: it is the shortcut table, and it can be tested
/// without a terminal.
pub fn map_key(key: KeyEvent, input: &mut Input, awaiting_approval: bool, busy: bool) -> KeyAction {
    // On Windows, every key also produces a release event; handling it would double every
    // typed character.
    if key.kind == KeyEventKind::Release {
        return KeyAction::None;
    }

    // An approval request monopolizes the keyboard: answering is the only useful action, and
    // letting a keystroke through to the input during a dialog causes confusion.
    if awaiting_approval {
        return match key.code {
            KeyCode::BackTab => KeyAction::CycleWorkMode,
            KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                KeyAction::Approve(true)
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => KeyAction::Approve(false),
            _ => KeyAction::None,
        };
    }

    let ctrl = po_term::input::is_shortcut_ctrl(key.modifiers);
    match (key.code, ctrl) {
        (KeyCode::Char('c'), true) => {
            if busy {
                KeyAction::Interrupt
            } else {
                KeyAction::Quit
            }
        }
        (KeyCode::Char('d'), true) if input.is_empty() => KeyAction::Quit,
        // Neither Ctrl+B nor Ctrl+A: they are the default prefixes of tmux and GNU screen. A
        // multiplexer intercepts them before the application — the shortcut would never get
        // here, and the product's central gesture would be dead where many users work.
        (KeyCode::Char('g'), true) => KeyAction::CycleBudget,
        (KeyCode::Char('o'), true) => KeyAction::ToggleExpand,
        (KeyCode::Char('l'), true) => KeyAction::ShowLocal,
        (KeyCode::Char('k'), true) => KeyAction::OpenPalette,
        (KeyCode::F(2), _) => KeyAction::OpenSettings,
        (KeyCode::Char('p'), true) => KeyAction::PinModel,
        (KeyCode::Char('r'), true) => KeyAction::StopServer,
        (KeyCode::F(1), _) => KeyAction::ToggleHelp,
        (KeyCode::F(3), _) => KeyAction::Dictate,
        (KeyCode::PageUp, _) => KeyAction::ScrollUp,
        (KeyCode::PageDown, _) => KeyAction::ScrollDown,
        // Explicit new line. Shift+Enter is only distinguishable on Windows and with the Kitty
        // keyboard protocol; Alt+Enter and Ctrl+J work everywhere, tmux included. Ctrl+Enter is
        // one of them: it is how ConPTY delivers a pasted `\n` (measured).
        (KeyCode::Enter, _)
            if key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL) =>
        {
            input.insert('\n');
            KeyAction::None
        }
        (KeyCode::Char('j'), true) => {
            input.insert('\n');
            KeyAction::None
        }
        (KeyCode::Enter, _) => {
            if input.is_empty() {
                KeyAction::None
            } else if busy {
                KeyAction::Queue(input.submit())
            } else {
                KeyAction::Submit(input.submit())
            }
        }
        // Esc: close the big card, or stop the agent after confirmation (second Esc). Ctrl+C,
        // for its part, interrupts immediately.
        (KeyCode::Esc, _) => KeyAction::Escape,
        (KeyCode::Tab, _) => KeyAction::Complete,
        (KeyCode::BackTab, _) => KeyAction::CycleWorkMode,
        // Line editing, the same gestures as in line mode and in a shell.
        (KeyCode::Char('a'), true) => {
            input.move_home();
            KeyAction::None
        }
        (KeyCode::Char('e'), true) => {
            input.move_end();
            KeyAction::None
        }
        (KeyCode::Char('u'), true) => {
            input.kill_to_start();
            KeyAction::None
        }
        (KeyCode::Char('w'), true) | (KeyCode::Backspace, true) => {
            input.delete_word_back();
            KeyAction::None
        }
        (KeyCode::Backspace, _) => {
            input.backspace();
            KeyAction::None
        }
        (KeyCode::Delete, _) => {
            input.delete();
            KeyAction::None
        }
        (KeyCode::Left, _) => {
            input.move_left();
            KeyAction::None
        }
        (KeyCode::Right, _) => {
            input.move_right();
            KeyAction::None
        }
        (KeyCode::Home, _) => {
            input.move_home();
            KeyAction::None
        }
        (KeyCode::End, _) => {
            input.move_end();
            KeyAction::None
        }
        (KeyCode::Up, _) => {
            input.history_previous();
            KeyAction::None
        }
        (KeyCode::Down, _) => {
            input.history_next();
            KeyAction::None
        }
        (KeyCode::Char(c), false) => {
            input.insert(c);
            KeyAction::None
        }
        _ => KeyAction::None,
    }
}

/// Report of a `/…` command: rows, each flagged as error or not.
#[derive(Debug, Default)]
pub struct CommandOutput {
    pub lines: Vec<CardLine>,
    pub quit: bool,
}

/// A report row, with its shape: rendering adapts it to the screen width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardLine {
    Plain(String),
    Warn(String),
    /// Discreet remark.
    Dim(String),
    /// Subheading.
    Section(String),
    /// List item: name, description.
    Item(String, String),
    /// Field: key, value.
    Field(String, String),
}

impl CardLine {
    pub fn is_warn(&self) -> bool {
        matches!(self, CardLine::Warn(_))
    }

    /// Flat text (brief message, remote control).
    pub fn text(&self) -> String {
        match self {
            CardLine::Plain(t) | CardLine::Warn(t) | CardLine::Dim(t) | CardLine::Section(t) => t.clone(),
            CardLine::Item(name, detail) if detail.is_empty() => name.clone(),
            CardLine::Item(name, detail) => format!("{name} — {detail}"),
            CardLine::Field(key, value) => po_i18n::t!("tui.card.field", key = key, value = value),
        }
    }

    /// Conversation row.
    pub fn line(self) -> Line {
        match self {
            CardLine::Plain(t) => Line::new(LineKind::Card, t),
            CardLine::Warn(t) => Line::new(LineKind::CardWarn, t),
            CardLine::Dim(t) => Line::new(LineKind::CardDim, t),
            CardLine::Section(t) => Line::new(LineKind::CardSection, t),
            CardLine::Item(name, detail) => Line::new(LineKind::CardItem, name).with_detail(detail),
            CardLine::Field(key, value) => Line::new(LineKind::CardField, key).with_detail(value),
        }
    }
}

/// Report of a command in the conversation; "done" when it said nothing.
fn push_output(app: &mut App, lines: Vec<CardLine>) {
    if lines.is_empty() {
        app.transcript.push(Line::new(LineKind::Card, po_i18n::t!("common.done")));
    }
    for line in lines {
        app.transcript.push(line.line());
    }
}

#[async_trait::async_trait(?Send)]
pub trait Host {
    /// Runs a `/…` command. It is never sent to the model.
    async fn command(&mut self, line: &str, agent: &mut Agent) -> CommandOutput;

    /// Commands offered to completion: name and description.
    fn completions(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Workspace, to complete `@…` paths.
    fn workspace(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Tabs of the settings panel, reread at each opening and after each change.
    fn settings(&mut self, _agent: &Agent) -> Vec<settings::Tab> {
        Vec::new()
    }

    /// Cycles the budget mode and returns its new label.
    fn cycle_budget(&mut self, agent: &mut Agent) -> String;
    /// Describes the available local models.
    fn local_listing(&mut self) -> Vec<String>;
    /// Toggles the pinned model.
    fn toggle_pin(&mut self, agent: &mut Agent) -> String;

    /// End of a turn: the host saves the conversation, so an abrupt stop does not lose it.
    fn after_turn(&mut self, _agent: &Agent) {}

    /// Inventory of local models for the picker, reread at each opening: free VRAM changes
    /// during the session, and a plan computed ten minutes ago lies.
    fn local_models(&mut self) -> Vec<state::LocalModel>;

    /// State of the local server, in one line.
    fn server_state(&mut self) -> String;

    /// Loads the model at the given index and adds it to the session. Returns a report to
    /// display — success or failure alike, because a startup that fails silently is worse than
    /// one that fails loudly.
    async fn activate_local(&mut self, index: usize, agent: &mut Agent) -> String;

    /// Stops the server launched by Po and gives the VRAM back.
    async fn stop_local(&mut self, agent: &mut Agent) -> String;
}

/// Main loop. Returns when the user quits.
///
/// Returns `false` if full-screen mode could not be established: the caller must fall back to
/// the streaming output rather than leave the user without an interface.
pub async fn run(
    terminal: &mut Terminal,
    agent: &mut Agent,
    host: &mut impl Host,
    approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: mpsc::UnboundedReceiver<question::QuestionRequest>,
) -> std::io::Result<bool> {
    let (guard, _profile) = terminal.enter_best_effort();
    let Some(guard) = guard else {
        return Ok(false);
    };
    watchdog::start();
    let result = event_loop(terminal, &guard, agent, host, approvals, questions).await;
    watchdog::stop();
    drop(guard);
    result.map(|()| true)
}

/// Wheel capture, handed back to the terminal on exit no matter what. Without capture, a finger
/// swipe (Termius) or a wheel turn never reaches the application.
struct MouseCapture(bool);

impl MouseCapture {
    fn set(&mut self, on: bool) {
        if on == self.0 {
            return;
        }
        let result = if on {
            crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
        } else {
            crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)
        };
        if result.is_ok() {
            self.0 = on;
        }
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        self.set(false);
    }
}

/// Waits for a long operation (loading a model) while keeping the beat: without it, the
/// watchdog would take the wait for a blockage.
async fn keep_alive<T>(work: impl std::future::Future<Output = T>) -> T {
    tokio::pin!(work);
    let mut alive = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            output = &mut work => return output,
            _ = alive.tick() => watchdog::beat(),
        }
    }
}

/// Shift+Tab: next mode. Switching to bypass also grants the pending request — it is the
/// natural gesture when an authorization has just appeared.
fn cycle_work_mode(app: &mut App) {
    let Some(handle) = app.work_mode.as_ref() else { return };
    let mode = handle.cycle();
    if mode == po_agent::work_mode::WorkMode::Bypass
        && let Some(request) = app.pending_approval.take()
    {
        request.answer(true);
    }
    app.toast(match mode {
        po_agent::work_mode::WorkMode::Plan => po_i18n::t!("tui.toast.mode_plan"),
        po_agent::work_mode::WorkMode::Build => po_i18n::t!("tui.toast.mode_build"),
        po_agent::work_mode::WorkMode::Bypass => po_i18n::t!("tui.toast.mode_bypass"),
    });
}

/// Esc. First closes what is open; otherwise, if there is something to stop, asks for
/// confirmation at the first press and returns true at the second.
fn escape_pressed(app: &mut App, running: bool, background: usize) -> bool {
    if app.monitor_open {
        app.monitor_open = false;
        return false;
    }
    if !running && background == 0 {
        return false;
    }
    if app.stop_armed.is_some_and(|at| at.elapsed() < STOP_CONFIRM) {
        app.stop_armed = None;
        return true;
    }
    app.stop_armed = Some(std::time::Instant::now());
    let what = match (running, background) {
        (true, 0) => po_i18n::t!("tui.stop.agent").to_owned(),
        (true, 1) => po_i18n::t!("tui.stop.agent_and_one", n = 1),
        (true, n) => po_i18n::t!("tui.stop.agent_and_many", n = n),
        (false, 1) => po_i18n::t!("tui.stop.background_one", n = 1),
        (false, n) => po_i18n::t!("tui.stop.background_many", n = n),
    };
    app.toast(po_i18n::t!("tui.toast.escape_again", what = what));
    false
}

/// During a turn, with an empty input: ↑ takes the last waiting message back into the input to
/// edit it — it no longer waits, so the agent cannot take it until it is sent again — and Esc
/// cancels it. Returns true if the key was used.
fn queue_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.input.text().is_empty() || app.queued.is_empty() || key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    match key.code {
        KeyCode::Up => {
            if let Some(text) = app.queued.pop_back() {
                app.input.insert_str(&text);
                app.toast(po_i18n::t!("tui.toast.queued_editing"));
            }
            true
        }
        KeyCode::Esc => {
            if let Some(text) = app.queued.pop_back() {
                app.toast(po_i18n::t!("tui.toast.queued_cancelled", text = text));
            }
            true
        }
        _ => false,
    }
}

/// Hands the waiting messages to the paused agent: they reach the model before its next step.
/// Commands and `!commands` stay for the end of the turn.
fn deliver_queued(app: &mut App, agent: &mut Agent) {
    let mut rest = std::collections::VecDeque::new();
    while let Some(text) = app.queued.pop_front() {
        if text.starts_with('/') || text.starts_with('!') {
            rest.push_back(text);
            continue;
        }
        agent.inject_user_message(&text);
        app.transcript.push_text(LineKind::User, &text);
        app.transcript.scroll_to_bottom();
    }
    app.queued = rest;
}

/// Subagents still running, according to their frame (the agent is busy with the turn).
fn agent_background_hint(app: &App) -> usize {
    app.monitor.views.iter().filter(|v| v.finished.is_none()).count()
}

/// Wheel: three rows per notch, the step of the usual terminals.
#[cfg(test)]
fn on_mouse(app: &mut App, event: MouseEvent, max_scroll: usize, block: Option<(usize, u16)>) -> bool {
    on_mouse_at(app, event, max_scroll, block, Hit::None)
}

/// What a click hits, outside the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    None,
    /// A subagent of the small frame: opens its big card.
    MonitorPanel(usize),
    /// The cross of the big card: closes it.
    MonitorClose,
    /// The inside of the big card: nothing to do, and above all do not touch the conversation.
    MonitorCard,
    /// The "dictate" button under the input.
    Voice,
    /// Left or right half of a "‹ prev · next ›" row.
    CardPrev,
    CardNext,
    /// The Po Labs indicator.
    Labs,
}

fn on_mouse_at(app: &mut App, event: MouseEvent, max_scroll: usize, block: Option<(usize, u16)>, hit: Hit) -> bool {
    // `/mouse test`: every event is displayed, to know what the terminal really sends.
    if app.mouse_debug {
        app.toast(po_i18n::t!("tui.toast.mouse_event", kind = format!("{:?}", event.kind), column = event.column, row = event.row, hit = format!("{:?}", hit)));
    }
    if let MouseEventKind::Down(MouseButton::Left) = event.kind {
        match hit {
            Hit::CardPrev => {
                app.switch_agent(-1);
                return true;
            }
            Hit::Labs => {
                app.pending_command = Some("/labs".into());
                return true;
            }
            Hit::CardNext => {
                app.switch_agent(1);
                return true;
            }
            // A card waits to know whether it is a touch or a swipe.
            Hit::MonitorPanel(_) | Hit::MonitorCard => {
                app.press = Some(Press { column: event.column, row: event.row, at: std::time::Instant::now(), hit, block });
                return false;
            }
            Hit::MonitorClose => {
                app.monitor_open = false;
                return true;
            }
            Hit::Voice => {
                app.toggle_voice();
                return true;
            }
            Hit::None if app.monitor_open => return false,
            Hit::None => {}
        }
    }
    // The release of a touch on an arrow must do nothing more.
    if matches!(hit, Hit::CardPrev | Hit::CardNext) && app.press.is_none() {
        return app.mouse_debug;
    }
    // After a press on a card: a horizontal drag changes card, a release in place is a touch.
    if let Some(press) = app.press {
        let dx = i32::from(event.column) - i32::from(press.column);
        let dy = i32::from(event.row) - i32::from(press.row);
        let swipe = dx.abs() >= SWIPE_COLUMNS && dx.abs() > dy.abs();
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) if swipe => {
                app.press = None;
                // The finger moves left: the next card arrives from the right.
                app.switch_agent(if dx < 0 { 1 } else { -1 });
                return true;
            }
            MouseEventKind::Drag(_) => return false,
            MouseEventKind::Up(_) => {
                app.press = None;
                if swipe {
                    app.switch_agent(if dx < 0 { 1 } else { -1 });
                    return true;
                }
                return app.tap(press);
            }
            _ => {}
        }
    }
    // Horizontal wheel (touchpad) on a card: same effect as a swipe.
    if matches!(hit, Hit::MonitorPanel(_) | Hit::MonitorCard) {
        match event.kind {
            MouseEventKind::ScrollRight => {
                app.switch_agent(1);
                return true;
            }
            MouseEventKind::ScrollLeft => {
                app.switch_agent(-1);
                return true;
            }
            _ => {}
        }
    }
    // Big card open: scrolling applies to the subagent's conversation.
    if app.monitor_open {
        let selected = app.monitor_selected;
        let Some(view) = app.monitor.views.get_mut(selected) else { return false };
        match event.kind {
            MouseEventKind::ScrollUp => view.transcript.scroll_rows(3, max_scroll),
            MouseEventKind::ScrollDown => view.transcript.scroll_rows(-3, max_scroll),
            _ => return false,
        }
        return true;
    }
    match event.kind {
        MouseEventKind::ScrollUp => app.transcript.scroll_rows(3, max_scroll),
        MouseEventKind::ScrollDown => app.transcript.scroll_rows(-3, max_scroll),
        // A touch (Termius) or a click on a folded block expands it; again, folds it.
        MouseEventKind::Down(MouseButton::Left) => {
            let Some((key, row)) = block else { return false };
            if !app.toggled.remove(&key) {
                app.toggled.insert(key);
            }
            app.anchor.set(Some((key, row)));
        }
        _ => return false,
    }
    true
}

/// Has the session title written after the first exchange, in a background task.
fn request_title(app: &mut App, agent: &Agent, titles: &mpsc::UnboundedSender<String>) {
    if app.title.is_some() || app.title_pending || agent.session.messages.is_empty() {
        return;
    }
    let Some(job) = agent.title_job() else { return };
    app.title_pending = true;
    let titles = titles.clone();
    tokio::spawn(async move {
        if let Some(title) = job.await {
            let _ = titles.send(title);
        }
    });
}

/// Commands specific to the interface, handled without going through the host. Returns true if
/// the command was handled here.
fn tui_command(
    app: &mut App,
    text: &str,
    tabs: &mut dyn FnMut() -> Vec<settings::Tab>,
    mouse: &mut MouseCapture,
) -> bool {
    let mut words = text.split_whitespace();
    let command = words.next().unwrap_or("");
    let argument = words.next();
    match (command, argument) {
        ("/souris" | "/mouse", Some("test")) => {
            app.mouse_debug = !app.mouse_debug;
            app.toast(if app.mouse_debug {
                po_i18n::t!("tui.toast.mouse_test_on")
            } else {
                po_i18n::t!("tui.toast.mouse_test_off")
            });
            true
        }
        ("/souris" | "/mouse", arg) => {
            let on = match arg {
                Some("on" | "oui") => true,
                Some("off" | "non") => false,
                _ => !app.mouse,
            };
            app.mouse = on;
            mouse.set(on);
            app.toast(if on {
                po_i18n::t!("tui.toast.mouse_on")
            } else {
                po_i18n::t!("tui.toast.mouse_off")
            });
            true
        }
        ("/titre" | "/title", Some(_)) => {
            let title = text.split_once(' ').map(|(_, t)| t.trim().to_owned()).unwrap_or_default();
            app.title = po_agent::clean_title(&title);
            app.title_pending = false;
            app.toast(po_i18n::t!("tui.toast.title_changed"));
            true
        }
        ("/titre" | "/title", None) => {
            app.title = None;
            app.title_pending = false;
            app.toast(po_i18n::t!("tui.toast.title_rewrite"));
            true
        }
        ("/parametres" | "/paramètres" | "/reglages" | "/réglages" | "/settings", _) => {
            app.panel = Some(settings::Panel::new(tabs()));
            true
        }
        ("/redemarrer" | "/redémarrer" | "/restart", _) => {
            app.restart = true;
            app.toast(po_i18n::t!("tui.toast.restarting"));
            true
        }
        ("/controle" | "/contrôle" | "/control", _) => {
            let lines = match &app.control_url {
                Some(url) => vec![
                    CardLine::Field(po_i18n::t!("tui.card.control.address").into(), url.clone()),
                    CardLine::Section(po_i18n::t!("tui.card.control.connect").into()),
                    CardLine::Item("Po".into(), po_i18n::t!("tui.card.control.po_add", placeholder = po_i18n::t!("tui.card.control.placeholder"))),
                    CardLine::Item("Claude Code".into(), format!("claude mcp add --transport http po-live {}", po_i18n::t!("tui.card.control.placeholder"))),
                    CardLine::Dim(po_i18n::t!("tui.card.control.tools").into()),
                ],
                None => vec![CardLine::Warn(po_i18n::t!("tui.card.control.unavailable").into())],
            };
            app.transcript.push(Line::new(LineKind::CardTitle, po_i18n::t!("tui.card.control.title")));
            push_output(app, lines);
            true
        }
        ("/agents" | "/agent" | "/sous-agents", None) => {
            app.panel = Some(settings::Panel::new(tabs()).at(po_i18n::t!("cmd.panel.tab.subagents")));
            true
        }
        _ => false,
    }
}

/// Runs a host command while keeping the animation alive.
async fn host_command(
    app: &mut App,
    text: &str,
    agent: &mut Agent,
    host: &mut impl Host,
    backend: &mut view::Backend,
    terminal: &Terminal,
) -> std::io::Result<CommandOutput> {
    app.status.busy = true;
    backend.draw(app, terminal)?;
    let output = {
        let command = host.command(text, agent);
        tokio::pin!(command);
        let mut beat = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                output = &mut command => break output,
                _ = beat.tick() => {
                    app.frame = app.frame.wrapping_add(1);
                    backend.draw(app, terminal)?;
                }
            }
        }
    };
    app.status.busy = false;
    app.status.mode = Some(agent.router().mode());
    refresh_context(app, agent);
    // A `/voice` command makes the "dictate" button appear or disappear; `/labs` turns the
    // indicator on; `/language` changes the offered commands.
    app.voice_enabled = voice::Settings::load().enabled;
    app.labs = po_i18n::prefs::get_bool("labs").unwrap_or(false);
    app.commands = host.completions();
    Ok(output)
}

/// Context gauge of the status bar.
fn refresh_context(app: &mut App, agent: &Agent) {
    app.status.context = agent.session_model().map(|m| (agent.estimated_request_tokens(), m.context_window));
}

/// Key in the settings panel. Returns false when the panel has closed.
async fn panel_key(
    app: &mut App,
    key: KeyEvent,
    agent: &mut Agent,
    host: &mut impl Host,
    backend: &mut view::Backend,
    terminal: &Terminal,
) -> std::io::Result<()> {
    let Some(effect) = panel_effect(app, key) else {
        return Ok(());
    };
    match effect {
        settings::Effect::None => {}
        settings::Effect::Close => app.panel = None,
        settings::Effect::Prompt(prefix) => {
            app.panel = None;
            let len = prefix.chars().count();
            app.input.replace(prefix, len);
        }
        settings::Effect::Run(command) => {
            let output = host_command(app, &command, agent, host, backend, terminal).await?;
            // A setting's report fits on one line: a brief message is enough, the value itself is
            // already visible in the panel.
            if let Some(first) = output.lines.iter().find(|l| l.is_warn()).or(output.lines.first()) {
                app.toast(first.text().trim().to_owned());
            }
            let tabs = host.settings(agent);
            if let Some(panel) = app.panel.as_mut() {
                panel.refresh(tabs);
            }
        }
    }
    Ok(())
}

/// What a key in the settings panel asks for.
fn panel_effect(app: &mut App, key: KeyEvent) -> Option<settings::Effect> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let panel = app.panel.as_mut()?;
    let effect = match key.code {
        KeyCode::Esc | KeyCode::F(2) | KeyCode::Char('q') => settings::Effect::Close,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => settings::Effect::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            panel.move_row(-1);
            settings::Effect::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            panel.move_row(1);
            settings::Effect::None
        }
        KeyCode::Tab => {
            panel.move_tab(1);
            settings::Effect::None
        }
        KeyCode::BackTab => {
            panel.move_tab(-1);
            settings::Effect::None
        }
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            let index = c as usize - '1' as usize;
            if index < panel.tabs.len() {
                panel.tab = index;
                panel.row = 0;
            }
            settings::Effect::None
        }
        KeyCode::Left | KeyCode::Char('h') => panel.cycle(-1),
        KeyCode::Right | KeyCode::Char('l') => panel.cycle(1),
        KeyCode::Enter | KeyCode::Char(' ') => panel.activate(),
        _ => settings::Effect::None,
    };
    Some(effect)
}

/// Key in the panel during a turn: the agent is busy, the change is noted and applied at the
/// next safe point. The panel already shows the new value.
fn panel_key_during_turn(app: &mut App, key: KeyEvent, pause: &AtomicBool) {
    match panel_effect(app, key) {
        None | Some(settings::Effect::None) => {}
        Some(settings::Effect::Close) => app.panel = None,
        Some(settings::Effect::Prompt(prefix)) => {
            app.panel = None;
            let len = prefix.chars().count();
            app.input.replace(prefix, len);
        }
        Some(settings::Effect::Run(command)) => defer_command(app, command, false, pause),
    }
}

/// Notes a setting command for the turn's next safe point.
fn defer_command(app: &mut App, command: String, typed: bool, pause: &AtomicBool) {
    // Two changes of the same setting: only the last one counts.
    let head: String = command
        .split_whitespace()
        .take(if command.starts_with("/agents") { 3 } else { 1 })
        .collect::<Vec<_>>()
        .join(" ");
    app.deferred.retain(|(c, _)| !c.starts_with(&head));
    app.deferred.push((command, typed));
    pause.store(true, std::sync::atomic::Ordering::Relaxed);
    app.toast(po_i18n::t!("tui.toast.setting_noted"));
}

/// Commands that only apply between two turns: they replace the conversation, summarize it, or
/// load a model — none of that can be done in the middle of a turn.
fn between_turns_only(command: &str) -> bool {
    let name = command.split_whitespace().next().unwrap_or("");
    matches!(
        name,
        "/nouveau"
            | "/new"
            | "/clear"
            | "/reprendre"
            | "/resume"
            | "/compacter"
            | "/compact"
            | "/quitter"
            | "/quit"
            | "/q"
            | "/exit"
            | "/local"
            | "/serveur"
            | "/server"
    ) || command.starts_with('!')
}

/// Applies the commands noted during the turn.
async fn apply_deferred(
    app: &mut App,
    agent: &mut Agent,
    host: &mut impl Host,
    backend: &mut view::Backend,
    terminal: &Terminal,
) -> std::io::Result<()> {
    if app.deferred.is_empty() {
        return Ok(());
    }
    let busy = app.status.busy;
    let mut applied = Vec::new();
    for (command, typed) in std::mem::take(&mut app.deferred) {
        let output = host_command(app, &command, agent, host, backend, terminal).await?;
        if typed {
            app.transcript.push(Line::new(LineKind::CardTitle, command.clone()));
            push_output(app, output.lines);
        }
        applied.push(command);
    }
    app.status.busy = busy;
    app.tabs_cache = host.settings(agent);
    if let Some(panel) = app.panel.as_mut() {
        panel.refresh(app.tabs_cache.clone());
    }
    app.toast(po_i18n::t!("tui.toast.applied", list = applied.join(", ")));
    Ok(())
}

/// Read replies of the remote control: state, screen, conversation.
fn control_read(app: &App, backend: &view::Backend, action: &control::Action) -> (String, bool) {
    match action {
        control::Action::Screen => (backend.screen_text(), false),
        control::Action::Transcript { last } => {
            let lines = app.transcript.lines();
            let start = lines.len().saturating_sub(*last);
            let text = lines[start..]
                .iter()
                .map(|line| {
                    let label = match line.kind {
                        LineKind::User => po_i18n::t!("tui.control.label.user"),
                        LineKind::Assistant => po_i18n::t!("tui.control.label.assistant"),
                        LineKind::Thinking | LineKind::ThinkingSummary => po_i18n::t!("tui.control.label.thinking"),
                        LineKind::Tool | LineKind::ToolRunning => po_i18n::t!("tui.control.label.tool"),
                        LineKind::ToolError => po_i18n::t!("tui.control.label.tool_error"),
                        LineKind::Delegation => po_i18n::t!("tui.control.label.subagent"),
                        LineKind::Notice => po_i18n::t!("tui.control.label.note"),
                        LineKind::Error => po_i18n::t!("tui.control.label.error"),
                        LineKind::Summary => po_i18n::t!("tui.control.label.summary"),
                        LineKind::CardTitle
                        | LineKind::Card
                        | LineKind::CardWarn
                        | LineKind::CardSection
                        | LineKind::CardItem
                        | LineKind::CardField
                        | LineKind::CardDim => po_i18n::t!("tui.control.label.card"),
                    };
                    match &line.progress {
                        Some(progress) => format!("[{label}] {} — {progress}", line.text),
                        None => format!("[{label}] {}", line.text),
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            (text, false)
        }
        _ => {
            let mut out = vec![
                po_i18n::t!("tui.control.status.title", title = app.title.as_deref().unwrap_or(po_i18n::t!("tui.control.status.no_title"))),
                po_i18n::t!("tui.control.status.model", model = app.status.model),
                po_i18n::t!("tui.control.status.busy", state = if app.status.busy { po_i18n::t!("common.yes") } else { po_i18n::t!("common.no") }),
            ];
            if let Some(mode) = app.work_mode.as_ref() {
                out.push(po_i18n::t!("tui.control.status.mode", mode = mode.get().label()));
            }
            if let Some((used, window)) = app.status.context {
                out.push(po_i18n::t!("tui.control.status.context", used = used, window = window));
            }
            for view in &app.monitor.views {
                out.push(po_i18n::t!("tui.control.status.subagent", agent = view.agent, status = view.status_line()));
            }
            if !app.queued.is_empty() {
                out.push(po_i18n::t!("tui.control.status.queued", n = app.queued.len()));
            }
            if !app.deferred.is_empty() {
                out.push(po_i18n::t!("tui.control.status.deferred", n = app.deferred.len()));
            }
            (out.join("\n"), false)
        }
    }
}

/// Keys of the big card: switch between subagents, scroll.
fn card_key(app: &mut App, key: KeyEvent, max_scroll: usize, height: usize) -> bool {
    if !app.monitor_open || key.kind == KeyEventKind::Release {
        return false;
    }
    let count = app.monitor.views.len();
    match key.code {
        KeyCode::Left if count > 0 => app.switch_agent(-1),
        KeyCode::Right if count > 0 => app.switch_agent(1),
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
            let Some(view) = app.monitor.views.get_mut(app.monitor_selected) else { return true };
            let step = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) { (height / 2).max(1) } else { 3 };
            let delta =
                if matches!(key.code, KeyCode::Up | KeyCode::PageUp) { step as isize } else { -(step as isize) };
            view.transcript.scroll_rows(delta, max_scroll);
        }
        _ => return false,
    }
    true
}

/// Key in the palette.
fn finder_key(app: &mut App, key: KeyEvent, tabs: &mut dyn FnMut() -> Vec<settings::Tab>) {
    if key.kind == KeyEventKind::Release {
        return;
    }
    let Some(finder) = app.finder.as_mut() else {
        return;
    };
    let ctrl = po_term::input::is_shortcut_ctrl(key.modifiers);
    match key.code {
        KeyCode::Esc => app.finder = None,
        KeyCode::Char('k' | 'c') if ctrl => app.finder = None,
        KeyCode::Up => finder.move_by(-1),
        KeyCode::Down | KeyCode::Tab => finder.move_by(1),
        KeyCode::Backspace => finder.backspace(),
        KeyCode::Enter => {
            let chosen = finder.chosen();
            app.finder = None;
            match chosen {
                Some(settings::Target::Command(name)) => {
                    let text = format!("{name} ");
                    let len = text.chars().count();
                    app.input.replace(text, len);
                }
                Some(settings::Target::Setting { tab, row }) => {
                    let mut panel = settings::Panel::new(tabs());
                    panel.tab = tab.min(panel.tabs.len().saturating_sub(1));
                    panel.row = row;
                    panel.move_row(0);
                    app.panel = Some(panel);
                }
                None => {}
            }
        }
        KeyCode::Char(c) if !ctrl => finder.type_char(c),
        _ => {}
    }
}

async fn event_loop(
    terminal: &Terminal,
    _guard: &TerminalGuard,
    agent: &mut Agent,
    host: &mut impl Host,
    mut approvals: mpsc::UnboundedReceiver<ApprovalRequest>,
    mut questions: mpsc::UnboundedReceiver<question::QuestionRequest>,
) -> std::io::Result<()> {
    let mut app = App::new();
    app.commands = host.completions();
    app.workspace = host.workspace();
    let mut events = EventStream::new();
    let mut backend = view::Backend::new(terminal)?;

    app.status.mode = Some(agent.router().mode());
    app.work_mode = Some(agent.work_mode());
    app.title = agent.title.clone();
    app.replay(&agent.session.messages);
    let (monitor_tx, mut monitor_rx) = mpsc::unbounded_channel();
    agent.set_subagent_monitor(monitor_tx);
    let (title_tx, mut title_rx) = mpsc::unbounded_channel::<String>();
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<control::Request>();
    let (voice_tx, mut voice_rx) = mpsc::unbounded_channel::<Result<String, String>>();
    app.voice_tx = Some(voice_tx);
    app.voice_enabled = voice::Settings::load().enabled;
    // Kept until exit: the file announcing the address disappears with it.
    let _control = control::start(app.workspace.clone(), control_tx).await.ok();
    app.control_url = _control.as_ref().map(|c| c.url.clone());
    refresh_context(&mut app, agent);
    let mut mouse = MouseCapture(false);
    mouse.set(app.mouse);
    if po_term::env::tmux_mouse_off() {
        // Without this option, tmux keeps the gestures for itself: finger or wheel scrolling
        // would never reach Po. Saying so beats a screen that does not react.
        app.note(po_i18n::t!("tui.note.tmux_mouse"));
    }
    backend.draw(&app, terminal)?;

    // At rest nothing is redrawn: this beat tells the watchdog that the loop is alive.
    let mut alive = tokio::time::interval(Duration::from_millis(250));
    alive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // A color wave was on screen at the last draw.
    let mut wave = render::take_wave();
    loop {
        wave |= render::take_wave();
        // A touch on an indicator requests a command.
        if let Some(command) = app.pending_command.take() {
            if !tui_command(&mut app, &command, &mut || host.settings(agent), &mut mouse) {
                app.transcript.push(Line::new(LineKind::CardTitle, command.clone()));
                let output = host_command(&mut app, &command, agent, host, &mut backend, terminal).await?;
                push_output(&mut app, output.lines);
                app.transcript.scroll_to_bottom();
            }
            backend.draw(&app, terminal)?;
            wave = render::take_wave();
            continue;
        }
        if app.restart {
            RESTART.store(true, std::sync::atomic::Ordering::Relaxed);
            break;
        }
        // A submitted message — from the keyboard or the remote control — goes out here.
        if let Some(text) = app.pending_submit.take() {
            app.transcript.push_text(LineKind::User, &text);
            app.transcript.scroll_to_bottom();
            run_turn(&mut app, agent, host, &text, &mut events, &mut approvals, &mut questions,
                     &mut monitor_rx, &mut control_rx, &mut voice_rx, &mut backend, terminal, &mut mouse).await?;
            if let Some(title) = &app.title {
                agent.title = Some(title.clone());
            }
            request_title(&mut app, agent, &title_tx);
            host.after_turn(agent);
            refresh_context(&mut app, agent);
            // Messages queued during the turn go out next. After an interruption, the user takes
            // control back: the first message waits in the input instead of leaving alone.
            while let Some(next) = app.queued.pop_front() {
                if app.restart {
                    app.queued.push_front(next);
                    break;
                }
                if app.last_interrupted || next.starts_with('/') || next.starts_with('!') {
                    app.input.insert_str(&next);
                    let rest: Vec<String> = app.queued.drain(..).collect();
                    for other in rest {
                        app.note(po_i18n::t!("tui.note.waiting", text = other));
                    }
                    break;
                }
                app.transcript.push_text(LineKind::User, &next);
                app.transcript.scroll_to_bottom();
                run_turn(&mut app, agent, host, &next, &mut events, &mut approvals, &mut questions,
                         &mut monitor_rx, &mut control_rx, &mut voice_rx, &mut backend, terminal, &mut mouse).await?;
                host.after_turn(agent);
            }
            backend.draw(&app, terminal)?;
            continue;
        }
        tokio::select! {
            // Turning card or color wave: closely spaced frames.
            _ = tokio::time::sleep(app.animation_delay(wave).unwrap_or_default()), if app.animation_delay(wave).is_some() => {
                backend.draw(&app, terminal)?;
                wave = render::take_wave();
            }
            Some(result) = voice_rx.recv() => {
                app.voice_result(result);
                backend.draw(&app, terminal)?;
            }
            Some(request) = control_rx.recv() => {
                let answer = match request.action.clone() {
                    control::Action::Command(command) => {
                        if tui_command(&mut app, &command, &mut || host.settings(agent), &mut mouse) {
                            (po_i18n::t!("common.done").to_owned(), false)
                        } else {
                            app.transcript.push(Line::new(LineKind::CardTitle, po_i18n::t!("tui.control.remote_title", command = command)));
                            let output = host_command(&mut app, &command, agent, host, &mut backend, terminal).await?;
                            let warned = output.lines.iter().any(CardLine::is_warn);
                            let text: Vec<String> = output.lines.iter().map(CardLine::text).collect();
                            push_output(&mut app, output.lines);
                            app.transcript.scroll_to_bottom();
                            if output.quit {
                                let _ = request.reply.send((po_i18n::t!("tui.control.closing").to_owned(), false));
                                break;
                            }
                            (if text.is_empty() { po_i18n::t!("common.done").to_owned() } else { text.join("\n") }, warned)
                        }
                    }
                    control::Action::Prompt(text) => {
                        app.pending_submit = Some(text);
                        (po_i18n::t!("tui.control.prompt_sent").to_owned(), false)
                    }
                    control::Action::Restart => {
                        app.restart = true;
                        (po_i18n::t!("tui.control.restart_now").to_owned(), false)
                    }
                    other => control_read(&app, &backend, &other),
                };
                let _ = request.reply.send(answer);
                backend.draw(&app, terminal)?;
            }
            _ = alive.tick() => {
                watchdog::beat();
                if app.expire_press() {
                    backend.draw(&app, terminal)?;
                }
                if watchdog::take_scribbled() {
                    backend.invalidate()?;
                    backend.draw(&app, terminal)?;
                } else if app.monitor.any_visible() || !app.monitor.views.is_empty() || app.voice_started.is_some() {
                    // A background subagent is still working: its frame animates, and disappears by
                    // itself a few seconds after the end.
                    app.frame = app.frame.wrapping_add(1);
                    backend.draw(&app, terminal)?;
                }
                // The time in the information row advances even at rest.
                if chrono::Local::now().timestamp() % 30 == 0 {
                    backend.draw(&app, terminal)?;
                }
            }
            Some(update) = monitor_rx.recv() => {
                app.apply_subagent(update);
                backend.draw(&app, terminal)?;
            }
            Some(title) = title_rx.recv() => {
                app.title = Some(title.clone());
                agent.title = Some(title);
                app.title_pending = false;
                backend.draw(&app, terminal)?;
            }
            maybe_event = events.next() => {
                let Some(Ok(event)) = maybe_event else { break };
                match event {
                    Event::Key(key) => {
                        if app.mouse_debug && !matches!(key.code, KeyCode::Char(_)) && key.kind != KeyEventKind::Release {
                            app.toast(po_i18n::t!("tui.toast.key_event", code = format!("{:?}", key.code), modifiers = format!("{:?}", key.modifiers)));
                        }
                        // Palette and panel are modal: as long as they are open, they take the
                        // keyboard.
                        if app.finder.is_some() {
                            finder_key(&mut app, key, &mut || host.settings(agent));
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        if app.panel.is_some() {
                            panel_key(&mut app, key, agent, host, &mut backend, terminal).await?;
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        // The picker is modal: as long as it is open, it takes the keyboard.
                        if app.picker.is_some() {
                            match map_picker_key(key) {
                                PickerAction::Move(delta) => {
                                    if let Some(picker) = app.picker.as_mut() {
                                        picker.move_by(delta);
                                    }
                                }
                                PickerAction::Choose => {
                                    let choice = app.picker.as_ref().and_then(|p| {
                                        p.current().map(|e| (p.selected(), e.clone()))
                                    });
                                    if let Some((index, entry)) = choice {
                                        app.picker = None;
                                        if !entry.viable {
                                            app.note(po_i18n::t!("tui.note.too_big", label = entry.label));
                                        } else {
                                            // Loading a multi-gigabyte model takes tens of
                                            // seconds: say so beforehand, otherwise the interface
                                            // looks frozen.
                                            app.note(po_i18n::t!("tui.note.loading", label = entry.label));
                                            app.status.busy = true;
                                            backend.draw(&app, terminal)?;
                                            let message = keep_alive(host.activate_local(index, agent)).await;
                                            app.status.busy = false;
                                            app.note(message);
                                        }
                                    } else {
                                        app.picker = None;
                                    }
                                }
                                PickerAction::Stop => {
                                    app.picker = None;
                                    app.status.busy = true;
                                    backend.draw(&app, terminal)?;
                                    let message = keep_alive(host.stop_local(agent)).await;
                                    app.status.busy = false;
                                    app.note(message);
                                }
                                PickerAction::Close => app.picker = None,
                                PickerAction::None => {}
                            }
                            backend.draw(&app, terminal)?;
                            continue;
                        }

                        if card_key(&mut app, key, backend.card_max_scroll().unwrap_or(0), backend.transcript_height()) {
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        if app.pending_approval.is_none() && app.completion_key(key) {
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        // An Enter followed by a burst of keystrokes is a paste delivered key by key
                        // (ConPTY strips the paste markers): it is a line break, not a send.
                        // Without this, the first line used to go out alone.
                        if key.code == KeyCode::Enter
                            && key.kind != KeyEventKind::Release
                            && (key.modifiers - KeyModifiers::CONTROL).is_empty()
                            && app.pending_approval.is_none()
                            && !app.input.is_empty()
                            && absorb_paste_burst(&mut events, &mut app.input, key.modifiers).await
                        {
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        let action = map_key(key, &mut app.input, app.pending_approval.is_some(), false);
                        match action {
                            KeyAction::Quit => break,
                            KeyAction::Submit(text) if text.starts_with('/') || text.starts_with('!') => {
                                if !tui_command(&mut app, &text, &mut || host.settings(agent), &mut mouse) {
                                    // The report goes into a card titled by the command: placed before
                                    // execution, it shows that the command has started.
                                    app.transcript.push(Line::new(LineKind::CardTitle, text.clone()));
                                    app.transcript.scroll_to_bottom();
                                    let output = host_command(&mut app, &text, agent, host, &mut backend, terminal).await?;
                                    push_output(&mut app, output.lines);
                                    app.transcript.scroll_to_bottom();
                                    if output.quit {
                                        break;
                                    }
                                }
                            }
                            KeyAction::Submit(text) => app.pending_submit = Some(text),
                            KeyAction::CycleBudget => {
                                let label = host.cycle_budget(agent);
                                app.status.mode = Some(agent.router().mode());
                                app.toast(po_i18n::t!("tui.toast.budget_mode", label = label));
                            }
                            KeyAction::ShowLocal => {
                                let entries = host.local_models();
                                if entries.is_empty() {
                                    app.note(po_i18n::t!("tui.note.no_gguf"));
                                } else {
                                    let server_state = host.server_state();
                                    app.picker = Some(state::Picker::new(entries, server_state));
                                }
                            }
                            KeyAction::PinModel => {
                                let message = host.toggle_pin(agent);
                                app.toast(message);
                            }
                            KeyAction::StopServer => {
                                app.status.busy = true;
                                backend.draw(&app, terminal)?;
                                let message = keep_alive(host.stop_local(agent)).await;
                                app.status.busy = false;
                                app.note(message);
                            }
                            KeyAction::ToggleHelp => app.show_help = !app.show_help,
                            KeyAction::Complete => {
                                app.complete();
                            }
                            KeyAction::CycleWorkMode => cycle_work_mode(&mut app),
                            KeyAction::Dictate => app.toggle_voice(),
                            KeyAction::Escape => {
                                if escape_pressed(&mut app, false, agent.background_count()) {
                                    let stopped = agent.cancel_background();
                                    app.monitor.stop_all();
                                    app.note(po_i18n::t!("tui.note.background_stopped", n = stopped));
                                }
                            }
                            KeyAction::ToggleExpand => {
                                app.expanded = !app.expanded;
                                app.toast(if app.expanded { po_i18n::t!("tui.toast.expanded") } else { po_i18n::t!("tui.toast.collapsed") });
                            }
                            KeyAction::OpenSettings => {
                                app.panel = Some(settings::Panel::new(host.settings(agent)));
                            }
                            KeyAction::OpenPalette => {
                                let tabs = host.settings(agent);
                                app.finder = Some(settings::Finder::new(&app.commands, &tabs));
                            }
                            KeyAction::ScrollUp => {
                                let height = backend.transcript_height();
                                app.transcript.scroll_rows((height / 2) as isize, backend.max_scroll());
                            }
                            KeyAction::ScrollDown => {
                                let height = backend.transcript_height();
                                app.transcript.scroll_rows(-((height / 2) as isize), backend.max_scroll());
                            }
                            _ => {}
                        }
                        backend.draw(&app, terminal)?;
                    }
                    Event::Resize(_, _) => backend.draw(&app, terminal)?,
                    Event::Mouse(event) => {
                        let max = backend.card_max_scroll().unwrap_or(backend.max_scroll());
                        if on_mouse_at(&mut app, event, max, backend.block_at(event.column, event.row), backend.hit(event.column, event.row)) {
                            backend.draw(&app, terminal)?;
                        }
                    }
                    // Bracketed paste (Unix): the text arrives as a block, line breaks included.
                    Event::Paste(text) if app.picker.is_none() && app.pending_approval.is_none() => {
                        app.input.insert_str(&text);
                        backend.draw(&app, terminal)?;
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Runs a turn while keeping the interface alive. Settings changed during the turn pause the
/// agent at the next safe point: they are applied, then the turn resumes.
#[allow(clippy::too_many_arguments)]
async fn run_turn(
    app: &mut App,
    agent: &mut Agent,
    host: &mut impl Host,
    prompt: &str,
    events: &mut EventStream,
    approvals: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: &mut mpsc::UnboundedReceiver<question::QuestionRequest>,
    monitor: &mut mpsc::UnboundedReceiver<po_agent::SubagentUpdate>,
    control: &mut mpsc::UnboundedReceiver<control::Request>,
    voice: &mut mpsc::UnboundedReceiver<Result<String, String>>,
    backend: &mut view::Backend,
    terminal: &Terminal,
    mouse: &mut MouseCapture,
) -> std::io::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.status.busy = true;
    app.progress = Some(Progress::new());
    app.frame = 0;
    // During the turn the agent is busy: the settings panel opens on these tabs.
    app.tabs_cache = host.settings(agent);
    backend.draw(app, terminal)?;
    // History length before the turn: if the user interrupts, this is the point we go back to so
    // the session stays consistent.
    let checkpoint = agent.session.messages.len();
    let pause = agent.pause_handle();
    let mut first = true;
    let (finished, interrupted) = loop {
        let mut sink = |event| {
            let _ = tx.send(event);
        };
        let (finished, interrupted) = if first {
            let turn = agent.turn(prompt, &mut sink);
            drive_turn(app, turn, &mut rx, events, approvals, questions, monitor, control, voice, backend, terminal, &pause, mouse)
                .await?
        } else {
            let turn = agent.resume(&mut sink);
            drive_turn(app, turn, &mut rx, events, approvals, questions, monitor, control, voice, backend, terminal, &pause, mouse)
                .await?
        };
        first = false;
        // The channel may still hold events emitted just before the end.
        while let Ok(event) = rx.try_recv() {
            app.absorb(event);
        }
        if interrupted || !agent.is_paused() {
            break (finished, interrupted);
        }
        // Safe point: the tools have returned their results, the model has not resumed yet.
        apply_deferred(app, agent, host, backend, terminal).await?;
        deliver_queued(app, agent);
    };
    pause.store(false, std::sync::atomic::Ordering::Relaxed);

    app.last_interrupted = interrupted;
    if interrupted {
        restore_after_interruption(&mut agent.session.messages, checkpoint);
        // Background subagents stop with the agent; those of the turn are already stopped with
        // it (their work was part of the turn).
        let stopped = agent.cancel_background();
        app.monitor.stop_all();
        app.note(if stopped > 0 {
            po_i18n::t!("tui.note.interrupted_with", n = stopped)
        } else {
            po_i18n::t!("tui.note.interrupted").to_owned()
        });
    } else if let Some(Err(err)) = finished {
        app.error(err.to_string());
    }
    app.progress = None;
    app.status.busy = false;
    // What found no safe point is applied now, before the next turn.
    apply_deferred(app, agent, host, backend, terminal).await?;
    app.status.update_from_ledger(&agent.session.ledger);
    app.transcript.scroll_to_bottom();
    backend.draw(app, terminal)?;
    Ok(())
}

/// Drives a turn (or its resumption) to its end, keeping the interface alive: rendering,
/// approval requests, questions, subagents, keyboard. Returns its outcome, and whether it was
/// interrupted.
#[allow(clippy::too_many_arguments)]
async fn drive_turn(
    app: &mut App,
    turn: impl std::future::Future<Output = Result<(), po_agent::AgentError>>,
    rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    events: &mut EventStream,
    approvals: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: &mut mpsc::UnboundedReceiver<question::QuestionRequest>,
    monitor: &mut mpsc::UnboundedReceiver<po_agent::SubagentUpdate>,
    control: &mut mpsc::UnboundedReceiver<control::Request>,
    voice: &mut mpsc::UnboundedReceiver<Result<String, String>>,
    backend: &mut view::Backend,
    terminal: &Terminal,
    pause: &AtomicBool,
    mouse: &mut MouseCapture,
) -> std::io::Result<(Option<Result<(), po_agent::AgentError>>, bool)> {
    let mut interrupted = false;
    let mut last_draw = std::time::Instant::now();
    let mut beat = tokio::time::interval(TICK);
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(turn);

    let mut finished = None;
    while finished.is_none() && !interrupted {
        tokio::select! {
            result = &mut turn => finished = Some(result),
            // The beat keeps the animation alive even when the model says nothing: a local model
            // reading a long prompt can stay silent for several seconds.
            _ = beat.tick() => {
                app.frame = app.frame.wrapping_add(1);
                app.expire_press();
                backend.draw(app, terminal)?;
                last_draw = std::time::Instant::now();
            }
            _ = tokio::time::sleep(Duration::from_millis(16)), if app.animating() => {
                backend.draw(app, terminal)?;
                last_draw = std::time::Instant::now();
            }
            Some(event) = rx.recv() => {
                app.absorb(event);
                // A fast model emits dozens of fragments per second: redrawing at each one wastes
                // CPU and makes the screen flicker. Beyond sixty frames per second, the beat
                // catches up with the rest.
                if app.transcript.is_at_bottom()
                    && last_draw.elapsed() >= std::time::Duration::from_millis(16)
                {
                    backend.draw(app, terminal)?;
                    last_draw = std::time::Instant::now();
                }
            }
            // One request at a time: the next one waits in the channel. Overwriting would abandon
            // the first, refused outright without the user having seen it.
            Some(request) = approvals.recv(), if app.pending_approval.is_none() => {
                app.pending_approval = Some(request);
                backend.draw(app, terminal)?;
            }
            Some(update) = monitor.recv() => {
                app.apply_subagent(update);
                if last_draw.elapsed() >= std::time::Duration::from_millis(16) {
                    backend.draw(app, terminal)?;
                    last_draw = std::time::Instant::now();
                }
            }
            Some(request) = questions.recv(), if app.pending_question.is_none() => {
                app.pending_question = Some(question::Pending::new(request));
                app.transcript.scroll_to_bottom();
                backend.draw(app, terminal)?;
            }
            // Dictation also works during a turn: the next message is being prepared.
            Some(result) = voice.recv() => {
                app.voice_result(result);
                backend.draw(app, terminal)?;
            }
            // Remote control during a turn: reading answers right away, a setting waits for the
            // next safe point, a message waits for the end of the turn.
            Some(request) = control.recv() => {
                let answer = match request.action.clone() {
                    control::Action::Command(command) if between_turns_only(&command) => {
                        app.queued.push_back(command);
                        (po_i18n::t!("tui.control.queued_command").to_owned(), false)
                    }
                    control::Action::Command(command) => {
                        let cache = app.tabs_cache.clone();
                        if !tui_command(app, &command, &mut || cache.clone(), mouse) {
                            defer_command(app, command, true, pause);
                        }
                        (po_i18n::t!("tui.control.noted").to_owned(), false)
                    }
                    control::Action::Prompt(text) => {
                        app.queued.push_back(text);
                        (po_i18n::t!("tui.control.queued_prompt").to_owned(), false)
                    }
                    control::Action::Restart => {
                        app.restart = true;
                        (po_i18n::t!("tui.control.restart_later").to_owned(), false)
                    }
                    other => control_read(app, backend, &other),
                };
                let _ = request.reply.send(answer);
                backend.draw(app, terminal)?;
            }
            maybe_event = events.next() => {
                if let Some(Ok(Event::Paste(text))) = &maybe_event
                    && app.pending_approval.is_none()
                {
                    app.input.insert_str(text);
                    backend.draw(app, terminal)?;
                }
                if let Some(Ok(Event::Mouse(event))) = &maybe_event {
                    let max = backend.card_max_scroll().unwrap_or(backend.max_scroll());
                    if on_mouse_at(app, *event, max, backend.block_at(event.column, event.row), backend.hit(event.column, event.row)) {
                        backend.draw(app, terminal)?;
                    }
                }
                if let Some(Ok(Event::Key(key))) = maybe_event
                    && let Some(pending) = app.pending_question.as_mut()
                {
                    // The question takes the keyboard; Ctrl+C still interrupts the turn.
                    if key.code == KeyCode::Char('c') && po_term::input::is_shortcut_ctrl(key.modifiers) {
                        interrupted = true;
                    } else if let Some(answer) = pending.key(key)
                        && let Some(done) = app.pending_question.take()
                    {
                        done.request.answer(answer);
                    }
                    backend.draw(app, terminal)?;
                    continue;
                }
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    // Panel and palette also open during a turn: a setting changed there is noted and
                    // applied at the next safe point.
                    if app.finder.is_some() {
                        let cache = app.tabs_cache.clone();
                        finder_key(app, key, &mut || cache.clone());
                        backend.draw(app, terminal)?;
                        continue;
                    }
                    if app.panel.is_some() {
                        panel_key_during_turn(app, key, pause);
                        backend.draw(app, terminal)?;
                        continue;
                    }
                    if app.pending_approval.is_none()
                        && card_key(app, key, backend.card_max_scroll().unwrap_or(0), backend.transcript_height())
                    {
                        backend.draw(app, terminal)?;
                        continue;
                    }
                    if app.pending_approval.is_none() && app.completion_key(key) {
                        backend.draw(app, terminal)?;
                        continue;
                    }
                    if app.pending_approval.is_none() && queue_key(app, key) {
                        backend.draw(app, terminal)?;
                        continue;
                    }
                    let action = map_key(key, &mut app.input, app.pending_approval.is_some(), true);
                    match action {
                        KeyAction::Approve(granted) => {
                            if let Some(request) = app.pending_approval.take() {
                                request.answer(granted);
                            }
                        }
                        // Dropping the turn's future closes the connection: the server stops
                        // generating, which frees a local model's GPU right away.
                        KeyAction::Interrupt => interrupted = true,
                        KeyAction::Escape => {
                            let background = agent_background_hint(app);
                            if escape_pressed(app, true, background) {
                                interrupted = true;
                            }
                        }
                        KeyAction::Complete => {
                            app.complete();
                        }
                        KeyAction::Queue(text) if text.starts_with('/') && !between_turns_only(&text) => {
                            // An interface command runs right away; a setting waits for the turn's
                            // next safe point.
                            let cache = app.tabs_cache.clone();
                            if !tui_command(app, &text, &mut || cache.clone(), mouse) {
                                defer_command(app, text, true, pause);
                            }
                        }
                        KeyAction::Queue(text) => {
                            app.toast(po_i18n::t!("tui.toast.queued", text = text));
                            // A message goes in at the next safe point, after the tools of the
                            // step in progress; a `!command` waits for the end of the turn.
                            if !text.starts_with('!') {
                                pause.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            app.queued.push_back(text);
                        }
                        KeyAction::ToggleExpand => app.expanded = !app.expanded,
                        KeyAction::CycleWorkMode => cycle_work_mode(app),
                        KeyAction::Dictate => app.toggle_voice(),
                        KeyAction::OpenSettings => {
                            app.panel = Some(settings::Panel::new(app.tabs_cache.clone()));
                        }
                        KeyAction::OpenPalette => {
                            app.finder = Some(settings::Finder::new(&app.commands, &app.tabs_cache));
                        }
                        KeyAction::ScrollUp => {
                            let height = backend.transcript_height();
                            app.transcript.scroll_rows((height / 2) as isize, backend.max_scroll());
                        }
                        KeyAction::ScrollDown => {
                            let height = backend.transcript_height();
                            app.transcript.scroll_rows(-((height / 2) as isize), backend.max_scroll());
                        }
                        _ => {}
                    }
                    backend.draw(app, terminal)?;
                }
            }
        }
    }
    Ok((finished, interrupted))
}

/// Maximum gap between two keys of the same paste. A human never chains Enter and the next key
/// in under a few dozen milliseconds; a paste delivered key by key does.
const PASTE_GAP: Duration = Duration::from_millis(8);

/// After an Enter, absorbs the keys that immediately follow it. Returns true if there were any:
/// the Enter was then part of a paste and has been converted into a line break.
async fn absorb_paste_burst(events: &mut EventStream, input: &mut Input, trigger: KeyModifiers) -> bool {
    let mut burst = false;
    // ConPTY delivers `\r` as Enter and `\n` as Ctrl+Enter (measured): a pasted CRLF makes two
    // keys for a single line end. We remember whether the last key was a CR.
    let mut last_cr = !trigger.contains(KeyModifiers::CONTROL);
    loop {
        let Ok(Some(Ok(event))) = tokio::time::timeout(PASTE_GAP, events.next()).await else {
            return burst;
        };
        let text: Option<String> = match event {
            // The release of the Enter itself arrives immediately on Windows: it proves nothing.
            Event::Key(key) if key.kind == KeyEventKind::Release => None,
            Event::Key(key) if key.code == KeyCode::Enter => {
                let lf = key.modifiers.contains(KeyModifiers::CONTROL);
                let second_half_of_crlf = lf && last_cr;
                last_cr = !lf;
                if second_half_of_crlf { Some(String::new()) } else { Some("\n".into()) }
            }
            Event::Key(key) => {
                last_cr = false;
                match key.code {
                    KeyCode::Tab => Some("\t".into()),
                    KeyCode::Char(c) => Some(c.to_string()),
                    _ => None,
                }
            }
            Event::Paste(text) => Some(text),
            _ => None,
        };
        if let Some(text) = text {
            if !burst {
                input.insert('\n');
                burst = true;
            }
            input.insert_str(&text);
        }
    }
}

/// Puts the history back into a state providers accept after an abandoned turn.
///
/// A turn cut mid-flight can leave a tool call without a result, or a user message without an
/// answer; most servers reject such a history. We keep the user's request, drop the rest, and
/// add an answer saying what happened so the model does not replay the half-done action.
pub fn restore_after_interruption(messages: &mut Vec<po_core::Message>, checkpoint: usize) {
    messages.truncate((checkpoint + 1).min(messages.len()));
    messages.push(po_core::Message::assistant(
        "[Turn interrupted by the user before the end. Some actions may have been carried out; \
         check the state before continuing.]",
    ));
}

/// Maximum wait between two refreshes while a turn is in progress.
pub const TICK: Duration = Duration::from_millis(120);

/// Builds an approver tied to a channel, and the receiver the loop will consume.
pub fn approval_channel() -> (Arc<ChannelApprover>, mpsc::UnboundedReceiver<ApprovalRequest>) {
    approval::channel()
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_core::Usage;
    use po_router::{Reason, TaskClass};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn the_picker_navigates_with_arrows_and_confirms_with_enter() {
        assert_eq!(map_picker_key(key(KeyCode::Up)), PickerAction::Move(-1));
        assert_eq!(map_picker_key(key(KeyCode::Down)), PickerAction::Move(1));
        assert_eq!(map_picker_key(key(KeyCode::Enter)), PickerAction::Choose);
        assert_eq!(map_picker_key(key(KeyCode::Esc)), PickerAction::Close);
        assert_eq!(map_picker_key(key(KeyCode::Char('s'))), PickerAction::Stop);
    }

    #[test]
    fn the_picker_also_accepts_vi_keys() {
        // Many tmux and SSH users navigate with j/k without thinking about it.
        assert_eq!(map_picker_key(key(KeyCode::Char('j'))), PickerAction::Move(1));
        assert_eq!(map_picker_key(key(KeyCode::Char('k'))), PickerAction::Move(-1));
    }

    #[test]
    fn ctrl_l_closes_what_it_opened() {
        assert_eq!(map_picker_key(ctrl('l')), PickerAction::Close);
    }

    #[test]
    fn the_picker_does_not_let_keys_through_to_the_input() {
        // The worst scenario: thinking you navigate a list while actually writing into the
        // conversation. While the picker is open, no unknown key has any effect.
        for code in [KeyCode::Char('a'), KeyCode::Char('7'), KeyCode::Tab, KeyCode::Backspace] {
            assert_eq!(map_picker_key(key(code)), PickerAction::None, "{code:?}");
        }
    }

    #[test]
    fn a_key_release_does_not_move_the_selection() {
        // Windows emits a release event for every key: handling it would make the selection jump
        // two notches on each arrow.
        let mut release = key(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        assert_eq!(map_picker_key(release), PickerAction::None);
    }

    #[test]
    fn walking_the_list_stays_within_bounds_whatever_the_key() {
        // The picker and its keyboard are written separately; this test makes them work
        // together, because overflows hide between the two.
        let entries: Vec<_> = (0..6)
            .map(|i| state::LocalModel {
                label: format!("model-{i}"),
                detail: "8592 Mio".into(),
                loaded: i == 2,
                viable: true,
            })
            .collect();
        let mut picker = state::Picker::new(entries, "ready");
        assert_eq!(picker.selected(), 2, "we start from the loaded model");

        for code in [KeyCode::PageDown, KeyCode::PageDown, KeyCode::Down] {
            if let PickerAction::Move(delta) = map_picker_key(key(code)) {
                picker.move_by(delta);
            }
        }
        assert_eq!(picker.selected(), 5, "bounded to the last");

        for _ in 0..4 {
            if let PickerAction::Move(delta) = map_picker_key(key(KeyCode::PageUp)) {
                picker.move_by(delta);
            }
        }
        assert_eq!(picker.selected(), 0, "bounded to the first");
        assert_eq!(picker.current().unwrap().label, "model-0");
    }

    #[test]
    fn the_keyboard_really_moves_subagents_from_cloud_to_local() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        // The concrete case of the requirements, verified in a single chain: the key pressed in
        // the middle of a conversation, the action it triggers, the resulting budget mode, and
        // the actual destination of the next subagent. Testing these links separately would
        // let a break between them through.
        use po_core::{Capabilities, ModelSpec, Origin, Pricing, Tier};
        use po_router::{BudgetMode, Demand, Router, TaskClass};

        fn model(id: &str, tier: Tier, local: bool, price: f64) -> ModelSpec {
            ModelSpec {
                id: id.into(),
                display_name: id.into(),
                origin: if local {
                    Origin::Local { backend: "llama.cpp".into(), endpoint: "http://127.0.0.1:8080".into() }
                } else {
                    Origin::Cloud { provider: "anthropic".into() }
                },
                tier,
                capabilities: Capabilities { tools: true, ..Default::default() },
                context_window: 200_000,
                max_output_tokens: 8192,
                pricing: Pricing { input_per_mtok: price, output_per_mtok: price * 5.0, ..Default::default() },
            }
        }

        let mut router = Router::new(
            vec![model("opus-5", Tier::Frontier, false, 15.0), model("qwen3.8-27b", Tier::Strong, true, 0.0)],
            BudgetMode::MaxQuality,
        );

        let destination = |router: &Router| -> bool {
            let choice = router.select(&Demand::new(TaskClass::Search)).unwrap();
            router.get(&choice.model_id).unwrap().is_local()
        };

        assert!(!destination(&router), "in max quality, the subagent must stay on the frontier model");

        // The real shortcut table, not a constant copied for the test.
        let mut input = Input::default();
        for _ in 0..2 {
            assert_eq!(
                map_key(ctrl('g'), &mut input, false, false),
                KeyAction::CycleBudget,
                "Ctrl+G must be recognized"
            );
            router.cycle_mode();
        }
        assert_eq!(router.mode(), BudgetMode::SaveMaxTokens);
        assert!(input.is_empty(), "the shortcut must write nothing into the input");

        assert!(destination(&router), "in max savings, the subagent must go to the local model");
    }

    #[test]
    fn no_shortcut_reuses_a_multiplexer_prefix() {
        // tmux listens to Ctrl+B, GNU screen to Ctrl+A. Reusing them would doom the shortcut
        // everywhere the user works inside a multiplexer — that is, on most remote sessions.
        for (key, _) in shortcuts() {
            assert_ne!(key, "Ctrl+B", "tmux prefix");
            assert_ne!(key, "Ctrl+A", "screen prefix");
        }
    }

    #[test]
    fn advertised_shortcuts_are_the_ones_that_respond() {
        // A help table that lies is worse than no help at all.
        let mut input = Input::default();
        for (key, _) in shortcuts() {
            let Some(letter) = key.strip_prefix("Ctrl+").and_then(|s| s.chars().next()) else {
                continue;
            };
            let letter = letter.to_ascii_lowercase();
            if letter == 'c' {
                continue; // Ctrl+C depends on the state: interrupt or quit.
            }
            assert_ne!(
                map_key(ctrl(letter), &mut input, false, false),
                KeyAction::None,
                "Ctrl+{letter} is advertised but does nothing"
            );
        }
    }

    #[test]
    fn ordinary_typing_feeds_the_input() {
        let mut input = Input::default();
        for c in "abc".chars() {
            assert_eq!(map_key(key(KeyCode::Char(c)), &mut input, false, false), KeyAction::None);
        }
        assert_eq!(input.text(), "abc");
    }

    #[test]
    fn enter_submits_the_input() {
        let mut input = Input::default();
        for c in "hello".chars() {
            map_key(key(KeyCode::Char(c)), &mut input, false, false);
        }
        assert_eq!(map_key(key(KeyCode::Enter), &mut input, false, false), KeyAction::Submit("hello".into()));
        assert!(input.is_empty());
    }

    #[test]
    fn enter_on_an_empty_input_does_nothing() {
        let mut input = Input::default();
        assert_eq!(map_key(key(KeyCode::Enter), &mut input, false, false), KeyAction::None);
    }

    #[test]
    fn during_a_turn_a_submitted_message_is_queued_without_leaving() {
        let mut input = Input::default();
        input.insert('a');
        // It is not sent right away (no Submit), it waits for the end of the turn.
        assert_eq!(map_key(key(KeyCode::Enter), &mut input, false, true), KeyAction::Queue("a".into()));
        assert!(input.is_empty());
    }

    #[test]
    fn ctrl_c_interrupts_during_a_turn_and_quits_otherwise() {
        let mut input = Input::default();
        assert_eq!(map_key(ctrl('c'), &mut input, false, true), KeyAction::Interrupt);
        assert_eq!(map_key(ctrl('c'), &mut input, false, false), KeyAction::Quit);
    }

    #[test]
    fn control_shortcuts_are_recognized() {
        let mut input = Input::default();
        assert_eq!(map_key(ctrl('g'), &mut input, false, false), KeyAction::CycleBudget);
        assert_eq!(map_key(ctrl('l'), &mut input, false, false), KeyAction::ShowLocal);
        assert_eq!(map_key(ctrl('o'), &mut input, false, false), KeyAction::ToggleExpand);
        assert_eq!(map_key(ctrl('k'), &mut input, false, false), KeyAction::OpenPalette);
        assert_eq!(map_key(key(KeyCode::F(2)), &mut input, false, false), KeyAction::OpenSettings);
        assert_eq!(map_key(ctrl('p'), &mut input, false, false), KeyAction::PinModel);
        assert_eq!(map_key(ctrl('r'), &mut input, false, false), KeyAction::StopServer);
        assert!(input.is_empty(), "a shortcut must not be written into the input");
    }

    #[test]
    fn the_budget_mode_changes_even_mid_turn() {
        // That is the requirement: switch at any time, without waiting for the end of the answer.
        let mut input = Input::default();
        assert_eq!(map_key(ctrl('g'), &mut input, false, true), KeyAction::CycleBudget);
    }

    #[test]
    fn an_approval_request_monopolizes_the_keyboard() {
        let mut input = Input::default();
        assert_eq!(map_key(key(KeyCode::Char('o')), &mut input, true, false), KeyAction::Approve(true));
        assert_eq!(map_key(key(KeyCode::Char('n')), &mut input, true, false), KeyAction::Approve(false));
        assert_eq!(map_key(key(KeyCode::Esc), &mut input, true, false), KeyAction::Approve(false));
        // Another key must not leak into the input.
        assert_eq!(map_key(key(KeyCode::Char('z')), &mut input, true, false), KeyAction::None);
        assert!(input.is_empty());
    }

    #[test]
    fn key_release_is_ignored() {
        // On Windows, every key produces press then release: without this filter, every
        // character would be written twice.
        let mut input = Input::default();
        let mut release = key(KeyCode::Char('a'));
        release.kind = KeyEventKind::Release;
        map_key(release, &mut input, false, false);
        assert!(input.is_empty());
    }

    #[test]
    fn ctrl_d_only_quits_on_an_empty_input() {
        let mut input = Input::default();
        assert_eq!(map_key(ctrl('d'), &mut input, false, false), KeyAction::Quit);
        input.insert('a');
        assert_ne!(map_key(ctrl('d'), &mut input, false, false), KeyAction::Quit);
    }

    #[test]
    fn a_waiting_message_can_be_taken_back_and_cancelled() {
        let mut app = App::new();
        app.queued.push_back("first".into());
        app.queued.push_back("second".into());
        assert!(queue_key(&mut app, key(KeyCode::Up)));
        assert_eq!(app.input.text(), "second", "the last one comes back into the input");
        assert_eq!(app.queued.len(), 1, "and no longer waits: the agent cannot take it");
        assert!(!queue_key(&mut app, key(KeyCode::Up)), "with text in the input, ↑ is the input's");
        app.input.clear();
        assert!(queue_key(&mut app, key(KeyCode::Esc)));
        assert!(app.queued.is_empty());
        assert!(!queue_key(&mut app, key(KeyCode::Esc)), "nothing waiting: Esc keeps its meaning");
    }

    #[test]
    fn agent_events_become_rows() {
        let mut app = App::new();
        app.absorb(AgentEvent::Text("answer".into()));
        app.absorb(AgentEvent::ToolStarted { name: "read".into(), summary: "a.rs".into() });
        app.absorb(AgentEvent::Delegated {
            model: "qwen".into(),
            agent: "explorer".into(),
            task: "search".into(),
            background: false,
        });
        app.absorb(AgentEvent::TurnCompleted {
            usage: Usage { input_tokens: 10, output_tokens: 5, ..Default::default() },
            cost: 0.01,
            local_share: 0.5,
            local_seconds: 1.5,
        });

        let kinds: Vec<_> = app.transcript.lines().iter().map(|l| l.kind).collect();
        assert!(kinds.contains(&LineKind::Assistant));
        // Not finished yet: the row stays marked "in progress".
        assert!(kinds.contains(&LineKind::ToolRunning));
        assert!(kinds.contains(&LineKind::Delegation));
        assert!(kinds.contains(&LineKind::Summary));
    }

    #[test]
    fn a_compliant_routing_stays_quiet() {
        let mut app = App::new();
        app.absorb(AgentEvent::ModelSelected {
            model: "opus".into(),
            class: TaskClass::Orchestration,
            reason: Reason::Policy,
        });
        assert!(app.transcript.is_empty(), "only deviations deserve a row");
        assert_eq!(app.status.model, "opus");
    }

    #[test]
    fn a_routing_fallback_is_flagged() {
        let mut app = App::new();
        app.absorb(AgentEvent::ModelSelected {
            model: "qwen".into(),
            class: TaskClass::Orchestration,
            reason: Reason::OfflineFallback,
        });
        // Flagged in the status bar, without cluttering the conversation.
        assert!(app.transcript.is_empty());
        assert!(app.current_toast().is_some_and(|t| t.contains("qwen")));
    }

    #[test]
    fn streamed_fragments_assemble_without_duplicates() {
        let mut app = App::new();
        for fragment in ["Hel", "lo", " world"] {
            app.absorb(AgentEvent::TextDelta(fragment.into()));
        }
        // The full text arrives afterwards: it must not rewrite what is already displayed.
        app.absorb(AgentEvent::Text("Hello world".into()));

        assert_eq!(app.transcript.len(), 1);
        assert_eq!(app.transcript.lines()[0].text, "Hello world");
    }

    #[test]
    fn a_fragment_containing_a_line_break_creates_a_row() {
        let mut app = App::new();
        app.absorb(AgentEvent::TextDelta(
            "first\nsecond"
                .into(),
        ));
        app.absorb(AgentEvent::Text(
            "first\nsecond"
                .into(),
        ));
        assert_eq!(app.transcript.len(), 2);
        assert_eq!(app.transcript.lines()[0].text, "first");
        assert_eq!(app.transcript.lines()[1].text, "second");
    }

    #[test]
    fn without_streaming_the_full_text_is_displayed() {
        // A provider that does not stream must remain perfectly usable.
        let mut app = App::new();
        app.absorb(AgentEvent::Text("whole answer".into()));
        assert_eq!(app.transcript.len(), 1);
        assert_eq!(app.transcript.lines()[0].text, "whole answer");
    }

    #[test]
    fn two_streamed_turns_do_not_mix() {
        let mut app = App::new();
        app.absorb(AgentEvent::TextDelta("one".into()));
        app.absorb(AgentEvent::Text("one".into()));
        app.absorb(AgentEvent::TextDelta("two".into()));
        app.absorb(AgentEvent::Text("two".into()));
        assert_eq!(app.transcript.len(), 2);
        assert_eq!(app.transcript.lines()[1].text, "two");
    }

    #[test]
    fn reasoning_does_not_clutter_the_screen() {
        let mut app = App::new();
        app.absorb(AgentEvent::Thinking("long reasoning".repeat(100)));
        assert!(app.transcript.is_empty());
    }

    #[test]
    fn all_advertised_shortcuts_are_documented() {
        for (key, description) in shortcuts() {
            assert!(!key.is_empty());
            assert!(!description.is_empty());
        }
        assert!(shortcuts().iter().any(|(k, _)| k.contains("Ctrl+G")));
    }

    #[test]
    fn progress_follows_what_the_model_does() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.progress = Some(Progress::new());
        assert_eq!(app.progress.as_ref().unwrap().phase, Phase::Waiting);

        app.absorb(AgentEvent::ThinkingDelta(400));
        let p = app.progress.as_ref().unwrap();
        assert_eq!(p.phase, Phase::Thinking);
        assert_eq!(p.estimated_tokens(), 100);

        app.absorb(AgentEvent::TextDelta("hello".into()));
        assert_eq!(app.progress.as_ref().unwrap().phase, Phase::Writing);

        app.absorb(AgentEvent::ToolStarted { name: "read".into(), summary: "a.rs".into() });
        assert_eq!(app.progress.as_ref().unwrap().label(), "Outil · read");
        app.absorb(AgentEvent::ToolFinished { name: "read".into(), summary: "ok".into(), is_error: false });
        assert_eq!(app.progress.as_ref().unwrap().phase, Phase::Waiting);
    }

    #[test]
    fn reasoning_boils_down_to_a_folded_header() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.progress = Some(Progress::new());
        app.absorb(AgentEvent::ThinkingDelta(10_000));
        app.absorb(AgentEvent::TextDelta("Answer.".into()));
        app.absorb(AgentEvent::Thinking("x".repeat(400)));
        app.absorb(AgentEvent::Text("Answer.".into()));
        let rows = app.transcript.lines();
        assert_eq!(rows.len(), 2, "a header, then the answer: nothing else");
        assert_eq!(rows[0].kind, LineKind::ThinkingSummary);
        assert!(rows[0].text.contains("~100 tokens"), "{}", rows[0].text);
        assert_eq!(rows[0].detail.as_deref().map(str::len), Some(400), "the text stays expandable");
        assert_eq!(rows[1].kind, LineKind::Assistant);
    }

    #[test]
    fn the_animation_has_an_ascii_version() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let p = Progress::new();
        let row = p.line(3, false);
        let glyph = row.split(' ').next().unwrap();
        assert!(glyph.is_ascii(), "non-ASCII glyph: {row}");
        assert!(p.line(0, true).contains("Attente du modèle"));
    }

    #[test]
    fn an_interruption_leaves_a_valid_history() {
        use po_core::{ContentBlock, Message, Role};
        let mut messages = vec![Message::user("before"), Message::assistant("answer")];
        let checkpoint = messages.len();
        messages.push(Message::user("request"));
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse { id: "1".into(), name: "bash".into(), input: serde_json::json!({}) }],
        });
        restore_after_interruption(&mut messages, checkpoint);
        assert_eq!(messages.len(), 4);
        assert!(messages.iter().all(|m| !m.has_tool_use()), "no tool call without a result");
        assert_eq!(messages[2].role, Role::User);
        assert_eq!(messages[3].role, Role::Assistant);
    }

    #[test]
    fn alt_enter_and_ctrl_j_insert_a_line_without_sending() {
        let mut input = Input::default();
        input.insert('a');
        let alt = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(map_key(alt, &mut input, false, false), KeyAction::None);
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(map_key(ctrl_j, &mut input, false, false), KeyAction::None);
        assert_eq!(input.text(), "a\n\n");
    }

    #[test]
    fn a_tool_call_fits_on_a_single_row() {
        let mut app = App::new();
        app.absorb(AgentEvent::ToolStarted { name: "read".into(), summary: "a.rs".into() });
        assert_eq!(app.transcript.lines()[0].kind, LineKind::ToolRunning);
        app.absorb(AgentEvent::Notice("in-between".into()));
        app.absorb(AgentEvent::ToolFinished {
            name: "read".into(),
            summary: "read a.rs (3 lines)".into(),
            is_error: false,
        });
        assert_eq!(app.transcript.len(), 2, "no second row for the result");
        assert_eq!(app.transcript.lines()[0].kind, LineKind::Tool);
        assert_eq!(app.transcript.lines()[0].text, "read read a.rs (3 lines)");
        assert!(app.transcript.lines()[0].secs.is_some(), "the duration is kept for the group summary");
    }

    #[test]
    fn the_same_routing_announcement_is_made_only_once() {
        use po_router::{Reason, TaskClass};
        let mut app = App::new();
        for _ in 0..3 {
            app.absorb(AgentEvent::ModelSelected {
                model: "m".into(),
                class: TaskClass::Orchestration,
                reason: Reason::OfflineFallback,
            });
        }
        assert!(app.transcript.is_empty());
        assert_eq!(app.last_route.as_deref().map(|r| r.starts_with("m ·")), Some(true));
    }

    #[test]
    fn altgr_produces_characters_not_shortcuts() {
        // Measured under ConPTY with a French keyboard: `#` arrives as Ctrl+Alt+#.
        let mut input = Input::default();
        for c in ['#', '@', '{', '}', '[', ']', '|', '\\', '~', '€'] {
            let event = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL | KeyModifiers::ALT);
            assert_eq!(map_key(event, &mut input, false, false), KeyAction::None);
        }
        assert_eq!(input.text(), "#@{}[]|\\~€");
        // Ctrl alone remains a shortcut.
        let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert_eq!(map_key(ctrl_g, &mut input, false, false), KeyAction::CycleBudget);
    }

    fn app_with_commands() -> App {
        let mut app = App::new();
        app.commands = vec![
            ("/plan".into(), "mode plan".into()),
            ("/builder".into(), "mode builder".into()),
            ("/bypass".into(), "mode bypass".into()),
        ];
        app
    }

    #[test]
    fn enter_on_a_half_typed_command_picks_the_first_without_sending() {
        let mut app = app_with_commands();
        app.input.insert('/');
        assert!(app.completion_key(key(KeyCode::Enter)), "Enter taken by the list");
        assert_eq!(app.input.text(), "/plan ");
        assert!(!app.completion_key(key(KeyCode::Enter)), "the second Enter sends");
        assert_eq!(map_key(key(KeyCode::Enter), &mut app.input, false, false), KeyAction::Submit("/plan".into()));
    }

    #[test]
    fn arrows_walk_the_list_and_fill_the_input() {
        let mut app = app_with_commands();
        app.input.insert('/');
        app.completion_key(key(KeyCode::Down));
        assert_eq!(app.input.text(), "/plan ");
        assert_eq!(app.suggestions().len(), 3, "the list stays that of \"/\"");
        app.completion_key(key(KeyCode::Down));
        assert_eq!(app.input.text(), "/builder ");
        assert_eq!(app.completion_selected(), 1);
        app.completion_key(key(KeyCode::Up));
        app.completion_key(key(KeyCode::Up));
        assert_eq!(app.input.text(), "/bypass ", "the list wraps around");
        // Enter sends the choice as is.
        assert!(!app.completion_key(key(KeyCode::Enter)));
        // Typing something else ends the walk.
        app.input.insert('x');
        assert!(app.suggestions().is_empty());
        assert!(!app.completion_key(key(KeyCode::Down)), "the arrows give the history back");
    }

    fn three_subagents() -> App {
        let mut app = App::new();
        for (id, agent) in [("a", "explorer"), ("b", "reviewer"), ("c", "verifier")] {
            app.apply_subagent(po_agent::SubagentUpdate::Started {
                id: id.into(),
                agent: agent.into(),
                model: "m".into(),
                task: "t".into(),
                background: false,
            });
        }
        app
    }

    fn mouse_event(kind: MouseEventKind, column: u16) -> MouseEvent {
        MouseEvent { kind, column, row: 5, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn a_swipe_on_the_deck_changes_card_by_turning_the_page() {
        let mut app = three_subagents();
        assert_eq!(app.front_card(), 0, "the first launched in front");
        // The finger moves left: the next card.
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Down(MouseButton::Left), 40), 0, None, Hit::MonitorPanel(0));
        assert!(on_mouse_at(&mut app, mouse_event(MouseEventKind::Drag(MouseButton::Left), 35), 0, None, Hit::None));
        assert_eq!(app.front_card(), 1);
        assert!(app.animating(), "the page turns");
        assert!(!app.monitor_open, "a swipe does not open the big card");
        // To the right: back.
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Down(MouseButton::Left), 30), 0, None, Hit::MonitorPanel(1));
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Up(MouseButton::Left), 36), 0, None, Hit::None);
        assert_eq!(app.front_card(), 0);
        // To the right again: before the first, the last.
        app.switch_agent(-1);
        assert_eq!(app.front_card(), 2);
    }

    #[test]
    fn a_touch_on_the_deck_opens_the_front_card() {
        let mut app = three_subagents();
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Down(MouseButton::Left), 40), 0, None, Hit::MonitorPanel(1));
        assert!(!app.monitor_open, "nothing until the gesture is finished");
        assert!(on_mouse_at(&mut app, mouse_event(MouseEventKind::Up(MouseButton::Left), 41), 0, None, Hit::None));
        assert!(app.monitor_open);
        assert_eq!(app.monitor_selected, 1);
        // In the big card, the arrows also switch agents.
        assert!(card_key(&mut app, key(KeyCode::Right), 0, 20));
        assert_eq!(app.monitor_selected, 2);
    }

    #[test]
    fn touch_arrows_change_card_without_a_swipe() {
        let mut app = three_subagents();
        assert!(on_mouse_at(&mut app, mouse_event(MouseEventKind::Down(MouseButton::Left), 50), 0, None, Hit::CardNext));
        assert_eq!(app.front_card(), 1);
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Up(MouseButton::Left), 50), 0, None, Hit::CardNext);
        assert_eq!(app.front_card(), 1, "the release does not change a second time");
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Down(MouseButton::Left), 30), 0, None, Hit::CardPrev);
        assert_eq!(app.front_card(), 0);
        assert!(!app.monitor_open);
    }

    #[test]
    fn a_never_released_press_ends_as_a_touch() {
        let mut app = three_subagents();
        on_mouse_at(&mut app, mouse_event(MouseEventKind::Down(MouseButton::Left), 40), 0, None, Hit::MonitorPanel(0));
        assert!(!app.expire_press(), "too early");
        app.press.as_mut().unwrap().at -= PRESS_AS_TAP;
        assert!(app.expire_press());
        assert!(app.monitor_open);
    }

    #[test]
    fn a_resumed_session_is_redisplayed() {
        let mut app = App::new();
        let call = po_core::Message {
            role: po_core::Role::Assistant,
            content: vec![
                po_core::ContentBlock::Thinking { text: "I am thinking".into(), signature: None },
                po_core::ContentBlock::ToolUse { id: "1".into(), name: "read".into(), input: serde_json::json!({"path": "a.rs"}) },
            ],
        };
        let result = po_core::Message::tool_results(vec![po_core::ContentBlock::ToolResult {
            tool_use_id: "1".into(),
            content: "content".into(),
            is_error: false,
        }]);
        app.replay(&[po_core::Message::user("read a.rs"), call, result, po_core::Message::assistant("done")]);
        let kinds: Vec<LineKind> = app.transcript.lines().iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [LineKind::User, LineKind::ThinkingSummary, LineKind::Tool, LineKind::Assistant, LineKind::Notice]
        );
        assert!(app.transcript.lines()[2].text.starts_with("read"));
    }

    #[test]
    fn a_subagents_row_shows_what_it_says_it_is_doing() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        // The start can arrive before the delegation row: the two find each other.
        app.apply_subagent(po_agent::SubagentUpdate::Started {
            id: "c1".into(),
            agent: "verifier".into(),
            model: "qwen".into(),
            task: "Context: very long".into(),
            background: false,
        });
        app.absorb(AgentEvent::Delegated {
            model: "qwen".into(),
            agent: "verifier".into(),
            task: "Context: very long".into(),
            background: false,
        });
        app.apply_subagent(po_agent::SubagentUpdate::Text {
            id: "c1".into(),
            text: "En cours : I list the PRs\n".into(),
        });
        let row = app.transcript.lines().iter().find(|l| l.kind == LineKind::Delegation).unwrap();
        assert_eq!(row.progress.as_deref(), Some("I list the PRs"));
        app.apply_subagent(po_agent::SubagentUpdate::Finished { id: "c1".into(), ok: true });
        let row = app.transcript.lines().iter().find(|l| l.kind == LineKind::Delegation).unwrap();
        assert!(row.progress.as_deref().unwrap().starts_with("terminé en"));
    }

    #[test]
    fn displayed_reasoning_precedes_the_answer_without_mixing() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.absorb(AgentEvent::ThinkingText("weighing\nthe options".into()));
        app.absorb(AgentEvent::TextDelta("Answer.".into()));
        app.absorb(AgentEvent::Text("Answer.".into()));
        let rows: Vec<_> = app.transcript.lines().iter().map(|l| (l.kind, l.text.clone())).collect();
        assert_eq!(
            rows,
            vec![
                (LineKind::ThinkingSummary, "Réflexion…".into()),
                (LineKind::Thinking, "weighing".into()),
                (LineKind::Thinking, "the options".into()),
                (LineKind::Assistant, "Answer.".into()),
            ]
        );
        // The full text arriving afterwards does not double what was streamed.
        app.absorb(AgentEvent::Thinking("weighing\nthe options".into()));
        assert!(app.transcript.lines()[0].text.starts_with("Réflexion ·"));
        assert!(app.transcript.lines()[0].detail.is_none());
    }

    #[test]
    fn a_tool_error_is_distinguished_from_a_harness_error() {
        let mut app = App::new();
        app.absorb(AgentEvent::ToolStarted { name: "bash".into(), summary: "cargo test".into() });
        app.absorb(AgentEvent::ToolFinished { name: "bash".into(), summary: "failure".into(), is_error: true });
        app.absorb(AgentEvent::ToolDenied { name: "write".into(), summary: "a.rs".into() });
        assert!(app.transcript.lines().iter().all(|l| l.kind == LineKind::ToolError));
    }

    #[test]
    fn a_command_report_becomes_a_card() {
        let mut app = App::new();
        app.card("/cache", vec![(false, "slot 0: free".into()), (true, "server silent".into())]);
        let kinds: Vec<_> = app.transcript.lines().iter().map(|l| l.kind).collect();
        assert_eq!(kinds, [LineKind::CardTitle, LineKind::Card, LineKind::CardWarn]);
    }

    #[test]
    fn the_wheel_scrolls_within_bounds() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut app = App::new();
        let wheel = |kind| MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::NONE };
        assert!(on_mouse(&mut app, wheel(MouseEventKind::ScrollUp), 4, None));
        assert_eq!(app.transcript.scroll(), 3);
        on_mouse(&mut app, wheel(MouseEventKind::ScrollUp), 4, None);
        assert_eq!(app.transcript.scroll(), 4, "never past the start");
        on_mouse(&mut app, wheel(MouseEventKind::ScrollDown), 4, None);
        on_mouse(&mut app, wheel(MouseEventKind::ScrollDown), 4, None);
        assert!(app.transcript.is_at_bottom());
        assert!(!on_mouse(&mut app, wheel(MouseEventKind::Moved), 4, None), "a simple movement redraws nothing");
    }

    #[test]
    fn the_context_gauge_stays_readable_everywhere() {
        assert_eq!(state::gauge(30_000, 131_072, true), "▰▰▱▱▱ 23 %");
        assert_eq!(state::gauge(0, 131_072, false), "[-----] 0%");
        assert_eq!(state::gauge(200_000, 131_072, false), "[#####] 100%");
    }

    #[test]
    fn during_a_turn_enter_queues_and_escape_interrupts() {
        let mut input = Input::default();
        input.insert('a');
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut input, false, true),
            KeyAction::Queue("a".into())
        );
        assert_eq!(
            map_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut input, false, true),
            KeyAction::Escape
        );
        assert_eq!(map_key(ctrl('c'), &mut input, false, true), KeyAction::Interrupt, "Ctrl+C stays immediate");
    }
}
