//! What crosses to Kotlin and Swift: flat records, easy to draw.

use po_wire::{Conversation, Item, OutLine, ToolState};

#[derive(Debug, Clone, uniffi::Record)]
pub struct SignIn {
    pub url: String,
    pub code: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PcInfo {
    pub id: String,
    pub name: String,
    /// Unix seconds when it last wrote its entry (the service does so every quarter of an hour).
    pub seen: u64,
    /// Its addresses can be read: it has accepted this phone.
    pub reachable: bool,
    pub connected: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PcList {
    pub pcs: Vec<PcInfo>,
    pub ready: bool,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum LinkState {
    Connecting,
    Up { address: String, punched: bool, name: String },
    Down { reason: String },
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum Alert {
    Approval { tool: String, summary: String },
    Question { question: String },
    TurnEnded { title: String, error: Option<String>, last: String },
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ProjectRec {
    pub path: String,
    pub name: String,
    pub saved: u32,
    pub live: u32,
    pub last: String,
}

impl From<po_wire::ProjectInfo> for ProjectRec {
    fn from(p: po_wire::ProjectInfo) -> Self {
        Self { path: p.path, name: p.name, saved: p.saved, live: p.live, last: p.last }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SessionRec {
    pub key: String,
    pub live: bool,
    pub headless: bool,
    pub saved_id: Option<String>,
    pub title: String,
    pub project: String,
    pub model: String,
    pub busy: bool,
    pub updated: String,
    pub exchanges: u32,
}

impl From<po_wire::SessionInfo> for SessionRec {
    fn from(s: po_wire::SessionInfo) -> Self {
        Self {
            key: s.key,
            live: s.live,
            headless: s.headless,
            saved_id: s.saved_id,
            title: s.title,
            project: s.project,
            model: s.model,
            busy: s.busy,
            updated: s.updated,
            exchanges: s.exchanges,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ItemKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Asked,
    Delegation,
    Notice,
    Warning,
    Output,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ToolStatus {
    None,
    Running,
    Done,
    Error,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LineRec {
    /// plain, warn, dim, section, item, field.
    pub kind: String,
    pub text: String,
    pub detail: String,
}

/// A row of the conversation, flattened: what each kind uses of it is said on each field.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ItemRec {
    pub kind: ItemKind,
    /// The text (message, answer, reasoning, notice; a tool's summary; a question).
    pub text: String,
    /// A tool's name, a subagent and its model, the command of a report.
    pub title: String,
    /// A tool's diff, a subagent's task, a question's answer.
    pub detail: String,
    pub status: ToolStatus,
    pub streaming: bool,
    /// Characters of reasoning (shown even when the text is not).
    pub chars: u64,
    pub lines: Vec<LineRec>,
}

impl ItemRec {
    fn new(kind: ItemKind, text: String) -> Self {
        Self { kind, text, title: String::new(), detail: String::new(), status: ToolStatus::None, streaming: false, chars: 0, lines: Vec::new() }
    }
}

impl From<&Item> for ItemRec {
    fn from(item: &Item) -> Self {
        match item {
            Item::User { text } => Self::new(ItemKind::User, text.clone()),
            Item::Assistant { text, streaming } => Self { streaming: *streaming, ..Self::new(ItemKind::Assistant, text.clone()) },
            Item::Thinking { text, chars, streaming } => Self { streaming: *streaming, chars: *chars as u64, ..Self::new(ItemKind::Thinking, text.clone()) },
            Item::Tool { name, summary, state, diff } => Self {
                title: name.clone(),
                detail: diff.clone().unwrap_or_default(),
                status: match state {
                    ToolState::Running => ToolStatus::Running,
                    ToolState::Done => ToolStatus::Done,
                    ToolState::Error => ToolStatus::Error,
                    ToolState::Denied => ToolStatus::Denied,
                },
                streaming: *state == ToolState::Running,
                ..Self::new(ItemKind::Tool, summary.clone())
            },
            Item::Asked { question, answer } => Self { detail: answer.clone(), ..Self::new(ItemKind::Asked, question.clone()) },
            Item::Delegation { agent, model, task, background } => Self {
                title: if model.is_empty() { agent.clone() } else { format!("{agent} · {model}") },
                detail: task.clone(),
                streaming: *background,
                ..Self::new(ItemKind::Delegation, agent.clone())
            },
            Item::Notice { text, warn } => Self::new(if *warn { ItemKind::Warning } else { ItemKind::Notice }, text.clone()),
            Item::Output { command, lines } => Self {
                title: command.clone(),
                lines: lines
                    .iter()
                    .map(|l| {
                        let (kind, text, detail) = match l {
                            OutLine::Plain { text } => ("plain", text.clone(), String::new()),
                            OutLine::Warn { text } => ("warn", text.clone(), String::new()),
                            OutLine::Dim { text } => ("dim", text.clone(), String::new()),
                            OutLine::Section { text } => ("section", text.clone(), String::new()),
                            OutLine::Item { name, detail } => ("item", name.clone(), detail.clone()),
                            OutLine::Field { key, value } => ("field", key.clone(), value.clone()),
                        };
                        LineRec { kind: kind.into(), text, detail }
                    })
                    .collect(),
                ..Self::new(ItemKind::Output, command.clone())
            },
            Item::Summary { text } => Self::new(ItemKind::Summary, text.clone()),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, uniffi::Record)]
pub struct StatusRec {
    pub title: String,
    pub model: String,
    pub pinned: Option<String>,
    pub models: Vec<String>,
    pub work_mode: String,
    pub budget: String,
    pub effort: Option<String>,
    pub context_used: u32,
    pub context_window: u32,
    pub cost: f64,
    pub queued: u32,
    pub labs: bool,
    pub workspace: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ApprovalRec {
    pub id: u64,
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OptionRec {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct QuestionRec {
    pub id: u64,
    pub question: String,
    pub options: Vec<OptionRec>,
}

/// A session's change: the rows from `from` to the end (those before are unchanged), and the
/// rest of its state.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SessionUpdate {
    pub from: u32,
    pub total: u32,
    pub items: Vec<ItemRec>,
    pub status: StatusRec,
    pub busy: bool,
    pub approval: Option<ApprovalRec>,
    pub question: Option<QuestionRec>,
    pub closed: bool,
}

impl SessionUpdate {
    pub fn from_conversation(c: &Conversation, from: usize) -> Self {
        let from = from.min(c.items.len());
        let s = &c.status;
        Self {
            from: from as u32,
            total: c.items.len() as u32,
            items: c.items[from..].iter().map(ItemRec::from).collect(),
            status: StatusRec {
                title: s.title.clone().unwrap_or_default(),
                model: s.model.clone(),
                pinned: s.pinned.clone(),
                models: s.models.clone(),
                work_mode: s.work_mode.clone(),
                budget: s.budget.clone(),
                effort: s.effort.clone(),
                context_used: s.context_used,
                context_window: s.context_window,
                cost: s.cost,
                queued: s.queued as u32,
                labs: s.labs,
                workspace: s.workspace.clone(),
            },
            busy: c.busy,
            approval: c.approval.as_ref().map(|a| ApprovalRec { id: a.id, tool: a.tool.clone(), summary: a.summary.clone() }),
            question: c.question.as_ref().map(|q| QuestionRec {
                id: q.id,
                question: q.question.clone(),
                options: q.options.iter().map(|o| OptionRec { label: o.label.clone(), description: o.description.clone() }).collect(),
            }),
            closed: c.closed,
        }
    }
}

/// What the user does in a session.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum InputRec {
    Prompt { text: String },
    Command { text: String },
    Approve { id: u64, granted: bool },
    Answer { id: u64, choice: Option<u32>, text: Option<String> },
    Interrupt,
}

impl From<InputRec> for po_wire::Input {
    fn from(i: InputRec) -> Self {
        match i {
            InputRec::Prompt { text } => po_wire::Input::Prompt { text },
            InputRec::Command { text } => po_wire::Input::Command { text },
            InputRec::Approve { id, granted } => po_wire::Input::Approve { id, granted },
            InputRec::Answer { id, choice, text } => po_wire::Input::Answer { id, choice, text },
            InputRec::Interrupt => po_wire::Input::Interrupt,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MenuItemRec {
    pub label: String,
    pub detail: String,
    /// run, show, fill.
    pub action: String,
    pub command: String,
    pub current: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MenuRec {
    pub title: String,
    pub note: Option<String>,
    pub items: Vec<MenuItemRec>,
}

impl From<po_wire::Menu> for MenuRec {
    fn from(m: po_wire::Menu) -> Self {
        Self {
            title: m.title,
            note: m.note,
            items: m
                .items
                .into_iter()
                .map(|i| MenuItemRec {
                    label: i.label,
                    detail: i.detail,
                    action: match i.action {
                        po_wire::MenuAction::Run => "run",
                        po_wire::MenuAction::Show => "show",
                        po_wire::MenuAction::Fill => "fill",
                    }
                    .into(),
                    command: i.command,
                    current: i.current,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct CommandRec {
    pub name: String,
    pub description: String,
}
