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
}

/// A command's choices, as the terminal's arrow menu offers them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Menu {
    pub title: String,
    pub note: Option<String>,
    pub items: Vec<MenuItem>,
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
    Ping { t: u64 },
}

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
