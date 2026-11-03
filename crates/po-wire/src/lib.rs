//! `po-wire` — what Po's remote control says, shared by the PC (each session, the service) and
//! the phone (`po-mobile`).
//!
//! Three things live here, and nothing that touches the network:
//!
//! - the **events** of a session, modelled on the agent's own (text arriving, a tool started,
//!   an approval asked…), plus what only the interface knew (the user's message, a command's
//!   report, the state of the status bar);
//! - the **conversation** those events build: the same data on both ends, so that the phone
//!   shows what the terminal shows without either side rendering for the other;
//! - the **journal**: events numbered in a ring, so that a phone coming back from a tunnel
//!   resumes from the last event it saw instead of downloading the whole session again.
//!
//! The format is JSON, one message per frame (a 4-byte length, then the text): readable when
//! something is off, and versioned by [`VERSION`].

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

pub mod msg;
pub use msg::*;

/// Version of the protocol. A peer speaking a newer one is told so instead of misread.
pub const VERSION: u32 = 1;

/// A tool's diff beyond this is cut: a phone has no use for a 2 MB rewrite, and a frame stays
/// small enough to go out at once.
pub const MAX_DIFF: usize = 64 * 1024;
/// A tool's output beyond this is cut: enough to read what it gave back on a phone.
pub const MAX_OUTPUT: usize = 16 * 1024;
/// A text beyond this is cut in a snapshot: a tool result pasted in full is not what a phone
/// shows anyway.
pub const MAX_TEXT: usize = 256 * 1024;
/// Items kept in a conversation: older ones fall off, like a terminal's scrollback.
pub const MAX_ITEMS: usize = 3000;

/// What a session lets be seen, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// The conversation was replaced (a new session, a resumed one, a compaction): this is the
    /// whole of it.
    Reset { conversation: Box<Conversation> },
    /// A message of the user, whoever typed it (terminal or phone).
    User { text: String },
    TurnStarted,
    /// The model chosen for the coming call.
    Model { model: String },
    TextDelta { text: String },
    /// The full text of the answer the deltas were building.
    Text { text: String },
    /// Reasoning is arriving; only its size, unless the user asked to see it.
    ThinkingDelta { chars: usize },
    ThinkingText { text: String },
    Thinking { text: String },
    ToolStarted { name: String, summary: String },
    ToolFinished { name: String, summary: String, error: bool },
    ToolDiff { diff: String },
    /// What the tool that just finished gave back.
    ToolOutput { output: String },
    ToolDenied { name: String, summary: String },
    /// The model asked a question and got this answer.
    Asked { question: String, answer: String },
    Delegated { agent: String, model: String, task: String, background: bool },
    Notice { text: String },
    Error { text: String },
    /// A tool waits for the user's go-ahead; answerable from any end.
    Approval { id: u64, tool: String, summary: String },
    ApprovalDone { id: u64, granted: bool },
    Question { id: u64, question: String, options: Vec<QuestionOption> },
    QuestionDone { id: u64 },
    TurnEnded {
        interrupted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Duration, tokens, cost: the line the terminal writes under a turn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// What the status bar shows changed.
    Status { status: Box<Status> },
    /// A `/…` command ran; what it said.
    CommandOutput { command: String, lines: Vec<OutLine> },
    /// What a subagent does (its text comes by whole lines).
    Subagent { update: Box<SubagentEvent> },
    /// The session ended.
    Closed,
}

/// A subagent's step, as the phone follows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SubagentEvent {
    Started { id: String, agent: String, model: String, task: String, background: bool },
    Thinking { id: String, text: String },
    Text { id: String, text: String },
    /// What it says it is doing ("Progress: …"), for its card.
    Progress { id: String, text: String },
    Tool { id: String, name: String, summary: String },
    ToolDone { id: String, name: String, ok: bool },
    Paused { id: String },
    Finished { id: String, ok: bool },
}

impl SubagentEvent {
    pub fn id(&self) -> &str {
        match self {
            Self::Started { id, .. }
            | Self::Thinking { id, .. }
            | Self::Text { id, .. }
            | Self::Progress { id, .. }
            | Self::Tool { id, .. }
            | Self::ToolDone { id, .. }
            | Self::Paused { id }
            | Self::Finished { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    #[default]
    Running,
    Paused,
    Done,
    Failed,
}

/// A subagent of the session, and its own conversation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Subagent {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub task: String,
    pub background: bool,
    pub state: SubagentState,
    pub progress: Option<String>,
    pub items: Vec<Item>,
}

/// Subagents kept in a conversation, and rows kept of each.
pub const MAX_SUBAGENTS: usize = 24;
pub const MAX_SUBAGENT_ITEMS: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// A row of a command's report, with its shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutLine {
    Plain { text: String },
    Warn { text: String },
    Dim { text: String },
    Section { text: String },
    Item { name: String, detail: String },
    Field { key: String, value: String },
}

/// What the status bar of a session says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    pub title: Option<String>,
    pub model: String,
    /// The model pinned with `/model`, if any.
    pub pinned: Option<String>,
    /// Models the session can use, for the phone's picker.
    pub models: Vec<String>,
    /// Plan, build, bypass.
    pub work_mode: String,
    /// Budget mode.
    pub budget: String,
    pub effort: Option<String>,
    pub busy: bool,
    pub context_used: u32,
    pub context_window: u32,
    pub cost: f64,
    /// Messages waiting for the end of the turn.
    pub queued: usize,
    /// Their text, oldest first.
    pub queue: Vec<String>,
    /// The turn is paused, waiting to go on.
    pub paused: bool,
    /// Po Labs is on in this session.
    pub labs: bool,
    /// What serves the model: `strata`, `llama.cpp`, a cloud provider.
    pub engine: String,
    /// Average speeds of the session's calls, tokens per second (0: not measured yet).
    pub generation_tok_s: f32,
    pub prefill_tok_s: f32,
    /// The project folder.
    pub workspace: String,
    /// A screen the session opened by itself and that waits for the user's choice (`/setup` at
    /// the first start): a phone opens it too, once.
    pub offer: Option<String>,
}

/// How a tool call went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Running,
    Done,
    Error,
    Denied,
}

/// A row of the conversation, as both ends show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    User { text: String },
    Assistant { text: String, streaming: bool },
    Thinking { text: String, chars: usize, streaming: bool },
    Tool {
        name: String,
        summary: String,
        state: ToolState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        /// What it gave back.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output: String,
    },
    Asked { question: String, answer: String },
    Delegation { agent: String, model: String, task: String, background: bool },
    Notice { text: String, warn: bool },
    Output { command: String, lines: Vec<OutLine> },
    Summary { text: String },
}

impl Item {
    fn streaming(&self) -> bool {
        matches!(self, Item::Assistant { streaming: true, .. } | Item::Thinking { streaming: true, .. })
    }

    fn finish(&mut self) {
        match self {
            Item::Assistant { streaming, .. } | Item::Thinking { streaming, .. } => *streaming = false,
            Item::Tool { state, .. } if *state == ToolState::Running => *state = ToolState::Done,
            _ => {}
        }
    }
}

/// An approval waiting, as shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: u64,
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingQuestion {
    pub id: u64,
    pub question: String,
    pub options: Vec<QuestionOption>,
}

/// The conversation of a session, built from its events. The same code runs on the PC (to give
/// a phone its snapshot) and on the phone (to show it): they cannot drift apart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Conversation {
    pub items: Vec<Item>,
    pub status: Status,
    pub busy: bool,
    pub approval: Option<PendingApproval>,
    pub question: Option<PendingQuestion>,
    pub closed: bool,
    pub subagents: Vec<Subagent>,
    /// The subagents changed since [`Conversation::take_subagents_changed`]. Not sent.
    #[serde(skip)]
    subagents_dirty: bool,
    /// First item changed since [`Conversation::take_changes`]: a phone redraws from there
    /// instead of the whole list. Not sent.
    #[serde(skip)]
    dirty_from: Option<usize>,
}

/// Cuts a text on a character boundary, saying it was cut.
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

impl Conversation {
    pub fn new(items: Vec<Item>) -> Self {
        let mut conversation = Self { items, ..Self::default() };
        conversation.trim();
        conversation.dirty_from = Some(0);
        conversation
    }

    fn mark(&mut self, index: usize) {
        self.dirty_from = Some(self.dirty_from.map_or(index, |d| d.min(index)));
    }

    fn push(&mut self, item: Item) {
        self.items.push(item);
        let last = self.items.len() - 1;
        self.mark(last);
        self.trim();
    }

    fn trim(&mut self) {
        if self.items.len() > MAX_ITEMS {
            let extra = self.items.len() - MAX_ITEMS;
            self.items.drain(..extra);
            // What a phone had is shifted: it redraws everything.
            self.dirty_from = Some(0);
        }
    }

    /// Where the current turn starts: the last user message (items before it are history).
    fn turn_start(&self) -> usize {
        self.items.iter().rposition(|i| matches!(i, Item::User { .. })).unwrap_or(0)
    }

    /// The last item of the turn matching `pred`.
    fn find_in_turn(&self, pred: impl Fn(&Item) -> bool) -> Option<usize> {
        let start = self.turn_start();
        (start..self.items.len()).rev().find(|&i| pred(&self.items[i]))
    }

    /// Items changed since the last call: from which index, and that end of the list. `None`
    /// when nothing changed.
    pub fn take_changes(&mut self) -> Option<usize> {
        self.dirty_from.take().map(|d| d.min(self.items.len()))
    }

    /// Did the subagents change since the last call?
    pub fn take_subagents_changed(&mut self) -> bool {
        std::mem::take(&mut self.subagents_dirty)
    }

    fn subagent(&mut self, update: &SubagentEvent) {
        use SubagentEvent as S;
        self.subagents_dirty = true;
        if let S::Started { id, agent, model, task, background } = update {
            // A paused subagent resumes in its own card, its conversation kept.
            if let Some(found) = self.subagents.iter_mut().find(|s| &s.id == id)
                && found.state == SubagentState::Paused
            {
                found.state = SubagentState::Running;
                return;
            }
            self.subagents.retain(|s| &s.id != id);
            let brief = Item::Delegation { agent: agent.clone(), model: model.clone(), task: task.clone(), background: *background };
            self.subagents.push(Subagent {
                id: id.clone(),
                agent: agent.clone(),
                model: model.clone(),
                task: task.clone(),
                background: *background,
                state: SubagentState::Running,
                progress: None,
                items: vec![brief],
            });
            if self.subagents.len() > MAX_SUBAGENTS {
                // The oldest finished one goes first; else simply the oldest.
                let at = self.subagents.iter().position(|s| matches!(s.state, SubagentState::Done | SubagentState::Failed)).unwrap_or(0);
                self.subagents.remove(at);
            }
            return;
        }
        let Some(sub) = self.subagents.iter_mut().find(|s| s.id == update.id()) else { return };
        let last_streaming = |items: &[Item], thinking: bool| -> Option<usize> {
            let last = items.len().checked_sub(1)?;
            let hit = if thinking {
                matches!(items[last], Item::Thinking { streaming: true, .. })
            } else {
                matches!(items[last], Item::Assistant { streaming: true, .. })
            };
            hit.then_some(last)
        };
        fn end_streams(items: &mut [Item]) {
            for item in items.iter_mut().filter(|i| i.streaming()) {
                item.finish();
            }
        }
        match update {
            S::Thinking { text, .. } => match last_streaming(&sub.items, true) {
                Some(i) => {
                    if let Item::Thinking { text: t, chars, .. } = &mut sub.items[i] {
                        t.push_str(text);
                        *chars += text.len();
                    }
                }
                None => {
                    end_streams(&mut sub.items);
                    sub.items.push(Item::Thinking { text: text.clone(), chars: text.len(), streaming: true });
                }
            },
            S::Text { text, .. } => match last_streaming(&sub.items, false) {
                Some(i) => {
                    if let Item::Assistant { text: t, .. } = &mut sub.items[i] {
                        t.push_str(text);
                    }
                }
                None => {
                    end_streams(&mut sub.items);
                    sub.items.push(Item::Assistant { text: text.clone(), streaming: true });
                }
            },
            S::Progress { text, .. } => sub.progress = Some(text.clone()),
            S::Tool { name, summary, .. } => {
                end_streams(&mut sub.items);
                sub.items.push(Item::Tool { name: name.clone(), summary: summary.clone(), state: ToolState::Running, diff: None, output: String::new() });
            }
            S::ToolDone { name, ok, .. } => {
                if let Some(Item::Tool { state, .. }) =
                    sub.items.iter_mut().rev().find(|i| matches!(i, Item::Tool { name: n, state: ToolState::Running, .. } if n == name))
                {
                    *state = if *ok { ToolState::Done } else { ToolState::Error };
                }
            }
            S::Paused { .. } => {
                end_streams(&mut sub.items);
                sub.state = SubagentState::Paused;
            }
            S::Finished { ok, .. } => {
                end_streams(&mut sub.items);
                for item in &mut sub.items {
                    if let Item::Tool { state, .. } = item
                        && *state == ToolState::Running
                    {
                        *state = if *ok { ToolState::Done } else { ToolState::Error };
                    }
                }
                sub.state = if *ok { SubagentState::Done } else { SubagentState::Failed };
            }
            S::Started { .. } => {}
        }
        if sub.items.len() > MAX_SUBAGENT_ITEMS {
            let extra = sub.items.len() - MAX_SUBAGENT_ITEMS;
            // The brief stays at the top.
            sub.items.drain(1..=extra);
        }
    }

    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Reset { conversation } => {
                *self = (**conversation).clone();
                self.dirty_from = Some(0);
                self.subagents_dirty = true;
            }
            Event::User { text } => self.push(Item::User { text: text.clone() }),
            Event::TurnStarted => self.busy = true,
            Event::Model { model } => self.status.model = model.clone(),
            Event::TextDelta { text } => match self.find_in_turn(|i| matches!(i, Item::Assistant { streaming: true, .. })) {
                Some(i) if i + 1 == self.items.len() => {
                    if let Item::Assistant { text: t, .. } = &mut self.items[i] {
                        t.push_str(text);
                    }
                    self.mark(i);
                }
                _ => self.push(Item::Assistant { text: text.clone(), streaming: true }),
            },
            Event::Text { text } => match self.find_in_turn(|i| matches!(i, Item::Assistant { streaming: true, .. })) {
                Some(i) => {
                    self.items[i] = Item::Assistant { text: text.clone(), streaming: false };
                    self.mark(i);
                }
                None if text.trim().is_empty() => {}
                None => self.push(Item::Assistant { text: text.clone(), streaming: false }),
            },
            Event::ThinkingDelta { chars } => match self.find_in_turn(|i| matches!(i, Item::Thinking { streaming: true, .. })) {
                Some(i) => {
                    if let Item::Thinking { chars: c, .. } = &mut self.items[i] {
                        *c += chars;
                    }
                    self.mark(i);
                }
                None => self.push(Item::Thinking { text: String::new(), chars: *chars, streaming: true }),
            },
            Event::ThinkingText { text } => match self.find_in_turn(|i| matches!(i, Item::Thinking { streaming: true, .. })) {
                Some(i) => {
                    if let Item::Thinking { text: t, .. } = &mut self.items[i] {
                        t.push_str(text);
                    }
                    self.mark(i);
                }
                None => self.push(Item::Thinking { text: text.clone(), chars: text.len(), streaming: true }),
            },
            Event::Thinking { text } => match self.find_in_turn(|i| matches!(i, Item::Thinking { streaming: true, .. })) {
                Some(i) => {
                    let chars = match &self.items[i] {
                        Item::Thinking { chars, .. } => (*chars).max(text.len()),
                        _ => text.len(),
                    };
                    self.items[i] = Item::Thinking { text: text.clone(), chars, streaming: false };
                    self.mark(i);
                }
                None if text.trim().is_empty() => {}
                None => self.push(Item::Thinking { text: text.clone(), chars: text.len(), streaming: false }),
            },
            Event::ToolStarted { name, summary } => {
                // A tool starts once the model is done writing this step.
                self.finish_streaming();
                self.push(Item::Tool { name: name.clone(), summary: summary.clone(), state: ToolState::Running, diff: None, output: String::new() });
            }
            Event::ToolFinished { name, summary, error } => {
                let state = if *error { ToolState::Error } else { ToolState::Done };
                match self.find_in_turn(|i| matches!(i, Item::Tool { name: n, state: ToolState::Running, .. } if n == name)) {
                    Some(i) => {
                        if let Item::Tool { summary: s, state: st, .. } = &mut self.items[i] {
                            if !summary.is_empty() {
                                *s = summary.clone();
                            }
                            *st = state;
                        }
                        self.mark(i);
                    }
                    None => self.push(Item::Tool { name: name.clone(), summary: summary.clone(), state, diff: None, output: String::new() }),
                }
            }
            Event::ToolOutput { output } => {
                if let Some(i) = self.find_in_turn(|i| matches!(i, Item::Tool { state: ToolState::Done | ToolState::Error, .. })) {
                    if let Item::Tool { output: o, .. } = &mut self.items[i] {
                        *o = clip(output, MAX_OUTPUT);
                    }
                    self.mark(i);
                }
            }
            Event::ToolDiff { diff } => {
                if let Some(i) = self.find_in_turn(|i| matches!(i, Item::Tool { state: ToolState::Done | ToolState::Error, .. })) {
                    if let Item::Tool { diff: d, .. } = &mut self.items[i] {
                        *d = Some(clip(diff, MAX_DIFF));
                    }
                    self.mark(i);
                }
            }
            Event::ToolDenied { name, summary } => {
                match self.find_in_turn(|i| matches!(i, Item::Tool { name: n, state: ToolState::Running, .. } if n == name)) {
                    Some(i) => {
                        if let Item::Tool { state, .. } = &mut self.items[i] {
                            *state = ToolState::Denied;
                        }
                        self.mark(i);
                    }
                    None => self.push(Item::Tool { name: name.clone(), summary: summary.clone(), state: ToolState::Denied, diff: None, output: String::new() }),
                }
            }
            Event::Asked { question, answer } => self.push(Item::Asked { question: question.clone(), answer: answer.clone() }),
            Event::Delegated { agent, model, task, background } => self.push(Item::Delegation {
                agent: agent.clone(),
                model: model.clone(),
                task: task.clone(),
                background: *background,
            }),
            Event::Notice { text } => self.push(Item::Notice { text: text.clone(), warn: false }),
            Event::Error { text } => self.push(Item::Notice { text: text.clone(), warn: true }),
            Event::Approval { id, tool, summary } => {
                self.approval = Some(PendingApproval { id: *id, tool: tool.clone(), summary: summary.clone() })
            }
            Event::ApprovalDone { id, .. } => {
                if self.approval.as_ref().is_some_and(|a| a.id == *id) {
                    self.approval = None;
                }
            }
            Event::Question { id, question, options } => {
                self.question = Some(PendingQuestion { id: *id, question: question.clone(), options: options.clone() })
            }
            Event::QuestionDone { id } => {
                if self.question.as_ref().is_some_and(|q| q.id == *id) {
                    self.question = None;
                }
            }
            Event::TurnEnded { error, summary, .. } => {
                self.busy = false;
                self.finish_streaming();
                let start = self.turn_start();
                for i in start..self.items.len() {
                    if matches!(self.items[i], Item::Tool { state: ToolState::Running, .. }) {
                        if error.is_some() {
                            if let Item::Tool { state, .. } = &mut self.items[i] {
                                *state = ToolState::Error;
                            }
                        } else {
                            self.items[i].finish();
                        }
                        self.mark(i);
                    }
                }
                self.approval = None;
                self.question = None;
                if let Some(error) = error {
                    self.push(Item::Notice { text: error.clone(), warn: true });
                }
                if let Some(summary) = summary {
                    self.push(Item::Summary { text: summary.clone() });
                }
            }
            Event::Status { status } => self.status = (**status).clone(),
            Event::CommandOutput { command, lines } => self.push(Item::Output { command: command.clone(), lines: lines.clone() }),
            Event::Subagent { update } => self.subagent(update),
            Event::Closed => {
                self.closed = true;
                self.busy = false;
                self.approval = None;
                self.question = None;
            }
        }
    }

    fn finish_streaming(&mut self) {
        let start = self.turn_start();
        for i in start..self.items.len() {
            if self.items[i].streaming() {
                self.items[i].finish();
                self.mark(i);
            }
        }
    }

    /// The conversation as sent in a snapshot: long texts cut.
    pub fn for_snapshot(&self) -> Conversation {
        let mut out = self.clone();
        for item in &mut out.items {
            match item {
                Item::User { text } | Item::Assistant { text, .. } | Item::Thinking { text, .. } => *text = clip(text, MAX_TEXT),
                Item::Tool { diff, output, .. } => {
                    if let Some(diff) = diff {
                        *diff = clip(diff, MAX_DIFF);
                    }
                    *output = clip(output, MAX_OUTPUT);
                }
                _ => {}
            }
        }
        for sub in &mut out.subagents {
            for item in &mut sub.items {
                if let Item::Assistant { text, .. } | Item::Thinking { text, .. } = item {
                    *text = clip(text, MAX_TEXT);
                }
            }
        }
        out.dirty_from = None;
        out
    }
}

/// Events numbered in a ring, with the conversation they built: what a session keeps for the
/// phones that follow it.
///
/// `epoch` names this run of the session: numbers restart with a new process, and a phone
/// holding number 812 of the previous run must not take number 812 of this one for the same.
#[derive(Debug, Clone)]
pub struct Journal {
    pub epoch: String,
    /// Number of the next event.
    next: u64,
    events: VecDeque<(u64, Event)>,
    capacity: usize,
    conversation: Conversation,
}

/// What a phone gets when it (re)subscribes.
#[derive(Debug, Clone, PartialEq)]
pub enum Catchup {
    /// The events it missed, in order (possibly none).
    Events(Vec<(u64, Event)>),
    /// Too far behind, or another run: the whole conversation, as of event `seq`.
    Snapshot { seq: u64, conversation: Box<Conversation> },
}

impl Journal {
    /// Default ring: a long turn of a fast model sends a few thousand fragments; a phone in a
    /// tunnel for a minute must still find its place.
    pub const CAPACITY: usize = 20_000;

    pub fn new(epoch: impl Into<String>, capacity: usize) -> Self {
        Self { epoch: epoch.into(), next: 1, events: VecDeque::new(), capacity: capacity.max(1), conversation: Conversation::default() }
    }

    /// Appends an event; its number.
    pub fn push(&mut self, event: Event) -> u64 {
        self.conversation.apply(&event);
        let seq = self.next;
        self.next += 1;
        // A reset makes everything before it useless to replay: dropping it keeps the ring for
        // what follows.
        if matches!(event, Event::Reset { .. }) {
            self.events.clear();
        }
        self.events.push_back((seq, event));
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
        seq
    }

    /// Number of the last event, 0 before any.
    pub fn last(&self) -> u64 {
        self.next - 1
    }

    pub fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// What a phone that last saw event `seq` of run `epoch` needs.
    pub fn since(&self, epoch: Option<&str>, seq: u64) -> Catchup {
        let snapshot = || Catchup::Snapshot { seq: self.last(), conversation: Box::new(self.conversation.for_snapshot()) };
        if epoch != Some(self.epoch.as_str()) || seq > self.last() {
            return snapshot();
        }
        if seq == self.last() {
            return Catchup::Events(Vec::new());
        }
        match self.events.front() {
            // Everything after `seq` is still in the ring.
            Some((first, _)) if *first <= seq + 1 => {
                Catchup::Events(self.events.iter().filter(|(n, _)| *n > seq).cloned().collect())
            }
            _ => snapshot(),
        }
    }
}

/// A frame: 4 bytes of length (big-endian), then the JSON text.
pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    let body = serde_json::to_vec(message).unwrap_or_default();
    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Beyond this, a frame is refused: nothing legitimate comes close (a snapshot is bounded by
/// [`MAX_ITEMS`] and the clipping), and a hostile length must not make us allocate gigabytes.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    TooLarge(usize),
    Invalid(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge(n) => write!(f, "frame of {n} bytes refused"),
            FrameError::Invalid(e) => write!(f, "invalid frame: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Reads frames out of bytes as they arrive, whatever the cuts.
#[derive(Debug, Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// The next whole frame's body, if one is there.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        if self.buffer.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buffer[0], self.buffer[1], self.buffer[2], self.buffer[3]]) as usize;
        if len > MAX_FRAME {
            return Err(FrameError::TooLarge(len));
        }
        if self.buffer.len() < 4 + len {
            return Ok(None);
        }
        let body = self.buffer[4..4 + len].to_vec();
        self.buffer.drain(..4 + len);
        Ok(Some(body))
    }

    /// The next whole frame, read as `T`.
    pub fn next_message<T: for<'de> Deserialize<'de>>(&mut self) -> Result<Option<T>, FrameError> {
        match self.next_frame()? {
            Some(body) => serde_json::from_slice(&body).map(Some).map_err(|e| FrameError::Invalid(e.to_string())),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
