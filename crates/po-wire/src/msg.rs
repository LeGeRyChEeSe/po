//! The messages between a phone and a PC.
//!
//! A connection carries one **control stream** (requests from the phone, each answered once,
//! matched by `rid`) and one **feed stream** per session followed (the PC sends the snapshot or
//! the missed events, then the events as they come).

use serde::{Deserialize, Serialize};

use crate::{Conversation, Event};

/// What the phone sends to act on a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// A message, as typed in the terminal.
    Prompt { text: String },
    /// A `/…` command.
    Command { text: String },
    Approve { id: u64, granted: bool },
    /// The answer to a model's question: an option, a free text, or neither (declined).
    Answer {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// Stops the turn in progress.
    Interrupt,
    /// Pauses the turn at its next safe point (after the tools of the step in progress): the
    /// agent keeps its turn, to go on with [`Input::Resume`].
    Pause,
    Resume,
    /// Takes a waiting message out of the queue (`index` counts from the oldest).
    Unqueue { index: u32 },
    /// Replaces the text of a waiting message.
    Requeue { index: u32, text: String },
}

/// A command's choices, as the terminal's arrow menu offers them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Menu {
    pub title: String,
    pub note: Option<String>,
    pub items: Vec<MenuItem>,
    /// The command line of the menu to go back to, if any.
    pub back: Option<String>,
    /// Built again while it is open: what it shows changes by itself (asked again every second).
    pub live: bool,
    /// Something is under way (a spinner turns).
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuAction {
    /// Runs `command` (a further menu may follow).
    #[default]
    Run,
    /// Runs `command` as it is, to see what it shows.
    Show,
    /// Puts `command` in the input, for the user to finish it.
    Fill,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuItem {
    pub label: String,
    pub detail: String,
    pub action: MenuAction,
    pub command: String,
    pub current: bool,
    /// The menu opened again once the command ran: the user stays where they were.
    pub then: Option<String>,
}

/// A command offered to completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
}

/// A folder where Po has worked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectInfo {
    pub path: String,
    /// The folder's own name, to show.
    pub name: String,
    /// Saved sessions there.
    pub saved: u32,
    /// Open sessions there (terminal or started from a phone).
    pub live: u32,
    /// Last activity, ISO 8601.
    pub last: String,
}

/// A session, open or saved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionInfo {
    /// What to follow it by: an open session's key, else `saved:<id>`.
    pub key: String,
    pub live: bool,
    /// Opened from a phone, without a terminal.
    pub headless: bool,
    pub saved_id: Option<String>,
    pub title: String,
    pub project: String,
    pub model: String,
    pub busy: bool,
    /// A model is being loaded for it on the PC: `busy` is false then (no turn has started yet),
    /// so a list needs this to say why the session waits.
    pub loading: bool,
    /// Last activity, ISO 8601.
    pub updated: String,
    pub exchanges: u32,
}

/// Requests of the phone on the control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// First request of a connection.
    Hello { v: u32, name: String, app: String },
    Projects,
    /// Sessions, of one project or all.
    Sessions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Acts on a session. `id` numbers the phone's inputs: one resent after a reconnection is
    /// recognised and not carried out twice.
    Input { session: String, id: u64, input: Input },
    /// The menu of a command line, if it needs a choice.
    Menu { session: String, line: String },
    Commands { session: String },
    /// Opens a session in `project`, without a terminal, with the model last used there.
    NewSession {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Opens a saved session again (or finds it already open).
    Resume { saved: String },
    /// Ends a session opened without a terminal.
    Close { session: String },
    /// A file for the session's folder (a photo, a document): base64, at most [`MAX_UPLOAD`]
    /// bytes once decoded. The answer is the `@path` to mention it with.
    Upload { session: String, name: String, data: String },
    /// Speech to text on the PC, by its local whisper: a 16 kHz mono WAV, base64.
    Transcribe { audio: String },
    /// Forgets a saved session: it goes to the PC's bin of sessions, it is not erased.
    Forget { saved: String },
    /// What the PC is using now: processor, memory, cards, disks.
    Usage,
    Ping { t: u64 },
}

/// What the PC is using now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineUsage {
    pub cpu_percent: f32,
    pub ram_used_mib: u64,
    pub ram_total_mib: u64,
    pub gpus: Vec<GpuUsage>,
    pub disks: Vec<DiskUsage>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuUsage {
    pub name: String,
    pub percent: f32,
    pub vram_used_mib: u64,
    pub vram_total_mib: u64,
    pub temperature_c: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiskUsage {
    pub name: String,
    pub free_gib: f32,
    pub total_gib: f32,
}

/// A file sent from the phone, decoded: beyond this it is refused.
pub const MAX_UPLOAD: usize = 10 * 1024 * 1024;

/// The control stream's frames from the phone: a request and its number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub rid: u64,
    #[serde(flatten)]
    pub body: T,
}

/// Answers of the PC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "re", rename_all = "snake_case")]
pub enum Reply {
    Welcome { v: u32, pc: String, name: String },
    Projects { projects: Vec<ProjectInfo> },
    Sessions { sessions: Vec<SessionInfo> },
    Done { text: String },
    Menu { menu: Option<Menu> },
    Commands { commands: Vec<CommandInfo> },
    Usage { usage: MachineUsage },
    /// The session opened (or found): its key, to follow it.
    Session { key: String },
    Error { message: String },
    Pong { t: u64 },
}

/// First frame of a feed stream, from the phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    pub session: String,
    /// The run and the last event the phone has, if it had followed this session before.
    #[serde(default)]
    pub epoch: Option<String>,
    #[serde(default)]
    pub since: u64,
}

/// Frames of a feed stream, from the PC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "feed", rename_all = "snake_case")]
pub enum Feed {
    /// The whole conversation, as of event `seq` of run `epoch`.
    Snapshot { epoch: String, seq: u64, conversation: Box<Conversation> },
    Event { seq: u64, event: Event },
    /// Nothing more will come: the session is gone, or never was.
    Gone { reason: String },
}
