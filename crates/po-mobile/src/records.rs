//! What crosses to Kotlin and Swift: flat records, easy to draw.

use po_wire::{Conversation, Item, OutLine, SubagentState, ToolState};

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
    /// A model is being loaded for it on the PC.
    pub loading: bool,
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
            loading: s.loading,
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
    /// What a tool gave back.
    pub output: String,
    pub status: ToolStatus,
    pub streaming: bool,
    /// Characters of reasoning (shown even when the text is not).
    pub chars: u64,
    pub lines: Vec<LineRec>,
}

impl ItemRec {
    fn new(kind: ItemKind, text: String) -> Self {
        Self { kind, text, title: String::new(), detail: String::new(), output: String::new(), status: ToolStatus::None, streaming: false, chars: 0, lines: Vec::new() }
    }
}

impl From<&Item> for ItemRec {
    fn from(item: &Item) -> Self {
        match item {
            Item::User { text } => Self { chars: text.chars().count() as u64, ..Self::new(ItemKind::User, text.clone()) },
            Item::Assistant { text, streaming } => Self { streaming: *streaming, chars: text.chars().count() as u64, ..Self::new(ItemKind::Assistant, text.clone()) },
            Item::Thinking { text, chars, streaming } => Self { streaming: *streaming, chars: if *chars > 0 { *chars as u64 } else { text.chars().count() as u64 }, ..Self::new(ItemKind::Thinking, text.clone()) },
            Item::Tool { name, summary, state, diff, output } => {
                let tool_chars = name.chars().count()
                    + summary.chars().count()
                    + diff.as_ref().map(|d| d.chars().count()).unwrap_or(0)
                    + output.chars().count();
                Self {
                    title: name.clone(),
                    detail: diff.clone().unwrap_or_default(),
                    output: output.clone(),
                    status: match state {
                        ToolState::Running => ToolStatus::Running,
                        ToolState::Done => ToolStatus::Done,
                        ToolState::Error => ToolStatus::Error,
                        ToolState::Denied => ToolStatus::Denied,
                    },
                    streaming: *state == ToolState::Running,
                    chars: tool_chars as u64,
                    ..Self::new(ItemKind::Tool, summary.clone())
                }
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

/// A model the session is waiting for: a server is loading on the PC.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct LoadingRec {
    pub model: String,
    /// What the loading is doing, already in the language the PC speaks.
    pub stage: String,
    /// When it started, epoch milliseconds: the app counts the seconds itself.
    pub since_ms: u64,
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
    /// The messages waiting, oldest first.
    pub queue: Vec<String>,
    /// The turn is paused, waiting to go on.
    pub paused: bool,
    pub labs: bool,
    pub workspace: String,
    /// What serves the model.
    pub engine: String,
    pub generation_tok_s: f32,
    pub prefill_tok_s: f32,
    /// A screen the session opened by itself and that waits for a choice (`/setup`): opened on
    /// the phone too, once.
    pub offer: Option<String>,
    /// A model is loading on the PC: shown while it loads, `None` when nothing is.
    pub loading: Option<LoadingRec>,
    /// Which GPU / card the model runs on (e.g. "GPU 0 (RTX 3090)", "Strata", "CPU").
    pub placement: Option<String>,
    pub branch: Option<String>,
    pub git_added: u32,
    pub git_removed: u32,
    pub context_tools: u32,
    pub context_thinking: u32,
    pub context_assistant: u32,
    pub context_user: u32,
    pub context_system: u32,
}

/// What the PC is using now.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct UsageRec {
    pub cpu_percent: f32,
    pub ram_used_mib: u64,
    pub ram_total_mib: u64,
    pub gpus: Vec<GpuRec>,
    pub disks: Vec<DiskRec>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct GpuRec {
    pub name: String,
    pub percent: f32,
    pub vram_used_mib: u64,
    pub vram_total_mib: u64,
    pub temperature_c: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DiskRec {
    pub name: String,
    pub free_gib: f32,
    pub total_gib: f32,
}

impl From<po_wire::MachineUsage> for UsageRec {
    fn from(u: po_wire::MachineUsage) -> Self {
        Self {
            cpu_percent: u.cpu_percent,
            ram_used_mib: u.ram_used_mib,
            ram_total_mib: u.ram_total_mib,
            gpus: u
                .gpus
                .into_iter()
                .map(|g| GpuRec { name: g.name, percent: g.percent, vram_used_mib: g.vram_used_mib, vram_total_mib: g.vram_total_mib, temperature_c: g.temperature_c })
                .collect(),
            disks: u.disks.into_iter().map(|d| DiskRec { name: d.name, free_gib: d.free_gib, total_gib: d.total_gib }).collect(),
        }
    }
}

/// A subagent of the session, and its conversation.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SubagentRec {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub task: String,
    pub background: bool,
    /// running, paused, done, failed.
    pub state: String,
    /// What it says it is doing.
    pub progress: String,
    pub items: Vec<ItemRec>,
}

impl From<&po_wire::Subagent> for SubagentRec {
    fn from(s: &po_wire::Subagent) -> Self {
        Self {
            id: s.id.clone(),
            agent: s.agent.clone(),
            model: s.model.clone(),
            task: s.task.clone(),
            background: s.background,
            state: match s.state {
                SubagentState::Running => "running",
                SubagentState::Paused => "paused",
                SubagentState::Done => "done",
                SubagentState::Failed => "failed",
            }
            .into(),
            progress: s.progress.clone().unwrap_or_default(),
            items: s.items.iter().map(ItemRec::from).collect(),
        }
    }
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
    /// The subagents, when they changed (`None`: as before).
    pub subagents: Option<Vec<SubagentRec>>,
}

impl SessionUpdate {
    pub fn from_conversation(c: &Conversation, from: usize, subagents: bool) -> Self {
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
                queue: s.queue.clone(),
                paused: s.paused,
                labs: s.labs,
                workspace: s.workspace.clone(),
                engine: s.engine.clone(),
                generation_tok_s: s.generation_tok_s,
                prefill_tok_s: s.prefill_tok_s,
                offer: s.offer.clone(),
                loading: s.loading.as_ref().map(|l| LoadingRec { model: l.model.clone(), stage: l.stage.clone(), since_ms: l.since_ms }),
                placement: s.placement.clone(),
                branch: s.branch.clone(),
                git_added: s.git_added as u32,
                git_removed: s.git_removed as u32,
                context_tools: s.context_tools,
                context_thinking: s.context_thinking,
                context_assistant: s.context_assistant,
                context_user: s.context_user,
                context_system: s.context_system,
            },
            busy: c.busy,
            approval: c.approval.as_ref().map(|a| ApprovalRec { id: a.id, tool: a.tool.clone(), summary: a.summary.clone() }),
            question: c.question.as_ref().map(|q| QuestionRec {
                id: q.id,
                question: q.question.clone(),
                options: q.options.iter().map(|o| OptionRec { label: o.label.clone(), description: o.description.clone() }).collect(),
            }),
            closed: c.closed,
            subagents: subagents.then(|| c.subagents.iter().map(SubagentRec::from).collect()),
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
    Pause,
    Resume,
    Unqueue { index: u32 },
    Requeue { index: u32, text: String },
}

impl From<InputRec> for po_wire::Input {
    fn from(i: InputRec) -> Self {
        match i {
            InputRec::Prompt { text } => po_wire::Input::Prompt { text },
            InputRec::Command { text } => po_wire::Input::Command { text },
            InputRec::Approve { id, granted } => po_wire::Input::Approve { id, granted },
            InputRec::Answer { id, choice, text } => po_wire::Input::Answer { id, choice, text },
            InputRec::Interrupt => po_wire::Input::Interrupt,
            InputRec::Pause => po_wire::Input::Pause,
            InputRec::Resume => po_wire::Input::Resume,
            InputRec::Unqueue { index } => po_wire::Input::Unqueue { index },
            InputRec::Requeue { index, text } => po_wire::Input::Requeue { index, text },
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
    /// The menu to open again once the command ran.
    pub then: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MenuRec {
    pub title: String,
    pub note: Option<String>,
    pub items: Vec<MenuItemRec>,
    /// The command line of the menu Back goes to.
    pub back: Option<String>,
    /// Asked again every second while open.
    pub live: bool,
    /// Something is under way.
    pub busy: bool,
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
                    then: i.then,
                })
                .collect(),
            back: m.back,
            live: m.live,
            busy: m.busy,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct CommandRec {
    pub name: String,
    pub description: String,
}
