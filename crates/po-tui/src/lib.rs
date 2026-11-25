//! `po-tui` - full-screen interface.
//!
//! It does not replace the streaming output: `po-term` decides which of the two suits the
//! terminal, and the TUI is only offered in the `rich` profile. What is visible here is also
//! visible in streaming mode; only the presentation changes.
//!
//! The agent turn runs *inside* the event loop rather than in a detached task: the agent thus
//! does not need to be shared between threads, and the interface stays responsive thanks to a
//! `select!` that keeps handling keys while the model works.

pub mod approval;
pub mod catalog;
pub mod labs;
pub mod mascot;
pub mod menu;
pub mod monitor;
pub mod question;
pub mod render;
pub mod settings;
pub mod state;
pub mod links;
pub mod statusline;
pub mod theme;
pub mod thumbnail;
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
    pub show_thinking: bool,
    pub input: Input,
    pub status: Status,
    /// Pending approval request, displayed as an overlay.
    pub pending_approval: Option<ApprovalRequest>,
    /// Help displayed over the conversation.
    pub show_help: bool,
    /// Local model picker, opened with Ctrl+O.
    pub picker: Option<state::Picker>,
    /// The community catalog's browser (`/catalogue`), modal.
    pub catalog: Option<catalog::Browser>,
    /// A command's choices, when it was typed without one; modal.
    pub menu: Option<menu::Menu>,
    /// The menus that open during a turn (the agent is busy then), prepared at its start.
    menus_cache: Vec<(String, menu::Menu)>,
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
    /// Names commands answer to in any language (see `Host::command_names`).
    pub command_names: Vec<String>,
    /// Workspace root, for path completion (`@…`).
    pub workspace: Option<std::path::PathBuf>,
    /// Messages submitted during a turn, sent in order once the turn is over.
    pub queued: std::collections::VecDeque<String>,
    /// Row of the last finished tool call, which a diff that follows attaches to.
    last_tool: Option<usize>,
    /// Po Labs notebook as last read, and when (reread every second at most).
    pub labs_book: labs::Notebook,
    labs_book_at: Option<std::time::Instant>,
    /// Revision of the notebook whose proposal the user set aside.
    pub labs_dismissed: u64,
    /// The agent not in front: the user's session while Po Labs is, or the Po Labs session kept
    /// aside (paused or done) while the user's session is.
    pub other_agent: Option<Agent>,
    /// Po Labs is in front; the user's session waits behind, with its view.
    pub labs_front: bool,
    behind: Option<ViewStash>,
    /// The notebook is open in the big card.
    pub labs_notebook_open: bool,
    /// Asked by a shortcut, a touch, the timer or a command, done at the next free moment.
    pub labs_launch: bool,
    pub labs_back: bool,
    /// `/labs go`: the Po Labs session in front, even when it could work alongside.
    pub labs_front_request: bool,
    /// The user's session since Po started: turns ended and tools called, so that the change on
    /// trial is asked about once it could be seen at work.
    pub labs_seen: (usize, usize),
    labs_since: std::time::Instant,
    /// The turn count when the user set the question about the change on trial aside.
    pub labs_verdict_later: Option<usize>,
    /// The next message goes to Po Labs: what to adjust in the change on trial.
    pub labs_revising: bool,
    /// A verdict given on the card, carried out at the next free moment.
    pub labs_verdict: Option<labs::Verdict>,
    /// The user's session, as Po Labs' holder: the lease follows it across a restart.
    pub labs_key: String,
    /// This session holds Po Labs.
    pub labs_held: bool,
    /// Another session holds it: this one shows it greyed.
    pub labs_elsewhere: Option<labs::Lease>,
    labs_presence_at: Option<std::time::Instant>,
    /// First Ctrl+Y on Po Labs held elsewhere: a second one takes it here.
    labs_takeover_at: Option<std::time::Instant>,
    /// Po Labs was taken by another session: the session it had here is let go.
    labs_lost: bool,
    /// The last install of Po Labs seen: another one restarts this Po on the new build.
    labs_mark: String,
    /// The contribution Po Labs is writing, in the background.
    pub labs_drafting: Option<u32>,
    /// It was started by the user's request: Po Labs goes on with the rest afterwards.
    labs_draft_chain: bool,
    /// The review of contributions, in the big card.
    pub labs_review: Option<labs::Review>,
    /// `/contributions`: the review of those set aside, at the next free moment.
    pub labs_review_request: bool,
    /// An action on the contribution shown, carried out at the next free moment.
    pub labs_contrib: Option<labs::ContribAction>,
    /// The next message says how to change the text of this contribution.
    pub labs_rewriting: Option<u32>,
    /// Where the writings of contributions come back.
    labs_jobs: Option<mpsc::UnboundedSender<LabsJobDone>>,
    /// A long work of Po Labs runs in the background (a build, a sending): what it is.
    pub labs_busy: Option<String>,
    /// Where those works come back.
    labs_works: Option<mpsc::UnboundedSender<LabsWorkDone>>,
    /// Po Labs works alongside the user's session (a server with several slots): what it is
    /// doing, for its card.
    pub labs_activity: Option<String>,
    /// Restart asked while Po Labs still works or the input is not empty: done once both are free.
    restart_noted: bool,
    /// Po Labs came back from working alongside: its session is to be saved.
    labs_save: bool,
    /// Last key or end of turn: the idle timer counts from there.
    pub last_activity: std::time::Instant,
    /// Paused by the user: the agent keeps its turn, resumed by the same shortcut.
    pub user_pause: bool,
    /// A message or a command was sent: the welcome screen has done its part.
    pub started: bool,
    pub pending_resume: bool,
    /// When the welcome screen opened, and the draw of its greeting.
    pub welcome: (std::time::Instant, u64),
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
    /// What the phone's status shows and only the agent knows: the models, the one pinned, the
    /// reasoning level. Read again whenever the context gauge is.
    pub remote_models: Vec<String>,
    pub remote_pinned: Option<String>,
    pub remote_effort: Option<String>,
    /// What serves the session's model, for the phone.
    pub remote_engine: String,
    /// Each model's engine (backend or cloud provider), by display name, so the engine shown
    /// follows a mid-turn model switch without the agent in hand. Filled at each refresh.
    pub model_engines: std::collections::HashMap<String, String>,
    pub remote_placement: Option<String>,
    /// What the context is made of (system, user, assistant, reasoning, tools), for the phone's
    /// token breakdown. Read with the context gauge.
    pub context_breakdown: Option<po_agent::ContextBreakdown>,
    /// The task goals and where each stands, for the status bar and the phone.
    pub goals: Vec<(String, Option<bool>)>,
    /// Turns finished in this session: the event loop sees from it that a turn just ended (it is
    /// blocked while one runs, so it never sees `busy` set).
    pub turns_done: u64,
    /// The local server was seen without the model since the last turn: when it comes back, the
    /// saved cache is loaded into it. A turn run meanwhile clears it (it rebuilt the cache
    /// itself, and an older copy must not replace that).
    pub unpark_pending: bool,
    pub context_warning: Option<String>,
    pub loop_warning: Option<String>,
    pub dynamic_action: Option<String>,
    pub model_vram_mib: Option<u64>,
    pub model_ram_mib: Option<u64>,
    pub model_gpu_name: Option<String>,
    pub secondary_model: Option<String>,
    pub subagent_parallel: u32,
    pub subagent_model: Option<String>,
    pub secondary_candidates: Vec<po_wire::SecondaryModelRec>,
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
    /// That command runs as it is, without offering its menu.
    pending_plain: bool,
    /// The menu opened again once the pending command has run.
    pending_then: Option<String>,
    /// When the open live menu was last built.
    menu_built: std::time::Instant,
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
/// A restart asked from outside the interface (Po Labs installed a new build): it happens once
/// the turn is over, like `/restart`.
static RESTART_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_restart() {
    RESTART_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// What a long command is doing (a model loading): shown while the interface waits for it.
static PROGRESS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Says what a running command is doing; `None` when it is done. Shown in full screen, ignored
/// in line mode.
pub fn set_progress(text: Option<String>) {
    if let Ok(mut slot) = PROGRESS.lock() {
        *slot = text;
    }
}

/// The progress line of the running command, if it gave one.
pub fn progress() -> Option<String> {
    PROGRESS.lock().ok().and_then(|slot| slot.clone())
}

/// Takes the command's last progress line off the screen once it is over; a toast that says
/// something else stays.
fn clear_progress_toast(app: &mut App, shown: Option<&str>) {
    if shown.is_some_and(|text| app.toast.as_ref().is_some_and(|(toast, _)| toast == text)) {
        app.toast = None;
    }
}

/// A command whose answer calls for a choice (a draft to save or drop) names the menu to open
/// right after it: the user chooses with the arrows instead of typing the next command.
static MENU_AFTER: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn open_menu_after(line: impl Into<String>) {
    if let Ok(mut slot) = MENU_AFTER.lock() {
        *slot = Some(line.into());
    }
}

fn take_menu_after() -> Option<String> {
    MENU_AFTER.lock().ok().and_then(|mut slot| slot.take())
}

/// A load cancellation asked by the user (Escape during a model load, the phone's cancel). The
/// waits in `before_turn` stop at their next checkpoint instead of being dropped mid-launch.
/// The flag a launch watches while the model loads: one for the whole process, so the terminal
/// (which asks) and the launch (which stops itself) hold the same one.
static LOAD_CANCEL: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> = std::sync::OnceLock::new();

pub fn load_cancel_cell() -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    LOAD_CANCEL.get_or_init(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).clone()
}

/// Asks to stop the model load in progress.
pub fn request_load_cancel() {
    load_cancel_cell().store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Has a cancellation been asked for? The waits poll it.
pub fn load_cancel_requested() -> bool {
    load_cancel_cell().load(std::sync::atomic::Ordering::Relaxed)
}

/// A fresh preparation starts with no pending cancellation.
pub fn clear_load_cancel() {
    load_cancel_cell().store(false, std::sync::atomic::Ordering::Relaxed);
}

/// The flag as a launch wants it: `Some(flag)` while the user has not cancelled.
pub fn load_cancel_flag() -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
    Some(load_cancel_cell())
}

/// A pause asked by the agent itself (the Po Labs `pause` tool, when the user asks for one).
static PAUSE_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Po Labs asked from a command (`/labs go`, `/labs back`): done by the interface at the next
/// free moment.
static LABS_REQUEST: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn request_labs(front: bool) {
    LABS_REQUEST.store(if front { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
}

fn take_labs_request() -> u8 {
    LABS_REQUEST.swap(0, std::sync::atomic::Ordering::Relaxed)
}

pub fn request_pause() {
    PAUSE_REQUESTED.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn take_pause_request() -> bool {
    PAUSE_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Takes a restart asked with [`request_restart`].
pub fn take_restart_request() -> bool {
    RESTART_REQUESTED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

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
            show_thinking: false,
            input: Input::default(),
            status: Status::default(),
            pending_approval: None,
            show_help: false,
            picker: None,
            catalog: None,
            menu: None,
            menus_cache: Vec::new(),
            progress: None,
            frame: 0,
            last_stats: None,
            running_tools: Vec::new(),
            last_route: None,
            streaming: false,
            partial: String::new(),
            commands: Vec::new(),
            command_names: Vec::new(),
            workspace: None,
            queued: std::collections::VecDeque::new(),
            last_tool: None,
            labs_book: labs::Notebook::load(),
            labs_book_at: Some(std::time::Instant::now()),
            labs_dismissed: 0,
            other_agent: None,
            labs_front_request: false,
            labs_seen: (0, 0),
            labs_since: std::time::Instant::now(),
            labs_verdict_later: None,
            labs_revising: false,
            labs_verdict: None,
            labs_key: String::new(),
            labs_held: false,
            labs_elsewhere: None,
            labs_presence_at: None,
            labs_takeover_at: None,
            labs_lost: false,
            labs_mark: String::new(),
            labs_drafting: None,
            labs_draft_chain: false,
            labs_review: None,
            labs_review_request: false,
            labs_contrib: None,
            labs_rewriting: None,
            labs_jobs: None,
            labs_busy: None,
            labs_works: None,
            labs_activity: None,
            restart_noted: false,
            labs_save: false,
            labs_front: false,
            behind: None,
            labs_notebook_open: false,
            labs_launch: false,
            labs_back: false,
            last_activity: std::time::Instant::now(),
            user_pause: false,
            started: false,
            pending_resume: false,
            welcome: (
                std::time::Instant::now(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64 / 1000),
            ),
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
            remote_models: Vec::new(),
            remote_pinned: None,
            remote_effort: None,
            remote_engine: String::new(),
            model_engines: std::collections::HashMap::new(),
            remote_placement: None,
            context_breakdown: None,
            goals: Vec::new(),
            turns_done: 0,
            unpark_pending: false,
            context_warning: None,
            loop_warning: None,
            dynamic_action: None,
            model_vram_mib: None,
            model_ram_mib: None,
            model_gpu_name: None,
            secondary_model: None,
            subagent_parallel: 1,
            subagent_model: None,
            secondary_candidates: Vec::new(),
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
            pending_plain: false,
            pending_then: None,
            menu_built: std::time::Instant::now(),
        }
    }

    /// Redisplays a resumed conversation (`--resume`, in-place restart): messages, folded
    /// reasoning, tool calls. Tool results stay in the session.
    /// The conversation rebuilt from a history; `root` resolves the images it mentions.
    pub fn replay(&mut self, messages: &[po_core::Message], root: &std::path::Path) {
        use po_core::{ContentBlock, Role};
        for message in messages {
            match message.role {
                // A tool-results message is not user input.
                Role::User if message.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })) => {}
                Role::User => {
                    for block in &message.content {
                        if let ContentBlock::Text { text } = block {
                            if po_agent::session::is_summary(text) {
                                self.note(po_i18n::t!("tui.note.compacted"));
                                continue;
                            }
                            let typed = po_agent::session::typed_text(text);
                            if !typed.is_empty() {
                                self.transcript.push_text(LineKind::User, &typed);
                                for image in po_agent::mentions::image_mentions(&typed) {
                                    self.transcript.push(Line::new(LineKind::Image, root.join(image).display().to_string()));
                                }
                            }
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
        } else if self.welcome_shown() {
            // The mascot bounces, then only blinks: frames can space out.
            let moving = mascot::moving(self.welcome.0.elapsed());
            Some(Duration::from_millis(if moving { 40 } else { 120 }))
        } else if wave {
            Some(Duration::from_millis(90))
        } else {
            None
        }
    }

    /// The welcome screen shows until the first message or command of the session: from then on,
    /// only what answers it.
    pub fn welcome_shown(&self) -> bool {
        !self.started && !self.transcript.lines().iter().any(|l| matches!(l.kind, LineKind::User | LineKind::Assistant | LineKind::CardTitle))
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
        self.monitor.show_thinking = self.show_thinking;
        let id = monitor::update_id(&update).to_owned();
        if let Some(view) = self.monitor.find(&id) {
            view.show_thinking = self.show_thinking;
        }
        if let po_agent::SubagentUpdate::Started { agent, .. } = &update
            && !self.delegations.contains_key(&id)
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
            // A half-typed command: Enter picks the highlighted item, without sending. A command
            // typed in full goes as it is: `/mode` must not become `/modeles`.
            (KeyCode::Enter, None)
                if query.starts_with('/') && !query.contains(char::is_whitespace) && !self.commands.iter().any(|(name, _)| *name == query)
                    && !self.command_names.contains(&query) =>
            {
                0
            }
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

    /// Adds a streamed fragment. Fragments arrive cut anywhere - sometimes in the middle of a
    /// word - so a line is only published once it is finished, and the last partial line is
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
        control::mirror().agent(&event);
        match event {
            AgentEvent::ModelSelected { model, class, reason } => {
                self.status.model = model.clone();
                // The engine shown follows the model, even when it switches mid-turn (a mixed
                // local/cloud setup picks per step); then the phone is told at once.
                if let Some(engine) = self.model_engines.get(&model) {
                    self.remote_engine = engine.clone();
                }
                publish_status(self);
                if reason.is_noteworthy() {
                    let announcement = format!("{model} · {} - {}", class.label(), reason.explain());
                    if self.last_route.as_deref() != Some(announcement.as_str()) {
                        self.toast(announcement.clone());
                        self.last_route = Some(announcement);
                    }
                }
            }
            // The reasoning is folded under its header: Ctrl+O expands it.
            AgentEvent::Thinking(text) => self.finish_thinking(Some(&text)),
            AgentEvent::ThinkingText(fragment) => {
                if self.show_thinking {
                    self.append_thinking(&fragment);
                }
            },
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
                self.transcript.push(Line::new(LineKind::ToolRunning, format!("{name} {}", po_agent::first_line(&summary))));
                self.running_tools.push((name, position, std::time::Instant::now()));
            }
            AgentEvent::ToolFinished { name, summary, is_error } => {
                if !self.labs_front {
                    self.labs_seen.1 += 1;
                }
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
                // A Po Labs request shows in its card of the right column, read again right away;
                // the conversation keeps a plain tool row.
                if name == "po_labs" {
                    self.labs_book_at = None;
                    self.labs_notebook();
                }
                let line = Line::new(kind, format!("{name} {}", po_agent::first_line(&summary))).with_secs(elapsed);
                let replaced = position.is_some_and(|p| self.transcript.replace_at(p, line.clone()));
                self.last_tool = if replaced {
                    position
                } else {
                    self.transcript.push(line);
                    Some(self.transcript.next_position() - 1)
                };
            }
            // The change a tool made goes with its row: shown as +/- counts folded, as lines expanded.
            // The terminal shows a call by its line; its output goes to the phone, opened.
            AgentEvent::ToolOutput { .. } => {}
            AgentEvent::Image { path } => self.transcript.push(Line::new(LineKind::Image, path)),
            AgentEvent::ToolDiff { diff } => {
                if let Some(position) = self.last_tool
                    && let Some(line) = self.transcript.get(position).cloned()
                {
                    self.transcript.replace_at(position, line.with_detail(diff));
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
            AgentEvent::Context { used, window, breakdown } => {
                self.status.context = Some((used, window));
                self.context_breakdown = Some(breakdown);
                publish_status(self);
            }
            AgentEvent::Goals(goals) => {
                self.goals = goals;
                publish_status(self);
            }
            AgentEvent::ContextWarning { message, .. } => {
                self.context_warning = Some(message);
                publish_status(self);
            }
            AgentEvent::ContextWarningCleared => {
                self.context_warning = None;
                publish_status(self);
            }
            AgentEvent::LoopWarning { message } => {
                self.loop_warning = Some(message);
                publish_status(self);
            }
            AgentEvent::LoopWarningCleared => {
                self.loop_warning = None;
                publish_status(self);
            }
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
        // multiplexer intercepts them before the application - the shortcut would never get
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
            CardLine::Item(name, detail) => format!("{name} - {detail}"),
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

    /// Every name a command answers to, in every language (`/compact` as well as `/compacter`):
    /// typed in full, Enter sends it instead of completing it into another.
    fn command_names(&self) -> Vec<String> {
        Vec::new()
    }

    /// What went wrong while starting (a configuration that does not read, an MCP server that
    /// did not start): shown in the conversation, the line printed before being covered.
    fn startup_warnings(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// What is worth knowing at startup without being wrong (a new version of Po installed):
    /// shown in the conversation as a note.
    fn startup_notes(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// A command to open as soon as the screen is up (the setup of the local models, the first
    /// time or when no model answers).
    fn startup_command(&mut self) -> Option<String> {
        None
    }

    /// Before a message goes to the model: what the session needs first (its default model
    /// started). `None`: nothing to do. `quit` set on the output: the message cannot go now.
    async fn before_turn(&mut self, _agent: &mut Agent) -> Option<CommandOutput> {
        None
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

    /// The Po Labs session: an agent of its own, working in Po's source, built when first
    /// needed (resuming the one left paused, if any). `None` when Po's source is not here.
    fn labs_agent(&mut self, _main: &Agent) -> Option<Agent> {
        None
    }

    /// What it takes to write contribution `kept` (or rewrite its text as `remark` asks).
    fn contribution_job(&mut self, _kept: &labs::Kept, _remark: Option<&str>) -> Option<labs::ContributionJob> {
        None
    }

    /// Puts Po on the version of contribution `kept`, to change its code; true if Po restarts.
    fn contribution_edit(&mut self, _kept: &labs::Kept) -> LabsWork<bool> {
        labs_unavailable("tui.labs.no_source")
    }

    /// Ends the change of a contribution's code, kept or undone, back on the latest version.
    fn contribution_edit_done(&mut self, _trial: &labs::Trial, _keep: bool) -> LabsWork<()> {
        labs_unavailable("tui.labs.no_source")
    }

    /// Shares contribution `kept` with Po's community; its address once sent. `progress` says
    /// how it goes (and how to connect to GitHub, the first time).
    fn contribution_share(&mut self, _kept: &labs::Kept, _progress: LabsProgress) -> LabsWork<String> {
        labs_unavailable("tui.labs.share_unavailable")
    }

    /// Where contributions are sent, to say so before sending.
    fn share_target(&self) -> String {
        String::new()
    }

    /// Undoes the change on trial: Po Labs' workspace and the installed binary go back to before
    /// it, and Po restarts.
    fn labs_undo(&mut self, _trial: &labs::Trial) -> LabsWork<()> {
        labs_unavailable("tui.labs.no_source")
    }

    /// The choices of command `line` when it needs one and has none: a menu to choose from.
    async fn command_menu(&mut self, _line: &str, _agent: &mut Agent) -> Option<menu::Menu> {
        None
    }

    /// The menus that can open during a turn, by the command line that opens them: those that
    /// only read the settings.
    async fn turn_menus(&mut self, _agent: &mut Agent) -> Vec<(String, menu::Menu)> {
        Vec::new()
    }

    /// A command that needs neither the agent nor the end of the turn (a display setting, the
    /// language, the work mode through its shared handle), carried out at once even during a
    /// turn; `None` when it has to wait for the turn's next safe point.
    async fn command_now(&mut self, _line: &str, _mode: Option<&po_agent::work_mode::WorkModeHandle>) -> Option<CommandOutput> {
        None
    }

    /// The community catalog, for its browser, in the order shown.
    async fn catalog_list(&mut self) -> Result<Vec<catalog::Entry>, String> {
        Err(po_i18n::t!("tui.catalog.unavailable").to_owned())
    }

    /// The files of extension `name`, to read before installing it: (path, text).
    async fn catalog_files(&mut self, _name: &str) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }

    /// Installs, removes or votes for extension `name`.
    async fn catalog_act(&mut self, _act: catalog::Act, _name: &str, _agent: &mut Agent) -> Result<catalog::Done, String> {
        Err(po_i18n::t!("tui.catalog.unavailable").to_owned())
    }

    /// Inventory of local models for the picker, reread at each opening: free VRAM changes
    /// during the session, and a plan computed ten minutes ago lies.
    fn local_models(&mut self) -> Vec<state::LocalModel>;

    /// State of the local server, in one line.
    fn server_state(&mut self) -> String;

    /// Loads the model at the given index and adds it to the session. Returns a report to
    /// display - success or failure alike, because a startup that fails silently is worse than
    /// one that fails loudly.
    async fn activate_local(&mut self, index: usize, agent: &mut Agent) -> String;

    /// Stops the server launched by Po and gives the VRAM back.
    async fn stop_local(&mut self, agent: &mut Agent) -> String;

    /// Polls if background model loading/detection has finished and registers it. Returns model name if just became ready.
    async fn poll_ready(&mut self, _agent: &mut Agent) -> Option<String> {
        None
    }

    /// Cancels any ongoing background model loading.
    fn cancel_model(&mut self) {}
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

/// Shift+Tab: next mode. Switching to bypass also grants the pending request - it is the
/// natural gesture when an authorization has just appeared.
fn cycle_work_mode(app: &mut App) {
    let Some(handle) = app.work_mode.as_ref() else { return };
    let mode = handle.cycle();
    if mode.gate() == po_agent::work_mode::Gate::Allow
        && let Some(request) = app.pending_approval.take()
    {
        request.answer(true);
    }
    app.toast(match mode {
        po_agent::work_mode::WorkMode::Auto => po_i18n::t!("tui.toast.mode_auto"),
        po_agent::work_mode::WorkMode::AutoBypass => po_i18n::t!("tui.toast.mode_auto_bypass"),
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
    if !running && app.labs_rewriting.is_some() && app.input.is_empty() {
        app.labs_rewriting = None;
        app.toast(po_i18n::t!("tui.labs.edit_cancelled"));
        return false;
    }
    if !running && app.labs_review.as_mut().is_some_and(labs::Review::back) {
        return false;
    }
    if !running && app.labs_review.is_some() {
        labs_close_review(app);
        return false;
    }
    if !running && app.labs_notebook_open {
        app.labs_notebook_open = false;
        return false;
    }
    if !running && app.labs_revising && app.input.is_empty() {
        app.labs_revising = false;
        app.toast(po_i18n::t!("tui.labs.revise_cancelled"));
        return false;
    }
    if !running && background == 0 {
        return false;
    }
    if app.stop_armed.is_some_and(|at| at.elapsed() < STOP_CONFIRM) {
        app.stop_armed = None;
        // "Esc again to stop" is done: left up, it still asked for it after the stop.
        app.toast = None;
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

/// What one session shows: kept aside while the other one is in front.
#[derive(Default)]
struct ViewStash {
    transcript: Transcript,
    toggled: std::collections::HashSet<usize>,
    anchor: std::cell::Cell<Option<(usize, u16)>>,
    title: Option<String>,
    monitor: monitor::Monitor,
    queued: std::collections::VecDeque<String>,
    delegations: std::collections::HashMap<String, usize>,
    unlinked: Vec<(String, String)>,
}

fn swap_view(app: &mut App, stash: &mut ViewStash) {
    std::mem::swap(&mut app.transcript, &mut stash.transcript);
    std::mem::swap(&mut app.toggled, &mut stash.toggled);
    std::mem::swap(&mut app.anchor, &mut stash.anchor);
    std::mem::swap(&mut app.title, &mut stash.title);
    std::mem::swap(&mut app.monitor, &mut stash.monitor);
    std::mem::swap(&mut app.queued, &mut stash.queued);
    std::mem::swap(&mut app.delegations, &mut stash.delegations);
    std::mem::swap(&mut app.unlinked, &mut stash.unlinked);
    app.monitor_open = false;
    app.last_tool = None;
}

impl App {
    /// The notebook, reread at most every second (the model writes it during the session).
    pub fn labs_notebook(&mut self) -> &labs::Notebook {
        if self.labs_book_at.is_none_or(|at| at.elapsed() > Duration::from_secs(1)) {
            self.labs_book = labs::Notebook::load();
            self.labs_book_at = Some(std::time::Instant::now());
        }
        &self.labs_book
    }

    /// The Po Labs card has something to show: requests waiting, or a session opened.
    pub fn labs_card_shown(&self) -> bool {
        self.labs
            && (!self.labs_book.pending().is_empty()
                || self.labs_book.trial.is_some()
                || self.labs_elsewhere.is_some()
                || self.labs_drafting.is_some()
                || self.labs_busy.is_some()
                || !self.labs_book.kept.iter().all(|k| k.share != labs::Share::Ready)
                || self.other_agent.is_some()
                || self.labs_front
                || self.labs_activity.is_some())
    }

    /// The change on trial could be seen at work by now: time to ask the user about it. A text
    /// shows at once; how tool calls look needs a few calls; the rest, a turn.
    pub fn labs_trial_ripe(&self) -> bool {
        let Some(trial) = self.labs_book.trial.as_ref().filter(|t| self.labs_book.awaiting_verdict() && !t.untouched()) else {
            return false;
        };
        let (turns, tools) = self.labs_seen;
        if self.labs_verdict_later.is_some_and(|at| turns < at + 2) {
            return false;
        }
        let since = self.labs_since.elapsed();
        since >= Duration::from_secs(600)
            || match trial.visible {
                labs::Visible::AtOnce => since >= Duration::from_secs(15),
                labs::Visible::NextTurn => turns >= 1,
                labs::Visible::ToolCalls => tools >= 2 || turns >= 3,
            }
    }

    /// The discreet proposal above the input, when Po is idle: the verdict on the change on trial
    /// once it could be seen, else what Po Labs could do now - unless the user set this state of
    /// the notebook aside.
    pub fn labs_proposal(&self) -> Option<LabsAsk> {
        if !self.labs
            || self.labs_front
            || self.labs_activity.is_some()
            || self.status.busy
            || self.labs_revising
            || self.labs_elsewhere.is_some()
        {
            return None;
        }
        let book = &self.labs_book;
        if book.awaiting_verdict() {
            return self.labs_trial_ripe().then_some(LabsAsk::Verdict);
        }
        if self.labs_dismissed == book.revision || self.labs_drafting.is_some() || self.labs_busy.is_some() || self.labs_review.is_some() || self.labs_rewriting.is_some() {
            return None;
        }
        if book.trial.is_some() {
            return Some(LabsAsk::Adjust);
        }
        if book.undrafted().is_some() {
            return Some(LabsAsk::Draft);
        }
        let pending = book.pending().len();
        if pending > 0 {
            return Some(LabsAsk::Requests(pending));
        }
        book.review_ready().then(|| LabsAsk::Share(book.to_review().len()))
    }
}

/// What the proposal above the input asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabsAsk {
    /// Requests wait: implement them now?
    Requests(usize),
    /// The change on trial has remarks to take up.
    Adjust,
    /// The change on trial could be seen: what does the user think of it?
    Verdict,
    /// A kept change waits for its contribution to be written.
    Draft,
    /// Everything is done: contributions are ready to share.
    Share(usize),
}

/// Work of Po Labs that takes a while (a build, a sending), done in the background: Po stays
/// usable meanwhile.
pub type LabsWork<T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'static>>;

/// Says how a work of Po Labs in the background goes.
pub type LabsProgress = Box<dyn Fn(String) + Send + Sync>;

fn labs_unavailable<T>(key: &'static str) -> LabsWork<T> {
    Box::pin(async move { Err(po_i18n::text(key).to_owned()) })
}

/// What a work of Po Labs in the background was, and how it ended.
pub enum LabsWorkDone {
    Progress(String),
    Share(labs::Kept, Result<String, String>),
    Edit(Result<bool, String>),
    EditDone(labs::Trial, bool, Result<(), String>),
    Undo(Result<(), String>),
}

/// Starts `work` in the background, `busy` saying what it is in Po Labs' card meanwhile.
fn labs_spawn<T: Send + 'static>(app: &mut App, busy: String, work: LabsWork<T>, done: impl FnOnce(Result<T, String>) -> LabsWorkDone + Send + 'static) {
    let Some(tx) = app.labs_works.clone() else { return };
    app.labs_busy = Some(busy);
    tokio::spawn(async move {
        let _ = tx.send(done(work.await));
    });
}

/// A contribution written (or rewritten) in the background.
pub struct LabsJobDone {
    id: u32,
    rewrite: bool,
    files: Vec<String>,
    core_by_rule: bool,
    result: Result<String, String>,
}

/// Has Po Labs write a contribution in the background, with the session's model: the kept change
/// next in line, or `remark` applied to the text of contribution `id`.
fn labs_write(app: &mut App, agent: &Agent, host: &mut impl Host, id: u32, remark: Option<&str>) {
    let Some(kept) = app.labs_book.kept.iter().find(|k| k.id == id).cloned() else { return };
    let Some(job) = host.contribution_job(&kept, remark) else {
        app.note(po_i18n::t!("tui.labs.no_source"));
        return;
    };
    let (Some(tx), Some(call)) = (app.labs_jobs.clone(), agent.once_job(&job.system, &job.prompt)) else {
        app.error(po_i18n::t!("tui.labs.draft_failed", err = "no model"));
        return;
    };
    let rewrite = remark.is_some();
    let (files, core_by_rule) = (job.files, job.core_by_rule);
    tokio::spawn(async move {
        let result = call.await;
        let _ = tx.send(LabsJobDone { id, rewrite, files, core_by_rule, result });
    });
    if rewrite {
        if let Some(review) = app.labs_review.as_mut() {
            review.rewriting = Some(id);
        }
    } else {
        app.labs_drafting = Some(id);
        app.toast(po_i18n::t!("tui.labs.drafting", n = id));
    }
}

/// Writes the contribution of the next kept change, if any; `chain`: then goes on with the rest.
fn labs_start_draft(app: &mut App, agent: &Agent, host: &mut impl Host, chain: bool) -> bool {
    if app.labs_drafting.is_some() {
        return true;
    }
    let Some(id) = app.labs_book.undrafted().map(|k| k.id) else { return false };
    app.labs_draft_chain = chain;
    labs_write(app, agent, host, id, None);
    app.labs_drafting.is_some()
}

/// A long work of Po Labs ended in the background.
fn labs_work_done(app: &mut App, done: LabsWorkDone) {
    if let LabsWorkDone::Progress(text) = done {
        app.labs_busy = Some(text);
        return;
    }
    app.labs_busy = None;
    match done {
        LabsWorkDone::Progress(_) => {}
        LabsWorkDone::Share(kept, Ok(url)) => {
            let _ = labs::Notebook::modify(|b| b.set_share(kept.id, labs::Share::Shared(url)));
            app.note(if kept.core { po_i18n::t!("tui.labs.shared_core") } else { po_i18n::t!("tui.labs.shared_extension") });
            if let Some(review) = app.labs_review.as_mut() {
                review.settle(kept.id, labs::ContribAction::Share);
            }
        }
        LabsWorkDone::Edit(Ok(restart)) => {
            app.note(if restart { po_i18n::t!("tui.labs.switched_restart") } else { po_i18n::t!("tui.labs.switched") });
        }
        LabsWorkDone::EditDone(trial, keep, Ok(())) => {
            labs_session_done(app);
            app.note(if keep && !trial.untouched() { po_i18n::t!("tui.labs.contribution_updated") } else { po_i18n::t!("tui.labs.contribution_unchanged") });
        }
        LabsWorkDone::Undo(Ok(())) => {
            let _ = labs::Notebook::modify(|b| b.refuse_trial());
            labs_session_done(app);
            app.note(po_i18n::t!("tui.labs.undone"));
        }
        LabsWorkDone::Share(_, Err(err)) | LabsWorkDone::Edit(Err(err)) => {
            app.toast(err.clone());
            app.error(err);
        }
        LabsWorkDone::EditDone(_, _, Err(err)) => app.error(po_i18n::t!("tui.labs.contribution_failed", err = err)),
        LabsWorkDone::Undo(Err(err)) => app.error(po_i18n::t!("tui.labs.undo_failed", err = err)),
    }
    app.labs_book = labs::Notebook::load();
    app.labs_book_at = Some(std::time::Instant::now());
}

/// A contribution came back written: saved, shown, and Po Labs goes on if asked to.
fn labs_job_done(app: &mut App, done: LabsJobDone) {
    if done.rewrite {
        if let Some(review) = app.labs_review.as_mut() {
            review.rewriting = None;
        }
    } else {
        app.labs_drafting = None;
    }
    let parsed = done.result.as_ref().ok().and_then(|text| labs::parse_draft(text));
    match parsed {
        Some((draft, core)) => {
            let title = draft.title.clone();
            if let Ok((book, _)) = labs::Notebook::modify(|b| b.set_draft(done.id, draft, core || done.core_by_rule, done.files)) {
                app.labs_book = book;
                app.labs_book_at = Some(std::time::Instant::now());
            }
            if done.rewrite {
                app.toast(po_i18n::t!("tui.labs.rewritten"));
            } else {
                app.toast(po_i18n::t!("tui.labs.drafted", title = title));
                if std::mem::take(&mut app.labs_draft_chain) {
                    app.labs_launch = true;
                }
            }
        }
        None => {
            app.labs_draft_chain = false;
            let err = match done.result {
                Err(err) => err,
                Ok(_) => po_i18n::t!("tui.labs.draft_unreadable").to_owned(),
            };
            app.error(po_i18n::t!("tui.labs.draft_failed", err = err));
        }
    }
}

/// Opens the review of contributions `ids`, in the big card.
fn labs_open_review(app: &mut App, ids: Vec<u32>) {
    if ids.is_empty() {
        app.note(po_i18n::t!("tui.labs.none_aside"));
        return;
    }
    app.labs_notebook_open = false;
    app.labs_review = Some(labs::Review::new(ids));
}

/// Closing the review closes Po Labs: its session kept aside is let go.
fn labs_close_review(app: &mut App) {
    app.labs_review = None;
    app.labs_rewriting = None;
    labs_session_done(app);
}

/// Puts an agent's server cache aside on disk (single-slot server), before another takes it.
async fn park_agent(agent: &Agent) {
    if agent.session.estimated_prompt_tokens() < 4000 {
        return;
    }
    if let (Some(parking), Some(key)) = (agent.parking(), &agent.session_key) {
        parking.park(key).await;
    }
}

async fn unpark_agent(agent: &Agent) {
    if let (Some(parking), Some(key)) = (agent.parking(), &agent.session_key) {
        parking.unpark(key).await;
    }
}

/// Brings the Po Labs session in front: the user's session and its view wait behind, their
/// server cache parked on disk.
async fn labs_swap_in(
    app: &mut App,
    agent: &mut Agent,
    host: &mut impl Host,
    monitor: &mpsc::UnboundedSender<po_agent::SubagentUpdate>,
) {
    if app.labs_front {
        return;
    }
    let labs = match app.other_agent.take() {
        Some(labs) => Some(labs),
        None => host.labs_agent(agent),
    };
    let Some(mut labs) = labs else {
        app.note(po_i18n::t!("tui.labs.no_source"));
        return;
    };
    labs.set_subagent_monitor(monitor.clone());
    park_agent(agent).await;
    std::mem::swap(agent, &mut labs);
    app.other_agent = Some(labs);
    let mut stash = ViewStash::default();
    swap_view(app, &mut stash);
    app.behind = Some(stash);
    app.title = agent.title.clone();
    app.replay(&agent.session.full_history(), agent.workspace_root());
    unpark_agent(agent).await;
    app.labs_front = true;
    app.labs_notebook_open = false;
    if agent.is_paused() {
        app.pending_resume = true;
    } else if !app.labs_book.awaiting_verdict() && (agent.session.messages.is_empty() || app.labs_book.has_work()) {
        // The notebook is in its instructions: one message starts the work.
        app.pending_submit = labs_prompt(&app.labs_book, agent);
    }
}

/// What sets Po Labs to work: the change on trial to adjust, else the next request; `None` when it
/// resumes where it was paused.
fn labs_prompt(book: &labs::Notebook, labs: &Agent) -> Option<String> {
    if labs.is_paused() {
        return None;
    }
    if let Some(trial) = book.trial.as_ref().filter(|t| !t.revisions.is_empty()) {
        let remarks: Vec<String> = trial.revisions.iter().map(|r| format!("- {r}")).collect();
        return Some(format!(
            "The user tried the change on trial ({}) and wants it adjusted:\n{}\nAdjust it, then call po_apply again.",
            trial.summary,
            remarks.join("\n")
        ));
    }
    Some(if labs.session.messages.is_empty() {
        "Implement the most important pending request of the notebook.".to_owned()
    } else {
        format!("Requests wait in the notebook:\n{}\nTake the most important one.", book.pending_text())
    })
}

/// The verdict on the change on trial that needs no build: kept (its requests done, the next one
/// may start), or to adjust (the next message says how). Undoing is done by the host.
fn labs_verdict(app: &mut App, verdict: labs::Verdict) {
    let book = labs::Notebook::load();
    if !book.awaiting_verdict() {
        return;
    }
    match verdict {
        labs::Verdict::Keep => {
            if let Ok((book, Some(kept))) = labs::Notebook::modify(|b| b.keep_trial()) {
                app.labs_book = book;
                app.labs_book_at = Some(std::time::Instant::now());
                app.toast(po_i18n::t!("tui.labs.kept", summary = kept.summary));
                labs_session_done(app);
            }
            return;
        }
        labs::Verdict::Revise => {
            app.labs_notebook_open = false;
            app.labs_revising = true;
            app.toast(po_i18n::t!("tui.labs.revise_hint"));
        }
        labs::Verdict::Undo => {}
    }
    app.labs_book = book;
    app.labs_book_at = Some(std::time::Instant::now());
}

/// Which session holds Po Labs, every two seconds: this one renews its lease while Po Labs is
/// open here (or its change on trial waits here), lets it go otherwise, and takes it back after a
/// restart; else, the session holding it shows. A new build installed by Po Labs from another
/// session restarts this one on it, once idle.
fn labs_presence(app: &mut App, running: bool) {
    if !app.labs || app.labs_presence_at.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
        return;
    }
    app.labs_presence_at = Some(std::time::Instant::now());
    let mark = labs::installed_mark();
    if mark != app.labs_mark {
        app.labs_mark = mark;
        if !app.labs_held && !app.restart {
            app.note(po_i18n::t!("tui.labs.updated_elsewhere"));
            app.restart = true;
        }
    }
    let needed = running
        || app.labs_front
        || app.other_agent.is_some()
        || app.labs_drafting.is_some()
        || app.labs_busy.is_some()
        || app.labs_review.is_some()
        || (app.labs_held && app.labs_book.trial.is_some());
    if app.labs_held && !needed {
        labs::release(&app.labs_key);
        app.labs_held = false;
    }
    let working = running || (app.labs_front && app.status.busy);
    let activity = app.labs_activity.clone().unwrap_or_default();
    let mine = labs::lease().is_some_and(|l| l.session == app.labs_key);
    if app.labs_held || mine {
        match labs::claim(&app.labs_key, &labs_project(app), working, &activity, false) {
            Ok(lease) if lease.session == app.labs_key => {
                app.labs_held = true;
                app.labs_elsewhere = None;
            }
            Ok(lease) => {
                // Taken from another session while Po Labs rested here.
                app.labs_held = false;
                app.labs_elsewhere = Some(lease);
                app.labs_lost = true;
                app.labs_notebook_open = false;
                if app.labs_front {
                    app.labs_back = true;
                }
                app.note(po_i18n::t!("tui.labs.taken", project = app.labs_elsewhere.as_ref().map(|l| l.project.clone()).unwrap_or_default()));
            }
            Err(_) => {}
        }
    } else {
        app.labs_elsewhere = labs::lease();
    }
    if app.labs_lost && !app.labs_front {
        app.labs_lost = false;
        app.other_agent = None;
    }
}

/// The folder name of the user's project, to say where Po Labs is.
fn labs_project(app: &App) -> String {
    app.workspace
        .as_ref()
        .and_then(|w| w.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Takes Po Labs for this session before opening it; `force` takes it from a session where it
/// rests. False, with a word, when another session keeps it.
fn labs_claim(app: &mut App, force: bool) -> bool {
    match labs::claim(&app.labs_key, &labs_project(app), false, "", force) {
        Ok(lease) if lease.session == app.labs_key => {
            app.labs_held = true;
            app.labs_elsewhere = None;
            app.labs_presence_at = Some(std::time::Instant::now());
            true
        }
        Ok(lease) => {
            app.toast(po_i18n::t!("tui.labs.elsewhere_working", project = lease.project));
            app.labs_elsewhere = Some(lease);
            false
        }
        // No shared folder to coordinate through: this session goes on alone.
        Err(_) => true,
    }
}

/// The change is settled: the Po Labs session kept aside is let go, the next request starts a new
/// one.
fn labs_session_done(app: &mut App) {
    if !app.labs_front && app.labs_activity.is_none() {
        app.other_agent = None;
    }
    app.labs_verdict_later = None;
}

/// Back to the user's session; the Po Labs session waits aside, its cache parked.
async fn labs_swap_out(app: &mut App, agent: &mut Agent) {
    if !app.labs_front {
        return;
    }
    let Some(mut main) = app.other_agent.take() else { return };
    park_agent(agent).await;
    std::mem::swap(agent, &mut main);
    app.other_agent = Some(main);
    if let Some(mut stash) = app.behind.take() {
        swap_view(app, &mut stash);
    }
    unpark_agent(agent).await;
    app.labs_front = false;
}

/// Id of the Po Labs card among the subagents' when it works alongside the user's session.
pub const LABS_VIEW: &str = "po-labs";

/// Po Labs at work in a task of its own, alongside the user's session: what it emits, and its
/// agent handed back at the end.
pub struct LabsRun {
    events: mpsc::UnboundedReceiver<AgentEvent>,
    done: tokio::sync::oneshot::Receiver<(Agent, Result<(), po_agent::AgentError>)>,
    pause: Arc<AtomicBool>,
    /// Text arrived in fragments: the full text that follows is already shown.
    streamed: bool,
}

enum LabsSignal {
    Event(AgentEvent),
    Done(Option<Box<(Agent, Result<(), po_agent::AgentError>)>>),
}

/// Next thing Po Labs has to say; never ready when it is not working alongside.
async fn labs_next(run: &mut Option<LabsRun>) -> LabsSignal {
    let Some(run) = run.as_mut() else {
        return std::future::pending().await;
    };
    tokio::select! {
        biased;
        Some(event) = run.events.recv() => LabsSignal::Event(event),
        done = &mut run.done => LabsSignal::Done(done.ok().map(Box::new)),
    }
}

/// Po Labs has something to do: requests waiting, or a session left paused.
fn labs_has_work(app: &App) -> bool {
    app.other_agent.as_ref().is_some_and(Agent::is_paused) || app.labs_book.has_work()
}

/// Opens the big card of Po Labs working alongside.
fn open_labs_card(app: &mut App) {
    if let Some(index) = app.monitor.views.iter().position(|v| v.id == LABS_VIEW) {
        app.monitor_open = true;
        app.monitor_selected = index;
    }
}

/// Starts Po Labs alongside the user's session, on a slot of its own: the user's session stays in
/// front and usable, Po Labs shows in a card. The slots they share go to whoever asks first.
fn labs_start_parallel(
    app: &mut App,
    agent: &mut Agent,
    host: &mut impl Host,
    ledger: &Arc<po_agent::SlotLedger>,
    run: &mut Option<LabsRun>,
) {
    let labs = match app.other_agent.take() {
        Some(labs) => Some(labs),
        None => host.labs_agent(agent),
    };
    let Some(mut labs) = labs else {
        app.note(po_i18n::t!("tui.labs.no_source"));
        return;
    };
    agent.share_slots(ledger.clone());
    labs.share_slots(ledger.clone());
    // Its subagents work inside its own card: their updates go nowhere.
    labs.set_subagent_monitor(mpsc::unbounded_channel().0);
    let prompt = labs_prompt(&app.labs_book, &labs);
    let task = match &app.labs_book.trial {
        Some(trial) => trial.summary.clone(),
        None => app.labs_book.pending().first().map(|r| r.summary.clone()).unwrap_or_default(),
    };
    app.monitor.apply(po_agent::SubagentUpdate::Started {
        id: LABS_VIEW.into(),
        agent: "Po Labs".into(),
        model: labs.session_model().map(|m| m.display_name).unwrap_or_default(),
        task,
        background: true,
    });
    let (tx, events) = mpsc::unbounded_channel();
    let (done_tx, done) = tokio::sync::oneshot::channel();
    let pause = labs.pause_handle();
    tokio::spawn(async move {
        let mut sink = |event| {
            let _ = tx.send(event);
        };
        let result = match prompt {
            Some(prompt) => labs.turn(&prompt, &mut sink).await,
            None => labs.resume(&mut sink).await,
        };
        let _ = done_tx.send((labs, result));
    });
    *run = Some(LabsRun { events, done, pause, streamed: false });
    app.labs_activity = Some(String::new());
    app.toast(po_i18n::t!("tui.labs.started_parallel"));
}

/// What Po Labs working alongside said: its card follows, and at the end it is put aside again.
fn labs_signal(app: &mut App, run: &mut Option<LabsRun>, signal: LabsSignal) {
    use po_agent::SubagentUpdate as U;
    let id = || LABS_VIEW.to_owned();
    match signal {
        LabsSignal::Event(event) => {
            let streamed = run.as_ref().is_some_and(|r| r.streamed);
            let mut now_streamed = streamed;
            let update = match event {
                AgentEvent::TextDelta(text) => {
                    now_streamed = true;
                    Some(U::Text { id: id(), text })
                }
                AgentEvent::Text(text) => {
                    now_streamed = false;
                    (!streamed).then(|| U::Text { id: id(), text: format!("{text}\n") })
                }
                AgentEvent::ThinkingText(text) => Some(U::Thinking { id: id(), text }),
                AgentEvent::ToolStarted { name, summary } => {
                    app.labs_activity = Some(format!("{name} {summary}"));
                    Some(U::Tool { id: id(), name, summary })
                }
                AgentEvent::ToolFinished { name, is_error, .. } => Some(U::ToolDone { id: id(), name, ok: !is_error }),
                AgentEvent::ToolDenied { name, .. } => Some(U::ToolDone { id: id(), name, ok: false }),
                AgentEvent::Notice(text) => Some(U::Text { id: id(), text: format!("{text}\n") }),
                _ => None,
            };
            if let Some(run) = run.as_mut() {
                run.streamed = now_streamed;
            }
            if let Some(update) = update {
                app.monitor.apply(update);
            }
            if let Some(activity) = app.labs_activity.clone().filter(|a| !a.is_empty()) {
                app.monitor.set_progress(LABS_VIEW, &activity);
            }
        }
        LabsSignal::Done(done) => {
            // What was still in the channel comes first.
            if let Some(mut current) = run.take() {
                let mut left = Vec::new();
                while let Ok(event) = current.events.try_recv() {
                    left.push(event);
                }
                let mut current = Some(current);
                for event in left {
                    labs_signal(app, &mut current, LabsSignal::Event(event));
                }
            }
            app.labs_activity = None;
            let Some(done) = done else {
                app.monitor.apply(U::Finished { id: id(), ok: false });
                return;
            };
            let (back, result) = *done;
            if back.is_paused() {
                app.monitor.apply(U::Paused { id: id() });
                app.toast(po_i18n::t!("tui.labs.paused_parallel", key = labs::shortcut("labs").label()));
            } else {
                app.monitor.apply(U::Finished { id: id(), ok: result.is_ok() });
                match &result {
                    Ok(()) => app.toast(po_i18n::t!("tui.labs.done_parallel")),
                    Err(err) => app.error(err.to_string()),
                }
            }
            app.other_agent = Some(back);
            app.labs_save = true;
        }
    }
}

/// Saves the Po Labs session that came back from working alongside.
fn labs_save_done(app: &mut App, host: &mut impl Host) {
    if std::mem::take(&mut app.labs_save)
        && let Some(labs) = &app.other_agent
    {
        host.after_turn(labs);
    }
}

/// During a turn, with an empty input: ↑ takes the last waiting message back into the input to
/// edit it - it no longer waits, so the agent cannot take it until it is sent again - and Esc
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
        control::mirror().push(po_wire::Event::User { text: text.clone() });
        app.transcript.push_text(LineKind::User, &text);
        app.transcript.scroll_to_bottom();
    }
    app.queued = rest;
}

/// Subagents still running, according to their frame (the agent is busy with the turn).
fn agent_background_hint(app: &App) -> usize {
    app.monitor.views.iter().filter(|v| v.running() && v.id != LABS_VIEW).count()
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
    /// The Po Labs card of the right column: opens the notebook, or the Po Labs session.
    LabsCard,
    /// The proposal above the input: implement now, or later.
    LabsYes,
    LabsLater,
    /// A verdict button of the change on trial, in the notebook.
    LabsVerdict(labs::Verdict),
    /// A button of the contribution on review, and its page arrows.
    LabsContrib(labs::ContribAction),
    LabsReviewPrev,
    LabsReviewNext,
    /// Back from the Po Labs session to the user's session.
    LabsBack,
}

/// A link clicked: opened in the browser, its address shown (a terminal reached from another
/// machine opens it on this one: the address can still be read and copied).
fn open_link(app: &mut App, url: &str) {
    if let Some(path) = url.strip_prefix(thumbnail::LINK) {
        match thumbnail::open(path) {
            Ok(()) => app.toast(po_i18n::t!("tui.image.opened", path = path)),
            Err(err) => app.toast(po_i18n::t!("tui.link.failed", url = path, error = err)),
        }
        return;
    }
    match links::open(url) {
        Ok(()) => app.toast(po_i18n::t!("tui.link.opened", url = url)),
        Err(err) => app.toast(po_i18n::t!("tui.link.failed", url = url, error = err)),
    }
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
                app.pending_plain = true;
                return true;
            }
            Hit::LabsCard => {
                if app.labs_front {
                    app.labs_back = true;
                } else if app.labs_elsewhere.is_some() {
                    // Held by another session: say where, or take it on a second request.
                    app.labs_launch = true;
                } else if app.labs_activity.is_none() && app.labs_book.awaiting_verdict() {
                    // The change on trial first: the card shows it with its verdict buttons.
                    app.labs_notebook_open = !app.labs_notebook_open;
                } else if app.other_agent.is_some() || app.labs_activity.is_some() {
                    app.labs_launch = true;
                } else {
                    app.labs_notebook_open = !app.labs_notebook_open;
                }
                return true;
            }
            Hit::LabsYes => {
                app.labs_launch = true;
                return true;
            }
            Hit::LabsLater => {
                if app.labs_proposal() == Some(LabsAsk::Verdict) {
                    // Asked again a little later: the next request waits for it anyway.
                    app.labs_verdict_later = Some(app.labs_seen.0);
                } else {
                    app.labs_dismissed = app.labs_book.revision;
                }
                app.labs_notebook_open = false;
                return true;
            }
            Hit::LabsVerdict(verdict) => {
                app.labs_verdict = Some(verdict);
                return true;
            }
            Hit::LabsContrib(action) => {
                app.labs_contrib = Some(action);
                return true;
            }
            Hit::LabsReviewPrev | Hit::LabsReviewNext => {
                if let Some(review) = app.labs_review.as_mut() {
                    review.turn(if hit == Hit::LabsReviewPrev { -1 } else { 1 });
                }
                return true;
            }
            Hit::LabsBack => {
                if app.labs_review.is_some() {
                    labs_close_review(app);
                } else if app.labs_notebook_open {
                    app.labs_notebook_open = false;
                } else {
                    app.labs_back = true;
                }
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
        ("/contributions", _) => {
            app.labs_review_request = true;
            true
        }
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

/// What a command of the interface (`/title`, `/mouse`, `/control`…) said, for whoever sent it
/// from outside: the card it wrote since `shown`, else its toast. It goes to the phone as the
/// command's report, and is the answer.
fn interface_report(app: &mut App, command: &str, shown: usize) -> String {
    use po_wire::OutLine as O;
    let mut lines: Vec<O> = app.transcript.lines()[shown.min(app.transcript.len())..]
        .iter()
        .filter_map(|line| match line.kind {
            LineKind::Card | LineKind::CardItem | LineKind::CardField => Some(O::Plain { text: line.text.clone() }),
            LineKind::CardWarn => Some(O::Warn { text: line.text.clone() }),
            LineKind::CardSection => Some(O::Section { text: line.text.clone() }),
            LineKind::CardDim => Some(O::Dim { text: line.text.clone() }),
            _ => None,
        })
        .collect();
    if lines.is_empty() {
        let text = app.toast.as_ref().map(|(t, _)| t.clone()).unwrap_or_else(|| po_i18n::t!("common.done").to_owned());
        lines.push(O::Plain { text });
    }
    let text = lines
        .iter()
        .map(|l| match l {
            O::Plain { text } | O::Warn { text } | O::Section { text } | O::Dim { text } => text.clone(),
            O::Item { name, detail } => format!("{name} {detail}"),
            O::Field { key, value } => format!("{key}: {value}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    control::mirror().push(po_wire::Event::CommandOutput { command: command.to_owned(), lines });
    text
}

/// Opens the menu of command `text` if it needs a choice it was not given; false otherwise.
async fn open_menu(app: &mut App, text: &str, agent: &mut Agent, host: &mut impl Host) -> bool {
    // The terminal has its panels: their menus are for a screen without them.
    if settings::opens_panel(text) {
        return false;
    }
    match keep_alive(host.command_menu(text, agent)).await {
        Some(mut menu) => {
            menu.source = text.to_owned();
            app.menu = Some(menu);
            app.menu_built = std::time::Instant::now();
            // A further menu opened: there is nothing to come back to after a command.
            app.pending_then = None;
            true
        }
        None => false,
    }
}

/// A live menu is built again every second while it is open, between turns.
async fn refresh_menu(app: &mut App, agent: &mut Agent, host: &mut impl Host) -> bool {
    let Some(source) = app.menu.as_ref().filter(|m| m.live && !m.source.is_empty()).map(|m| m.source.clone()) else { return false };
    if app.menu_built.elapsed() < Duration::from_secs(1) {
        return false;
    }
    app.menu_built = std::time::Instant::now();
    let Some(new) = keep_alive(host.command_menu(&source, agent)).await else { return false };
    // Closed or replaced while it was being built: nothing to refresh.
    match app.menu.as_mut() {
        Some(menu) if menu.source == source => {
            menu.refresh(new);
            true
        }
        _ => false,
    }
}

/// A key in a command's menu.
fn menu_key(app: &mut App, key: KeyEvent) {
    let Some(menu) = app.menu.as_mut() else { return };
    // A letter of its own chooses an item at once (before j/k/q mean moving or closing).
    if key.kind != KeyEventKind::Release
        && let KeyCode::Char(c) = key.code
        && !po_term::input::is_shortcut_ctrl(key.modifiers)
        && let Some(index) = menu.hotkey(c)
    {
        menu_choose(app, Some(index));
        return;
    }
    let chosen = match menu::map_key(key) {
        menu::Key::Move(delta) => {
            menu.move_by(delta);
            return;
        }
        // Esc and ← go back to the menu above, if there is one; Ctrl+C closes everything.
        menu::Key::Close => {
            let back = menu.back.clone().filter(|_| !(key.code == KeyCode::Char('c') && po_term::input::is_shortcut_ctrl(key.modifiers)));
            app.menu = None;
            // Closed here: a screen the session offered the phones is no longer waited on.
            if back.is_none() {
                control::mirror().offer(None);
            }
            app.pending_command = back;
            return;
        }
        menu::Key::None => return,
        menu::Key::Choose => None,
        menu::Key::Pick(index) => Some(index),
    };
    menu_choose(app, chosen);
}

/// The menu prepared for `text`, typed during a turn.
fn turn_menu(app: &App, text: &str) -> Option<menu::Menu> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    app.menus_cache.iter().find(|(l, _)| *l == line).map(|(_, m)| m.clone())
}

/// During a turn, what a menu's choice asks: a further menu, else a command noted for the
/// next safe point.
fn turn_menu_chosen(app: &mut App, pause: &AtomicBool, mouse: &mut MouseCapture) {
    let Some(command) = app.pending_command.take() else { return };
    let plain = std::mem::take(&mut app.pending_plain);
    // Menus come back only between turns.
    app.pending_then = None;
    if !plain && let Some(menu) = turn_menu(app, &command) {
        app.menu = Some(menu);
        return;
    }
    // Loading a model or resuming a session cannot happen mid-turn: it waits for the turn's end,
    // like the same command typed.
    if between_turns_only(&command) {
        app.toast(po_i18n::t!("tui.toast.queued", text = command));
        app.queued.push_back(command);
        return;
    }
    let cache = app.tabs_cache.clone();
    if !tui_command(app, &command, &mut || cache.clone(), mouse) {
        defer_command(app, command, true, pause);
    }
}

/// Carries out an item of the open menu: `index`, else the one highlighted.
fn menu_choose(app: &mut App, index: Option<usize>) {
    let Some(menu) = app.menu.as_ref() else { return };
    let Some(item) = index.map_or_else(|| menu.current(), |i| menu.items.get(i)).cloned() else { return };
    app.menu = None;
    if item.closes() {
        control::mirror().offer(None);
        return;
    }
    app.pending_then = item.then.clone();
    match item.action {
        menu::Action::Run(command) => app.pending_command = Some(command),
        menu::Action::Show(command) => {
            app.pending_command = Some(command);
            app.pending_plain = true;
        }
        menu::Action::Fill(start) => {
            app.input.clear();
            app.input.insert_str(&start);
            app.toast(po_i18n::t!("tui.menu.fill"));
        }
    }
}

/// `/catalogue` alone or with a category opens the browser (`Some(category)`); its other uses
/// (`voir`, `installer`…) stay commands.
fn catalog_wanted(text: &str) -> Option<Option<String>> {
    let mut words = text.split_whitespace();
    if !matches!(words.next(), Some("/catalogue" | "/catalog")) {
        return None;
    }
    match words.next() {
        None => Some(None),
        Some("voir" | "show" | "installer" | "install" | "retirer" | "remove" | "desinstaller" | "désinstaller" | "uninstall" | "voter" | "vote") => None,
        Some(category) => Some(Some(category.to_owned())),
    }
}

/// Opens the catalog's browser, on category `filter` if given.
async fn open_catalog(
    app: &mut App,
    filter: Option<String>,
    host: &mut impl Host,
    backend: &mut view::Backend,
    terminal: &Terminal,
) -> std::io::Result<()> {
    app.catalog = Some(catalog::Browser::loading());
    backend.draw(app, terminal)?;
    let found = keep_alive(host.catalog_list()).await;
    let Some(browser) = app.catalog.as_mut() else { return Ok(()) };
    match found {
        Ok(entries) => {
            browser.set_entries(entries);
            if let Some(wanted) = filter {
                let lower = wanted.to_lowercase();
                let hit = browser
                    .categories()
                    .into_iter()
                    .find(|c| *c == lower || labs::category_label(c).to_lowercase() == lower)
                    .map(str::to_owned);
                match hit {
                    Some(category) => browser.category = Some(category),
                    None => browser.message = Some((po_i18n::t!("tui.catalog.no_category", name = wanted), true)),
                }
            }
        }
        Err(err) => {
            browser.loading = false;
            browser.message = Some((err, true));
        }
    }
    Ok(())
}

/// A key in the catalog's browser.
async fn catalog_key(
    app: &mut App,
    key: KeyEvent,
    agent: &mut Agent,
    host: &mut impl Host,
    backend: &mut view::Backend,
    terminal: &Terminal,
) -> std::io::Result<()> {
    let Some(browser) = app.catalog.as_mut() else { return Ok(()) };
    let action = catalog::map_key(key, browser.details.is_some());
    let name = browser.current().map(|e| e.name.clone());
    match action {
        catalog::Key::Move(delta) => browser.move_by(delta),
        catalog::Key::Category(delta) => browser.turn_category(delta),
        catalog::Key::Back => browser.close_details(),
        catalog::Key::Close => app.catalog = None,
        catalog::Key::None => {}
        catalog::Key::Show => {
            let Some(name) = name else { return Ok(()) };
            browser.busy = true;
            browser.message = None;
            backend.draw(app, terminal)?;
            let files = keep_alive(host.catalog_files(&name)).await;
            if let Some(browser) = app.catalog.as_mut() {
                browser.busy = false;
                match files {
                    Ok(files) => {
                        browser.details = Some(files);
                        browser.scroll = 0;
                    }
                    Err(err) => browser.message = Some((err, true)),
                }
            }
        }
        catalog::Key::Act(act) => {
            let Some(name) = name else { return Ok(()) };
            browser.busy = true;
            browser.message = None;
            backend.draw(app, terminal)?;
            let done = keep_alive(host.catalog_act(act, &name, agent)).await;
            if let Some(browser) = app.catalog.as_mut() {
                browser.busy = false;
                match done {
                    Ok(done) => {
                        if let Some(entry) = browser.entries.iter_mut().find(|e| e.name == name) {
                            entry.installed = done.installed.unwrap_or(entry.installed);
                            entry.votes = done.votes.unwrap_or(entry.votes);
                        }
                        browser.message = Some((done.message, false));
                    }
                    Err(err) => browser.message = Some((err, true)),
                }
            }
        }
    }
    Ok(())
}

/// A command that means "stop loading the model": sent by Escape in the terminal, by the phone's
/// cancel button, or typed as a command.
pub fn cancel_load_wanted(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        [name] => matches!(
            *name,
            "/interrupt" | "/arreter" | "/arrêter" | "/stop" | "/annuler" | "/quitte"
        ),
        ["/modele" | "/model" | "/modèle", rest] => matches!(
            *rest,
            "cancel" | "annuler" | "stop" | "arrete" | "arrête" | "unload" | "decharger" | "décharger"
        ),
        _ => false,
    }
}

/// The session's needs before a message (its default model started), the screen kept alive
/// meanwhile; false when the message cannot go now.
async fn prepare_turn(
    app: &mut App,
    agent: &mut Agent,
    host: &mut impl Host,
    backend: &mut view::Backend,
    terminal: &Terminal,
    events: &mut EventStream,
    control_rx: &mut mpsc::UnboundedReceiver<control::Request>,
) -> std::io::Result<bool> {
    app.status.busy = true;
    // The phone learns at once that the message is being prepared: a model can take minutes to
    // load, and `follow` reports what it is doing meanwhile.
    publish_status(app);
    let output = {
        let work = host.before_turn(agent);
        tokio::pin!(work);
        let mut beat = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                output = &mut work => break output,
                _ = beat.tick() => {
                    app.frame = app.frame.wrapping_add(1);
                    if let Some(text) = progress() {
                        app.toast(text);
                    }
                    backend.draw(app, terminal)?;
                }
                // A model can take minutes to load: the keyboard stays live meanwhile. Escape
                // cancels the load (the message goes back to the input), typing keeps working.
                event = events.next() => {
                    let Some(Ok(event)) = event else { return Ok(true) };
                    if let Event::Key(key) = event {
                        match map_key(key, &mut app.input, false, true) {
                            KeyAction::Escape | KeyAction::Interrupt
                                if control::mirror().current_loading().is_some() || progress().is_some() =>
                            {
                                // The preparation is asked to stop at its next checkpoint; the
                                // future is not dropped here (a launch dropped mid-spawn would
                                // leave its server holding the card).
                                request_load_cancel();
                                app.toast(po_i18n::t!("setup.load_cancelled"));
                            }
                            KeyAction::Queue(text) => {
                                app.toast(po_i18n::t!("tui.toast.queued", text = text));
                                app.queued.push_back(text);
                            }
                            // Typing keeps working: `map_key` already moved the characters into
                            // the input. Anything else is of no use while the model loads.
                            _ => {}
                        }
                        backend.draw(app, terminal)?;
                    }
                }
                Some(request) = control_rx.recv() => {
                    let answer = match request.action {
                        control::Action::Input(po_wire::Input::Interrupt) => {
                            request_load_cancel();
                            (po_i18n::t!("setup.load_cancelled").to_owned(), false)
                        }
                        control::Action::Command(line) if cancel_load_wanted(&line) => {
                            request_load_cancel();
                            (po_i18n::t!("setup.load_cancelled").to_owned(), false)
                        }
                        control::Action::Command(line) => {
                            app.queued.push_back(line);
                            (po_i18n::t!("tui.control.queued_command").to_owned(), false)
                        }
                        control::Action::Prompt(text) => {
                            app.queued.push_back(text);
                            (po_i18n::t!("tui.control.queued_prompt").to_owned(), false)
                        }
                        other => control_read(app, backend, &other),
                    };
                    let _ = request.reply.send(answer);
                    app.frame = app.frame.wrapping_add(1);
                    backend.draw(app, terminal)?;
                }
            }
        }
    };
    set_progress(None);
    app.status.busy = false;
    let Some(output) = output else { return Ok(true) };
    refresh_context(app, agent);
    publish_status(app);
    if !output.lines.is_empty() {
        control::mirror().push(po_wire::Event::CommandOutput { command: po_i18n::t!("tui.model.preparing_title").to_owned(), lines: control::wire_lines(&output.lines) });
        app.transcript.push(Line::new(LineKind::CardTitle, po_i18n::t!("tui.model.preparing_title")));
        push_output(app, output.lines);
        app.transcript.scroll_to_bottom();
    }
    Ok(!output.quit)
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
    publish_status(app);
    backend.draw(app, terminal)?;
    let mut shown_progress: Option<String> = None;
    let output = {
        let command = host.command(text, agent);
        tokio::pin!(command);
        let mut beat = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                output = &mut command => break output,
                _ = beat.tick() => {
                    app.frame = app.frame.wrapping_add(1);
                    // A model takes minutes to load: what it is doing stays on screen.
                    if let Some(text) = progress() {
                        app.toast(text.clone());
                        shown_progress = Some(text);
                    }
                    backend.draw(app, terminal)?;
                }
            }
        }
    };
    // What the command was doing is over: its line does not stay on screen after it (seen: "The
    // model is writing the profile…" still shown under the saved profile).
    clear_progress_toast(app, shown_progress.as_deref());
    set_progress(None);
    if let Some(line) = take_menu_after() {
        app.pending_command = Some(line);
    }
    app.status.busy = false;
    app.status.mode = Some(agent.router().mode());
    app.show_thinking = agent.settings.show_thinking;
    app.monitor.show_thinking = app.show_thinking;
    for view in &mut app.monitor.views {
        view.show_thinking = app.show_thinking;
    }
    refresh_context(app, agent);
    if text.starts_with("/new")
        || text.starts_with("/nouveau")
        || text.starts_with("/clear")
        || text.starts_with("/resume")
        || text.starts_with("/reprendre")
        || text.starts_with("/compact")
    {
        app.title = agent.title.clone();
        app.workspace = host.workspace();
        app.transcript.clear();
        app.replay(&agent.session.full_history(), agent.workspace_root());
        app.transcript.scroll_to_bottom();
    }
    // The history first, its report after: a command that replaces the conversation (`/compact`,
    // `/new`, `/reprendre`) sends a reset, and a report sent before it would be wiped out with
    // the rest - the phone would see the conversation shrink, with no word about why.
    control::mirror().sync(agent.session_key.as_deref(), &agent.session.messages, &agent.session.archive);
    control::mirror().push(po_wire::Event::CommandOutput { command: text.to_owned(), lines: control::wire_lines(&output.lines) });
    publish_status(app);
    // A `/voice` command makes the "dictate" button appear or disappear; `/labs` turns the
    // indicator on; `/language` changes the offered commands.
    app.voice_enabled = voice::Settings::load().enabled;
    app.labs = po_i18n::prefs::get_bool("labs").unwrap_or(false);
    app.commands = host.completions();
    app.command_names = host.command_names();
    Ok(output)
}

/// Quiet time after a turn before its conversation is saved to disk: long enough not to save
/// between two quick messages, short enough to be done before the user stops the server.
const IDLE_PARK: Duration = Duration::from_secs(20);

/// Last answer of the local server about its model, and when the next look is due.
static LOCAL_LOADED: std::sync::Mutex<(Option<bool>, Option<std::time::Instant>)> = std::sync::Mutex::new((None, None));

/// Is the local model in memory on its server? Strata says it in `/v1/status` ("loaded");
/// another server that answers is taken as loaded; no answer (stopped, unloaded by `po unload`):
/// not loaded. A plain HTTP/1.0 request: the address is local and the answer small.
async fn local_model_loaded(endpoint: &str) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let address = endpoint.trim_start_matches("http://").split('/').next().unwrap_or_default();
    let address = if address.contains(':') { address.to_owned() } else { format!("{address}:80") };
    let ask = async {
        let mut stream = tokio::net::TcpStream::connect(&address).await.ok()?;
        stream.write_all(format!("GET /v1/status HTTP/1.0\r\nHost: {address}\r\n\r\n").as_bytes()).await.ok()?;
        let mut answer = Vec::new();
        stream.take(1 << 20).read_to_end(&mut answer).await.ok()?;
        Some(answer)
    };
    let Ok(Some(answer)) = tokio::time::timeout(Duration::from_millis(1500), ask).await else {
        return false;
    };
    let text = String::from_utf8_lossy(&answer);
    let body = text.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v["loaded"].as_bool()).unwrap_or(true)
}

/// Follows whether the session's local model is loaded: a look every 5 s in the background, its
/// answer taken at the next beat. True when the status changed.
fn follow_local_model(app: &mut App, agent: &Agent) -> bool {
    let endpoint = match agent.session_model().map(|m| m.origin) {
        Some(po_core::Origin::Local { endpoint, .. }) => endpoint,
        _ => {
            let changed = app.status.model_loaded.is_some();
            app.status.model_loaded = None;
            return changed;
        }
    };
    let mut state = LOCAL_LOADED.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if state.1.is_none_or(|due| now >= due) {
        state.1 = Some(now + Duration::from_secs(5));
        tokio::spawn(async move {
            let loaded = local_model_loaded(&endpoint).await;
            LOCAL_LOADED.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(loaded);
        });
    }
    let seen = state.0;
    drop(state);
    if seen.is_some() && seen != app.status.model_loaded {
        // The server came back without this conversation in memory: its saved copy goes back
        // in before the next request, which then reads only what is new.
        if seen == Some(false) {
            app.unpark_pending = true;
        }
        if seen == Some(true) && app.unpark_pending && !app.status.busy {
            app.unpark_pending = false;
            agent.unpark_in_background();
        }
        app.status.model_loaded = seen;
        return true;
    }
    false
}

/// Context gauge of the status bar.
fn refresh_context(app: &mut App, agent: &Agent) {
    app.status.context = agent.session_model().map(|m| (agent.estimated_request_tokens(), m.context_window));
    app.context_breakdown = Some(agent.context_breakdown());
    app.goals = agent.goal_states();
    let budget = agent.session_model().map(|m| m.context_window.saturating_sub(po_agent::RESERVE_TOKENS)).unwrap_or(0);
    let prompt = agent.estimated_request_tokens();
    if budget > 0 && (prompt as f32) <= (budget as f32 * po_agent::COMPACTION_WARNING_RATIO) {
        app.context_warning = None;
    }
    app.remote_models = agent.router().models().iter().map(|m| m.id.clone()).collect();
    app.remote_pinned = agent.router().pinned().map(str::to_owned);
    app.remote_effort = agent.settings.reasoning_effort.clone();
    app.remote_engine = engine_of(agent);
    app.model_engines = agent
        .router()
        .models()
        .iter()
        .map(|m| {
            let engine = match &m.origin {
                po_core::Origin::Local { backend, .. } => backend.clone(),
                po_core::Origin::Cloud { provider } => provider.clone(),
            };
            (m.display_name.clone(), engine)
        })
        .collect();
}

/// What serves the session's model: the local backend, else the cloud provider.
pub fn engine_of(agent: &Agent) -> String {
    match agent.session_model().map(|m| m.origin) {
        Some(po_core::Origin::Local { backend, .. }) => backend,
        Some(po_core::Origin::Cloud { provider }) => provider,
        None => String::new(),
    }
}

/// What the status bar says, for the phone: sent only when it changed.
pub fn publish_status(app: &App) {
    let (context_used, context_window) = app.status.context.unwrap_or((0, 0));
    let git = crate::statusline::git();
    control::mirror().status(po_wire::Status {
        title: app.title.clone(),
        model: app.status.model.clone(),
        pinned: app.remote_pinned.clone(),
        models: app.remote_models.clone(),
        work_mode: app.work_mode.as_ref().map(|m| m.get().label().to_owned()).unwrap_or_default(),
        budget: app.status.mode.map(|m| m.label().to_owned()).unwrap_or_default(),
        effort: app.remote_effort.clone(),
        busy: app.status.busy,
        context_used,
        context_window,
        cost: app.status.cost,
        queued: app.queued.len(),
        queue: app.queued.iter().cloned().collect(),
        paused: app.user_pause,
        labs: app.labs,
        engine: app.remote_engine.clone(),
        generation_tok_s: 0.0,
        prefill_tok_s: 0.0,
        // The mirror puts it in.
        offer: None,
        loading: None,
        workspace: app.workspace.as_ref().map(|w| w.display().to_string()).unwrap_or_default(),
        placement: app.remote_placement.clone(),
        branch: git.as_ref().and_then(|g| g.branch.clone()),
        git_added: git.as_ref().map(|g| g.added).unwrap_or(0),
        git_removed: git.as_ref().map(|g| g.removed).unwrap_or(0),
        context_tools: app.context_breakdown.as_ref().map(|b| b.tools).unwrap_or(0),
        context_thinking: app.context_breakdown.as_ref().map(|b| b.thinking).unwrap_or(0),
        context_assistant: app.context_breakdown.as_ref().map(|b| b.assistant).unwrap_or(0),
        context_user: app.context_breakdown.as_ref().map(|b| b.user).unwrap_or(0),
        context_system: app.context_breakdown.as_ref().map(|b| b.system).unwrap_or(0),
        context_warning: app.context_warning.clone(),
        loop_warning: app.loop_warning.clone(),
        instant_tok_s: app.progress.as_ref().and_then(|p| p.live_generation()).map(|v| v as f32).unwrap_or(0.0),
        last_prefill_tok_s: app.speeds.map(|s| s.prefill_tok_s as f32).unwrap_or(0.0),
        model_vram_mib: app.model_vram_mib,
        model_ram_mib: app.model_ram_mib,
        model_gpu_name: app.model_gpu_name.clone(),
        secondary_model: app.secondary_model.clone(),
        dynamic_action: app.dynamic_action.clone(),
        subagent_parallel: app.subagent_parallel,
        subagent_model: app.subagent_model.clone(),
        secondary_candidates: app.secondary_candidates.clone(),
        model_loaded: app.status.model_loaded,
        goals: app
            .goals
            .iter()
            .map(|(text, passed)| po_wire::GoalRec {
                text: text.clone(),
                state: match passed {
                    Some(true) => "met",
                    Some(false) => "failed",
                    None => "pending",
                }
                .to_owned(),
            })
            .collect(),
    });
}

/// An approval or a question answered from the phone: the one on screen, if it is the one meant
/// (`id` 0: whichever is on screen, for an MCP client that sees no numbers).
fn remote_answer(app: &mut App, input: &po_wire::Input) -> (String, bool) {
    match input {
        po_wire::Input::Unqueue { index } => {
            let done = app.queued.remove(*index as usize);
            publish_status(app);
            match done {
                Some(text) => (po_i18n::t!("tui.toast.queued_cancelled", text = text), false),
                None => (po_i18n::t!("tui.control.queue_gone").to_owned(), true),
            }
        }
        po_wire::Input::Requeue { index, text } if !text.trim().is_empty() => match app.queued.get_mut(*index as usize) {
            Some(waiting) => {
                *waiting = text.trim().to_owned();
                publish_status(app);
                (po_i18n::t!("tui.control.queue_edited").to_owned(), false)
            }
            None => (po_i18n::t!("tui.control.queue_gone").to_owned(), true),
        },
        po_wire::Input::Pause => (po_i18n::t!("tui.control.nothing_running").to_owned(), false),
        po_wire::Input::Approve { id, granted } => match app.pending_approval.take() {
            Some(request) if *id == 0 || request.id == *id => {
                request.answer(*granted);
                (po_i18n::t!("common.done").to_owned(), false)
            }
            other => {
                app.pending_approval = other;
                (po_i18n::t!("tui.control.nothing_pending").to_owned(), true)
            }
        },
        po_wire::Input::Answer { id, choice, text } => match app.pending_question.take() {
            Some(pending) if *id == 0 || pending.request.id == *id => {
                let options = pending.request.question.options.len();
                let answer = match (choice, text) {
                    (Some(i), _) if (*i as usize) < options => po_agent::question::Answer::Choice(*i as usize),
                    (_, Some(t)) if !t.trim().is_empty() => po_agent::question::Answer::Text(t.trim().to_owned()),
                    _ => po_agent::question::Answer::Declined,
                };
                pending.request.answer(answer);
                (po_i18n::t!("common.done").to_owned(), false)
            }
            other => {
                app.pending_question = other;
                (po_i18n::t!("tui.control.nothing_pending").to_owned(), true)
            }
        },
        _ => (po_i18n::t!("tui.control.nothing_running").to_owned(), false),
    }
}

/// The commands offered to completion, as JSON for the phone.
fn commands_json(app: &App) -> String {
    let list: Vec<po_wire::CommandInfo> = app.commands.iter().map(|(name, description)| po_wire::CommandInfo { name: name.clone(), description: description.clone() }).collect();
    serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
}

fn menu_json(menu: Option<&menu::Menu>) -> String {
    serde_json::to_string(&menu.map(control::wire_menu)).unwrap_or_else(|_| "null".into())
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
        settings::Effect::Open(command) => {
            app.panel = None;
            app.pending_command = Some(command);
        }
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
        Some(settings::Effect::Open(command)) => {
            app.panel = None;
            app.pending_command = Some(command);
        }
    }
}

/// A tool stopped with the turn will never finish: its row stops turning and says so (it spun
/// on after an interruption, observed).
fn stop_running_tools(app: &mut App) {
    for (_, position, started) in std::mem::take(&mut app.running_tools) {
        if let Some(text) = app.transcript.get(position).filter(|l| l.kind == LineKind::ToolRunning).map(|l| l.text.clone()) {
            let line = Line::new(LineKind::ToolError, po_i18n::t!("tui.tool.stopped", text = text)).with_secs(started.elapsed().as_secs_f64());
            app.transcript.replace_at(position, line);
        }
    }
}

/// Notes a setting command for the turn's next safe point.
fn defer_command(app: &mut App, command: String, typed: bool, pause: &AtomicBool) {
    // Two values of the same setting (`/mode plan` then `/mode build`): only the last one counts.
    // A longer command (`/reflexion budget 2048`, `/criteres ajouter …`) is kept as it is: they
    // all apply, in order.
    let words: Vec<&str> = command.split_whitespace().collect();
    if let [name, _] = words[..] {
        app.deferred.retain(|(c, _)| {
            let earlier: Vec<&str> = c.split_whitespace().collect();
            !(earlier.len() == 2 && earlier[0] == name)
        });
    }
    app.deferred.push((command, typed));
    pause.store(true, std::sync::atomic::Ordering::Relaxed);
    app.toast(po_i18n::t!("tui.toast.setting_noted"));
}

/// Commands that only apply between two turns: they replace the conversation, summarize it, or
/// load a model - none of that can be done in the middle of a turn.
pub fn between_turns_only(command: &str) -> bool {
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
            | "/moteurs"
            | "/moteur"
            | "/engines"
            | "/engine"
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
                        LineKind::Image => "image",
                        LineKind::CardTitle
                        | LineKind::Card
                        | LineKind::CardWarn
                        | LineKind::CardSection
                        | LineKind::CardItem
                        | LineKind::CardField
                        | LineKind::CardDim => po_i18n::t!("tui.control.label.card"),
                    };
                    match &line.progress {
                        Some(progress) => format!("[{label}] {} - {progress}", line.text),
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
    app.command_names = host.command_names();
    app.workspace = host.workspace();
    let mut events = EventStream::new();
    let mut backend = view::Backend::new(terminal)?;

    app.status.mode = Some(agent.router().mode());
    app.work_mode = Some(agent.work_mode());
    app.title = agent.title.clone();
    app.replay(&agent.session.full_history(), agent.workspace_root());
    // What went wrong at startup was written before the screen took over, and hidden by it.
    for warning in host.startup_warnings() {
        app.error(warning);
    }
    for note in host.startup_notes() {
        app.note(note);
    }
    if let Some(command) = host.startup_command() {
        app.pending_command = Some(command);
    }
    // A session saved in the middle of a paused turn: it goes on where it stopped.
    if agent.is_paused() {
        app.user_pause = true;
        app.note(po_i18n::t!("tui.labs.paused", key = labs::shortcut("labs").label()));
    }
    let (monitor_tx, mut monitor_rx) = mpsc::unbounded_channel();
    agent.set_subagent_monitor(monitor_tx.clone());
    // Po Labs at work alongside the user's session, and the server slots they share.
    let mut labs_run: Option<LabsRun> = None;
    let ledger = Arc::new(po_agent::SlotLedger::default());
    app.labs_key = agent.session_key.clone().unwrap_or_else(|| format!("pid-{}", std::process::id()));
    app.labs_mark = labs::installed_mark();
    let (title_tx, mut title_rx) = mpsc::unbounded_channel::<String>();
    let (jobs_tx, mut jobs_rx) = mpsc::unbounded_channel::<LabsJobDone>();
    app.labs_jobs = Some(jobs_tx);
    let (works_tx, mut works_rx) = mpsc::unbounded_channel::<LabsWorkDone>();
    app.labs_works = Some(works_tx);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<control::Request>();
    let (voice_tx, mut voice_rx) = mpsc::unbounded_channel::<Result<String, String>>();
    app.voice_tx = Some(voice_tx);
    app.voice_enabled = voice::Settings::load().enabled;
    // Kept until exit: the file announcing the address disappears with it.
    let _control = control::start(app.workspace.clone(), control_tx).await.ok();
    app.control_url = _control.as_ref().map(|c| c.url.clone());
    refresh_context(&mut app, agent);
    control::mirror().sync(agent.session_key.as_deref(), &agent.session.messages, &agent.session.archive);
    publish_status(&app);
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
    // A turn that ended is saved to disk once the session has been quiet for a while (see
    // `IDLE_PARK`): `None` while nothing new is to save.
    let mut park_at: Option<std::time::Instant> = None;
    let mut turns_seen = app.turns_done;
    loop {
        wave |= render::take_wave();
        // A touch on an indicator requests a command.
        if let Some(command) = app.pending_command.take() {
            let plain = std::mem::take(&mut app.pending_plain);
            if !plain && open_menu(&mut app, &command, agent, host).await {
                backend.draw(&app, terminal)?;
                continue;
            }
            if let Some(filter) = catalog_wanted(&command) {
                open_catalog(&mut app, filter, host, &mut backend, terminal).await?;
            } else if !tui_command(&mut app, &command, &mut || host.settings(agent), &mut mouse) {
                app.transcript.push(Line::new(LineKind::CardTitle, command.clone()));
                let output = host_command(&mut app, &command, agent, host, &mut backend, terminal).await?;
                push_output(&mut app, output.lines);
                app.transcript.scroll_to_bottom();
            }
            // The menu the choice came from opens again, showing what changed.
            if let Some(then) = app.pending_then.take() {
                app.pending_command = Some(then);
            }
            backend.draw(&app, terminal)?;
            wave = render::take_wave();
            continue;
        }
        if take_restart_request() {
            app.restart = true;
        }
        if app.restart && (labs_run.is_some() || !app.input.is_empty()) {
            // Po Labs installed a new version while working alongside: Po restarts once it has
            // finished and the user is not typing.
            if !std::mem::replace(&mut app.restart_noted, true) {
                app.note(po_i18n::t!("tui.labs.restart_waiting"));
                backend.draw(&app, terminal)?;
            }
        } else if app.restart {
            // The user's session is the one saved and resumed.
            keep_alive(labs_swap_out(&mut app, agent)).await;
            RESTART.store(true, std::sync::atomic::Ordering::Relaxed);
            break;
        }
        match take_labs_request() {
            1 => app.labs_front_request = true,
            2 => app.labs_back = true,
            _ => {}
        }
        let front = std::mem::take(&mut app.labs_front_request);
        if std::mem::take(&mut app.labs_launch) || front {
            // Po Labs is one for every Po open: held by another session, it stays there unless
            // the user asks again to take it here (only while it rests there).
            if let Some(busy) = app.labs_busy.clone() {
                // A long work runs: one thing at a time.
                app.toast(po_i18n::t!("tui.labs.busy", what = busy));
                backend.draw(&app, terminal)?;
                continue;
            }
            let mut force = false;
            if let Some(other) = app.labs_elsewhere.clone().filter(|_| labs_run.is_none()) {
                let again = app.labs_takeover_at.take().is_some_and(|at| at.elapsed() < Duration::from_secs(5));
                if other.working {
                    app.toast(po_i18n::t!("tui.labs.elsewhere_working", project = other.project));
                } else if !again {
                    app.labs_takeover_at = Some(std::time::Instant::now());
                    app.toast(po_i18n::t!("tui.labs.elsewhere_take", project = other.project, key = labs::shortcut("labs").label()));
                } else {
                    force = true;
                }
                if !force {
                    backend.draw(&app, terminal)?;
                    continue;
                }
            }
            if labs_run.is_none() && !labs_claim(&mut app, force) {
                backend.draw(&app, terminal)?;
                continue;
            }
            if labs_run.is_some() {
                // Already at work alongside: its big card shows what it does.
                open_labs_card(&mut app);
            } else if !front && app.labs_book.awaiting_verdict() {
                // One change at a time: the one on trial waits for the user's verdict first.
                app.labs_notebook_open = true;
            } else if !front && app.labs_book.trial.is_none() && labs_start_draft(&mut app, agent, host, true) {
                // A kept change has its contribution written before the next request.
            } else if !front && !app.labs_book.has_work() && !app.labs_book.to_review().is_empty() && !agent.is_paused() {
                // Everything is done: the contributions are reviewed, one page each.
                let ids = app.labs_book.to_review().iter().map(|k| k.id).collect();
                labs_open_review(&mut app, ids);
            } else if !front && !app.labs_front && agent.session_slots() >= 2 && labs_has_work(&app) {
                labs_start_parallel(&mut app, agent, host, &ledger, &mut labs_run);
            } else {
                keep_alive(labs_swap_in(&mut app, agent, host, &monitor_tx)).await;
            }
            refresh_context(&mut app, agent);
            backend.draw(&app, terminal)?;
            continue;
        }
        if std::mem::take(&mut app.labs_back) {
            keep_alive(labs_swap_out(&mut app, agent)).await;
            if std::mem::take(&mut app.labs_lost) {
                app.other_agent = None;
            }
            refresh_context(&mut app, agent);
            backend.draw(&app, terminal)?;
            continue;
        }
        if std::mem::take(&mut app.labs_review_request) {
            if labs_claim(&mut app, false) {
                let book = labs::Notebook::load();
                let ids = book.aside().iter().chain(book.to_review().iter()).map(|k| k.id).collect();
                app.labs_book = book;
                labs_open_review(&mut app, ids);
            }
            backend.draw(&app, terminal)?;
            continue;
        }
        if let Some(action) = app.labs_contrib.take() {
            let current = app.labs_review.as_ref().filter(|r| r.rewriting.is_none()).and_then(labs::Review::current);
            let kept = current.and_then(|id| app.labs_book.kept.iter().find(|k| k.id == id).cloned());
            let title = kept.as_ref().and_then(|k| k.draft.as_ref()).map(|d| d.title.clone()).unwrap_or_default();
            if let Some(busy) = app.labs_busy.clone().filter(|_| kept.is_some()) {
                // One long work at a time.
                app.toast(po_i18n::t!("tui.labs.busy", what = busy));
            } else if let Some(kept) = kept {
                let id = kept.id;
                let settled = match action {
                    labs::ContribAction::Share => {
                        // Nothing leaves without the user's say-so: what goes where, first.
                        let target = host.share_target();
                        if let Some(review) = app.labs_review.as_mut() {
                            review.confirming = true;
                            review.target = target;
                        }
                        false
                    }
                    labs::ContribAction::Send => {
                        // Sent in the background; how it goes shows on the card.
                        if let Some(review) = app.labs_review.as_mut() {
                            review.confirming = false;
                        }
                        let tx = app.labs_works.clone();
                        let progress: LabsProgress = Box::new(move |text| {
                            if let Some(tx) = &tx {
                                let _ = tx.send(LabsWorkDone::Progress(text));
                            }
                        });
                        let work = host.contribution_share(&kept, progress);
                        labs_spawn(&mut app, po_i18n::t!("tui.labs.sharing", title = title), work, move |r| LabsWorkDone::Share(kept, r));
                        false
                    }
                    labs::ContribAction::Back => {
                        if let Some(review) = app.labs_review.as_mut() {
                            review.back();
                        }
                        false
                    }
                    labs::ContribAction::Later => labs::Notebook::modify(|b| b.set_share(kept.id, labs::Share::Aside)).is_ok(),
                    labs::ContribAction::Delete => labs::Notebook::modify(|b| b.set_share(kept.id, labs::Share::Deleted)).is_ok(),
                    labs::ContribAction::Edit => {
                        if let Some(review) = app.labs_review.as_mut() {
                            review.choosing = true;
                        }
                        false
                    }
                    labs::ContribAction::EditText => {
                        if let Some(review) = app.labs_review.as_mut() {
                            review.choosing = false;
                        }
                        app.labs_rewriting = Some(kept.id);
                        app.toast(po_i18n::t!("tui.labs.edit_hint"));
                        false
                    }
                    labs::ContribAction::EditCode => {
                        // Po goes to the version of this contribution, built in the background:
                        // the user then works on it as usual, and Po Labs adjusts the contribution.
                        let work = host.contribution_edit(&kept);
                        labs_spawn(&mut app, po_i18n::t!("tui.labs.switching", title = title), work, LabsWorkDone::Edit);
                        labs_close_review(&mut app);
                        false
                    }
                };
                if settled && let Some(review) = app.labs_review.as_mut() {
                    review.settle(id, action);
                }
                app.labs_book = labs::Notebook::load();
                app.labs_book_at = Some(std::time::Instant::now());
            }
            backend.draw(&app, terminal)?;
            continue;
        }
        if let Some(verdict) = app.labs_verdict.take() {
            if let Some(busy) = app.labs_busy.clone() {
                app.toast(po_i18n::t!("tui.labs.busy", what = busy));
                backend.draw(&app, terminal)?;
                continue;
            }
            let book = labs::Notebook::load();
            let contribution = book.trial.clone().filter(|t| t.contribution.is_some() && book.awaiting_verdict());
            if let Some(trial) = contribution.filter(|_| verdict != labs::Verdict::Revise) {
                // The change of a contribution's code ends: back to the latest version.
                let keep = verdict == labs::Verdict::Keep;
                app.labs_notebook_open = false;
                let work = host.contribution_edit_done(&trial, keep);
                labs_spawn(&mut app, po_i18n::t!("tui.labs.contribution_ending").to_owned(), work, move |r| LabsWorkDone::EditDone(trial, keep, r));
                backend.draw(&app, terminal)?;
                continue;
            }
            match (verdict, book.trial.clone().filter(|_| book.awaiting_verdict())) {
                (labs::Verdict::Undo, Some(trial)) => {
                    app.labs_notebook_open = false;
                    let work = host.labs_undo(&trial);
                    labs_spawn(&mut app, po_i18n::t!("tui.labs.undoing", summary = trial.summary), work, LabsWorkDone::Undo);
                }
                (verdict, _) => {
                    labs_verdict(&mut app, verdict);
                    // With a slot to spare, its contribution is written at once; with one, when
                    // the user next lets Po Labs work.
                    if verdict == labs::Verdict::Keep && agent.session_slots() >= 2 {
                        labs_start_draft(&mut app, agent, host, false);
                    }
                }
            }
            backend.draw(&app, terminal)?;
            continue;
        }
        if std::mem::take(&mut app.pending_resume) && agent.is_paused() {
            app.user_pause = false;
            app.note(po_i18n::t!("tui.labs.resumed"));
            run_turn(&mut app, agent, host, None, &mut events, &mut approvals, &mut questions,
                     &mut monitor_rx, &mut control_rx, &mut voice_rx, &mut backend, terminal, &mut mouse, &mut labs_run).await?;
            labs_save_done(&mut app, host);
            host.after_turn(agent);
            refresh_context(&mut app, agent);
            app.last_activity = std::time::Instant::now();
            backend.draw(&app, terminal)?;
            continue;
        }
        // A submitted message - from the keyboard or the remote control - goes out here.
        if let Some(text) = app.pending_submit.take_if(|_| app.labs_rewriting.is_some()) {
            // How to change the text of a contribution: rewritten in the background.
            if let Some(id) = app.labs_rewriting.take() {
                labs_write(&mut app, agent, host, id, Some(&text));
            }
            backend.draw(&app, terminal)?;
            continue;
        }
        if let Some(text) = app.pending_submit.take_if(|_| app.labs_revising) {
            // What to adjust in the change on trial: for Po Labs, which takes it up now.
            app.labs_revising = false;
            if let Ok((book, true)) = labs::Notebook::modify(|b| b.revise(&text)) {
                app.labs_book = book;
                app.note(po_i18n::t!("tui.labs.revise_sent", text = text));
                app.labs_launch = true;
            }
            backend.draw(&app, terminal)?;
            continue;
        }
        if let Some(text) = app.pending_submit.take() {
            app.transcript.push_text(LineKind::User, &text);
            app.transcript.scroll_to_bottom();
            // A message sets the Po Labs proposal aside, until the notebook changes.
            app.labs_dismissed = app.labs_book.revision;
            // Its model first: started if it is not serving (the message waits meanwhile).
            if !agent.is_paused() && !prepare_turn(&mut app, agent, host, &mut backend, terminal, &mut events, &mut control_rx).await? {
                app.input.insert_str(&text);
                backend.draw(&app, terminal)?;
                continue;
            }
            // A paused agent takes the message where it stopped, and goes on.
            let prompt = if agent.is_paused() {
                agent.inject_user_message(&text);
                control::mirror().push(po_wire::Event::User { text: text.clone() });
                app.user_pause = false;
                None
            } else {
                Some(text.as_str())
            };
            run_turn(&mut app, agent, host, prompt, &mut events, &mut approvals, &mut questions,
                     &mut monitor_rx, &mut control_rx, &mut voice_rx, &mut backend, terminal, &mut mouse, &mut labs_run).await?;
            labs_save_done(&mut app, host);
            if let Some(title) = &app.title {
                agent.title = Some(title.clone());
            }
            request_title(&mut app, agent, &title_tx);
            host.after_turn(agent);
            refresh_context(&mut app, agent);
            app.last_activity = std::time::Instant::now();
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
                run_turn(&mut app, agent, host, Some(&next), &mut events, &mut approvals, &mut questions,
                         &mut monitor_rx, &mut control_rx, &mut voice_rx, &mut backend, terminal, &mut mouse, &mut labs_run).await?;
                labs_save_done(&mut app, host);
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
                if matches!(request.action, control::Action::Command(_) | control::Action::Prompt(_)) {
                    app.started = true;
                }
                let answer = match request.action.clone() {
                    control::Action::Command(command) => {
                        let shown = app.transcript.len();
                        if tui_command(&mut app, &command, &mut || host.settings(agent), &mut mouse) {
                            (interface_report(&mut app, &command, shown), false)
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
                    // A paused turn goes on.
                    control::Action::Input(po_wire::Input::Resume) if agent.is_paused() => {
                        app.pending_resume = true;
                        (po_i18n::t!("tui.labs.resumed").to_owned(), false)
                    }
                    control::Action::Input(input) => remote_answer(&mut app, &input),
                    control::Action::Menu(line) => (menu_json(keep_alive(host.command_menu(&line, agent)).await.as_ref()), false),
                    control::Action::Commands => (commands_json(&app), false),
                    control::Action::Close => (po_i18n::t!("tui.control.terminal_session").to_owned(), true),
                    other => control_read(&app, &backend, &other),
                };
                let _ = request.reply.send(answer);
                backend.draw(&app, terminal)?;
            }
            _ = alive.tick() => {
                watchdog::beat();
                if follow_local_model(&mut app, agent) {
                    backend.draw(&app, terminal)?;
                }
                if app.turns_done != turns_seen {
                    // A turn just ended: its conversation is saved once the session stays quiet.
                    turns_seen = app.turns_done;
                    park_at = Some(std::time::Instant::now() + IDLE_PARK);
                } else if app.status.busy {
                    park_at = park_at.map(|_| std::time::Instant::now() + IDLE_PARK);
                } else if park_at.is_some_and(|at| std::time::Instant::now() >= at) {
                    park_at = None;
                    if app.status.model_loaded == Some(true) {
                        agent.park_in_background();
                    }
                }
                if let Some(ready_model) = host.poll_ready(agent).await {
                    app.status.model = ready_model.clone();
                    app.status.local_server = Some(ready_model.clone());
                    refresh_context(&mut app, agent);
                    publish_status(&app);
                    control::mirror().push(po_wire::Event::Notice { text: po_i18n::t!("setup.model_ready", model = ready_model.clone()) });
                    app.toast(po_i18n::t!("setup.model_ready", model = ready_model));
                    backend.draw(&app, terminal)?;
                } else {
                    publish_status(&app);
                    if control::mirror().current_loading().is_some() || progress().is_some() {
                        app.frame = app.frame.wrapping_add(1);
                        backend.draw(&app, terminal)?;
                    }
                }
                // Po idle long enough with requests waiting: Po Labs starts by itself.
                let before = (app.labs_book.revision, app.labs_elsewhere.clone());
                app.labs_notebook();
                labs_presence(&mut app, labs_run.is_some());
                if before != (app.labs_book.revision, app.labs_elsewhere.clone()) {
                    // Another session changed the notebook, or where Po Labs is: shown at once.
                    backend.draw(&app, terminal)?;
                }
                let minutes = labs::idle_minutes();
                if minutes > 0
                    && matches!(app.labs_proposal(), Some(LabsAsk::Requests(_) | LabsAsk::Adjust | LabsAsk::Draft))
                    && app.last_activity.elapsed() >= Duration::from_secs(minutes * 60)
                {
                    app.labs_launch = true;
                }
                if app.expire_press() {
                    backend.draw(&app, terminal)?;
                }
                // A live menu shows what changes by itself (a connection validated, a phone
                // seen); its spinner turns while something is under way.
                let refreshed = refresh_menu(&mut app, agent, host).await;
                if app.menu.as_ref().is_some_and(|m| m.busy) {
                    app.frame = app.frame.wrapping_add(1);
                    backend.draw(&app, terminal)?;
                } else if refreshed {
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
            signal = labs_next(&mut labs_run) => {
                labs_signal(&mut app, &mut labs_run, signal);
                labs_save_done(&mut app, host);
                backend.draw(&app, terminal)?;
            }
            // Po Labs at work alongside may ask while the user's session rests.
            Some(request) = approvals.recv(), if app.pending_approval.is_none() && labs_run.is_some() => {
                app.pending_approval = Some(request);
                backend.draw(&app, terminal)?;
            }
            Some(request) = questions.recv(), if app.pending_question.is_none() && labs_run.is_some() => {
                app.pending_question = Some(question::Pending::new(request));
                app.transcript.scroll_to_bottom();
                backend.draw(&app, terminal)?;
            }
            Some(done) = jobs_rx.recv() => {
                labs_job_done(&mut app, done);
                backend.draw(&app, terminal)?;
            }
            Some(done) = works_rx.recv() => {
                labs_work_done(&mut app, done);
                backend.draw(&app, terminal)?;
            }
            Some(title) = title_rx.recv() => {
                app.title = Some(title.clone());
                agent.title = Some(title);
                app.title_pending = false;
                publish_status(&app);
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
                        if app.menu.is_some() {
                            menu_key(&mut app, key);
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        if app.catalog.is_some() {
                            catalog_key(&mut app, key, agent, host, &mut backend, terminal).await?;
                            backend.draw(&app, terminal)?;
                            continue;
                        }

                        app.last_activity = std::time::Instant::now();
                        if let Some(pending) = app.pending_question.as_mut() {
                            if let Some(answer) = pending.key(key)
                                && let Some(done) = app.pending_question.take()
                            {
                                done.request.answer(answer);
                            }
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        if key.kind != KeyEventKind::Release && labs::shortcut("labs").matches(&key) {
                            if agent.is_paused() {
                                app.pending_resume = true;
                            } else if app.labs && !app.labs_front && app.labs_card_shown() {
                                app.labs_launch = true;
                            }
                            continue;
                        }
                        if key.kind != KeyEventKind::Release && labs::shortcut("pause").matches(&key) && agent.is_paused() {
                            app.pending_resume = true;
                            continue;
                        }
                        // At rest, the pause goes to Po Labs working alongside.
                        if key.kind != KeyEventKind::Release
                            && labs::shortcut("pause").matches(&key)
                            && let Some(run) = &labs_run
                        {
                            run.pause.store(true, std::sync::atomic::Ordering::Relaxed);
                            app.toast(po_i18n::t!("tui.labs.pausing_parallel"));
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        // The review of contributions: plain letters and arrows, while it is open and
                        // nothing is being typed.
                        if key.kind != KeyEventKind::Release
                            && app.labs_review.is_some()
                            && app.input.is_empty()
                            && app.labs_rewriting.is_none()
                            && (key.modifiers - KeyModifiers::SHIFT).is_empty()
                        {
                            let used = match key.code {
                                KeyCode::Left | KeyCode::Right => {
                                    if let Some(review) = app.labs_review.as_mut() {
                                        review.turn(if key.code == KeyCode::Left { -1 } else { 1 });
                                    }
                                    true
                                }
                                KeyCode::Char(c) => {
                                    let among = app.labs_review.as_ref().map_or(&labs::ContribAction::ALL[..], labs::Review::choices);
                                    match labs::ContribAction::from_key_in(c, among) {
                                        Some(action) if app.labs_review.as_ref().is_some_and(|r| !r.finished()) => {
                                            app.labs_contrib = Some(action);
                                            true
                                        }
                                        _ => false,
                                    }
                                }
                                _ => false,
                            };
                            if used {
                                backend.draw(&app, terminal)?;
                                continue;
                            }
                        }
                        // The verdict on the change on trial: plain letters, while its card is open and
                        // nothing is being typed.
                        if key.kind != KeyEventKind::Release
                            && app.labs_notebook_open
                            && app.input.is_empty()
                            && app.labs_elsewhere.is_none()
                            && app.labs_book.awaiting_verdict()
                            && (key.modifiers - KeyModifiers::SHIFT).is_empty()
                            && let KeyCode::Char(c) = key.code
                            && let Some(verdict) = labs::Verdict::from_key(c)
                        {
                            app.labs_verdict = Some(verdict);
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
                        if matches!(action, KeyAction::Submit(_)) {
                            app.started = true;
                        }
                        match action {
                            KeyAction::Approve(granted) => {
                                if let Some(request) = app.pending_approval.take() {
                                    request.answer(granted);
                                }
                            }
                            KeyAction::Quit => break,
                            KeyAction::Submit(text) if catalog_wanted(&text).is_some() => {
                                open_catalog(&mut app, catalog_wanted(&text).flatten(), host, &mut backend, terminal).await?;
                            }
                            KeyAction::Submit(text) if text.starts_with('/') && open_menu(&mut app, &text, agent, host).await => {}
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
                                if control::mirror().current_loading().is_some() || progress().is_some() {
                                    host.cancel_model();
                                    app.toast(po_i18n::t!("setup.load_cancelled"));
                                    backend.draw(&app, terminal)?;
                                } else if escape_pressed(&mut app, false, agent.background_count()) {
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
                    // A finger's swipe (or the wheel) walks the catalog's list.
                    Event::Mouse(event) if app.menu.is_some() => {
                        // A click or a finger on a row chooses it, like Enter on it.
                        if let crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) = event.kind {
                            if let Some(index) = backend.overlay_row(event.column, event.row) {
                                menu_choose(&mut app, Some(index));
                            }
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        let delta = match event.kind {
                            crossterm::event::MouseEventKind::ScrollUp => -1,
                            crossterm::event::MouseEventKind::ScrollDown => 1,
                            _ => 0,
                        };
                        if delta != 0
                            && let Some(menu) = app.menu.as_mut()
                        {
                            menu.move_by(delta);
                            backend.draw(&app, terminal)?;
                        }
                    }
                    Event::Mouse(event) if app.catalog.is_some() => {
                        // A click or a finger on an extension opens its details: installing stays
                        // a deliberate key.
                        if let crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) = event.kind {
                            let row = backend.overlay_row(event.column, event.row);
                            if let (Some(index), Some(browser)) = (row, app.catalog.as_mut())
                                && browser.details.is_none()
                            {
                                browser.select(index);
                                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                                catalog_key(&mut app, enter, agent, host, &mut backend, terminal).await?;
                            }
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        let delta = match event.kind {
                            crossterm::event::MouseEventKind::ScrollUp => -1,
                            crossterm::event::MouseEventKind::ScrollDown => 1,
                            _ => 0,
                        };
                        if delta != 0
                            && let Some(browser) = app.catalog.as_mut()
                        {
                            browser.move_by(delta);
                            backend.draw(&app, terminal)?;
                        }
                    }
                    Event::Mouse(event) => {
                        let max = backend.card_max_scroll().unwrap_or(backend.max_scroll());
                        if let MouseEventKind::Down(MouseButton::Left) = event.kind
                            && let Some(url) = backend.link_at(event.column, event.row)
                        {
                            open_link(&mut app, &url);
                            backend.draw(&app, terminal)?;
                            continue;
                        }
                        if on_mouse_at(&mut app, event, max, backend.block_at(event.column, event.row), backend.hit(event.column, event.row)) {
                            backend.draw(&app, terminal)?;
                        }
                    }
                    // Bracketed paste (Unix): the text arrives as a block, line breaks included.
                    Event::Paste(text) if app.picker.is_none() && app.catalog.is_none() && app.menu.is_none() && app.pending_approval.is_none() => {
                        app.input.insert_str(&text);
                        backend.draw(&app, terminal)?;
                    }
                    _ => {}
                }
            }
        }
    }
    // Leaving while Po Labs works alongside: it stops at its next safe point, for a few seconds
    // at most, so its session is saved where it was.
    if let Some(run) = &labs_run {
        run.pause.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if labs_run.is_some() {
        let wait = async {
            loop {
                let signal = labs_next(&mut labs_run).await;
                let done = matches!(signal, LabsSignal::Done(_));
                labs_signal(&mut app, &mut labs_run, signal);
                if done {
                    break;
                }
            }
        };
        let _ = keep_alive(tokio::time::timeout(Duration::from_secs(5), wait)).await;
        labs_save_done(&mut app, host);
    }
    // Closed, Po Labs is free for the next session, exactly as left; kept across a restart.
    if app.labs_held && !RESTART.load(std::sync::atomic::Ordering::Relaxed) {
        labs::release(&app.labs_key);
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
    prompt: Option<&str>,
    events: &mut EventStream,
    approvals: &mut mpsc::UnboundedReceiver<ApprovalRequest>,
    questions: &mut mpsc::UnboundedReceiver<question::QuestionRequest>,
    monitor: &mut mpsc::UnboundedReceiver<po_agent::SubagentUpdate>,
    control: &mut mpsc::UnboundedReceiver<control::Request>,
    voice: &mut mpsc::UnboundedReceiver<Result<String, String>>,
    backend: &mut view::Backend,
    terminal: &Terminal,
    mouse: &mut MouseCapture,
    labs_run: &mut Option<LabsRun>,
) -> std::io::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.status.busy = true;
    app.unpark_pending = false;
    control::mirror().turn_started(prompt);
    let turn_start = std::time::Instant::now();
    publish_status(app);
    app.progress = Some(Progress::new());
    app.frame = 0;
    // During the turn the agent is busy: the settings panel and the menus open on these.
    app.tabs_cache = host.settings(agent);
    app.menus_cache = keep_alive(host.turn_menus(agent)).await;
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
        let (finished, interrupted) = if let Some(prompt) = prompt.filter(|_| first) {
            let turn = agent.turn(prompt, &mut sink);
            drive_turn(app, host, turn, &mut rx, events, approvals, questions, monitor, control, voice, backend, terminal, &pause, mouse, labs_run)
                .await?
        } else {
            let turn = agent.resume(&mut sink);
            drive_turn(app, host, turn, &mut rx, events, approvals, questions, monitor, control, voice, backend, terminal, &pause, mouse, labs_run)
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
        // Paused by the user: the agent keeps its turn, to resume later.
        if app.user_pause || take_pause_request() {
            app.user_pause = true;
            app.note(po_i18n::t!("tui.labs.paused", key = labs::shortcut("labs").label()));
            break (finished, interrupted);
        }
        // Safe point: the tools have returned their results, the model has not resumed yet.
        apply_deferred(app, agent, host, backend, terminal).await?;
        deliver_queued(app, agent);
        publish_status(app);
    };
    pause.store(false, std::sync::atomic::Ordering::Relaxed);
    // An interrupted turn gives its server slot back.
    agent.end_turn_slot();
    if !app.labs_front {
        app.labs_seen.0 += 1;
    }

    app.last_interrupted = interrupted;
    stop_running_tools(app);
    if interrupted {
        // Background subagents stop with the agent; those of the turn keep what they did.
        let stopped = agent.cancel_background();
        match agent.keep_after_interruption(checkpoint) {
            po_agent::Interrupted::Resumable => {
                // Paused rather than lost: ▶, the pause key or a message takes it up again.
                app.user_pause = true;
                app.monitor.pause_running();
                app.note(po_i18n::t!("tui.note.interrupted_kept"));
            }
            outcome => {
                if outcome == po_agent::Interrupted::Nothing {
                    restore_after_interruption(&mut agent.session.messages, checkpoint);
                }
                app.monitor.stop_all();
                app.note(if stopped > 0 {
                    po_i18n::t!("tui.note.interrupted_with", n = stopped)
                } else {
                    po_i18n::t!("tui.note.interrupted").to_owned()
                });
            }
        }
    } else if let Some(Err(err)) = &finished {
        app.error(err.to_string());
    }
    let error = match &finished {
        Some(Err(err)) if !interrupted => Some(err.to_string()),
        _ => None,
    };
    control::mirror().turn_ended(interrupted, error, turn_start.elapsed().as_secs_f64());
    app.progress = None;
    app.status.busy = false;
    app.turns_done += 1;
    // What found no safe point is applied now, before the next turn.
    apply_deferred(app, agent, host, backend, terminal).await?;
    app.status.update_from_ledger(&agent.session.ledger);
    refresh_context(app, agent);
    control::mirror().sync(agent.session_key.as_deref(), &agent.session.messages, &agent.session.archive);
    publish_status(app);
    app.transcript.scroll_to_bottom();
    backend.draw(app, terminal)?;
    Ok(())
}

/// During a turn, a command that needs neither the agent nor the turn's end runs at once: its
/// report goes in the conversation (and to the phone). `None` when it has to wait.
async fn command_now(app: &mut App, host: &mut impl Host, command: &str) -> Option<String> {
    let output = keep_alive(host.command_now(command, app.work_mode.as_ref())).await?;
    // Bypass lets the approval waiting through, as the key that cycles the modes does.
    if app.work_mode.as_ref().is_some_and(|m| m.get().gate() == po_agent::work_mode::Gate::Allow)
        && let Some(request) = app.pending_approval.take()
    {
        request.answer(true);
    }
    control::mirror().push(po_wire::Event::CommandOutput { command: command.to_owned(), lines: control::wire_lines(&output.lines) });
    let text: Vec<String> = output.lines.iter().map(CardLine::text).collect();
    app.transcript.push(Line::new(LineKind::CardTitle, command.to_owned()));
    push_output(app, output.lines);
    app.transcript.scroll_to_bottom();
    app.voice_enabled = voice::Settings::load().enabled;
    publish_status(app);
    Some(if text.is_empty() { po_i18n::t!("common.done").to_owned() } else { text.join("\n") })
}

/// A menu's choice during a turn: a command that can run at once does, the rest as before.
async fn turn_menu_chosen_now(app: &mut App, host: &mut impl Host, pause: &AtomicBool, mouse: &mut MouseCapture) {
    if let Some(command) = app.pending_command.clone()
        && (app.pending_plain || turn_menu(app, &command).is_none())
        && command_now(app, host, &command).await.is_some()
    {
        app.pending_command = None;
        app.pending_plain = false;
        app.pending_then = None;
        return;
    }
    turn_menu_chosen(app, pause, mouse);
}

/// Drives a turn (or its resumption) to its end, keeping the interface alive: rendering,
/// approval requests, questions, subagents, keyboard. Returns its outcome, and whether it was
/// interrupted.
#[allow(clippy::too_many_arguments)]
async fn drive_turn(
    app: &mut App,
    host: &mut impl Host,
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
    labs_run: &mut Option<LabsRun>,
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
                watchdog::beat();
                publish_status(app);
                app.labs_notebook();
                // A long turn of Po Labs keeps its lease: otherwise another session would see it
                // free while it works.
                labs_presence(app, labs_run.is_some());
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
            signal = labs_next(labs_run) => {
                labs_signal(app, labs_run, signal);
                if last_draw.elapsed() >= std::time::Duration::from_millis(16) {
                    backend.draw(app, terminal)?;
                    last_draw = std::time::Instant::now();
                }
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
                        let shown = app.transcript.len();
                        if let Some(text) = command_now(app, host, &command).await {
                            (text, false)
                        } else if tui_command(app, &command, &mut || cache.clone(), mouse) {
                            (interface_report(app, &command, shown), false)
                        } else {
                            defer_command(app, command, true, pause);
                            (po_i18n::t!("tui.control.noted").to_owned(), false)
                        }
                    }
                    // Written while a question waits (set aside on the phone): the turn is held
                    // by that question, so the message goes to it, and the model judges whether
                    // it answers or is about something else.
                    control::Action::Prompt(text) if !text.starts_with(['!', '/']) && app.pending_question.is_some() => {
                        if let Some(pending) = app.pending_question.take() {
                            pending.request.answer(po_agent::question::Answer::Aside(text.trim().to_owned()));
                        }
                        (po_i18n::t!("tui.control.prompt_sent").to_owned(), false)
                    }
                    // As if typed: it reaches the model at the next safe point, after the tools of
                    // the step in progress.
                    control::Action::Prompt(text) => {
                        if !text.starts_with('!') {
                            pause.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        app.toast(po_i18n::t!("tui.toast.queued", text = text));
                        app.queued.push_back(text);
                        publish_status(app);
                        (po_i18n::t!("tui.control.queued_prompt").to_owned(), false)
                    }
                    control::Action::Restart => {
                        app.restart = true;
                        (po_i18n::t!("tui.control.restart_later").to_owned(), false)
                    }
                    control::Action::Input(po_wire::Input::Interrupt) => {
                        interrupted = true;
                        (po_i18n::t!("tui.control.interrupted").to_owned(), false)
                    }
                    // As the pause key: the agent stops after the tool in progress and keeps its turn.
                    control::Action::Input(po_wire::Input::Pause) => {
                        pause.store(true, std::sync::atomic::Ordering::Relaxed);
                        app.user_pause = true;
                        app.toast(po_i18n::t!("tui.labs.pausing"));
                        publish_status(app);
                        (po_i18n::t!("tui.labs.pausing").to_owned(), false)
                    }
                    // Changed its mind before the safe point: the turn goes on.
                    control::Action::Input(po_wire::Input::Resume) => {
                        app.user_pause = false;
                        publish_status(app);
                        (po_i18n::t!("tui.labs.resumed").to_owned(), false)
                    }
                    control::Action::Input(input) => remote_answer(app, &input),
                    control::Action::Menu(line) => (menu_json(turn_menu(app, &line).as_ref()), false),
                    control::Action::Commands => (commands_json(app), false),
                    control::Action::Close => (po_i18n::t!("tui.control.terminal_session").to_owned(), true),
                    other => control_read(app, backend, &other),
                };
                let _ = request.reply.send(answer);
                backend.draw(app, terminal)?;
            }
            maybe_event = events.next() => {
                if let Some(Ok(Event::Paste(text))) = &maybe_event
                    && app.pending_approval.is_none()
                    && app.menu.is_none()
                {
                    app.input.insert_str(text);
                    backend.draw(app, terminal)?;
                }
                if let Some(Ok(Event::Mouse(event))) = &maybe_event
                    && app.menu.is_some()
                {
                    match event.kind {
                        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            if let Some(index) = backend.overlay_row(event.column, event.row) {
                                menu_choose(app, Some(index));
                                turn_menu_chosen_now(app, host, pause, mouse).await;
                            }
                        }
                        crossterm::event::MouseEventKind::ScrollUp => app.menu.as_mut().into_iter().for_each(|m| m.move_by(-1)),
                        crossterm::event::MouseEventKind::ScrollDown => app.menu.as_mut().into_iter().for_each(|m| m.move_by(1)),
                        _ => {}
                    }
                    backend.draw(app, terminal)?;
                    continue;
                }
                if let Some(Ok(Event::Mouse(event))) = &maybe_event {
                    let max = backend.card_max_scroll().unwrap_or(backend.max_scroll());
                    if let MouseEventKind::Down(MouseButton::Left) = event.kind
                        && let Some(url) = backend.link_at(event.column, event.row)
                    {
                        open_link(app, &url);
                        backend.draw(app, terminal)?;
                        continue;
                    }
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
                    // A command's menu also opens during a turn: its choice is noted like a typed
                    // command, and applied at the next safe point.
                    if app.menu.is_some() {
                        menu_key(app, key);
                        turn_menu_chosen_now(app, host, pause, mouse).await;
                        backend.draw(app, terminal)?;
                        continue;
                    }
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
                    // Pause: the agent stops after the tool in progress and keeps its turn.
                    if key.kind != KeyEventKind::Release && labs::shortcut("pause").matches(&key) {
                        pause.store(true, std::sync::atomic::Ordering::Relaxed);
                        app.user_pause = true;
                        app.toast(po_i18n::t!("tui.labs.pausing"));
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
                            if let Some(menu) = turn_menu(app, &text).filter(|_| !settings::opens_panel(&text)) {
                                app.menu = Some(menu);
                            } else if command_now(app, host, &text).await.is_none() {
                                let cache = app.tabs_cache.clone();
                                if !tui_command(app, &text, &mut || cache.clone(), mouse) {
                                    defer_command(app, text, true, pause);
                                }
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
    messages.push(po_core::Message::assistant("[Interrupted by the user before answering.]"));
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
        // everywhere the user works inside a multiplexer - that is, on most remote sessions.
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

    #[tokio::test]
    async fn a_local_model_is_seen_loaded_unloaded_or_gone() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        async fn server(body: &'static str) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 512];
                let _ = socket.read(&mut request).await;
                let _ = socket.write_all(format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{body}").as_bytes()).await;
            });
            format!("http://{address}/v1")
        }
        assert!(!local_model_loaded(&server(r#"{"service":"strata","loaded":false}"#).await).await);
        assert!(local_model_loaded(&server(r#"{"service":"strata","loaded":true}"#).await).await);
        // Another server that answers: taken as loaded.
        assert!(local_model_loaded(&server("not json").await).await);
        // Nobody listens: the model is not in memory.
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        assert!(!local_model_loaded(&format!("http://{free}/v1")).await);
    }

    #[test]
    fn a_command_s_progress_line_leaves_with_it_and_another_message_stays() {
        let mut app = App::new();
        app.toast("The model is writing the profile…");
        clear_progress_toast(&mut app, Some("The model is writing the profile…"));
        assert_eq!(app.current_toast(), None);
        app.toast("Profile saved");
        clear_progress_toast(&mut app, Some("The model is writing the profile…"));
        assert_eq!(app.current_toast(), Some("Profile saved"));
        clear_progress_toast(&mut app, None);
        assert_eq!(app.current_toast(), Some("Profile saved"));
    }

    #[test]
    fn the_labs_proposal_shows_when_idle_and_stays_aside_until_the_notebook_changes() {
        let mut app = App::new();
        app.labs = true;
        app.labs_book = labs::Notebook::default();
        app.labs_dismissed = u64::MAX;
        assert_eq!(app.labs_proposal(), None, "nothing waiting");
        app.labs_book.add("make tool calls orange", "improvement", false);
        assert_eq!(app.labs_proposal(), Some(LabsAsk::Requests(1)));
        app.status.busy = true;
        assert_eq!(app.labs_proposal(), None, "never while Po works");
        app.status.busy = false;
        app.labs_dismissed = app.labs_book.revision;
        assert_eq!(app.labs_proposal(), None, "set aside by a message or « later »");
        app.labs_book.add("show the branch", "idea", false);
        assert_eq!(app.labs_proposal(), Some(LabsAsk::Requests(2)), "back when the notebook changes");
        app.labs_front = true;
        assert_eq!(app.labs_proposal(), None, "not inside the Po Labs session");
        app.labs = false;
        app.labs_front = false;
        assert!(!app.labs_card_shown());
    }

    #[test]
    fn the_change_on_trial_is_asked_about_once_seen_and_holds_the_next_request() {
        let mut app = App::new();
        app.labs = true;
        app.labs_book = labs::Notebook::default();
        let a = app.labs_book.add("make tool calls orange", "improvement", false);
        app.labs_book.add("show the branch", "idea", false);
        app.labs_book.start_trial(&[a], "tool calls are orange", "b", "h", labs::Visible::ToolCalls, None);
        assert!(app.labs_card_shown());
        assert_eq!(app.labs_proposal(), None, "not asked before a tool call could show it");
        app.labs_seen = (1, 0);
        assert_eq!(app.labs_proposal(), None, "a turn without tools does not show it");
        app.labs_seen = (1, 2);
        assert_eq!(app.labs_proposal(), Some(LabsAsk::Verdict));
        assert!(!labs_has_work(&app), "the next request waits for the verdict");
        // A message does not set the question aside; « later » does, for two turns.
        app.labs_dismissed = app.labs_book.revision;
        assert_eq!(app.labs_proposal(), Some(LabsAsk::Verdict));
        app.labs_verdict_later = Some(1);
        assert_eq!(app.labs_proposal(), None);
        app.labs_seen = (3, 2);
        assert_eq!(app.labs_proposal(), Some(LabsAsk::Verdict));
        // A remark to take up: Po Labs has work again, on the same change.
        app.labs_book.revise("a lighter orange");
        app.labs_dismissed = 0;
        assert_eq!(app.labs_proposal(), Some(LabsAsk::Adjust));
        assert!(labs_has_work(&app));
    }

    #[test]
    fn the_verdict_keys_follow_the_interface_language() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        assert_eq!(labs::Verdict::from_key('o'), Some(labs::Verdict::Keep));
        assert_eq!(labs::Verdict::from_key('N'), Some(labs::Verdict::Undo));
        assert_eq!(labs::Verdict::from_key('m'), Some(labs::Verdict::Revise));
        assert_eq!(labs::Verdict::from_key('y'), None);
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
    fn during_a_turn_a_menu_opens_from_those_prepared_and_its_choice_waits_for_the_safe_point() {
        use menu::{Item, Menu};
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.menus_cache = vec![
            ("/reflexion".into(), Menu::new("r", vec![Item::run("high", "/reflexion high"), Item::run("budget", "/reflexion budget")])),
            ("/reflexion budget".into(), Menu::new("b", vec![Item::run("2048", "/reflexion budget 2048")])),
        ];
        assert!(turn_menu(&app, "  /reflexion ").is_some(), "spaces do not matter");
        assert!(turn_menu(&app, "/contexte").is_none());
        let pause = AtomicBool::new(false);
        let mut mouse = MouseCapture(false);

        // A choice that needs another one opens the next menu.
        app.menu = turn_menu(&app, "/reflexion");
        menu_choose(&mut app, Some(1));
        turn_menu_chosen(&mut app, &pause, &mut mouse);
        assert_eq!(app.menu.as_ref().map(|m| m.title.as_str()), Some("b"));

        // The last one is noted for the turn's next safe point.
        menu_choose(&mut app, Some(0));
        turn_menu_chosen(&mut app, &pause, &mut mouse);
        assert!(app.menu.is_none());
        assert_eq!(app.deferred.last().map(|(c, _)| c.as_str()), Some("/reflexion budget 2048"));
        assert!(pause.load(std::sync::atomic::Ordering::Relaxed));

        // A choice that only applies between two turns waits for the turn's end.
        app.menu = Some(Menu::new("s", vec![Item::run("load", "/serveur charger")]));
        menu_choose(&mut app, Some(0));
        turn_menu_chosen(&mut app, &pause, &mut mouse);
        assert_eq!(app.queued.back().map(String::as_str), Some("/serveur charger"));
        assert!(!app.deferred.iter().any(|(c, _)| c.starts_with("/serveur")));
    }

    #[test]
    fn a_menu_letter_chooses_esc_goes_back_and_an_action_reopens_its_screen() {
        use crate::menu::{Item, Menu};
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut app = App::new();
        let screen = || Menu::new("s", vec![Item::close("quit"), Item::run("yes", "/rc install").key('o').then("/rc"), Item::run("no", "/rc menu").key('n')]).back("/rc menu");
        // A letter of its own: chosen at once, the screen to come back to remembered.
        app.menu = Some(screen());
        menu_key(&mut app, key(KeyCode::Char('O')));
        assert!(app.menu.is_none());
        assert_eq!(app.pending_command.as_deref(), Some("/rc install"));
        assert_eq!(app.pending_then.as_deref(), Some("/rc"));
        // Esc goes back to the screen above; Ctrl+C closes everything.
        app.pending_command = None;
        app.menu = Some(screen());
        menu_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.pending_command.as_deref(), Some("/rc menu"));
        app.pending_command = None;
        app.menu = Some(screen());
        menu_key(&mut app, KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.menu.is_none() && app.pending_command.is_none());
        // Enter on "quit": closed, nothing run.
        app.menu = Some(screen());
        menu_key(&mut app, key(KeyCode::Enter));
        assert!(app.menu.is_none() && app.pending_command.is_none());
        // j and k still move where no item takes them.
        app.menu = Some(screen());
        menu_key(&mut app, key(KeyCode::Char('j')));
        assert_eq!(app.menu.as_ref().unwrap().selected(), 1);
    }

    #[test]
    fn settings_noted_during_a_turn_keep_each_other_but_a_new_value_replaces_the_old() {
        let mut app = App::new();
        let pause = AtomicBool::new(false);
        for command in ["/modele qwen", "/reflexion budget 2048", "/mode plan", "/reflexion high", "/criteres ajouter a", "/criteres ajouter b", "/mode build"] {
            defer_command(&mut app, command.into(), true, &pause);
        }
        let noted: Vec<&str> = app.deferred.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(noted, ["/modele qwen", "/reflexion budget 2048", "/reflexion high", "/criteres ajouter a", "/criteres ajouter b", "/mode build"]);
    }

    #[test]
    fn enter_on_a_command_typed_in_full_sends_it_even_if_a_longer_one_starts_alike() {
        let mut app = App::new();
        app.commands = vec![("/modeles".into(), "list".into()), ("/mode".into(), "budget".into())];
        app.input.insert_str("/mode");
        assert!(!app.completion_key(key(KeyCode::Enter)), "Enter is not taken: `/mode` is a command");
        assert_eq!(app.input.text(), "/mode");
    }

    #[test]
    fn enter_on_a_name_of_the_other_language_sends_it() {
        let mut app = App::new();
        app.commands = vec![("/compacter".into(), "summarize".into())];
        app.command_names = vec!["/compacter".into(), "/compact".into()];
        app.input.insert_str("/compact");
        assert!(!app.completion_key(key(KeyCode::Enter)), "`/compact` is a command: it goes as typed");
        assert_eq!(app.input.text(), "/compact");
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
    fn a_compacted_session_is_redisplayed_whole_with_its_summary_as_a_note() {
        // Compaction moved the old exchanges to the archive and left a summary in their place:
        // resuming shows them all again, and the summary is not a message of the user's.
        let mut session = po_agent::session::Session::new("system");
        session.archive = vec![po_core::Message::user("first request"), po_core::Message::assistant("first answer")];
        session.messages = vec![
            po_core::Message::user(format!("{}

The user asked for X; it was done.", po_agent::session::SUMMARY_MARKER)),
            po_core::Message::user("second request"),
            po_core::Message::assistant("second answer"),
        ];
        let mut app = App::new();
        app.replay(&session.full_history(), std::path::Path::new("."));
        let lines = app.transcript.lines();
        let users: Vec<&str> = lines.iter().filter(|l| l.kind == LineKind::User).map(|l| l.text.as_str()).collect();
        assert_eq!(users, ["first request", "second request"], "the summary is not shown as typed");
        assert!(lines.iter().any(|l| l.text == "first answer"), "the archived exchange is back");
        assert_eq!(lines.iter().filter(|l| l.kind == LineKind::Notice).count(), 2, "compaction note + resumed note");
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
        app.replay(&[po_core::Message::user("read a.rs"), call, result, po_core::Message::assistant("done")], std::path::Path::new("."));
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
    fn a_cancel_command_is_recognised_while_a_model_loads() {
        for line in ["/interrupt", "/stop", "/annuler", "/arreter", "/arrêter", "/modele cancel", "/model annuler", "/modèle stop", "/model unload", "/modele décharger", "/model decharger"] {
            assert!(cancel_load_wanted(line), "{line}");
        }
        for line in ["/modele qwen", "/model auto", "/moteurs", "/quit", "hello", "/modeles"] {
            assert!(!cancel_load_wanted(line), "{line}");
        }
    }

    #[test]
    fn displayed_reasoning_precedes_the_answer_without_mixing() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.show_thinking = true;
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

    #[test]
    fn a_tool_stopped_with_the_turn_stops_turning() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.absorb(AgentEvent::ToolStarted { name: "bash".into(), summary: "sleep 40".into() });
        stop_running_tools(&mut app);
        let line = &app.transcript.lines()[0];
        assert_eq!(line.kind, LineKind::ToolError);
        assert!(line.text.ends_with("arrêté"), "{}", line.text);
    }

    #[test]
    fn the_second_escape_stops_and_takes_its_hint_away() {
        let mut app = App::new();
        assert!(!escape_pressed(&mut app, true, 0), "the first one asks for confirmation");
        assert!(app.toast.is_some());
        assert!(escape_pressed(&mut app, true, 0));
        assert!(app.toast.is_none(), "no \"Esc again\" left after the stop");
    }

    #[test]
    fn po_labs_working_alongside_shows_in_its_own_card_and_survives_an_interrupted_turn() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let mut app = App::new();
        app.monitor.apply(po_agent::SubagentUpdate::Started {
            id: LABS_VIEW.into(),
            agent: "Po Labs".into(),
            model: "m".into(),
            task: "Rename the greeting".into(),
            background: true,
        });
        app.labs_activity = Some(String::new());
        let mut run = None;
        labs_signal(&mut app, &mut run, LabsSignal::Event(AgentEvent::TextDelta("Reading main.rs.".into())));
        labs_signal(&mut app, &mut run, LabsSignal::Event(AgentEvent::ToolStarted { name: "read".into(), summary: "main.rs".into() }));
        assert_eq!(app.labs_activity.as_deref(), Some("read main.rs"));
        let view = &app.monitor.views[0];
        assert_eq!(view.progress.as_deref(), Some("read main.rs"));
        assert!(view.transcript.lines().iter().any(|l| l.text == "Reading main.rs."));
        // Esc on the user's turn stops its subagents, not Po Labs.
        app.monitor.stop_all();
        assert!(app.monitor.views[0].running());
        assert_eq!(agent_background_hint(&app), 0);
        labs_signal(&mut app, &mut run, LabsSignal::Done(None));
        assert!(app.labs_activity.is_none());
        assert!(!app.monitor.views[0].running());
    }
}
