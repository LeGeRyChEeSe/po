//! `po-agent` — the loop that makes the models work.
//!
//! It assembles the four other crates: the router chooses *who* answers, the provider carries
//! the request, the tools act on the disk, the session keeps the books.
//!
//! Two points set it apart from an ordinary agent loop:
//!
//! - the model is **chosen on every turn** based on the context size and the current budget
//!   mode, rather than fixed at startup;
//! - the `task` tool delegates to a **subagent whose model may be different** — this is the
//!   mechanism that lets a cloud orchestrator put local models to work.

pub mod mentions;
pub mod question;
pub mod session;
pub mod store;
pub mod subagents;
pub mod work_mode;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{
    Completion, ContentBlock, Message, ModelSpec, StopReason, ToolOutcome, ToolSpec, Usage,
};
use po_provider::{Provider, ProviderError};
use po_router::{Demand, Reason, Router, TaskClass};
use po_tools::{Registry, ToolContext};
use subagents::{Profile, SubagentSettings, ToolScope};
use work_mode::{Gate, WorkMode, WorkModeHandle};
use serde_json::{Value, json};

pub use session::Session;
pub use store::{SavedSession, SessionStore, StoreError};

/// Maximum number of model/tool round trips within one turn, before handing control back.
/// An agent looping on a failing tool must stop and say so, not burn the budget.
const MAX_ITERATIONS: usize = 40;
/// Searches the archive of compacted messages.
const RECALL: &str = "recall";
/// What a compaction summary must hold.
const SUMMARY_INSTRUCTION: &str = "You summarize a work conversation so that the agent can carry on after its \
     context window is cleared. Write, in this order: the user's goal; the decisions made; what the work already \
     found, with file paths and the exact names and values that matter (functions, types, settings, identifiers); \
     the changes already made; the step in progress and the next steps. Never invent progress the conversation \
     does not show. Drop pleasantries. Return dense text, without preamble, in the language of the conversation.";
/// Margin reserved for the response when computing the context window.
const RESERVE_TOKENS: u32 = 8192;
/// Attempts on a transient error (network drop, 503, momentary quota).
const MAX_ATTEMPTS: usize = 3;
/// Initial wait between two attempts; it doubles on every failure.
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(600);
/// Proportion of the usable budget beyond which the user is warned.
///
/// The reference is the **compaction point** (`window - reserve`), not the end of the window:
/// on a small-window model, compaction happens well before the window is full, and an alert
/// pegged to the window would never fire in time.
const COMPACTION_WARNING_RATIO: f32 = 0.85;
/// Approximate cost of a subagent's system instruction and its declared tools.
/// Used to estimate what it will actually receive, without having to build the request.
const SUBAGENT_SYSTEM_TOKENS: u32 = 1200;
/// Successive models tried within one turn before giving up. Without this bound, a fleet that
/// is entirely down would make the loop spin through every model on each iteration.
const MAX_MODEL_SWITCHES: usize = 4;

/// What the loop lets be seen. The interface subscribes to it; so do the tests.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// A model was picked for this turn, with the reason when it is worth stating.
    ModelSelected { model: String, class: TaskClass, reason: Reason },
    Thinking(String),
    /// Reasoning is arriving during generation. Only its size is passed along: the full text
    /// follows in an [`AgentEvent::Thinking`], and the interface only needs to know the model is
    /// working. Without this signal, a model that thinks for a minute leaves the screen still and
    /// the user believes it is stuck.
    ThinkingDelta(usize),
    /// Text of the reasoning in progress, emitted only if the user asked to see it.
    ThinkingText(String),
    /// Timing of the call that just finished.
    CallStats(CallStats),
    /// The local server is busy with another request; this one waits its turn.
    Queued,
    Text(String),
    /// A tool is about to run.
    ToolStarted { name: String, summary: String },
    ToolFinished { name: String, summary: String, is_error: bool },
    /// Line diff of the file the tool that just finished changed (see `po_tools::diff`).
    ToolDiff { diff: String },
    /// The user refused the execution.
    ToolDenied { name: String, summary: String },
    /// The model asked the user a question, and the user answered.
    Asked { question: String, answer: String },
    /// A task was handed to a subagent. `background`: the main agent carries on without
    /// waiting for it.
    Delegated { model: String, agent: String, task: String, background: bool },
    /// Response fragment that arrived during generation. The interface shows it immediately;
    /// the full text arrives afterwards in an [`AgentEvent::Text`].
    TextDelta(String),
    /// An attempt failed transiently; another will follow.
    Retrying { model: String, attempt: usize, error: String },
    /// A model is set aside for the session, and the rest will go through another.
    ModelDisabled { model: String, reason: String },
    /// Information to display that is not a model response (compaction, fallback, quota).
    Notice(String),
    TurnCompleted {
        usage: Usage,
        cost: f64,
        local_share: f64,
        /// Local compute seconds accumulated over the session. Zero when everything goes through the cloud.
        local_seconds: f64,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("{}", no_model_text(.0))]
    NoModel(String),
    #[error("{}", no_provider_text(.0))]
    NoProvider(String),
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

fn no_model_text(what: &str) -> String {
    po_i18n::t!("agent.error.no_model", what = what)
}

fn no_provider_text(model: &str) -> String {
    po_i18n::t!("agent.error.no_provider", model = model)
}

/// Measurements of a model call, taken client-side: they apply to every server, including
/// those that publish no timing figures.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CallStats {
    /// Delay before the first fragment received (reasoning or text), in seconds. For a local
    /// model, this is mostly prompt reading.
    pub ttft: f64,
    /// Tokens generated per second, once the first fragment has arrived.
    pub generation_tok_s: f64,
    /// Prompt tokens actually read (excluding cache) per second of waiting.
    pub prefill_tok_s: f64,
    /// Prompt tokens served by the server's cache: read for free.
    pub cached_tokens: u64,
    pub completion_tokens: u64,
}

impl CallStats {
    pub fn measure(ttft: f64, generation_secs: f64, usage: &Usage) -> Self {
        let rate = |tokens: u64, secs: f64| {
            if tokens > 0 && secs >= 0.05 { (tokens as f64 / secs * 10.0).round() / 10.0 } else { 0.0 }
        };
        Self {
            ttft,
            generation_tok_s: rate(usage.output_tokens, generation_secs),
            prefill_tok_s: rate(usage.input_tokens, ttft),
            cached_tokens: usage.cache_read_tokens,
            completion_tokens: usage.output_tokens,
        }
    }

    /// Readable line, or `None` when there is nothing meaningful to say (a tiny response:
    /// a speed computed over a few tokens would be noise).
    pub fn summary(&self) -> Option<String> {
        if self.completion_tokens < 8 || self.generation_tok_s <= 0.0 {
            return None;
        }
        let mut parts = vec![
            format!("{:.1} tok/s", self.generation_tok_s),
            po_i18n::t!("agent.stats.first_token", secs = format!("{:.1}", self.ttft)),
        ];
        if self.cached_tokens > 0 {
            parts.push(po_i18n::t!("agent.stats.cached", n = self.cached_tokens));
        }
        Some(parts.join(" · "))
    }
}

/// Who decides whether to allow a tool call that is subject to the user.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, tool: &str, input: &Value) -> bool;
}

/// Accepts everything not explicitly forbidden. Meant for non-interactive runs; the hard
/// ban (`Permission::Deny`) is still enforced.
pub struct AutoApprove;

#[async_trait]
impl Approver for AutoApprove {
    async fn approve(&self, _tool: &str, _input: &Value) -> bool {
        true
    }
}

/// Refuses everything that asks for approval. Useful for a read-only session.
pub struct ReadOnly;

#[async_trait]
impl Approver for ReadOnly {
    async fn approve(&self, _tool: &str, _input: &Value) -> bool {
        false
    }
}

/// Declaration of the delegation tool. It is not in the registry because it does not act on
/// the disk: it restarts the loop on a subagent, which only the agent can do.
///
/// The subagent list is part of the declaration: it only changes with the profiles, not from
/// one turn to the next, so the prefix sent to the model stays stable for the cache.
/// `background` is only offered if the server can actually run a subagent alongside the main
/// agent: promising parallelism that does not exist would make it wait in silence.
fn task_tool_spec(profiles: &[Profile], parallel: usize, background: bool) -> ToolSpec {
    let mut catalog = String::new();
    for p in profiles {
        catalog.push_str(&format!("\n- {} ({}): {}", p.name, p.tools.model_label(), p.description));
    }
    let mut description = format!(
        "Hands a self-contained task to a subagent. The subagent does not see the conversation: \
         describe the whole task (files, context, expected result). Several `task` calls in the \
         same response run in parallel, up to {parallel} at a time.\nSubagents:{catalog}"
    );
    let mut properties = json!({
        "agent": {
            "type": "string",
            "enum": profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            "description": "Subagent to hand the task to"
        },
        "task": {"type": "string", "description": "Complete, self-contained instruction"}
    });
    if background {
        description.push_str(
            "\nWith `background: true`, you continue your work without waiting: the result will be \
             delivered to you as soon as it is ready.",
        );
        properties["background"] = json!({
            "type": "boolean",
            "description": "Do not wait for the result before continuing"
        });
    }
    ToolSpec::new(
        "task",
        description,
        json!({"type": "object", "properties": properties, "required": ["agent", "task"]}),
    )
}

/// Role matching the old `class` parameter, still present in saved sessions and in models that
/// learned it.
fn legacy_role(class: &str) -> &'static str {
    match class {
        "edit" => "builder",
        "test" => "verifier",
        "review" => "reviewer",
        "summarize" => "summarizer",
        _ => "explorer",
    }
}

/// Progress line of a subagent ("Progress: …"; the old French "En cours : …" is still accepted),
/// without its prefix or formatting.
pub fn progress_line(line: &str) -> Option<&str> {
    let line = line.trim().trim_start_matches(['*', '_', '-', '>', ' ']);
    let strip_ci = |prefix: &str| {
        line.get(..prefix.len()).filter(|head| head.eq_ignore_ascii_case(prefix)).map(|_| &line[prefix.len()..])
    };
    let rest = strip_ci("Progress").or_else(|| strip_ci("En cours"))?;
    let rest = rest.trim_start_matches(['*', '_', ' ']).strip_prefix(':')?;
    let rest = rest.trim_matches(['*', '_', ' ']);
    (!rest.is_empty()).then_some(rest)
}

/// Media a model can read, according to its capabilities.
fn media_of(capabilities: &po_core::Capabilities) -> po_tools::Media {
    po_tools::Media { image: capabilities.vision, video: capabilities.video, audio: capabilities.audio }
}

/// Tool results, followed by the media they attach (an image read by `read`): a tool result
/// carries only text on most servers, so the media goes alongside, announced.
fn result_blocks(outcomes: &[ToolOutcome]) -> Vec<ContentBlock> {
    let mut blocks: Vec<ContentBlock> = outcomes.iter().map(ToolOutcome::to_block).collect();
    let media: Vec<ContentBlock> = outcomes.iter().flat_map(|o| o.attachments.iter().cloned()).collect();
    if !media.is_empty() {
        blocks.push(ContentBlock::text("[Files attached by the tool results above, in order]"));
        blocks.extend(media);
    }
    blocks
}

/// A subagent's text without its progress lines.
fn without_progress_lines(text: &str) -> String {
    text.lines().filter(|l| progress_line(l).is_none()).collect::<Vec<_>>().join("\n").trim().to_owned()
}

/// First useful line of a reply, stripped of what a model readily adds to it (quotes,
/// "Title:", formatting). `None` if nothing is usable.
pub fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let ornament = |c: char| matches!(c, '"' | '«' | '»' | '*' | '`' | '\'' | '.' | '#');
    let line = line.trim_matches(ornament).trim();
    let line = ["Title:", "Title :", "Titre :", "Titre:"].iter().find_map(|p| line.strip_prefix(p)).unwrap_or(line);
    let title: String = line
        .trim()
        .trim_matches(ornament)
        .trim()
        .chars()
        .take(60)
        .collect();
    (!title.is_empty()).then_some(title)
}

/// Key of the server serving a model: slots are counted per server, not per model.
/// Puts a conversation's server cache aside and back: on a single-slot server, another
/// conversation (a subagent, Po Labs) using the slot would otherwise make it read again in full.
#[async_trait]
pub trait Parking: Send + Sync {
    /// Saves what the slot holds as the conversation `key`.
    async fn park(&self, key: &str) -> bool;
    /// Loads the conversation `key` back into the slot; false when there was nothing to load.
    async fn unpark(&self, key: &str) -> bool;
    /// Drops what was saved for `key`: the conversation was cleared, compacted or closed.
    fn forget(&self, key: &str);
}

/// Below this many tokens, reading the conversation again is quick: no parking.
const PARK_MIN_TOKENS: u32 = 4000;

fn endpoint_key(model: &ModelSpec) -> String {
    match &model.origin {
        po_core::Origin::Local { endpoint, .. } => endpoint.clone(),
        other => other.label().to_owned(),
    }
}

/// What a subagent is doing, as it goes, to display apart from the conversation.
/// `id` is that of the `task` call that launched it.
#[derive(Debug, Clone, PartialEq)]
pub enum SubagentUpdate {
    Started {
        id: String,
        agent: String,
        model: String,
        task: String,
        background: bool,
    },
    /// Reasoning fragment.
    Thinking {
        id: String,
        text: String,
    },
    /// Produced text fragment.
    Text { id: String, text: String },
    Tool { id: String, name: String, summary: String },
    ToolDone { id: String, name: String, ok: bool },
    /// Paused with the main agent; a new `Started` for the same id resumes it.
    Paused { id: String },
    Finished { id: String, ok: bool },
}

pub type SubagentMonitor = tokio::sync::mpsc::UnboundedSender<SubagentUpdate>;

/// A task handed to a subagent, ready to go: it borrows nothing from the agent, so several can
/// run at the same time, or in the background.
struct SubagentJob {
    call_id: String,
    agent: String,
    model: ModelSpec,
    provider: Arc<dyn Provider>,
    registry: Registry,
    ctx: ToolContext,
    approver: Arc<dyn Approver>,
    approvals: Arc<tokio::sync::Mutex<()>>,
    work_mode: WorkModeHandle,
    settings: SessionSettings,
    system: String,
    /// Tools declared to the model. On the session's server, these are the main agent's, byte
    /// for byte: the request prefix is then already cached.
    tools: Vec<ToolSpec>,
    /// Tools the subagent is allowed to run; `None`: all those of the registry.
    allowed: Option<Vec<String>>,
    task: String,
    monitor: Option<SubagentMonitor>,
    /// The main agent's pause flag: a subagent the main agent waits for stops with it, at its
    /// own next safe point. `None` for a background subagent.
    pause: Option<Arc<AtomicBool>>,
    /// The conversation of a subagent paused earlier, to go on from.
    resume_from: Option<Vec<Message>>,
}

struct SubagentResult {
    outcome: ToolOutcome,
    agent: String,
    model: ModelSpec,
    /// Usage of each call, with its duration: charged to the session on return.
    calls: Vec<(Usage, f64)>,
    /// Paused before finishing: its conversation so far, to resume from.
    paused: Option<Vec<Message>>,
}

/// A subagent paused with the main agent, waiting to go on: the `task` call it answers, and its
/// conversation word for word. Saved with the session, so it resumes even after a restart.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PausedSubagent {
    pub call_id: String,
    pub input: Value,
    pub messages: Vec<Message>,
}

/// The slots of a server shared by several sessions (the user's and Po Labs, side by side): who
/// holds what, seen by all at once. A session's turn holds one while it generates; each subagent
/// holds one while it works. The first to ask for a free slot gets it, and the others see it taken.
#[derive(Debug, Default)]
pub struct SlotLedger {
    used: std::sync::Mutex<HashMap<String, usize>>,
}

impl SlotLedger {
    /// Slots of the server at `endpoint` held right now, by every session.
    pub fn used(&self, endpoint: &str) -> usize {
        self.used.lock().map(|m| m.get(endpoint).copied().unwrap_or(0)).unwrap_or(0)
    }

    /// Takes a slot of the server at `endpoint`, until the guard is dropped.
    pub fn take(self: &Arc<Self>, endpoint: &str) -> SlotGuard {
        if let Ok(mut map) = self.used.lock() {
            *map.entry(endpoint.to_owned()).or_insert(0) += 1;
        }
        SlotGuard { ledger: self.clone(), endpoint: endpoint.to_owned() }
    }
}

/// A slot held in a [`SlotLedger`], given back when dropped.
#[derive(Debug)]
pub struct SlotGuard {
    ledger: Arc<SlotLedger>,
    endpoint: String,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.ledger.used.lock()
            && let Some(count) = map.get_mut(&self.endpoint)
        {
            *count = count.saturating_sub(1);
        }
    }
}

/// Stands for the result of a paused subagent until it finishes. Never read by the model: the
/// turn is paused, and the real result replaces it before the next call.
const SUBAGENT_PAUSED: &str = "[subagent paused by the user; its result comes when it resumes]";

struct BackgroundJob {
    label: String,
    agent: String,
    endpoint: String,
    handle: tokio::task::JoinHandle<SubagentResult>,
}

/// Applies the session settings to a request.
fn apply_settings(settings: &SessionSettings, mut request: Completion) -> Completion {
    request.reasoning_effort = settings.reasoning_effort.clone();
    // Only a local server gets a budget: on a cloud API, `thinking_tokens` would switch
    // extended thinking on for every request.
    if request.model.origin.is_local() {
        request.thinking_tokens = settings.thinking_budget(request.model.context_window);
    }
    if settings.temperature.is_some() {
        request.temperature = settings.temperature;
    }
    request
}

/// Makes a subagent work until it hands control back.
async fn run_subagent(job: SubagentJob) -> SubagentResult {
    let SubagentJob {
        call_id,
        agent,
        model,
        provider,
        registry,
        ctx,
        approver,
        approvals,
        work_mode,
        settings,
        system,
        tools,
        allowed,
        task,
        monitor,
        pause,
        resume_from,
    } = job;
    let notify = |update: SubagentUpdate| {
        if let Some(monitor) = &monitor {
            let _ = monitor.send(update);
        }
    };
    let mut sub = Session::new(system);
    match resume_from {
        Some(messages) => sub.messages = messages,
        None => sub.push(Message::user(task)),
    }
    let mut calls = Vec::new();
    let mut last_text = String::new();
    let fail = |text: String, calls: Vec<(Usage, f64)>, agent: String, model: ModelSpec| SubagentResult {
        outcome: ToolOutcome::error(&call_id, text),
        agent,
        model,
        calls,
        paused: None,
    };

    for step in 0..MAX_ITERATIONS {
        // Paused with the main agent: stop after the tools of the step just done, keeping the
        // conversation as it is.
        if step > 0 && pause.as_ref().is_some_and(|p| p.load(Ordering::Relaxed)) {
            notify(SubagentUpdate::Paused { id: call_id.clone() });
            return SubagentResult {
                outcome: ToolOutcome::ok(&call_id, SUBAGENT_PAUSED),
                agent,
                model,
                calls,
                paused: Some(sub.messages),
            };
        }
        let request = apply_settings(&settings, Completion::new(model.clone(), sub.system.clone(), sub.messages.clone()))
            .with_tools(tools.clone());
        let started = std::time::Instant::now();
        // Streamed when possible: the interface shows the subagent writing live.
        let result = if monitor.is_some() && provider.supports_streaming() {
            let id = call_id.clone();
            let monitor = monitor.clone();
            provider
                .stream(&request, &mut |delta| {
                    let update = match delta {
                        po_provider::Delta::Text(text) => SubagentUpdate::Text { id: id.clone(), text },
                        po_provider::Delta::Thinking(text) => SubagentUpdate::Thinking { id: id.clone(), text },
                        _ => return,
                    };
                    if let Some(monitor) = &monitor {
                        let _ = monitor.send(update);
                    }
                })
                .await
        } else {
            provider.complete(&request).await
        };
        let response = match result {
            Ok(value) => value,
            Err(err) => {
                notify(SubagentUpdate::Finished { id: call_id.clone(), ok: false });
                return fail(format!("subagent {agent}: {err}"), calls, agent, model);
            }
        };
        calls.push((response.usage, started.elapsed().as_secs_f64()));

        // Progress lines are for the screen, not for the main agent.
        let text = without_progress_lines(&response.message.visible_text());
        if !text.is_empty() {
            last_text = text;
        }
        let wants_tools = response.wants_tools();
        sub.push(response.message.clone());
        if !wants_tools {
            break;
        }

        let mut outcomes = Vec::new();
        for (tool_id, name, input) in response
            .message
            .tool_uses()
            .map(|(i, n, v)| (i.to_owned(), n.to_owned(), v.clone()))
            .collect::<Vec<_>>()
        {
            if name == "task" {
                outcomes.push(ToolOutcome::error(&tool_id, "a subagent does not delegate in turn: do the task yourself"));
                continue;
            }
            if name == "question" {
                outcomes.push(ToolOutcome::error(
                    &tool_id,
                    "a subagent does not ask the user questions: finish, and put your question in your result \
                     for the main agent",
                ));
                continue;
            }
            if allowed.as_ref().is_some_and(|list| !list.contains(&name)) {
                outcomes.push(ToolOutcome::error(
                    &tool_id,
                    format!("\"{name}\" is not allowed for subagent {agent} (read-only): return your result without modifying anything"),
                ));
                continue;
            }
            notify(SubagentUpdate::Tool { id: call_id.clone(), name: name.clone(), summary: summarize_call(&name, &input) });
            let tool_name = name.clone();
            let outcome = match registry.get(&name).map(|tool| tool.permission(&input)) {
                Some(Permission::Deny) => {
                    ToolOutcome::error(&tool_id, "call refused by the harness security policy")
                }
                // Approval is requested from the user as for the main agent, one request at a
                // time: several subagents in parallel must not stack their questions.
                Some(Permission::Ask) if work_mode.get().gate() == Gate::Refuse => {
                    ToolOutcome::error(&tool_id, work_mode::PLAN_REFUSAL)
                }
                Some(Permission::Ask) => {
                    let label = po_i18n::t!("agent.display.subagent_tool", name = name, agent = agent);
                    let granted = work_mode.get().gate() == Gate::Allow || {
                        let _turn = approvals.lock().await;
                        approver.approve(&label, &input).await
                    };
                    if granted {
                        registry.dispatch(&tool_id, &name, input, &ctx).await
                    } else {
                        ToolOutcome::error(&tool_id, "the user refused this call — propose another approach")
                    }
                }
                _ => registry.dispatch(&tool_id, &name, input, &ctx).await,
            };
            notify(SubagentUpdate::ToolDone { id: call_id.clone(), name: tool_name, ok: !outcome.is_error });
            outcomes.push(outcome);
        }
        sub.push(Message::tool_results(result_blocks(&outcomes)));
    }

    notify(SubagentUpdate::Finished { id: call_id.clone(), ok: !last_text.is_empty() });
    if last_text.is_empty() {
        return fail(format!("subagent {agent} produced no result"), calls, agent, model);
    }
    let outcome = ToolOutcome::ok(&call_id, last_text).with_display(po_i18n::t!("agent.display.subagent", agent = agent, model = model.display_name));
    SubagentResult { outcome, agent, model, calls, paused: None }
}

/// Settings changeable mid-session, applied to every request. Saved with the session:
/// resuming a conversation resumes it with the settings it was given.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionSettings {
    /// Reasoning level requested from local servers. Changing it mid-session alters the
    /// prompt template on the server side: the cache is then reread once.
    pub reasoning_effort: Option<String>,
    /// Imposed temperature; `None` leaves the server's settings.
    pub temperature: Option<f32>,
    /// Acceptance criteria. As long as there are some, each turn ends with a check made by the
    /// model itself, in the same conversation — hence on the already warm cache.
    pub criteria: Vec<String>,
    /// Show the reasoning as it comes, rather than a mere counter.
    pub show_thinking: bool,
    /// Most tokens a local model may think per reply. `None` derives it from the reasoning
    /// level and the context window (see [`SessionSettings::thinking_budget`]); `Some(0)`
    /// lifts the cap and leaves the server's own.
    pub thinking_budget: Option<u32>,
    /// Work mode (plan, builder, bypass), kept so that a restart does not change it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_mode: Option<String>,
}

impl SessionSettings {
    /// Thinking budget sent to a local server, per reply.
    ///
    /// Without one, a server derives it from `max_tokens`: Strata leaves the answer a quarter
    /// and lets the model think up to 24,576 tokens of a 32,768 cap — each step of an agentic
    /// turn, on a window of 131,072. Observed: five steps were enough to fill the context, and
    /// a Po Labs session spent its whole window thinking. The defaults follow the usual tiers
    /// (Anthropic's `budget_tokens`, Gemini's `thinkingBudget`, Strata's own levels): a few
    /// thousand tokens per step suffice to choose the next tool call.
    pub fn thinking_budget(&self, context_window: u32) -> Option<u32> {
        if let Some(budget) = self.thinking_budget {
            return (budget > 0).then_some(budget);
        }
        // Never more than an eighth of the window per step, whatever the level.
        let ceiling = (context_window / 8).max(1024);
        let budget = match self.reasoning_effort.as_deref() {
            Some("none") => return None,
            Some("low") => 2048,
            Some("high") => 16_384,
            Some("medium") => 8192,
            // No level chosen: a sixteenth of the window, between 2,048 and 8,192.
            _ => (context_window / 16).clamp(2048, 8192),
        };
        Some(budget.min(ceiling))
    }
}

/// Number of retries granted when criteria still fail. Beyond that, control is handed back: a
/// model that cannot manage in three passes will not manage in ten, and the user must know
/// rather than wait.
const MAX_VERIFICATION_ROUNDS: usize = 2;

/// Resumes granted when a response is cut off by the output limit. A local model that reasons at
/// length can hit it before having written its answer; handing back that cut text as if it were
/// complete would be worse than saying so.
const MAX_CONTINUATIONS: usize = 2;
const CONTINUE_PROMPT: &str = "[Your previous reply was cut off by the length limit. \
     Resume exactly where it stops, without repeating what is already written.]";

/// Verification instruction. The response format is imposed so failures can be counted without
/// asking the model for JSON, which a small local model readily breaks.
fn verification_prompt(criteria: &[String]) -> String {
    let mut text = String::from(
        "[Automatic verification] Now verify each of the acceptance criteria below, using the tools \
         to check (read, run, test) rather than assuming. First fix anything that does not pass. \
         End your reply with one line per criterion, exactly in the form `OK: <criterion>` or \
         `FAIL: <criterion> — <reason>`.\n",
    );
    for (i, c) in criteria.iter().enumerate() {
        text.push_str(&format!("{}. {c}\n", i + 1));
    }
    text
}

/// Counts the verdicts of a report: (passed, failed). Lines that do not follow the format are
/// ignored; a report without any verdict counts as inconclusive. The old French `ÉCHEC` is still
/// accepted.
pub fn count_verdicts(text: &str) -> (usize, Vec<String>) {
    let mut ok = 0;
    let mut failed = Vec::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', '•', ' ']).trim_start_matches('`');
        let upper = line.to_uppercase();
        if upper.starts_with("OK:") || upper.starts_with("OK :") {
            ok += 1;
        } else if upper.starts_with("ÉCHEC") || upper.starts_with("ECHEC") || upper.starts_with("FAIL") {
            failed.push(line.split_once(':').map(|(_, r)| r.trim().trim_end_matches('`').to_owned()).unwrap_or_default());
        }
    }
    (ok, failed)
}

pub struct Agent {
    providers: HashMap<String, Arc<dyn Provider>>,
    router: Router,
    registry: Registry,
    ctx: ToolContext,
    approver: Arc<dyn Approver>,
    pub session: Session,
    /// Allow the agent to delegate. A subagent does not delegate itself: otherwise a chain of
    /// delegation could fold back on itself indefinitely.
    delegation_enabled: bool,
    pub settings: SessionSettings,
    /// Timing of the last call, for `/cache`.
    last_call: Option<CallStats>,
    /// Context added by the user outside the conversation (output of a `!command`), attached
    /// to their next request.
    pending_context: Vec<String>,
    /// Folders where file changes need no approval (Po's own source under Po Labs).
    approved_dirs: Vec<std::path::PathBuf>,
    /// Parking of the server cache, with the server it applies to.
    parking: Option<(String, Arc<dyn Parking>)>,
    /// Identifier of the conversation for the parking (the saved session's id).
    pub session_key: Option<String>,
    /// Model calls allowed in one turn before handing back: enough for a conversation, more for
    /// an autonomous session (Po Labs explores, changes, then builds).
    pub max_steps: usize,
    /// Subagents paused with the turn, resumed first when it resumes.
    paused_subagents: Vec<PausedSubagent>,
    /// Model orchestrating the current turn: by default, the subagents' one.
    orchestrator: Option<ModelSpec>,
    /// Available subagents (built-in, permanent, project roles).
    profiles: Vec<Profile>,
    pub subagent_settings: SubagentSettings,
    /// Simultaneous sessions announced by each server (its slots), by server key.
    server_slots: HashMap<String, usize>,
    /// Slots shared with another session working at the same time, if any.
    ledger: Option<Arc<SlotLedger>>,
    /// The slot this agent's turn holds while it generates.
    turn_slot: Option<SlotGuard>,
    /// Subagents launched without being waited for.
    background: Vec<BackgroundJob>,
    next_background: usize,
    /// Environment (workspace, system, shell) also given to the subagents: without it, they
    /// do not know where their commands start from.
    subagent_preamble: String,
    /// Only one approval request on screen at a time, whichever agent raises it.
    approvals: Arc<tokio::sync::Mutex<()>>,
    /// Who puts the model's questions to the user. Without it, the `question` tool is not
    /// offered.
    asker: Option<Arc<dyn question::Asker>>,
    /// Plan, builder or bypass: shared with the interface, reread on every tool call.
    work_mode: WorkModeHandle,
    /// Last mode the model was informed of.
    announced_mode: WorkMode,
    /// Where to send the subagents' activity (the interface shows it separately).
    monitor: Option<SubagentMonitor>,
    /// Session title, written by the model after the first exchange.
    pub title: Option<String>,
    /// Pause requested by the interface (settings changed during the turn), and the paused turn.
    pause: Arc<AtomicBool>,
    paused: Option<TurnState>,
}

/// Where a turn stands: what is needed to resume it after a pause.
#[derive(Debug, Clone, Copy)]
struct TurnState {
    start: usize,
    usage: Usage,
    cost_before: f64,
    verification_round: usize,
    continuations: usize,
}

impl Agent {
    pub fn new(
        router: Router,
        registry: Registry,
        ctx: ToolContext,
        system: impl Into<String>,
        approver: Arc<dyn Approver>,
    ) -> Self {
        Self {
            providers: HashMap::new(),
            router,
            registry,
            ctx,
            approver,
            session: Session::new(system),
            delegation_enabled: true,
            settings: SessionSettings::default(),
            last_call: None,
            pending_context: Vec::new(),
            approved_dirs: Vec::new(),
            parking: None,
            session_key: None,
            max_steps: MAX_ITERATIONS,
            paused_subagents: Vec::new(),
            orchestrator: None,
            profiles: subagents::builtins(),
            subagent_settings: SubagentSettings::default(),
            server_slots: HashMap::new(),
            ledger: None,
            turn_slot: None,
            background: Vec::new(),
            next_background: 1,
            subagent_preamble: String::new(),
            approvals: Arc::new(tokio::sync::Mutex::new(())),
            asker: None,
            work_mode: WorkModeHandle::default(),
            announced_mode: WorkMode::Build,
            monitor: None,
            title: None,
            pause: Arc::new(AtomicBool::new(false)),
            paused: None,
        }
    }

    /// Request that has the model write a session title, ready to go without borrowing the
    /// agent: the interface launches it in the background after the first exchange.
    ///
    /// It reuses the system prompt, tools and history identically, then adds the request: on a
    /// single-slot server it starts from the conversation's cache and does not wipe it — a
    /// separate prompt would force the next turn to reread everything.
    pub fn title_job(&self) -> Option<impl std::future::Future<Output = Option<String>> + Send + 'static> {
        let model = self.session_model()?;
        let provider = self.providers.get(&model.id).cloned()?;
        let mut messages = self.session.messages.clone();
        messages.push(Message::user(
            "[Harness request, outside the conversation] Give this work session a title: 3 to 6 words, \
             in the language of the conversation, no quotation marks or final punctuation. Reply with the \
             title only, without any tool.",
        ));
        let request = apply_settings(
            &self.settings,
            Completion::new(model.clone(), self.session.system.clone(), messages).with_tools(self.main_tools(&model)),
        );
        Some(async move {
            let response = provider.complete(&request).await.ok()?;
            clean_title(&response.message.visible_text())
        })
    }

    /// A single call to the session's model outside the conversation, to run in a task of its
    /// own (the interface goes on meanwhile): `system` and `prompt` alone, no tools.
    pub fn once_job(
        &self,
        system: &str,
        prompt: &str,
    ) -> Option<impl std::future::Future<Output = Result<String, String>> + Send + 'static> {
        let model = self.session_model()?;
        let provider = self.providers.get(&model.id).cloned()?;
        let request = apply_settings(&self.settings, Completion::new(model, system.to_owned(), vec![Message::user(prompt)]));
        Some(async move {
            provider.complete(&request).await.map(|r| r.message.visible_text()).map_err(|e| e.to_string())
        })
    }

    /// Plugs in the display of the subagents' activity.
    pub fn set_subagent_monitor(&mut self, monitor: SubagentMonitor) {
        self.monitor = Some(monitor);
    }

    /// Shared work mode: the interface changes it, the agent rereads it on every tool.
    pub fn work_mode(&self) -> WorkModeHandle {
        self.work_mode.clone()
    }

    /// Plugs in the interface that puts the model's questions to the user.
    pub fn set_asker(&mut self, asker: Arc<dyn question::Asker>) {
        self.asker = Some(asker);
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Replaces the subagent profiles (at startup, or after creating one).
    pub fn set_profiles(&mut self, profiles: Vec<Profile>) {
        self.profiles = profiles;
    }

    pub fn set_subagent_preamble(&mut self, text: impl Into<String>) {
        self.subagent_preamble = text.into();
    }

    /// Records the number of simultaneous sessions of a local server (`/props`).
    pub fn set_server_slots(&mut self, endpoint: impl Into<String>, slots: usize) {
        self.server_slots.insert(endpoint.into(), slots.max(1));
    }

    /// Slots announced by a model's server. Unknown: one for a local server (the common case,
    /// and the only safe one), four for a remote service.
    pub fn detected_slots(&self, model: &ModelSpec) -> usize {
        let key = endpoint_key(model);
        self.server_slots.get(&key).copied().unwrap_or(if model.is_local() { 1 } else { 4 })
    }

    /// Model serving the session: the last turn's, otherwise the one the router would pick.
    pub fn session_model(&self) -> Option<ModelSpec> {
        self.orchestrator.clone().or_else(|| self.pick(TaskClass::Orchestration).ok().map(|(m, _)| m))
    }

    /// Simultaneous sessions allowed on a server, main agent included: the user's setting for
    /// the session's server, otherwise what the server announces.
    /// Slots the server at `endpoint` announced (1 when unknown).
    fn parallel_limit_at(&self, endpoint: &str) -> usize {
        self.server_slots.get(endpoint).copied().unwrap_or(1)
    }

    /// Shares the servers' slots with another session working at the same time.
    pub fn share_slots(&mut self, ledger: Arc<SlotLedger>) {
        self.ledger = Some(ledger);
    }

    /// Slots of the session's server, for the sessions working side by side.
    pub fn session_slots(&self) -> usize {
        self.session_model().map(|m| self.parallel_limit(&m)).unwrap_or(1)
    }

    /// Gives back the slot of a turn that ended without finishing (interrupted).
    pub fn end_turn_slot(&mut self) {
        self.turn_slot = None;
    }

    /// The session's slot, taken for the turn (only when slots are shared).
    fn hold_turn_slot(&mut self) {
        if let (Some(ledger), Some(model)) = (self.ledger.clone(), self.session_model()) {
            self.turn_slot = Some(ledger.take(&endpoint_key(&model)));
        }
    }

    /// Slots held on a server: by the sessions sharing it, or else by this agent's background tasks.
    fn held_on(&self, endpoint: &str) -> usize {
        match &self.ledger {
            Some(ledger) => ledger.used(endpoint),
            None => self.background_on(endpoint),
        }
    }

    pub fn parallel_limit(&self, model: &ModelSpec) -> usize {
        let on_session_server = self.session_model().is_some_and(|s| endpoint_key(&s) == endpoint_key(model));
        match self.subagent_settings.max_parallel {
            Some(n) if on_session_server => n.max(1),
            _ => self.detected_slots(model),
        }
    }

    /// Background subagents not yet collected.
    pub fn background_count(&self) -> usize {
        self.background.len()
    }

    /// Model picked for a profile, with how it was chosen.
    pub fn model_choice(&self, profile: &Profile) -> String {
        self.subagent_settings
            .models
            .get(&profile.name)
            .cloned()
            .or_else(|| profile.model.clone())
            .unwrap_or_else(|| "session".to_owned())
    }

    /// Single call to the session's model, outside the conversation: the history is neither
    /// read nor modified. Used to write a subagent from a description.
    pub async fn complete_once(&mut self, system: &str, prompt: &str) -> Result<String, AgentError> {
        let model = self.session_model().ok_or_else(|| AgentError::NoModel("orchestration".into()))?;
        let provider = self.providers.get(&model.id).cloned().ok_or_else(|| AgentError::NoProvider(model.id.clone()))?;
        let request = if model.origin.is_local() {
            self.side_request(&model, &format!("{system}\n\n{prompt}"), self.session.messages.len())
        } else {
            apply_settings(&self.settings, Completion::new(model.clone(), system.to_owned(), vec![Message::user(prompt)]))
        };
        let started = std::time::Instant::now();
        let response = provider.complete(&request).await?;
        self.session.record(&model.id, response.usage, &model.pricing, started.elapsed().as_secs_f64());
        Ok(response.message.visible_text())
    }

    /// Runs a command typed by the user themselves (`!command`). No approval request: they
    /// wrote it. The output will be attached to their next request.
    pub async fn run_user_command(&mut self, command: &str) -> ToolOutcome {
        let outcome = match self.registry.get("bash") {
            Some(tool) => tool.run("user", json!({"command": command}), &self.ctx).await,
            None => ToolOutcome::error("user", "bash tool unavailable"),
        };
        self.pending_context.push(format!(
            "Output of the command `{command}` run by the user{}:\n```\n{}\n```",
            if outcome.is_error { " (failed)" } else { "" },
            outcome.content.trim_end()
        ));
        outcome
    }

    /// The folder this agent works in.
    pub fn workspace_root(&self) -> &std::path::Path {
        self.ctx.workspace.root()
    }

    /// Plugs in the parking of the server cache of `endpoint` (a single-slot local server).
    pub fn set_parking(&mut self, endpoint: impl Into<String>, parking: Arc<dyn Parking>) {
        self.parking = Some((endpoint.into(), parking));
    }

    pub fn parking(&self) -> Option<Arc<dyn Parking>> {
        self.parking.as_ref().map(|(_, p)| p.clone())
    }

    /// Parks this conversation if it is worth it; returns the key to unpark with.
    async fn park_main(&self, endpoint: &str) -> Option<String> {
        let (parked_endpoint, parking) = self.parking.as_ref()?;
        let key = self.session_key.clone()?;
        if parked_endpoint != endpoint
            || self.parallel_limit_at(endpoint) > 1
            || self.session.estimated_prompt_tokens() < PARK_MIN_TOKENS
        {
            return None;
        }
        parking.park(&key).await.then_some(key)
    }

    async fn unpark_main(&self, key: Option<String>) {
        if let (Some(key), Some((_, parking))) = (key, &self.parking) {
            parking.unpark(&key).await;
        }
    }

    /// Opens a folder outside the workspace for writing, changes there needing no approval: the
    /// user switched Po Labs on so that Po changes its own source without asking each time.
    pub fn open_for_changes(&mut self, dir: &std::path::Path) {
        self.ctx.workspace.allow_write(dir);
        let dir = po_tools::workspace::canonical(dir.to_path_buf());
        if !self.approved_dirs.contains(&dir) {
            self.approved_dirs.push(dir);
        }
    }

    /// Undoes [`open_for_changes`](Self::open_for_changes).
    pub fn close_for_changes(&mut self, dir: &std::path::Path) {
        self.ctx.workspace.forbid_write(dir);
        let dir = po_tools::workspace::canonical(dir.to_path_buf());
        self.approved_dirs.retain(|d| *d != dir);
    }

    /// A file change inside a folder opened with `open_for_changes`.
    fn pre_approved(&self, name: &str, input: &Value) -> bool {
        if !matches!(name, "edit" | "write") || self.approved_dirs.is_empty() {
            return false;
        }
        let Some(path) = input.get("path").and_then(Value::as_str) else {
            return false;
        };
        self.ctx.workspace.resolve(path).is_ok_and(|resolved| self.approved_dirs.iter().any(|d| resolved.starts_with(d)))
    }

    /// Delivers a message the user typed while the agent was working, at a safe point: after the
    /// tool results, before the model thinks again. It joins the results message, so the model
    /// reads it right away instead of at the end of the whole turn.
    pub fn inject_user_message(&mut self, text: &str) {
        let block = ContentBlock::text(format!("{}\n\n{}", text.trim(), session::INJECTED_NOTE));
        match self.session.messages.last_mut() {
            Some(last) if last.role == po_core::Role::User => last.content.push(block),
            _ => self.session.push(Message { role: po_core::Role::User, content: vec![block] }),
        }
    }

    /// Note attached to the user's next request (a setting that changes the model's behavior,
    /// for example).
    pub fn add_context(&mut self, text: impl Into<String>) {
        self.pending_context.push(text.into());
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The registry changes mid-session when an MCP server is plugged in or removed. The tool
    /// list is part of the prefix sent to the model: changing it costs one prompt reread.
    pub fn registry_mut(&mut self) -> &mut Registry {
        &mut self.registry
    }

    /// Estimated size of a full request: history, system prompt and tool declarations. The
    /// latter often weigh more than the conversation at the start of a session.
    pub fn estimated_request_tokens(&self) -> u32 {
        let tools: u32 = self
            .registry
            .specs()
            .iter()
            .map(|spec| session::estimate_tokens(&serde_json::to_string(&spec.input_schema).unwrap_or_default())
                + session::estimate_tokens(&spec.description)
                + session::estimate_tokens(&spec.name))
            .sum();
        self.session.estimated_prompt_tokens() + tools
    }

    pub fn last_call(&self) -> Option<CallStats> {
        self.last_call
    }

    /// Enables or disables delegation to subagents.
    pub fn set_delegation(&mut self, enabled: bool) {
        self.delegation_enabled = enabled;
    }

    pub fn delegation_enabled(&self) -> bool {
        self.delegation_enabled
    }

    /// Applies the session settings to a request.
    fn configure(&self, request: Completion) -> Completion {
        apply_settings(&self.settings, request)
    }

    /// Associates a provider with a model. Several models may share a provider.
    pub fn attach(&mut self, model_id: impl Into<String>, provider: Arc<dyn Provider>) {
        self.providers.insert(model_id.into(), provider);
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    /// A full turn: the user's request, then as many tool round trips as the model asks for,
    /// until it hands control back.
    pub async fn turn(
        &mut self,
        input: &str,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Result<(), AgentError> {
        let mode = self.work_mode.get();
        if mode != self.announced_mode {
            self.pending_context.push(mode.note().to_owned());
            self.announced_mode = mode;
        }
        // What the session can read: one model that sees is enough, the router will pick it
        // for a message carrying an image.
        let media = self.router.models().iter().fold(po_tools::Media::default(), |acc, m| {
            let own = media_of(&m.capabilities);
            po_tools::Media { image: acc.image || own.image, video: acc.video || own.video, audio: acc.audio || own.audio }
        });
        self.ctx.media = media;
        let input = if self.pending_context.is_empty() {
            input.to_owned()
        } else {
            let mut joined = self.pending_context.drain(..).collect::<Vec<_>>().join("\n\n");
            joined.push_str("\n\n");
            joined.push_str(input);
            joined
        };
        let (message, attachments) = mentions::build_user_message(&input, &self.ctx.workspace, media);
        for attachment in attachments {
            sink(AgentEvent::Notice(match attachment {
                mentions::Attachment::Text { path, lines, truncated: false } => {
                    po_i18n::t!("agent.attach.text", path = path, lines = lines)
                }
                mentions::Attachment::Text { path, lines, truncated: true } => {
                    po_i18n::t!("agent.attach.text_truncated", path = path, lines = lines)
                }
                mentions::Attachment::Image { path, bytes, kind } => {
                    po_i18n::t!("agent.attach.media", kind = kind.label(), path = path, kb = bytes.div_ceil(1024))
                }
                mentions::Attachment::SkippedImage { path, kind } => {
                    po_i18n::t!("agent.attach.skipped", path = path, kind = kind.label())
                }
            }));
        }
        self.session.push(message);
        self.paused = None;
        self.pause.store(false, Ordering::Relaxed);
        let state = TurnState {
            start: 0,
            usage: Usage::default(),
            cost_before: self.session.ledger.total_cost(),
            verification_round: 0,
            continuations: 0,
        };
        self.run_loop(state, sink).await
    }

    /// The turn stopped at the interface's request, between two model calls: settings are
    /// waiting to be applied before continuing.
    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    /// Flag the interface raises to request a pause at the next safe point: right after the
    /// tool results, before calling the model again.
    pub fn pause_handle(&self) -> Arc<AtomicBool> {
        self.pause.clone()
    }

    /// Resumes a paused turn, where it stopped: the subagents paused with it go on first, from
    /// their own conversations, and their results replace what stood for them.
    pub async fn resume(&mut self, sink: &mut (impl FnMut(AgentEvent) + Send)) -> Result<(), AgentError> {
        let Some(state) = self.paused.take() else {
            return Ok(());
        };
        self.pause.store(false, Ordering::Relaxed);
        let mut still = Vec::new();
        for paused in std::mem::take(&mut self.paused_subagents) {
            let outcome = match self.prepare_subagent(&paused.call_id, &paused.input, sink) {
                Ok((mut job, _)) => {
                    job.resume_from = Some(paused.messages.clone());
                    job.pause = Some(self.pause.clone());
                    if let Some(monitor) = &self.monitor {
                        let _ = monitor.send(SubagentUpdate::Started {
                            id: job.call_id.clone(),
                            agent: job.agent.clone(),
                            model: job.model.display_name.clone(),
                            task: paused.input.get("task").and_then(Value::as_str).unwrap_or("").to_owned(),
                            background: false,
                        });
                    }
                    let shared = self.ledger.clone().map(|l| l.take(&endpoint_key(&job.model)));
                    let result = run_subagent(job).await;
                    drop(shared);
                    for (usage, elapsed) in &result.calls {
                        self.session.record(&result.model.id, *usage, &result.model.pricing, *elapsed);
                    }
                    if let Some(messages) = result.paused {
                        still.push(PausedSubagent { messages, ..paused });
                        continue;
                    }
                    sink(AgentEvent::ToolFinished {
                        name: "task".into(),
                        summary: po_i18n::t!(
                            "agent.display.subagent_line",
                            agent = result.agent,
                            line = result.outcome.content.lines().next().unwrap_or_default()
                        ),
                        is_error: result.outcome.is_error,
                    });
                    result.outcome
                }
                Err(outcome) => outcome,
            };
            self.replace_result(&paused.call_id, outcome);
        }
        if !still.is_empty() {
            // Paused again before finishing: the turn stays paused.
            self.paused_subagents = still;
            self.paused = Some(state);
            return Ok(());
        }
        self.run_loop(state, sink).await
    }

    /// Puts the result of call `id` in place of what stood for it in the last tool results.
    fn replace_result(&mut self, id: &str, outcome: ToolOutcome) {
        for message in self.session.messages.iter_mut().rev() {
            for block in &mut message.content {
                if let ContentBlock::ToolResult { tool_use_id, content, is_error } = block
                    && tool_use_id == id
                {
                    *content = outcome.content;
                    *is_error = outcome.is_error;
                    return;
                }
            }
        }
    }

    /// The paused turn, to save with the session: its paused subagents (possibly none).
    /// `None` when no turn is paused.
    pub fn paused_turn(&self) -> Option<Vec<PausedSubagent>> {
        self.paused.as_ref().map(|_| self.paused_subagents.clone())
    }

    /// Restores a turn saved paused (a session resumed after a restart): it goes on at the next
    /// `resume`, from where it stopped.
    pub fn restore_paused(&mut self, subagents: Vec<PausedSubagent>) {
        self.paused = Some(TurnState {
            start: 0,
            usage: Usage::default(),
            cost_before: self.session.ledger.total_cost(),
            verification_round: 0,
            continuations: 0,
        });
        self.paused_subagents = subagents;
    }

    async fn run_loop(
        &mut self,
        state: TurnState,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Result<(), AgentError> {
        self.hold_turn_slot();
        let result = self.run_steps(state, sink).await;
        self.turn_slot = None;
        result
    }

    async fn run_steps(
        &mut self,
        state: TurnState,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Result<(), AgentError> {
        let TurnState { start, usage: mut turn_usage, cost_before, mut verification_round, mut continuations } = state;
        let max_steps = self.max_steps;
        for iteration in start..max_steps {
            // Compact **before** choosing: the router rules out any model whose window cannot
            // absorb the prompt, so a history that has grown too long would leave no
            // candidate. The session would then fail on "no model available" instead of
            // shrinking — and stay blocked for good.
            let largest = self
                .router
                .models()
                .iter()
                .map(|model| model.context_window)
                .max()
                .unwrap_or(0);
            if largest > 0 && self.session.needs_compaction(largest, RESERVE_TOKENS) {
                let by_model = self.compact_history(largest).await;
                sink(AgentEvent::Notice(
                    if by_model {
                        po_i18n::t!("agent.notice.history_summarized")
                    } else {
                        po_i18n::t!("agent.notice.history_truncated")
                    }
                    .to_owned(),
                ));

                // Removing messages is not always enough: only one, huge, may remain. Their
                // content is then reduced, otherwise the session would stay above the limit and
                // thus blocked.
                if self
                    .session
                    .shrink_to(largest.saturating_sub(RESERVE_TOKENS))
                {
                    sink(AgentEvent::Notice(po_i18n::t!("agent.notice.messages_shrunk").to_owned()));
                }
            }

            let (model, decision) = self.pick(TaskClass::Orchestration)?;
            self.orchestrator = Some(model.clone());

            if iteration == 0 || decision.reason.is_noteworthy() {
                sink(AgentEvent::ModelSelected {
                    model: model.display_name.clone(),
                    class: TaskClass::Orchestration,
                    reason: decision.reason.clone(),
                });
            }

            // Warn before compacting: the user can then decide to split their work rather than
            // let the agent lose the start of the conversation.
            let used = self.session.estimated_prompt_tokens();
            let budget = model.context_window.saturating_sub(RESERVE_TOKENS);
            if budget > 0
                && (used as f32) > (budget as f32 * COMPACTION_WARNING_RATIO)
                && !self.session.needs_compaction(model.context_window, RESERVE_TOKENS)
            {
                sink(AgentEvent::Notice(po_i18n::t!(
                    "agent.notice.context_warning",
                    pct = (used * 100 / budget).min(100),
                    left = budget.saturating_sub(used)
                )));
            }

            // Compaction is decided on the *chosen* model's window: switching to a short-window
            // local model can make it necessary when it was not before.
            // Second pass: the chosen model may have a shorter window than the largest in the
            // fleet — which is the case when switching to a local model.
            if self.session.needs_compaction(model.context_window, RESERVE_TOKENS) {
                let by_model = self.compact_history(model.context_window).await;
                sink(AgentEvent::Notice(if by_model {
                    po_i18n::t!("agent.notice.history_summarized_for", model = model.display_name)
                } else {
                    po_i18n::t!("agent.notice.history_truncated_for", model = model.display_name)
                }));
            }

            // A local model costs nothing in money, but it costs compute time and VRAM.
            // Measuring only tokens would give the illusion that local is free, whereas that is
            // precisely the trade-off the user must be able to make.
            let started = std::time::Instant::now();
            let mut first_output: Option<std::time::Instant> = None;
            let response = {
                let mut observed = |event: AgentEvent| {
                    if first_output.is_none()
                        && matches!(event, AgentEvent::TextDelta(_) | AgentEvent::ThinkingDelta(_))
                    {
                        first_output = Some(std::time::Instant::now());
                    }
                    sink(event)
                };
                self.call_resilient(&model, &mut observed).await?
            };
            let elapsed = started.elapsed().as_secs_f64();
            let stats = first_output.map(|first| {
                let ttft = first.duration_since(started).as_secs_f64();
                CallStats::measure(ttft, (elapsed - ttft).max(0.0), &response.usage)
            });
            turn_usage += response.usage;
            self.session.record(&model.id, response.usage, &model.pricing, elapsed);

            for block in &response.message.content {
                match block {
                    ContentBlock::Thinking { text, .. } if !text.is_empty() => {
                        sink(AgentEvent::Thinking(text.clone()))
                    }
                    ContentBlock::Text { text } if !text.is_empty() => {
                        sink(AgentEvent::Text(text.clone()))
                    }
                    _ => {}
                }
            }
            // After the text: the speed line must not stick to a response still being written
            // on screen.
            if let Some(stats) = stats {
                self.last_call = Some(stats);
                sink(AgentEvent::CallStats(stats));
            }

            let wants_tools = response.wants_tools();
            let truncated = response.stop_reason == StopReason::MaxTokens && !response.message.has_tool_use();
            let final_text = response.message.visible_text();
            self.session.push(response.message.clone());

            if !wants_tools && truncated {
                if continuations < MAX_CONTINUATIONS {
                    continuations += 1;
                    sink(AgentEvent::Notice(po_i18n::t!(
                        "agent.notice.output_cut",
                        n = continuations,
                        max = MAX_CONTINUATIONS
                    )));
                    self.session.push(Message::user(CONTINUE_PROMPT));
                    continue;
                }
                sink(AgentEvent::Notice(po_i18n::t!("agent.notice.output_cut_again").to_owned()));
                break;
            }

            // The model wants to hand control back while subagents are still working for it: we
            // wait for them, and it resumes with their results. Handing back control without
            // them would leave their work without a recipient.
            if !wants_tools && !self.background.is_empty() {
                sink(AgentEvent::Notice(match self.background.len() {
                    1 => po_i18n::t!("agent.notice.waiting_background_one").to_owned(),
                    n => po_i18n::t!("agent.notice.waiting_background", n = n),
                }));
                let text = self.collect_background(true, sink).await;
                self.session.push(Message::user(text));
                continue;
            }

            if !wants_tools {
                if self.settings.criteria.is_empty() {
                    break;
                }
                // Acceptance criteria: control is only handed back once the model has gone
                // through an explicit verification, or after the allowed number of retries.
                if verification_round > 0 {
                    let (ok, failed) = count_verdicts(&final_text);
                    let total = self.settings.criteria.len();
                    if failed.is_empty() && ok >= total {
                        sink(AgentEvent::Notice(po_i18n::t!("agent.notice.criteria_ok", ok = ok, total = total)));
                        break;
                    }
                    if verification_round > MAX_VERIFICATION_ROUNDS {
                        sink(AgentEvent::Notice(po_i18n::t!(
                            "agent.notice.criteria_giveup",
                            ok = ok,
                            total = total,
                            max = MAX_VERIFICATION_ROUNDS,
                            failed = if failed.is_empty() { String::new() } else { format!(" ({})", failed.join(" ; ")) }
                        )));
                        break;
                    }
                    sink(AgentEvent::Notice(po_i18n::t!(
                        "agent.notice.criteria_again",
                        ok = ok,
                        total = total,
                        round = verification_round,
                        max = MAX_VERIFICATION_ROUNDS
                    )));
                } else {
                    sink(AgentEvent::Notice(po_i18n::t!("agent.notice.criteria_checking").to_owned()));
                }
                verification_round += 1;
                self.session.push(Message::user(verification_prompt(&self.settings.criteria)));
                continue;
            }

            let outcomes = self.run_tools(&response.message, sink).await;
            let mut blocks = result_blocks(&outcomes);
            // Background subagents that finished in the meantime hand in their result with the
            // tools': the model receives them as early as possible, without an extra message.
            if self.background.iter().any(|job| job.handle.is_finished()) {
                blocks.push(ContentBlock::text(self.collect_background(false, sink).await));
            }
            self.session.push(Message::tool_results(blocks));

            // Safe point: the history is complete (calls and results). Settings changed during
            // the turn apply here, before the model's next action.
            if iteration + 1 < max_steps && self.pause.swap(false, Ordering::Relaxed) {
                self.paused = Some(TurnState {
                    start: iteration + 1,
                    usage: turn_usage,
                    cost_before,
                    verification_round,
                    continuations,
                });
                return Ok(());
            }

            if iteration == max_steps - 1 {
                sink(AgentEvent::Notice(po_i18n::t!("agent.notice.loop_limit", n = max_steps)));
            }
        }

        sink(AgentEvent::TurnCompleted {
            usage: turn_usage,
            cost: self.session.ledger.total_cost() - cost_before,
            local_share: self.session.ledger.local_share(),
            local_seconds: self.session.ledger.total_local_seconds(),
        });
        Ok(())
    }

    /// Runs a message's tool calls, in order, applying the permissions. Consecutive `task`
    /// calls go out together: their subagents run in parallel within the server's slots.
    async fn run_tools(
        &mut self,
        message: &Message,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Vec<ToolOutcome> {
        let calls: Vec<(String, String, Value)> = message
            .tool_uses()
            .map(|(id, name, input)| (id.to_owned(), name.to_owned(), input.clone()))
            .collect();

        let mut outcomes = Vec::new();
        let mut tasks: Vec<(String, Value)> = Vec::new();
        for (index, (id, name, input)) in calls.iter().cloned().enumerate() {
            if name == "question" {
                outcomes.push(self.ask_user(&id, &input, sink).await);
                continue;
            }
            if name == RECALL {
                sink(AgentEvent::ToolStarted { name: name.clone(), summary: summarize_call(&name, &input) });
                let outcome = self.recall(&id, &input);
                sink(AgentEvent::ToolFinished {
                    name,
                    summary: outcome.display.clone().unwrap_or_default(),
                    is_error: outcome.is_error,
                });
                outcomes.push(outcome);
                continue;
            }
            if name == "task" {
                tasks.push((id, input));
                let next_is_task = calls.get(index + 1).is_some_and(|(_, n, _)| n == "task");
                if !next_is_task {
                    outcomes.extend(self.delegate(std::mem::take(&mut tasks), sink).await);
                }
                continue;
            }

            let Some(tool) = self.registry.get(&name) else {
                outcomes.push(
                    self.registry
                        .dispatch(&id, &name, input, &self.ctx)
                        .await,
                );
                continue;
            };

            let summary = summarize_call(&name, &input);
            let permission = if self.pre_approved(&name, &input) { Permission::Allow } else { tool.permission(&input) };
            match permission {
                Permission::Deny => {
                    sink(AgentEvent::ToolDenied { name: name.clone(), summary });
                    outcomes.push(ToolOutcome::error(
                        &id,
                        "call refused by the harness security policy",
                    ));
                    continue;
                }
                Permission::Ask if self.work_mode.get().gate() == Gate::Refuse => {
                    sink(AgentEvent::ToolDenied { name: name.clone(), summary: po_i18n::t!("agent.display.plan_mode", summary = summary) });
                    outcomes.push(ToolOutcome::error(&id, work_mode::PLAN_REFUSAL));
                    continue;
                }
                Permission::Ask => {
                    let approvals = self.approvals.clone();
                    let granted = self.work_mode.get().gate() == Gate::Allow || {
                        let _turn = approvals.lock().await;
                        self.approver.approve(&name, &input).await
                    };
                    if !granted {
                        sink(AgentEvent::ToolDenied { name: name.clone(), summary });
                        // The refusal goes back to the model as a result: it must be able to
                        // propose something else rather than stay stuck.
                        outcomes.push(ToolOutcome::error(
                            &id,
                            "the user refused this call — propose another approach",
                        ));
                        continue;
                    }
                }
                Permission::Allow => {}
            }

            sink(AgentEvent::ToolStarted { name: name.clone(), summary });
            let outcome = self.registry.dispatch(&id, &name, input, &self.ctx).await;
            sink(AgentEvent::ToolFinished {
                name,
                summary: outcome.display.clone().unwrap_or_else(|| {
                    outcome.content.lines().next().unwrap_or_default().to_owned()
                }),
                is_error: outcome.is_error,
            });
            if let Some(diff) = &outcome.diff {
                sink(AgentEvent::ToolDiff { diff: diff.clone() });
            }
            outcomes.push(outcome);
        }
        outcomes
    }

    /// Tools declared to the main agent. Subagents on the same server receive exactly the same
    /// list, to share the already cached prefix.
    fn main_tools(&self, model: &ModelSpec) -> Vec<ToolSpec> {
        let mut tools = self.registry.specs();
        if self.asker.is_some() {
            tools.push(question::tool_spec());
        }
        // Offered once something was compacted away: before, there is nothing to search.
        if !self.session.archive.is_empty() {
            tools.push(ToolSpec::new(
                RECALL,
                "Searches the part of this conversation that was compacted away, word for word: file contents read, \
                 command outputs, decisions. Use it when the summary lacks an exact detail, instead of redoing the work.",
                json!({
                    "type": "object",
                    "properties": {"query": {"type": "string", "description": "Words that must all appear, e.g. a function or file name"}},
                    "required": ["query"]
                }),
            ));
        }
        if self.delegation_enabled && !self.profiles.is_empty() {
            let parallel = self.parallel_limit(model);
            tools.push(task_tool_spec(&self.profiles, parallel, parallel >= 2));
        }
        tools
    }

    /// Puts the model's question to the user, one at a time, and returns their answer.
    async fn ask_user(&mut self, id: &str, input: &Value, sink: &mut (impl FnMut(AgentEvent) + Send)) -> ToolOutcome {
        let parsed = match question::parse(input) {
            Ok(q) => q,
            Err(err) => return ToolOutcome::error(id, err),
        };
        let answer = match self.asker.clone() {
            Some(asker) => {
                let approvals = self.approvals.clone();
                let _turn = approvals.lock().await;
                asker.ask(&parsed).await
            }
            None => None,
        };
        let (for_model, shown) = question::outcome(&parsed, &answer);
        sink(AgentEvent::Asked { question: parsed.question.clone(), answer: shown });
        ToolOutcome::ok(id, for_model)
    }

    /// Background subagents running on a server.
    fn background_on(&self, endpoint: &str) -> usize {
        self.background.iter().filter(|job| job.endpoint == endpoint && !job.handle.is_finished()).count()
    }

    /// Can a subagent go to the background on this model? Only if a slot remains once the main
    /// agent — which will keep generating — and the background tasks already launched on this
    /// server are counted. Otherwise the main agent would wait anyway, in the server's queue,
    /// without knowing it.
    fn can_run_in_background(&self, model: &ModelSpec) -> bool {
        let endpoint = endpoint_key(model);
        if self.ledger.is_some() {
            // The ledger already counts this turn's slot.
            return self.held_on(&endpoint) < self.parallel_limit(model);
        }
        let main_here = self.session_model().is_some_and(|s| endpoint_key(&s) == endpoint);
        self.background_on(&endpoint) + usize::from(main_here) < self.parallel_limit(model)
    }

    /// A subagent's model: the user's setting, otherwise the profile's, otherwise the session's
    /// model. Also returns a note to display, if any.
    fn subagent_model(&mut self, profile: &Profile, prompt_tokens: u32) -> Result<(ModelSpec, Option<String>), String> {
        match self.model_choice(profile).as_str() {
            "session" => self
                .session_model()
                .map(|m| (m, None))
                .ok_or_else(|| "no model available for the session".to_owned()),
            "auto" => self
                .pick_for_subagent(profile.class(), prompt_tokens)
                .map(|(m, d)| {
                    let note = d.reason.is_noteworthy().then(|| d.reason.explain().to_owned());
                    (m, note)
                })
                .map_err(|e| e.to_string()),
            id => match self.router.get(id) {
                Some(model) if !self.router.is_disabled(id) => Ok((model.clone(), None)),
                _ => Err(format!(
                    "model \"{id}\" unavailable for subagent {} — see /modeles, or /agents modele {} session",
                    profile.name, profile.name
                )),
            },
        }
    }

    /// Prepares the job of a `task` call. Returns the job and whether it should go to the background.
    // The error is the result handed to the model, built at most once per call: boxing it would
    // only add noise.
    #[allow(clippy::result_large_err)]
    fn prepare_subagent(
        &mut self,
        id: &str,
        input: &Value,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Result<(SubagentJob, bool), ToolOutcome> {
        if !self.delegation_enabled {
            return Err(ToolOutcome::error(id, "a subagent cannot delegate in turn"));
        }
        let Some(task) = input.get("task").and_then(Value::as_str) else {
            return Err(ToolOutcome::error(id, "parameter \"task\" missing"));
        };
        let name = input
            .get("agent")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| input.get("class").and_then(Value::as_str).map(|c| legacy_role(c).to_owned()))
            .unwrap_or_else(|| "explorer".to_owned());
        let Some(profile) = self.profiles.iter().find(|p| p.name == name).cloned() else {
            let names: Vec<&str> = self.profiles.iter().map(|p| p.name.as_str()).collect();
            return Err(ToolOutcome::error(
                id,
                format!("unknown subagent \"{name}\" — available: {}", names.join(", ")),
            ));
        };

        // The subagent only receives its instruction and its own system prompt: that size is
        // what decides whether it fits in a small window, not the session's.
        let prompt_tokens = session::estimate_tokens(task) + SUBAGENT_SYSTEM_TOKENS;
        let (model, note) = self.subagent_model(&profile, prompt_tokens).map_err(|e| ToolOutcome::error(id, e))?;
        let model_capabilities = model.capabilities;
        let Some(provider) = self.providers.get(&model.id).cloned() else {
            return Err(ToolOutcome::error(id, format!("no provider for {}", model.id)));
        };
        if let Some(note) = note {
            sink(AgentEvent::Notice(po_i18n::t!(
                "agent.notice.subagent_model",
                agent = profile.name,
                model = model.display_name,
                note = note
            )));
        }
        let same_server = self.session_model().is_some_and(|s| endpoint_key(&s) == endpoint_key(&model));

        const READ_ONLY: [&str; 4] = ["read", "list", "glob", "grep"];
        let rules = format!(
            "You are the subagent \"{}\" of a development agent. You do not see its conversation: \
             only the task you received matters. Follow these rules to the letter:\n{}\n\n\
             Before each tool call, write a single short line starting with \"Progress:\" \
             that says where you stand (what you just learned, what you are doing now): the user \
             sees it live. Then return a concise, directly usable result, without preamble. \
             Answer in the language of the task.",
            profile.name,
            profile.rules.trim()
        );
        // On the session's server, the subagent reuses the main agent's system prompt and tools,
        // and receives its role in its first message. Measured on Strata (single slot): with a
        // prompt of its own, returning to the main conversation reread everything (0 tokens
        // reused); with the shared prefix, the subagent starts from the cache and the main
        // conversation resumes at least its system prompt and tools.
        let session_tools = self.session_model().map(|m| self.main_tools(&m)).unwrap_or_default();
        let (system, tools, allowed, task) = if same_server {
            let allowed = match profile.tools {
                ToolScope::Full => None,
                ToolScope::ReadOnly => Some(READ_ONLY.iter().map(|n| (*n).to_owned()).collect()),
            };
            let message = format!("[Subagent \"{}\"]\n{rules}\n\nTask:\n{task}", profile.name);
            (self.session.system.clone(), session_tools, allowed, message)
        } else {
            // Elsewhere, nothing to share: a short prompt and only the useful tools cost fewer
            // tokens every turn, which matters on a small model.
            let registry = match profile.tools {
                ToolScope::Full => self.registry.clone(),
                ToolScope::ReadOnly => self.registry.subset(&READ_ONLY),
            };
            let mut system = rules;
            if !self.subagent_preamble.is_empty() {
                system.push_str("\n\n");
                system.push_str(&self.subagent_preamble);
            }
            (system, registry.specs(), None, task.to_owned())
        };
        let wants_background = input.get("background").and_then(Value::as_bool).unwrap_or(false);
        let job = SubagentJob {
            pause: None,
            resume_from: None,
            call_id: id.to_owned(),
            agent: profile.name.clone(),
            model,
            provider,
            registry: self.registry.clone(),
            ctx: ToolContext { media: media_of(&model_capabilities), ..self.ctx.clone() },
            approver: self.approver.clone(),
            approvals: self.approvals.clone(),
            work_mode: self.work_mode.clone(),
            settings: self.settings.clone(),
            system,
            tools,
            allowed,
            task,
            monitor: self.monitor.clone(),
        };
        Ok((job, wants_background))
    }

    /// Hands tasks to subagents. Those that go to the background immediately return a launch
    /// acknowledgement; the others are awaited, in parallel within each server's slots.
    async fn delegate(
        &mut self,
        calls: Vec<(String, Value)>,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Vec<ToolOutcome> {
        let mut outcomes: Vec<Option<ToolOutcome>> = vec![None; calls.len()];
        let mut waited: Vec<(usize, SubagentJob)> = Vec::new();
        for (index, (id, input)) in calls.iter().enumerate() {
            let (job, wants_background) = match self.prepare_subagent(id, input, sink) {
                Ok(value) => value,
                Err(outcome) => {
                    outcomes[index] = Some(outcome);
                    continue;
                }
            };
            // The instruction as the model wrote it, not the subagent's full message (which
            // starts with its rules): that is what the user wants to see go by.
            let task_line = input.get("task").and_then(Value::as_str).unwrap_or("").to_owned();
            if wants_background {
                if self.can_run_in_background(&job.model) {
                    let label = format!("t{}", self.next_background);
                    self.next_background += 1;
                    sink(AgentEvent::Delegated {
                        model: job.model.display_name.clone(),
                        agent: job.agent.clone(),
                        task: task_line,
                        background: true,
                    });
                    if let Some(monitor) = &self.monitor {
                        let _ = monitor.send(SubagentUpdate::Started {
                            id: job.call_id.clone(),
                            agent: job.agent.clone(),
                            model: job.model.display_name.clone(),
                            task: input.get("task").and_then(Value::as_str).unwrap_or("").to_owned(),
                            background: true,
                        });
                    }
                    let agent = job.agent.clone();
                    let endpoint = endpoint_key(&job.model);
                    outcomes[index] = Some(ToolOutcome::ok(
                        id,
                        format!(
                            "Subagent {agent} started in the background ({label}). Continue your work: \
                             its result will be delivered to you as soon as it is ready."
                        ),
                    ).with_display(po_i18n::t!("agent.display.subagent_background", agent = agent, label = label)));
                    let slot = self.ledger.as_ref().map(|l| l.take(&endpoint));
                    let handle = tokio::spawn(async move {
                        let _slot = slot;
                        run_subagent(job).await
                    });
                    self.background.push(BackgroundJob { label, agent, endpoint, handle });
                    continue;
                }
                sink(AgentEvent::Notice(po_i18n::t!("agent.notice.no_free_slot", agent = job.agent)));
            }
            sink(AgentEvent::Delegated {
                model: job.model.display_name.clone(),
                agent: job.agent.clone(),
                task: task_line,
                background: false,
            });
            if let Some(monitor) = &self.monitor {
                let _ = monitor.send(SubagentUpdate::Started {
                    id: job.call_id.clone(),
                    agent: job.agent.clone(),
                    model: job.model.display_name.clone(),
                    task: input.get("task").and_then(Value::as_str).unwrap_or("").to_owned(),
                    background: false,
                });
            }
            waited.push((index, job));
        }

        // One limit per server: the slots left by the background tasks running there. The main
        // agent waits for these results and occupies no slot in the meantime.
        let mut limits: HashMap<String, Arc<tokio::sync::Semaphore>> = HashMap::new();
        // Waiting, the turn holds no slot: its subagents may take it.
        let held_turn = !waited.is_empty() && self.turn_slot.take().is_some();
        for (_, job) in &waited {
            let endpoint = endpoint_key(&job.model);
            let free = self.parallel_limit(&job.model).saturating_sub(self.held_on(&endpoint)).max(1);
            limits.entry(endpoint).or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(free)));
        }
        // A subagent on the session's single-slot server takes the slot: the conversation's cache
        // goes to disk meanwhile and comes back after, instead of being read again in full.
        let shared = self.orchestrator.as_ref().map(endpoint_key);
        let parked = match shared.filter(|e| waited.iter().any(|(_, job)| endpoint_key(&job.model) == *e)) {
            Some(endpoint) => self.park_main(&endpoint).await,
            None => None,
        };
        let calls_by_index: Vec<(String, Value)> = calls.clone();
        let runs = waited.into_iter().map(|(index, mut job)| {
            job.pause = Some(self.pause.clone());
            let limit = limits[&endpoint_key(&job.model)].clone();
            let ledger = self.ledger.clone();
            async move {
                let _slot = limit.acquire_owned().await;
                let _shared = ledger.map(|l| l.take(&endpoint_key(&job.model)));
                (index, run_subagent(job).await)
            }
        });
        for (index, result) in futures_util::future::join_all(runs).await {
            for (usage, elapsed) in &result.calls {
                self.session.record(&result.model.id, *usage, &result.model.pricing, *elapsed);
            }
            if let Some(messages) = result.paused {
                let (call_id, input) = calls_by_index[index].clone();
                self.paused_subagents.push(PausedSubagent { call_id, input, messages });
                outcomes[index] = Some(result.outcome);
                continue;
            }
            sink(AgentEvent::ToolFinished {
                name: "task".into(),
                summary: result.outcome.display.clone().unwrap_or_else(|| {
                    po_i18n::t!(
                        "agent.display.subagent_line",
                        agent = result.agent,
                        line = result.outcome.content.lines().next().unwrap_or_default()
                    )
                }),
                is_error: result.outcome.is_error,
            });
            outcomes[index] = Some(result.outcome);
        }
        self.unpark_main(parked).await;
        if held_turn {
            self.hold_turn_slot();
        }
        outcomes.into_iter().flatten().collect()
    }

    /// Collects the finished background subagents — or all of them, by waiting. Returns the
    /// text that hands their results to the model.
    async fn collect_background(&mut self, wait_all: bool, sink: &mut (impl FnMut(AgentEvent) + Send)) -> String {
        let (done, running): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.background).into_iter().partition(|job| wait_all || job.handle.is_finished());
        self.background = running;
        let mut text = String::new();
        for job in done {
            let result = match job.handle.await {
                Ok(result) => result,
                Err(err) => {
                    text.push_str(&format!("[Subagent {} ({}) interrupted: {err}]\n\n", job.agent, job.label));
                    continue;
                }
            };
            for (usage, elapsed) in &result.calls {
                self.session.record(&result.model.id, *usage, &result.model.pricing, *elapsed);
            }
            sink(AgentEvent::ToolFinished {
                name: "task".into(),
                summary: po_i18n::t!("agent.display.subagent_done", agent = result.agent, label = job.label),
                is_error: result.outcome.is_error,
            });
            text.push_str(&format!(
                "[Result of subagent {} started in the background ({}){}]\n{}\n\n",
                result.agent,
                job.label,
                if result.outcome.is_error { ", failed" } else { "" },
                result.outcome.content.trim()
            ));
        }
        text.trim_end().to_owned()
    }

    /// Stops the background subagents (conversation cleared, session closed). Returns their count.
    pub fn cancel_background(&mut self) -> usize {
        let count = self.background.len();
        for job in self.background.drain(..) {
            job.handle.abort();
        }
        count
    }

    /// Compaction requested by the user (`/compact`). Returns the estimated sizes before and
    /// after, and whether the summary comes from a model (otherwise, from truncation).
    pub async fn compact_now(&mut self) -> (u32, u32, bool) {
        let before = self.session.estimated_prompt_tokens();
        if self.session.messages.len() <= 6 {
            return (before, before, false);
        }
        let original = (self.session.messages.clone(), self.session.archive.len());
        let window = self.session_model().map_or(32_768, |m| m.context_window);
        let by_model = self.compact_history(window).await;
        let after = self.session.estimated_prompt_tokens();
        // On a short history, the summary can be longer than what it replaces (observed:
        // 456 → 473 tokens). Compacting would then only have drawbacks: we keep the original,
        // and its cache.
        if after >= before {
            self.session.messages = original.0;
            self.session.archive.truncate(original.1);
            return (before, before, by_model);
        }
        (before, after, by_model)
    }

    /// Produces a summary of the history, preferably by entrusting it to a model.
    ///
    /// Truncating loses the start of the conversation without anyone knowing what disappeared.
    /// A summary keeps the decisions made and the files touched. This is typically a task of
    /// class `summarize`: the router will send it to the smallest sufficient model, often
    /// local — which makes compaction nearly free.
    ///
    /// The fallback to truncation is not a detail: compaction happens precisely when the
    /// context overflows, which is the worst moment to depend on a network call. A failure here
    /// must never interrupt the session.
    /// Compacts so that the conversation fits `window` again: the latest fifth of the window
    /// stays word for word, the rest becomes a summary — of that part only — and goes to the
    /// archive that `recall` searches. Returns whether a model wrote the summary.
    async fn compact_history(&mut self, window: u32) -> bool {
        let cut = self.session.cut_for_budget(window / 5);
        if cut == 0 {
            return false;
        }
        let (summary, by_model) = self.summarize_history(cut).await;
        self.session.compact_at(summary, cut);
        // The parked cache was of the history before: it matches nothing anymore.
        if let (Some((_, parking)), Some(key)) = (&self.parking, &self.session_key) {
            parking.forget(key);
        }
        by_model
    }

    /// Searches what compaction removed (the `recall` tool).
    fn recall(&self, id: &str, input: &Value) -> ToolOutcome {
        let query = input.get("query").and_then(Value::as_str).unwrap_or("").trim();
        if query.is_empty() {
            return ToolOutcome::error(id, "missing `query`");
        }
        let found = self.session.recall(query, 8);
        if found.is_empty() {
            return ToolOutcome::ok(id, format!("nothing in the archive contains all of: {query}"))
                .with_display(po_i18n::t!("agent.display.recall_none", query = query));
        }
        let count = found.len();
        ToolOutcome::ok(id, found.join("\n\n")).with_display(po_i18n::t!("agent.display.recall", query = query, n = count))
    }

    /// A request outside the conversation, shaped for the cache of a local server: the session's
    /// own prefix — system prompt, tools, the first `upto` messages, all already read by the
    /// server — then the instruction. A request with its own system prompt made a single-slot
    /// server drop the conversation, reread in full on the next turn.
    fn side_request(&self, model: &ModelSpec, instruction: &str, upto: usize) -> Completion {
        let mut messages = self.session.messages[..upto.min(self.session.messages.len())].to_vec();
        messages.push(Message::user(format!(
            "[Harness request, outside the conversation: answer it directly, without any tool]\n{instruction}"
        )));
        apply_settings(
            &self.settings,
            Completion::new(model.clone(), self.session.system.clone(), messages).with_tools(self.main_tools(model)),
        )
    }

    async fn summarize_history(&mut self, cut: usize) -> (String, bool) {
        // On the session's local model, the summary is asked at the end of the conversation
        // itself: everything up to there is already in the server's cache, so the summary costs
        // no rereading — where sending the transcript apart meant reading it all again.
        if let Some(model) = self.session_model().filter(|m| m.origin.is_local())
            && let Some(provider) = self.providers.get(&model.id).cloned()
        {
            let request = self.side_request(&model, &format!("{SUMMARY_INSTRUCTION} Summarize the conversation above."), cut);
            let started = std::time::Instant::now();
            if let Ok(response) = provider.complete(&request).await {
                self.session.record(&model.id, response.usage, &model.pricing, started.elapsed().as_secs_f64());
                let text = response.message.visible_text();
                if !text.trim().is_empty() {
                    return (text, true);
                }
            }
        }
        let transcript = session::transcript_of(&self.session.messages[..cut], 400);
        let fallback = summarize(&transcript);

        // We ask for a model able to absorb the history to summarize. If none exists — the
        // history exceeds every window — truncation is the only recourse, which is exactly
        // what the fallback does.
        let Ok((model, _)) = self.pick_for(
            TaskClass::Summarize,
            session::estimate_tokens(&transcript) + SUBAGENT_SYSTEM_TOKENS,
        ) else {
            return (fallback, false);
        };
        let Some(provider) = self.providers.get(&model.id).cloned() else {
            return (fallback, false);
        };

        // The session's thinking budget applies here too: without one, a local server let the
        // summary think for 24k tokens, at the very moment the window was full.
        let request = apply_settings(
            &self.settings,
            Completion::new(model.clone(), SUMMARY_INSTRUCTION.to_owned(), vec![Message::user(transcript)]),
        );

        let started = std::time::Instant::now();
        match provider.complete(&request).await {
            Ok(response) => {
                let text = response.message.visible_text();
                self.session.record(
                    &model.id,
                    response.usage,
                    &model.pricing,
                    started.elapsed().as_secs_f64(),
                );
                if text.trim().is_empty() {
                    (fallback, false)
                } else {
                    (text, true)
                }
            }
            Err(_) => (fallback, false),
        }
    }

    fn pick(&self, class: TaskClass) -> Result<(ModelSpec, po_router::Decision), AgentError> {
        self.pick_for(class, self.session.estimated_prompt_tokens())
    }

    /// Picks a subagent's model, avoiding the orchestrator's local server when possible: on a
    /// single-slot server, the subagent would wipe the conversation's cache there, and even
    /// with several slots it would compete for the GPU. Another server is taken if it reaches
    /// the task's floor; otherwise the same server remains allowed.
    /// Observed before this choice existed: in offline mode, a "search" subagent went to
    /// Strata, which was already orchestrating, while a model was running on the other card.
    fn pick_for_subagent(
        &mut self,
        class: TaskClass,
        prompt_tokens: u32,
    ) -> Result<(ModelSpec, po_router::Decision), AgentError> {
        let shared = match self.orchestrator.as_ref().map(|m| m.origin.clone()) {
            Some(po_core::Origin::Local { endpoint, .. }) => self
                .router
                .models()
                .iter()
                .filter(|m| matches!(&m.origin, po_core::Origin::Local { endpoint: e, .. } if *e == endpoint))
                .filter(|m| !self.router.is_disabled(&m.id))
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        if shared.is_empty() {
            return self.pick_for(class, prompt_tokens);
        }
        for id in &shared {
            self.router.disable_model(id);
        }
        let elsewhere = self.pick_for(class, prompt_tokens);
        for id in &shared {
            self.router.enable_model(id);
        }
        // Deprived of the orchestrator, the router may relax the floor to avoid staying without
        // an answer (it does so in offline mode): such a choice is refused, and the task goes
        // back to the capable server.
        match elsewhere {
            Ok((model, decision)) if model.tier >= class.minimum_tier() => Ok((model, decision)),
            _ => self.pick_for(class, prompt_tokens),
        }
    }

    /// Picks a model for a given prompt size.
    ///
    /// The distinction matters for delegation: a subagent starts from a **fresh** context, not
    /// from the session's history. Applying the size of the ongoing conversation to it would
    /// rule out small-window models — precisely those the saving mode wants to use — for a
    /// context they will never have to process.
    fn pick_for(
        &self,
        class: TaskClass,
        prompt_tokens: u32,
    ) -> Result<(ModelSpec, po_router::Decision), AgentError> {
        let demand = Demand::new(class).with_prompt(prompt_tokens);
        let decision = self
            .router
            .select(&demand)
            .ok_or_else(|| AgentError::NoModel(class.label().to_owned()))?;
        let model = self
            .router
            .get(&decision.model_id)
            .cloned()
            .ok_or_else(|| AgentError::NoModel(class.label().to_owned()))?;
        Ok((model, decision))
    }

    /// Calls the model, absorbing failures.
    ///
    /// Three distinct behaviors, depending on the nature of the error:
    /// - **transient** (drop, 503, momentary quota): retry, with spacing;
    /// - **fatal for this model** (key rejected, unknown model, context exceeded): insisting
    ///   would be pointless, so the model is set aside and the router picks another;
    /// - **other**: the error is propagated.
    ///
    /// This is what makes a multi-source fleet useful in practice: a provider that goes down
    /// must not take the session down with it.
    async fn call_resilient(
        &mut self,
        model: &ModelSpec,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Result<po_core::CompletionResponse, AgentError> {
        let mut current = model.clone();

        for _ in 0..MAX_MODEL_SWITCHES {
            let mut wait = RETRY_BACKOFF;

            for attempt in 1..=MAX_ATTEMPTS {
                match self.call(&current, sink).await {
                    Ok(response) => return Ok(response),
                    Err(AgentError::Provider(err)) if err.is_retryable() && attempt < MAX_ATTEMPTS => {
                        sink(AgentEvent::Retrying {
                            model: current.display_name.clone(),
                            attempt,
                            error: err.to_string(),
                        });
                        tokio::time::sleep(wait).await;
                        wait *= 2;
                    }
                    Err(AgentError::Provider(err)) if err.is_fatal_for_model() => {
                        sink(AgentEvent::ModelDisabled {
                            model: current.display_name.clone(),
                            reason: err.to_string(),
                        });
                        self.router.disable_model(&current.id);
                        break;
                    }
                    Err(other) => return Err(other),
                }
            }

            // The current model is out of the game: ask the router again, which now ignores it.
            if !self.router.is_disabled(&current.id) {
                self.router.disable_model(&current.id);
            }
            match self.pick(TaskClass::Orchestration) {
                Ok((next, decision)) => {
                    sink(AgentEvent::ModelSelected {
                        model: next.display_name.clone(),
                        class: TaskClass::Orchestration,
                        reason: decision.reason.clone(),
                    });
                    current = next;
                }
                Err(_) => {
                    return Err(AgentError::NoModel(po_i18n::t!("agent.error.all_failed").to_owned()));
                }
            }
        }
        Err(AgentError::NoModel(po_i18n::t!("agent.error.too_many_switches").to_owned()))
    }

    async fn call(
        &self,
        model: &ModelSpec,
        sink: &mut (impl FnMut(AgentEvent) + Send),
    ) -> Result<po_core::CompletionResponse, AgentError> {
        let provider = self
            .providers
            .get(&model.id)
            .ok_or_else(|| AgentError::NoProvider(model.id.clone()))?;

        let tools = self.main_tools(model);
        let request = self.configure(
            Completion::new(model.clone(), self.session.system.clone(), self.session.messages.clone())
                .with_tools(tools),
        );

        if !provider.supports_streaming() {
            return Ok(provider.complete(&request).await?);
        }

        // Fragments are handed to the interface as they arrive: accumulating them to deliver
        // them in one block at the end would defeat the whole point of streaming.
        let show_thinking = self.settings.show_thinking;
        let response = provider
            .stream(&request, &mut |delta| {
                match delta {
                    po_provider::Delta::Text(text) => sink(AgentEvent::TextDelta(text)),
                    po_provider::Delta::Queued => sink(AgentEvent::Queued),
                    po_provider::Delta::Thinking(text) => {
                        sink(AgentEvent::ThinkingDelta(text.chars().count()));
                        if show_thinking {
                            sink(AgentEvent::ThinkingText(text));
                        }
                    }
                    _ => {}
                }
            })
            .await?;
        Ok(response)
    }
}

/// Summary of a tool call, displayed before execution. What the user reads to decide.
pub fn summarize_call(name: &str, input: &Value) -> String {
    match name {
        "bash" => input.get("command").and_then(Value::as_str).unwrap_or("").to_owned(),
        "read" | "write" | "edit" | "list" => {
            input.get("path").and_then(Value::as_str).unwrap_or("").to_owned()
        }
        "glob" | "grep" => input.get("pattern").and_then(Value::as_str).unwrap_or("").to_owned(),
        "po_labs" => input.get("summary").or_else(|| input.get("resume")).and_then(Value::as_str).unwrap_or("").to_owned(),
        _ => input.to_string(),
    }
}

/// Fallback summary for compaction: the latest exchanges, truncated.
///
/// Having a model produce this summary would be better, but it would fail precisely when it is
/// needed most — when the context overflows. We therefore keep a fallback that calls nothing.
fn summarize(transcript: &str) -> String {
    const MAX: usize = 2000;
    if transcript.len() <= MAX {
        return transcript.to_owned();
    }
    let start = transcript.len() - MAX;
    let start = transcript
        .char_indices()
        .find(|(index, _)| *index >= start)
        .map(|(index, _)| index)
        .unwrap_or(0);
    format!("[start truncated]\n{}", &transcript[start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thinking_budget_follows_the_level_and_the_window() {
        let mut settings = SessionSettings::default();
        assert_eq!(settings.thinking_budget(131_072), Some(8192), "Strata's window, no level chosen");
        assert_eq!(settings.thinking_budget(32_768), Some(2048));
        settings.reasoning_effort = Some("high".into());
        assert_eq!(settings.thinking_budget(131_072), Some(16_384));
        assert_eq!(settings.thinking_budget(32_768), Some(4096), "never more than an eighth of the window");
        settings.reasoning_effort = Some("none".into());
        assert_eq!(settings.thinking_budget(131_072), None);
        settings.thinking_budget = Some(3000);
        assert_eq!(settings.thinking_budget(131_072), Some(3000), "an explicit budget wins");
        settings.thinking_budget = Some(0);
        assert_eq!(settings.thinking_budget(131_072), None, "0 leaves the server's own");
    }
    use po_core::{Capabilities, CompletionResponse, Origin, Pricing, Role, StopReason, Tier};
    use po_router::BudgetMode;
    use po_tools::Workspace;
    use std::sync::Mutex;

    /// Scripted provider: returns the planned responses, in order, and records what it received.
    /// Allows testing the whole loop without a network or a model.
    struct ScriptedProvider {
        name: String,
        responses: Mutex<Vec<CompletionResponse>>,
        seen: Arc<Mutex<Vec<Completion>>>,
    }

    impl ScriptedProvider {
        fn new(name: &str, responses: Vec<CompletionResponse>) -> (Arc<Self>, Arc<Mutex<Vec<Completion>>>) {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let provider = Arc::new(Self {
                name: name.into(),
                responses: Mutex::new(responses),
                seen: Arc::clone(&seen),
            });
            (provider, seen)
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn name(&self) -> &str {
            &self.name
        }

        async fn complete(&self, request: &Completion) -> Result<CompletionResponse, ProviderError> {
            self.seen.lock().unwrap().push(request.clone());
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(ProviderError::Transport("script exhausted".into()));
            }
            Ok(responses.remove(0))
        }
    }

    fn text_reply(text: &str) -> CompletionResponse {
        CompletionResponse {
            message: Message::assistant(text),
            stop_reason: StopReason::EndTurn,
            usage: Usage { input_tokens: 100, output_tokens: 20, ..Default::default() },
            model_id: "test".into(),
        }
    }

    fn tool_call_reply(id: &str, name: &str, input: Value) -> CompletionResponse {
        CompletionResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.into(),
                    name: name.into(),
                    input,
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage { input_tokens: 100, output_tokens: 20, ..Default::default() },
            model_id: "test".into(),
        }
    }

    fn test_model(id: &str, tier: Tier, local: bool) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            display_name: id.into(),
            origin: if local {
                Origin::Local { backend: "llama.cpp".into(), endpoint: "http://x".into() }
            } else {
                Origin::Cloud { provider: "cloud".into() }
            },
            tier,
            capabilities: Capabilities::AGENTIC,
            context_window: 200_000,
            max_output_tokens: 4096,
            pricing: if local {
                Pricing::FREE
            } else {
                Pricing { input_per_mtok: 15.0, output_per_mtok: 75.0, ..Default::default() }
            },
        }
    }

    /// Lets the router choose the model of each role (the behavior from before per-role
    /// settings, still available with `/agents test_model <role> auto`).
    fn route_all_via_router(agent: &mut Agent) {
        for p in subagents::builtins() {
            agent.subagent_settings.models.insert(p.name, "auto".into());
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        agent: Agent,
        events: Vec<AgentEvent>,
    }

    fn harness(mode: BudgetMode) -> (Harness, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path());
        let ctx = ToolContext::new(workspace);
        let router = Router::new(
            vec![test_model("opus", Tier::Frontier, false), test_model("qwen-local", Tier::Strong, true)],
            mode,
        );
        let agent = Agent::new(
            router,
            Registry::with_builtins(),
            ctx,
            "system",
            Arc::new(AutoApprove),
        );
        let holder = tempfile::tempdir().unwrap();
        (Harness { _dir: dir, agent, events: Vec::new() }, holder)
    }

    fn write_call() -> CompletionResponse {
        tool_call_reply("w1", "write", json!({"path": "x.txt", "content": "content"}))
    }

    #[tokio::test]
    async fn plan_mode_modifies_nothing_and_the_model_knows() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        h.agent.work_mode().set(WorkMode::Plan);
        let (cloud, seen) = ScriptedProvider::new("cloud", vec![write_call(), text_reply("here is the plan")]);
        h.agent.attach("opus", cloud);
        h.agent.turn("write x.txt", &mut |e| h.events.push(e)).await.unwrap();
        assert!(!h._dir.path().join("x.txt").exists(), "even with automatic approval, plan mode writes nothing");
        let seen = seen.lock().unwrap();
        assert!(seen[0].messages.last().unwrap().visible_text().contains("[Plan mode"), "the change is announced");
        let refusal = &seen[1].messages.last().unwrap().content[0];
        assert!(matches!(refusal, ContentBlock::ToolResult { content, is_error: true, .. } if content.contains("plan mode")));
    }

    #[tokio::test]
    async fn bypass_mode_asks_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let router = Router::new(vec![test_model("opus", Tier::Frontier, false)], BudgetMode::MaxQuality);
        // An approver that refuses everything: only the mode can let the write through.
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "system", Arc::new(ReadOnly));
        agent.work_mode().set(WorkMode::Bypass);
        let (cloud, _) = ScriptedProvider::new("cloud", vec![write_call(), text_reply("written")]);
        agent.attach("opus", cloud);
        agent.turn("write", &mut |_| {}).await.unwrap();
        assert!(dir.path().join("x.txt").exists());
    }

    #[tokio::test]
    async fn a_requested_pause_stops_after_the_tools_and_resumes_where_it_left_off() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (cloud, seen) = ScriptedProvider::new("cloud", vec![write_call(), text_reply("written")]);
        h.agent.attach("opus", cloud);
        // The interface raises the flag during the turn, when a setting changes.
        let pause = h.agent.pause_handle();
        h.agent
            .turn("write", &mut |e| {
                if matches!(e, AgentEvent::ToolStarted { .. }) {
                    pause.store(true, Ordering::Relaxed);
                }
                h.events.push(e)
            })
            .await
            .unwrap();
        assert!(h.agent.is_paused(), "stopped at the safe point");
        assert!(h._dir.path().join("x.txt").exists(), "the tool ran before the pause");
        assert_eq!(seen.lock().unwrap().len(), 1, "the model was not called again");
        assert!(!h.events.iter().any(|e| matches!(e, AgentEvent::TurnCompleted { .. })));
        // A setting changed during the pause applies to the rest of the turn.
        h.agent.settings.temperature = Some(0.2);
        h.agent.resume(&mut |e| h.events.push(e)).await.unwrap();
        assert!(!h.agent.is_paused());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].temperature, Some(0.2));
        assert!(matches!(seen[1].messages.last().unwrap().content[0], ContentBlock::ToolResult { .. }));
        match h.events.last().unwrap() {
            AgentEvent::TurnCompleted { usage, .. } => {
                assert_eq!(usage.output_tokens, 40, "the turn counts both of its calls")
            }
            other => panic!("unexpected last event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_message_sent_mid_turn_reaches_the_model_before_its_next_step() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (cloud, seen) = ScriptedProvider::new("cloud", vec![write_call(), text_reply("ok")]);
        h.agent.attach("opus", cloud);
        let pause = h.agent.pause_handle();
        h.agent
            .turn("write", &mut |e| {
                if matches!(e, AgentEvent::ToolStarted { .. }) {
                    pause.store(true, Ordering::Relaxed);
                }
            })
            .await
            .unwrap();
        assert!(h.agent.is_paused());
        h.agent.inject_user_message("use tabs, not spaces");
        h.agent.resume(&mut |_| {}).await.unwrap();

        let seen = seen.lock().unwrap();
        let last = seen[1].messages.last().unwrap();
        assert!(matches!(last.content[0], ContentBlock::ToolResult { .. }), "the results come first");
        assert!(last.visible_text().contains("use tabs, not spaces"), "then the message, in the same step");
        drop(seen);
        // A later compaction still knows what the user asked mid-turn.
        let note = session::INJECTED_NOTE;
        assert!(h.agent.session.messages.iter().any(|m| m.visible_text().contains(note)));
    }

    #[tokio::test]
    async fn a_simple_turn_produces_text_and_accounting() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (provider, _) = ScriptedProvider::new("cloud", vec![text_reply("hello")]);
        h.agent.attach("opus", provider);

        h.agent
            .turn("hi", &mut |event| h.events.push(event))
            .await
            .unwrap();

        assert!(h.events.contains(&AgentEvent::Text("hello".into())));
        match h.events.last().unwrap() {
            AgentEvent::TurnCompleted { usage, cost, .. } => {
                assert_eq!(usage.output_tokens, 20);
                assert!(*cost > 0.0, "a cloud model must cost something");
            }
            other => panic!("unexpected last event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_history_too_long_is_compacted_instead_of_blocking() {
        // Defect found while writing the compaction test: the router rules out any model whose
        // window cannot absorb the prompt. Compacting after choosing therefore left the session
        // permanently blocked on "no model available".
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let mut small = test_model("small", Tier::Frontier, true);
        small.context_window = 12_000;

        let router = Router::new(vec![small], BudgetMode::Offline);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "sys", Arc::new(AutoApprove));
        let (provider, _) = ScriptedProvider::new(
            "local",
            vec![text_reply("summary"), text_reply("answer")],
        );
        agent.attach("small", provider);

        // A history that far exceeds the window of the only available model.
        agent.session.push(Message::user("z".repeat(80_000)));

        let mut events = Vec::new();
        let result = agent.turn("and now?", &mut |e| events.push(e)).await;

        assert!(result.is_ok(), "the session must shrink, not fail");
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Text(_))));
    }

    #[tokio::test]
    async fn compaction_has_a_model_summarize() {
        po_i18n::set_lang(po_i18n::Lang::En);
        // Truncating loses the start of the conversation; summarizing keeps the decisions made.
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let mut small = test_model("small", Tier::Frontier, true);
        small.context_window = 12_000;

        let router = Router::new(vec![small], BudgetMode::Offline);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "sys", Arc::new(AutoApprove));

        let (provider, seen) = ScriptedProvider::new(
            "local",
            vec![text_reply("CONDENSED SUMMARY"), text_reply("answer to the turn")],
        );
        agent.attach("small", provider);

        // About ten messages, large enough to exceed the usable budget (12,000 - 8,192 = 3,808
        // tokens) but small enough for the summarizer to absorb them.
        for _ in 0..10 {
            agent.session.push(Message::user("y".repeat(1_800)));
        }

        let mut events = Vec::new();
        agent.turn("next", &mut |e| events.push(e)).await.unwrap();

        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Notice(t) if t.contains("summarized"))),
            "compaction must go through a model when possible"
        );
        // The first call is the summary, the second is the turn itself.
        let requests = seen.lock().unwrap();
        assert!(requests.len() >= 2, "{} requests", requests.len());
        // On a local model, the summary is asked at the end of the conversation itself, with the
        // session's own system prompt: the server's cache covers everything before it.
        assert_eq!(requests[0].system, "sys");
        assert!(requests[0].messages.last().unwrap().visible_text().contains("summarize"));
        assert!(requests[0].messages.len() > 2, "the conversation is the prefix");
        drop(requests);
        // And the produced summary does end up in the compacted history.
        assert!(
            agent.session.messages.iter().any(|m| m.visible_text().contains("CONDENSED SUMMARY")),
            "the summary must replace the discarded exchanges"
        );
    }

    #[tokio::test]
    async fn an_impossible_summary_falls_back_to_truncation() {
        po_i18n::set_lang(po_i18n::Lang::En);
        // Compaction happens when the context overflows: the worst moment to depend on a
        // network call. When the history exceeds every available window, no model can
        // summarize it — and the session must nonetheless carry on.
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let mut small = test_model("small", Tier::Frontier, true);
        small.context_window = 12_000;

        let router = Router::new(vec![small], BudgetMode::Offline);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "sys", Arc::new(AutoApprove));

        // A single response: if the summary consumed a call, the turn would fail.
        let (provider, seen) = ScriptedProvider::new("local", vec![text_reply("answer")]);
        agent.attach("small", provider);

        // ~20,000 tokens: beyond the only model's window, hence unsummarizable.
        agent.session.push(Message::user("y".repeat(80_000)));

        let mut events = Vec::new();
        let result = agent.turn("next", &mut |e| events.push(e)).await;

        assert!(result.is_ok(), "an impossible summary must not take the session down");
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Notice(t) if t.contains("truncated"))));
        assert_eq!(seen.lock().unwrap().len(), 1, "no call wasted on a summary");
    }

    #[tokio::test]
    async fn approaching_the_context_limit_is_announced() {
        po_i18n::set_lang(po_i18n::Lang::En);
        // A silent compaction gives the impression that the agent forgets; warning leaves the
        // user the choice of splitting their work.
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let mut small = test_model("small", Tier::Frontier, true);
        // Usable budget: 20,000 - 8,192 = 11,808 tokens. The warning fires beyond 85% of this
        // budget, i.e. ~10,037 tokens — reached by the prompt below.
        small.context_window = 20_000;

        let router = Router::new(vec![small], BudgetMode::Offline);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "sys", Arc::new(AutoApprove));
        let (provider, _) = ScriptedProvider::new("local", vec![text_reply("ok")]);
        agent.attach("small", provider);

        // A prompt calibrated just above the warning threshold but below the compaction point.
        agent.session.push(Message::user("x".repeat(44_000)));

        let mut events = Vec::new();
        agent.turn("next", &mut |e| events.push(e)).await.unwrap();

        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::Notice(t) if t.contains("context at"))),
            "the user must be warned before compaction"
        );
    }

    #[tokio::test]
    async fn a_short_session_triggers_no_warning() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (provider, _) = ScriptedProvider::new("cloud", vec![text_reply("ok")]);
        h.agent.attach("opus", provider);

        h.agent.turn("hello", &mut |e| h.events.push(e)).await.unwrap();
        assert!(
            !h.events.iter().any(|e| matches!(e, AgentEvent::Notice(t) if t.contains("context at"))),
            "warning for no reason would be noise"
        );
    }

    #[tokio::test]
    async fn local_compute_time_is_measured() {
        // A local model is free in money, not in seconds. Without this measurement, the status
        // bar would show "100% local" without ever saying what it costs.
        let (mut h, _keep) = harness(BudgetMode::Offline);
        let (local, _) = ScriptedProvider::new("local", vec![text_reply("answer")]);
        h.agent.attach("qwen-local", local);

        h.agent.turn("hi", &mut |e| h.events.push(e)).await.unwrap();

        let seconds = h.agent.session.ledger.total_local_seconds();
        assert!(seconds >= 0.0, "the time must be recorded, not left at zero by default");
        assert_eq!(h.agent.session.ledger.total_cost(), 0.0, "and stay free in money");

        match h.events.last().unwrap() {
            AgentEvent::TurnCompleted { local_seconds, local_share, .. } => {
                assert_eq!(*local_share, 1.0);
                assert!(*local_seconds >= 0.0);
            }
            other => panic!("unexpected last event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_loop_runs_the_tools_then_goes_on() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        std::fs::write(h.agent.ctx.workspace.root().join("a.txt"), "content").unwrap();

        let (provider, seen) = ScriptedProvider::new(
            "cloud",
            vec![tool_call_reply("c1", "read", json!({"path": "a.txt"})), text_reply("the file says: content")],
        );
        h.agent.attach("opus", provider);

        h.agent
            .turn("read a.txt", &mut |event| h.events.push(event))
            .await
            .unwrap();

        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::ToolStarted { name, .. } if name == "read")));
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::ToolFinished { is_error: false, .. })));
        assert!(h.events.contains(&AgentEvent::Text("the file says: content".into())));

        // The second call must contain the tool result, otherwise the model would answer
        // without having seen the file.
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let last = requests[1].messages.last().unwrap();
        assert!(last.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })));
    }

    #[tokio::test]
    async fn a_user_refusal_goes_back_to_the_model_as_a_result() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let router = Router::new(vec![test_model("opus", Tier::Frontier, false)], BudgetMode::MaxQuality);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "sys", Arc::new(ReadOnly));

        let (provider, seen) = ScriptedProvider::new(
            "cloud",
            vec![
                tool_call_reply("c1", "write", json!({"path": "a.txt", "content": "x"})),
                text_reply("understood, I will not write it"),
            ],
        );
        agent.attach("opus", provider);

        let mut events = Vec::new();
        agent.turn("write a.txt", &mut |e| events.push(e)).await.unwrap();

        assert!(events.iter().any(|e| matches!(e, AgentEvent::ToolDenied { name, .. } if name == "write")));
        assert!(!dir.path().join("a.txt").exists(), "the file must not have been written");

        let requests = seen.lock().unwrap();
        let tool_result = requests[1].messages.last().unwrap();
        match &tool_result.content[0] {
            ContentBlock::ToolResult { is_error, content, .. } => {
                assert!(is_error);
                assert!(content.contains("refused"), "the model must understand why");
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_forbidden_command_is_blocked_even_with_auto_approval() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (provider, _) = ScriptedProvider::new(
            "cloud",
            vec![tool_call_reply("c1", "bash", json!({"command": "rm -rf /"})), text_reply("okay")],
        );
        h.agent.attach("opus", provider);

        h.agent.turn("clean up", &mut |e| h.events.push(e)).await.unwrap();

        assert!(
            h.events.iter().any(|e| matches!(e, AgentEvent::ToolDenied { name, .. } if name == "bash")),
            "automatic approval must not lift the ban"
        );
    }

    #[tokio::test]
    async fn delegation_uses_another_model_and_costs_nothing() {
        // The scenario from the specification: cloud orchestrator, local subagent.
        let (mut h, _keep) = harness(BudgetMode::SaveMaxTokens);

        let (cloud, _) = ScriptedProvider::new(
            "cloud",
            vec![
                tool_call_reply("c1", "task", json!({"task": "find the Rust files", "class": "search"})),
                text_reply("here is what the subagent found"),
            ],
        );
        let (local, seen_local) = ScriptedProvider::new("local", vec![text_reply("src/main.rs")]);
        h.agent.attach("opus", cloud);
        h.agent.attach("qwen-local", local);
        route_all_via_router(&mut h.agent);

        h.agent
            .turn("look for the Rust files", &mut |e| h.events.push(e))
            .await
            .unwrap();

        let delegated = h.events.iter().find_map(|e| match e {
            AgentEvent::Delegated { model, agent, .. } => Some((model.clone(), agent.clone())),
            _ => None,
        });
        assert_eq!(delegated, Some(("qwen-local".into(), "explorer".into())), "the old `class` parameter is still understood");

        // The subagent was indeed called, with a fresh context and not the full history.
        let requests = seen_local.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages.len(), 1);
        assert!(requests[0].system.contains("subagent"));

        // And part of the tokens cost nothing.
        assert!(h.agent.session.ledger.local_share() > 0.0);
    }

    #[tokio::test]
    async fn delegation_judges_the_window_on_the_task_not_the_session() {
        // A subagent starts from a fresh context. If the router applies the size of the
        // ongoing conversation to it, small-window models — the ones savings aim at — are
        // ruled out for a context they will never have to process.
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));

        let mut large = test_model("large", Tier::Frontier, false);
        large.context_window = 200_000;
        let mut small = test_model("small-local", Tier::Small, true);
        small.context_window = 8_192;

        let router = Router::new(vec![large, small], BudgetMode::SaveMaxTokens);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "sys", Arc::new(AutoApprove));

        // An already long session: 100,000 tokens of history.
        agent.session.push(Message::user("x".repeat(400_000)));

        let (cloud, _) = ScriptedProvider::new(
            "cloud",
            vec![
                tool_call_reply("c1", "task", json!({"task": "summarize this", "class": "summarize"})),
                text_reply("done"),
            ],
        );
        let (local, seen_local) = ScriptedProvider::new("local", vec![text_reply("short summary")]);
        agent.attach("large", cloud);
        agent.attach("small-local", local);
        route_all_via_router(&mut agent);

        let mut events = Vec::new();
        agent.turn("summarize", &mut |e| events.push(e)).await.unwrap();

        assert_eq!(
            seen_local.lock().unwrap().len(),
            1,
            "the small model must have been used despite the long session"
        );
        let delegated = events.iter().find_map(|e| match e {
            AgentEvent::Delegated { model, .. } => Some(model.clone()),
            _ => None,
        });
        assert_eq!(delegated, Some("small-local".into()));
    }

    #[tokio::test]
    async fn the_subagent_gets_fewer_tools_than_the_main_agent() {
        let (mut h, _keep) = harness(BudgetMode::SaveMaxTokens);
        let (cloud, _) = ScriptedProvider::new(
            "cloud",
            vec![tool_call_reply("c1", "task", json!({"task": "search", "agent": "explorer"})), text_reply("finished")],
        );
        let (local, seen_local) = ScriptedProvider::new("local", vec![text_reply("result")]);
        h.agent.attach("opus", cloud);
        h.agent.attach("qwen-local", local);
        route_all_via_router(&mut h.agent);

        h.agent.turn("search", &mut |e| h.events.push(e)).await.unwrap();

        let requests = seen_local.lock().unwrap();
        let names: Vec<_> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"read"));
        assert!(!names.contains(&"bash"), "a search subagent does not need a shell");
        assert!(!names.contains(&"task"), "a subagent does not delegate in turn");
    }

    #[tokio::test]
    async fn the_main_agent_is_offered_delegation() {
        let (mut h, _keep) = harness(BudgetMode::Balanced);
        let (provider, seen) = ScriptedProvider::new("cloud", vec![text_reply("ok")]);
        h.agent.attach("opus", provider);

        h.agent.turn("hello", &mut |e| h.events.push(e)).await.unwrap();

        let requests = seen.lock().unwrap();
        assert!(requests[0].tools.iter().any(|t| t.name == "task"));
    }

    #[tokio::test]
    async fn a_provider_outage_propagates_without_corrupting_the_session() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (provider, _) = ScriptedProvider::new("cloud", vec![]); // empty script → error
        h.agent.attach("opus", provider);

        let result = h.agent.turn("hi", &mut |e| h.events.push(e)).await;
        assert!(result.is_err());
        // The user's message stays in the history: the turn can be relaunched.
        assert_eq!(h.agent.session.messages.len(), 1);
    }

    #[tokio::test]
    async fn a_model_without_provider_is_clearly_reported() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let err = h.agent.turn("hi", &mut |e| h.events.push(e)).await.unwrap_err();
        assert!(matches!(err, AgentError::NoProvider(_)));
    }

    /// Provider that fails a given number of times before succeeding.
    struct FlakyProvider {
        remaining_failures: Mutex<usize>,
        error: fn() -> ProviderError,
        attempts: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl Provider for FlakyProvider {
        fn name(&self) -> &str {
            "flaky"
        }

        async fn complete(&self, _request: &Completion) -> Result<CompletionResponse, ProviderError> {
            *self.attempts.lock().unwrap() += 1;
            let mut remaining = self.remaining_failures.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Err((self.error)());
            }
            Ok(text_reply("at last"))
        }
    }

    #[tokio::test]
    async fn a_transient_failure_is_retried() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let attempts = Arc::new(Mutex::new(0));
        h.agent.attach(
            "opus",
            Arc::new(FlakyProvider {
                remaining_failures: Mutex::new(2),
                error: || ProviderError::Transport("connection lost".into()),
                attempts: Arc::clone(&attempts),
            }),
        );

        h.agent.turn("hi", &mut |e| h.events.push(e)).await.unwrap();

        assert_eq!(*attempts.lock().unwrap(), 3, "two failures then a success");
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Retrying { .. })));
        assert!(h.events.contains(&AgentEvent::Text("at last".into())));
    }

    #[tokio::test]
    async fn a_rejected_key_sets_the_model_aside_and_switches() {
        // The scenario that matters for a multi-source fleet: the top model is unusable, the
        // session must carry on with another.
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        h.agent.attach(
            "opus",
            Arc::new(FlakyProvider {
                remaining_failures: Mutex::new(usize::MAX),
                error: || ProviderError::Unauthorized { provider: "cloud".into() },
                attempts: Arc::new(Mutex::new(0)),
            }),
        );
        let (local, _) = ScriptedProvider::new("local", vec![text_reply("taken over by the relay")]);
        h.agent.attach("qwen-local", local);

        h.agent.turn("hi", &mut |e| h.events.push(e)).await.unwrap();

        assert!(
            h.events.iter().any(|e| matches!(e, AgentEvent::ModelDisabled { .. })),
            "the user must see which model was set aside"
        );
        assert!(h.events.contains(&AgentEvent::Text("taken over by the relay".into())));
        assert!(h.agent.router().is_disabled("opus"));
    }

    #[tokio::test]
    async fn a_fatal_error_is_not_retried() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let attempts = Arc::new(Mutex::new(0));
        h.agent.attach(
            "opus",
            Arc::new(FlakyProvider {
                remaining_failures: Mutex::new(usize::MAX),
                error: || ProviderError::Unauthorized { provider: "cloud".into() },
                attempts: Arc::clone(&attempts),
            }),
        );
        let (local, _) = ScriptedProvider::new("local", vec![text_reply("relay")]);
        h.agent.attach("qwen-local", local);

        h.agent.turn("hi", &mut |e| h.events.push(e)).await.unwrap();
        assert_eq!(*attempts.lock().unwrap(), 1, "insisting on a rejected key is useless");
    }

    #[tokio::test]
    async fn a_fully_down_fleet_eventually_gives_up() {
        // Without a bound, the loop would spin forever on dead models.
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        for id in ["opus", "qwen-local"] {
            h.agent.attach(
                id,
                Arc::new(FlakyProvider {
                    remaining_failures: Mutex::new(usize::MAX),
                    error: || ProviderError::Unauthorized { provider: "x".into() },
                    attempts: Arc::new(Mutex::new(0)),
                }),
            );
        }
        let result = h.agent.turn("hi", &mut |e| h.events.push(e)).await;
        assert!(matches!(result, Err(AgentError::NoModel(_))));
    }

    #[test]
    fn the_fallback_summary_bounds_its_size() {
        let long = "conversation line\n".repeat(1000);
        let summary = summarize(&long);
        assert!(summary.len() < 2200);
        assert!(summary.starts_with("[start truncated]"));
    }

    #[test]
    fn the_fallback_summary_does_not_split_a_character() {
        let long = "é".repeat(5000);
        assert!(!summarize(&long).is_empty(), "must not panic on multibyte UTF-8");
    }

    #[test]
    fn calls_are_summarized_readably() {
        assert_eq!(summarize_call("bash", &json!({"command": "ls -la"})), "ls -la");
        assert_eq!(summarize_call("read", &json!({"path": "src/main.rs"})), "src/main.rs");
        assert_eq!(summarize_call("grep", &json!({"pattern": "fn main"})), "fn main");
    }

    #[test]
    fn progress_lines_are_recognized_and_removed_from_the_result() {
        assert_eq!(progress_line("En cours : je lis src/main.rs"), Some("je lis src/main.rs"));
        assert_eq!(progress_line("**En cours :** trois fichiers seen"), Some("trois fichiers seen"));
        assert_eq!(progress_line("En coursive"), None);
        assert_eq!(progress_line("Progress: reading src/main.rs"), Some("reading src/main.rs"));
        assert_eq!(progress_line("**Progress:** three files seen"), Some("three files seen"));
        assert_eq!(progress_line("> - progress : done"), Some("done"));
        assert_eq!(progress_line("Progressing well"), None);
        assert_eq!(progress_line("Progress:"), None);
        assert_eq!(without_progress_lines("Progress: reading\nResult: nothing"), "Result: nothing");
        assert_eq!(without_progress_lines("En cours : je lis\nRésultat : rien"), "Résultat : rien");
    }

    #[test]
    fn a_title_is_stripped_of_its_ornaments() {
        assert_eq!(clean_title("« Refonte de l'affichage ».\n"), Some("Refonte de l'affichage".into()));
        assert_eq!(clean_title("\n**Titre : Sous-agents en parallèle**"), Some("Sous-agents en parallèle".into()));
        assert_eq!(clean_title("   \n "), None);
    }

    #[test]
    fn an_unknown_task_class_does_not_block_delegation() {
        assert_eq!(legacy_role("invented"), "explorer");
        assert_eq!(legacy_role("review"), "reviewer");
        assert_eq!(legacy_role("edit"), "builder");
    }

    #[test]
    fn speeds_are_computed_over_the_right_intervals() {
        let usage = Usage { input_tokens: 400, output_tokens: 100, cache_read_tokens: 5000, cache_write_tokens: 0 };
        let stats = CallStats::measure(2.0, 4.0, &usage);
        assert_eq!(stats.ttft, 2.0);
        assert_eq!(stats.generation_tok_s, 25.0, "100 tokens in 4 s");
        assert_eq!(stats.prefill_tok_s, 200.0, "400 tokens read in 2 s, the cache is not counted");
        assert_eq!(stats.cached_tokens, 5000);
        po_i18n::set_lang(po_i18n::Lang::En);
        let line = stats.summary().unwrap();
        assert!(line.contains("25.0 tok/s") && line.contains("5000 tokens cached"), "{line}");
    }

    #[test]
    fn a_tiny_response_produces_no_speed() {
        let usage = Usage { input_tokens: 10, output_tokens: 3, ..Default::default() };
        assert!(CallStats::measure(0.5, 0.1, &usage).summary().is_none());
    }

    #[test]
    fn verdicts_are_counted_even_when_formatted() {
        let report = "I checked.\n- OK: the tests pass\n* `ÉCHEC: the docs — out of date`\nOK : compiles\nanother line";
        let (ok, failures) = count_verdicts(report);
        assert_eq!(ok, 2);
        assert_eq!(failures, vec!["the docs — out of date".to_owned()]);
        assert_eq!(count_verdicts("nothing formatted"), (0, vec![]));
        assert_eq!(count_verdicts("- OK: build\nFAIL: docs — outdated"), (1, vec!["docs — outdated".to_owned()]));
    }

    #[test]
    fn the_prompt_numbers_each_criterion() {
        let prompt = verification_prompt(&["compiles".into(), "tests green".into()]);
        assert!(prompt.contains("1. compiles") && prompt.contains("2. tests green"));
        assert!(prompt.contains("OK: <criterion>") && prompt.contains("FAIL: <criterion>"));
    }

    #[tokio::test]
    async fn a_cut_response_is_resumed_automatically() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let mut cut_reply = text_reply("one, two, thr");
        cut_reply.stop_reason = StopReason::MaxTokens;
        let (provider, seen) = ScriptedProvider::new("cloud", vec![cut_reply, text_reply("ee, four")]);
        h.agent.attach("opus", provider);

        h.agent.turn("count", &mut |event| h.events.push(event)).await.unwrap();

        assert_eq!(seen.lock().unwrap().len(), 2, "one resume, no more");
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Notice(n) if n.contains("automatic resume"))));
        let last_message = seen.lock().unwrap()[1].messages.last().unwrap().visible_text();
        assert!(last_message.contains("cut off"), "{last_message}");
    }

    #[tokio::test]
    async fn resumes_are_bounded() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let cut_replies: Vec<_> = (0..5)
            .map(|_| {
                let mut r = text_reply("…");
                r.stop_reason = StopReason::MaxTokens;
                r
            })
            .collect();
        let (provider, seen) = ScriptedProvider::new("cloud", cut_replies);
        h.agent.attach("opus", provider);
        h.agent.turn("count", &mut |event| h.events.push(event)).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1 + MAX_CONTINUATIONS);
    }

    /// Two local servers: the orchestrator on one, a small model on the other.
    fn two_servers(mode: BudgetMode) -> (Harness, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        let mut large = test_model("strata", Tier::Strong, true);
        large.origin = Origin::Local { backend: "strata".into(), endpoint: "http://127.0.0.1:8080/v1".into() };
        let mut small = test_model("qwen-0.8b", Tier::Small, true);
        small.origin = Origin::Local { backend: "llama.cpp".into(), endpoint: "http://127.0.0.1:18080/v1".into() };
        let router = Router::new(vec![large, small], mode);
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "system", Arc::new(AutoApprove));
        route_all_via_router(&mut agent);
        let holder = tempfile::tempdir().unwrap();
        (Harness { _dir: dir, agent, events: Vec::new() }, holder)
    }

    #[tokio::test]
    async fn the_subagent_avoids_the_orchestrators_server() {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        let (orchestrator, _) = ScriptedProvider::new(
            "strata",
            vec![tool_call_reply("c1", "task", json!({"task": "summarize this text", "class": "summarize"})), text_reply("finished")],
        );
        let (small, seen) = ScriptedProvider::new("small", vec![text_reply("summary")]);
        h.agent.attach("strata", orchestrator);
        h.agent.attach("qwen-0.8b", small);
        h.agent.turn("search", &mut |e| h.events.push(e)).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1, "the search goes to the other server");
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Delegated { model, .. } if model == "qwen-0.8b")));
    }

    #[tokio::test]
    async fn a_too_demanding_task_stays_on_the_capable_server() {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        let (orchestrator, seen_orchestrator) = ScriptedProvider::new(
            "strata",
            vec![
                tool_call_reply("c1", "task", json!({"task": "modify main.rs", "class": "edit"})),
                text_reply("main.rs modified"),
                text_reply("finished"),
            ],
        );
        let (small, seen_small) = ScriptedProvider::new("small", vec![]);
        h.agent.attach("strata", orchestrator);
        h.agent.attach("qwen-0.8b", small);
        h.agent.turn("edit", &mut |e| h.events.push(e)).await.unwrap();
        assert!(seen_small.lock().unwrap().is_empty(), "the quality floor is never crossed");
        // The builder runs on the capable server, the session's: three calls.
        assert_eq!(seen_orchestrator.lock().unwrap().len(), 3);
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Delegated { model, agent, .. } if model == "strata" && agent == "builder")));
    }

    #[tokio::test]
    async fn by_default_the_subagent_uses_the_session_model() {
        let (mut h, _keep) = harness(BudgetMode::SaveMaxTokens);
        let (cloud, seen_cloud) = ScriptedProvider::new(
            "cloud",
            vec![
                tool_call_reply("c1", "task", json!({"task": "reread lib.rs", "agent": "reviewer"})),
                text_reply("nothing to report"),
                text_reply("reread"),
            ],
        );
        let (local, seen_local) = ScriptedProvider::new("local", vec![]);
        h.agent.attach("opus", cloud);
        h.agent.attach("qwen-local", local);
        h.agent.turn("reread", &mut |e| h.events.push(e)).await.unwrap();
        assert!(seen_local.lock().unwrap().is_empty());
        let seen = seen_cloud.lock().unwrap();
        assert_eq!(seen.len(), 3);
        // Same server: same system prompt and tools as the main agent, so the server reuses
        // this prefix from its cache; the role arrives in the first message.
        assert_eq!(seen[1].system, seen[0].system);
        assert_eq!(seen[1].tools, seen[0].tools);
        let instruction = seen[1].messages[0].visible_text();
        assert!(instruction.contains("\"reviewer\"") && instruction.contains("path:line") && instruction.contains("reread lib.rs"), "{instruction}");
    }

    struct RecordingParking {
        seen: Arc<Mutex<Vec<Completion>>>,
        log: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Parking for RecordingParking {
        async fn park(&self, key: &str) -> bool {
            self.log.lock().unwrap().push(format!("park {key} after {}", self.seen.lock().unwrap().len()));
            true
        }
        async fn unpark(&self, key: &str) -> bool {
            self.log.lock().unwrap().push(format!("unpark {key} after {}", self.seen.lock().unwrap().len()));
            true
        }
        fn forget(&self, key: &str) {
            self.log.lock().unwrap().push(format!("forget {key}"));
        }
    }

    #[tokio::test]
    async fn a_subagent_on_the_single_slot_parks_the_conversation_around_it() {
        let (mut h, _keep) = harness(BudgetMode::Offline);
        let (local, seen) = ScriptedProvider::new(
            "local",
            vec![
                tool_call_reply("c1", "task", json!({"task": "look around", "agent": "explorer"})),
                text_reply("nothing special"),
                text_reply("done"),
            ],
        );
        h.agent.attach("qwen-local", local);
        let log = Arc::new(Mutex::new(Vec::new()));
        h.agent.set_parking("http://x", Arc::new(RecordingParking { seen: Arc::clone(&seen), log: Arc::clone(&log) }));
        h.agent.session_key = Some("s1".into());
        // Large enough to be worth a file.
        h.agent.session.push(Message::user("x".repeat(20_000)));
        h.agent.session.push(Message::assistant("noted"));
        h.agent.turn("explore", &mut |_| {}).await.unwrap();

        let log = log.lock().unwrap().clone();
        assert_eq!(log, ["park s1 after 1", "unpark s1 after 2"], "saved before the subagent's request, back before the next one");
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    #[test]
    fn the_slot_ledger_counts_every_holder_and_gives_back_on_drop() {
        let ledger = Arc::new(SlotLedger::default());
        let a = ledger.take("srv");
        let b = ledger.take("srv");
        assert_eq!(ledger.used("srv"), 2);
        assert_eq!(ledger.used("other"), 0);
        drop(a);
        assert_eq!(ledger.used("srv"), 1);
        drop(b);
        assert_eq!(ledger.used("srv"), 0);
    }

    #[tokio::test]
    async fn a_pause_stops_the_subagent_at_its_safe_point_and_resumes_it_where_it_was() {
        let (mut h, _keep) = harness(BudgetMode::Offline);
        let (local, seen) = ScriptedProvider::new(
            "local",
            vec![
                tool_call_reply("c1", "task", json!({"task": "find the config", "agent": "explorer"})),
                tool_call_reply("s1", "list", json!({"path": "."})),
                text_reply("the config is in po.toml"),
                text_reply("done"),
            ],
        );
        h.agent.attach("qwen-local", local);
        let pause = h.agent.pause_handle();
        h.agent
            .turn("where is the config?", &mut |e| {
                // The user pauses while the subagent works.
                if matches!(e, AgentEvent::Delegated { .. }) {
                    pause.store(true, Ordering::Relaxed);
                }
            })
            .await
            .unwrap();
        assert!(h.agent.is_paused());
        assert_eq!(seen.lock().unwrap().len(), 2, "the subagent stopped after its first tool");
        let saved = h.agent.paused_turn().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].messages.len(), 3, "its task, its call, the result: nothing lost");
        // It survives a save and a load, as after a restart.
        let text = serde_json::to_string(&saved).unwrap();
        let reloaded: Vec<PausedSubagent> = serde_json::from_str(&text).unwrap();
        h.agent.restore_paused(reloaded);

        h.agent.resume(&mut |_| {}).await.unwrap();
        assert!(!h.agent.is_paused());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[2].messages.len(), 3, "the subagent goes on from its own conversation");
        let results = seen[3].messages.last().unwrap().content[0].as_text().to_owned();
        assert!(results.contains("po.toml"), "the real result replaced the placeholder: {results}");
    }

    #[tokio::test]
    async fn a_short_conversation_is_not_parked() {
        let (mut h, _keep) = harness(BudgetMode::Offline);
        let (local, seen) = ScriptedProvider::new(
            "local",
            vec![
                tool_call_reply("c1", "task", json!({"task": "look around", "agent": "explorer"})),
                text_reply("nothing special"),
                text_reply("done"),
            ],
        );
        h.agent.attach("qwen-local", local);
        let log = Arc::new(Mutex::new(Vec::new()));
        h.agent.set_parking("http://x", Arc::new(RecordingParking { seen, log: Arc::clone(&log) }));
        h.agent.session_key = Some("s1".into());
        h.agent.turn("explore", &mut |_| {}).await.unwrap();
        assert!(log.lock().unwrap().is_empty(), "read again in a few seconds: no file");
    }

    #[tokio::test]
    async fn a_read_only_subagent_cannot_write() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (cloud, _) = ScriptedProvider::new(
            "cloud",
            vec![
                tool_call_reply("c1", "task", json!({"task": "reread", "agent": "reviewer"})),
                tool_call_reply("s1", "write", json!({"path": "x.txt", "content": "hijack"})),
                text_reply("I changed nothing"),
                text_reply("finished"),
            ],
        );
        h.agent.attach("opus", cloud);
        h.agent.turn("reread", &mut |e| h.events.push(e)).await.unwrap();
        assert!(!h._dir.path().join("x.txt").exists(), "the write is refused at execution");
    }

    #[tokio::test]
    async fn a_role_can_be_given_to_another_model() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        h.agent.subagent_settings.models.insert("builder".into(), "qwen-local".into());
        let (cloud, _) = ScriptedProvider::new(
            "cloud",
            vec![tool_call_reply("c1", "task", json!({"task": "add a test", "agent": "builder"})), text_reply("done")],
        );
        let (local, seen_local) = ScriptedProvider::new("local", vec![text_reply("test added")]);
        h.agent.attach("opus", cloud);
        h.agent.attach("qwen-local", local);
        h.agent.turn("test", &mut |e| h.events.push(e)).await.unwrap();
        let seen = seen_local.lock().unwrap();
        assert_eq!(seen.len(), 1, "the per-role setting overrides the router");
        assert!(seen[0].tools.iter().any(|t| t.name == "bash"), "a builder has the full tools");
    }

    #[tokio::test]
    async fn an_unknown_subagent_is_explained_to_the_model() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (cloud, _) = ScriptedProvider::new(
            "cloud",
            vec![tool_call_reply("c1", "task", json!({"task": "x", "agent": "ghost"})), text_reply("never mind")],
        );
        h.agent.attach("opus", cloud);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        let explained = h.agent.session.messages.iter().flat_map(|m| &m.content).any(|b| {
            matches!(b, ContentBlock::ToolResult { content, is_error: true, .. } if content.contains("available: explorer"))
        });
        assert!(explained);
    }

    /// Provider whose subagents only answer if two of them wait at the same time: proves that
    /// tasks really run in parallel.
    struct Rendezvous {
        barrier: tokio::sync::Barrier,
        main: Mutex<Vec<CompletionResponse>>,
    }

    #[async_trait]
    impl Provider for Rendezvous {
        fn name(&self) -> &str {
            "rdv"
        }

        async fn complete(&self, request: &Completion) -> Result<CompletionResponse, ProviderError> {
            if request.messages.first().is_some_and(|m| m.visible_text().starts_with("[Subagent")) {
                return match tokio::time::timeout(std::time::Duration::from_millis(400), self.barrier.wait()).await {
                    Ok(_) => Ok(text_reply("found")),
                    Err(_) => Err(ProviderError::Transport("alone at the rendezvous".into())),
                };
            }
            Ok(self.main.lock().unwrap().remove(0))
        }
    }

    fn two_tasks() -> CompletionResponse {
        let mut r = tool_call_reply("c1", "task", json!({"task": "search a", "agent": "explorer"}));
        r.message.content.push(ContentBlock::ToolUse {
            id: "c2".into(),
            name: "task".into(),
            input: json!({"task": "search b", "agent": "explorer"}),
        });
        r
    }

    async fn run_two_subagents_with(slots: usize) -> usize {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        h.agent.subagent_settings.models.clear();
        h.agent.set_server_slots("http://127.0.0.1:8080/v1", slots);
        let provider = Arc::new(Rendezvous {
            barrier: tokio::sync::Barrier::new(2),
            main: Mutex::new(vec![two_tasks(), text_reply("finished")]),
        });
        h.agent.attach("strata", provider);
        h.agent.turn("search", &mut |e| h.events.push(e)).await.unwrap();
        h.agent
            .session
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::ToolResult { is_error: false, .. }))
            .count()
    }

    #[tokio::test]
    async fn two_slots_run_two_subagents_together() {
        assert_eq!(run_two_subagents_with(2).await, 2);
    }

    #[tokio::test]
    async fn a_single_slot_runs_them_one_after_the_other() {
        assert!(run_two_subagents_with(1).await < 2, "in series, the first waits alone and gives up");
    }

    #[tokio::test]
    async fn the_parallelism_setting_overrides_the_server() {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        h.agent.subagent_settings.models.clear();
        h.agent.set_server_slots("http://127.0.0.1:8080/v1", 4);
        h.agent.subagent_settings.max_parallel = Some(1);
        let (provider, _) = ScriptedProvider::new("strata", vec![text_reply("ok")]);
        h.agent.attach("strata", provider);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        let strata = h.agent.router().get("strata").cloned().unwrap();
        assert_eq!(h.agent.parallel_limit(&strata), 1);
        assert!(!h.agent.can_run_in_background(&strata), "one slot: the main agent must wait");
    }

    #[tokio::test]
    async fn sessions_sharing_a_server_see_each_others_slots_at_once() {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        h.agent.subagent_settings.models.clear();
        h.agent.set_server_slots("http://127.0.0.1:8080/v1", 3);
        let (provider, _) = ScriptedProvider::new("strata", vec![text_reply("ok")]);
        h.agent.attach("strata", provider);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        let strata = h.agent.router().get("strata").cloned().unwrap();
        let endpoint = endpoint_key(&strata);
        let ledger = Arc::new(SlotLedger::default());
        h.agent.share_slots(ledger.clone());
        assert_eq!(h.agent.session_slots(), 3);
        // Po Labs works on one slot, this session's turn holds another: one is left.
        let _labs = ledger.take(&endpoint);
        h.agent.hold_turn_slot();
        assert!(h.agent.can_run_in_background(&strata), "one slot left: the first to ask gets it");
        // Po Labs asked first: nothing is left for this session.
        let labs_subagent = ledger.take(&endpoint);
        assert!(!h.agent.can_run_in_background(&strata), "taken by Po Labs, seen at once");
        drop(labs_subagent);
        assert!(h.agent.can_run_in_background(&strata), "given back as soon as it is done");
        h.agent.end_turn_slot();
        assert_eq!(ledger.used(&endpoint), 1);
    }

    #[tokio::test]
    async fn a_background_task_lets_the_agent_continue_then_returns_its_result() {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        h.agent.subagent_settings.models.clear();
        h.agent.set_server_slots("http://127.0.0.1:8080/v1", 2);
        let (provider, seen) = ScriptedProvider::new(
            "strata",
            vec![
                tool_call_reply("c1", "task", json!({"task": "search", "agent": "explorer", "background": true})),
                text_reply("I carry on my own"),
                text_reply("background result"),
                text_reply("final synthesis"),
            ],
        );
        h.agent.attach("strata", provider);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Delegated { background: true, .. })));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen[0].tools.iter().any(|t| t.name == "task" && t.input_schema["properties"]["background"].is_object()));
        let handed_over = seen[3].messages.last().unwrap().visible_text();
        assert!(handed_over.contains("background result") && handed_over.contains("explorer"), "{handed_over}");
        assert_eq!(h.agent.background_count(), 0);
    }

    struct Answers(question::Answer);

    #[async_trait]
    impl question::Asker for Answers {
        async fn ask(&self, _question: &question::Question) -> Option<question::Answer> {
            Some(self.0.clone())
        }
    }

    fn question_call() -> CompletionResponse {
        tool_call_reply("q1", "question", json!({"question": "Which format?", "options": [
            {"label": "JSON", "description": "readable"},
            {"label": "TOML", "description": "like Cargo"}
        ]}))
    }

    #[tokio::test]
    async fn the_users_answer_goes_back_to_the_model() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        h.agent.set_asker(Arc::new(Answers(question::Answer::Choice(1))));
        let (cloud, seen) = ScriptedProvider::new("cloud", vec![question_call(), text_reply("TOML it is")]);
        h.agent.attach("opus", cloud);
        h.agent.turn("configure", &mut |e| h.events.push(e)).await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen[0].tools.iter().any(|t| t.name == "question"), "the tool is offered");
        let tool_result = seen[1].messages.last().unwrap();
        assert!(matches!(&tool_result.content[0], ContentBlock::ToolResult { content, is_error: false, .. } if content.contains("\"TOML\"")));
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Asked { answer, .. } if answer == "TOML")));
    }

    #[tokio::test]
    async fn without_an_interface_the_question_tool_is_not_offered() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        let (cloud, seen) = ScriptedProvider::new("cloud", vec![text_reply("ok")]);
        h.agent.attach("opus", cloud);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        assert!(!seen.lock().unwrap()[0].tools.iter().any(|t| t.name == "question"));
    }

    #[tokio::test]
    async fn a_subagent_does_not_ask_questions() {
        let (mut h, _keep) = harness(BudgetMode::MaxQuality);
        h.agent.set_asker(Arc::new(Answers(question::Answer::Choice(0))));
        let (cloud, seen) = ScriptedProvider::new(
            "cloud",
            vec![tool_call_reply("c1", "task", json!({"task": "choose", "agent": "planner"})), question_call(), text_reply("plan"), text_reply("finished")],
        );
        h.agent.attach("opus", cloud);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        assert!(!h.events.iter().any(|e| matches!(e, AgentEvent::Asked { .. })), "the question does not reach the user");
        let seen = seen.lock().unwrap();
        let refusal = &seen[2].messages.last().unwrap().content[0];
        assert!(matches!(refusal, ContentBlock::ToolResult { content, is_error: true, .. } if content.contains("main agent")));
    }

    #[tokio::test]
    async fn without_a_free_slot_the_background_task_is_awaited() {
        let (mut h, _keep) = two_servers(BudgetMode::Offline);
        h.agent.subagent_settings.models.clear();
        let (provider, seen) = ScriptedProvider::new(
            "strata",
            vec![
                tool_call_reply("c1", "task", json!({"task": "search", "agent": "explorer", "background": true})),
                text_reply("found"),
                text_reply("finished"),
            ],
        );
        h.agent.attach("strata", provider);
        h.agent.turn("x", &mut |e| h.events.push(e)).await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen[0].tools.iter().any(|t| t.name == "task" && t.input_schema["properties"]["background"].is_null()), "not offered with a single slot");
        assert!(h.events.iter().any(|e| matches!(e, AgentEvent::Delegated { background: false, .. })));
        assert_eq!(seen.len(), 3);
    }
}
