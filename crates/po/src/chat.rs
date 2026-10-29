//! Interactive session.
//!
//! Rendering follows the profile negotiated by `po-term`: in `stream`, everything is written
//! line by line, with no alternate screen or exotic sequence, which works equally well in a
//! pipe, in CI, under Termux or over a flaky SSH link. Richer profiles add color, not a
//! different layout — the same exchange must stay readable everywhere.

use std::io::Write;
use std::sync::Arc;

use async_trait::async_trait;
use po_agent::{Agent, AgentEvent, Approver};
use po_provider::{AnthropicProvider, OpenAiProvider, Provider};
use po_router::{BudgetMode, Router};
use po_term::{ColorDepth, Input, LineEditor, RenderProfile, Terminal};
use po_tools::{Registry, ToolContext, Workspace};
use serde_json::Value;

use po_agent::{SavedSession, SessionStore};

use crate::config::{self, Config, ResolvedModel};
use crate::commands::{self, Extras, Tone};
use crate::local_ui::LocalManager;

/// What the user asks for regarding session resumption.
#[derive(Debug, Clone, PartialEq)]
pub enum Resume {
    /// New session.
    No,
    /// Resume the last session of this directory.
    Last,
    /// Resume a specific session, by identifier.
    Id(String),
}

/// Stable key of a budget mode, for persistence.
pub fn mode_key(mode: BudgetMode) -> &'static str {
    match mode {
        BudgetMode::MaxQuality => "max-quality",
        BudgetMode::Balanced => "balanced",
        BudgetMode::SaveMaxTokens => "save-max-tokens",
        BudgetMode::Offline => "offline",
    }
}

/// Saves the session if it contains anything (an empty session has no business cluttering the
/// resume list), without displaying anything. `id` keeps the identifier from one save to the
/// next: the session is rewritten in the same place every turn, not duplicated.
fn save_session(
    store: &SessionStore,
    agent: &Agent,
    workspace: &std::path::Path,
    mode: BudgetMode,
    id: &mut Option<String>,
) -> Option<Result<String, String>> {
    if agent.session.messages.is_empty() {
        return None;
    }
    // Each agent is saved under its own key and folder: the Po Labs session, working in Po's
    // source, must not overwrite the user's session it was opened from.
    let workspace = if agent.session_key.is_some() { agent.workspace_root() } else { workspace };
    if agent.session_key.is_some() {
        *id = agent.session_key.clone();
    }
    let mut settings = agent.settings.clone();
    settings.work_mode = Some(agent.work_mode().get().label().to_owned());
    let mut saved = SavedSession::from_session(&agent.session, workspace, Some(mode_key(mode))).with_settings(settings);
    // The title written by the model replaces the one taken from the first message.
    if let Some(title) = &agent.title {
        saved.title = title.clone();
    }
    // A paused turn resumes from where it stopped, even after a restart.
    saved.paused = agent.paused_turn();
    if let Some(id) = id.as_ref() {
        saved.id = id.clone();
    }
    Some(match store.save(&saved) {
        Ok(_) => {
            *id = Some(saved.id.clone());
            Ok(saved.id)
        }
        Err(err) => Err(err.to_string()),
    })
}

/// Relaunches Po on session `id` and ends this process with the new instance's exit code.
///
/// The binary is the one `scripts/install.ps1` just installed if it exists, otherwise the
/// current binary. On Windows, a process cannot replace itself: this one waits for the new
/// instance, which inherits the console (tmux, Termius, PowerShell, cmd) and its arguments,
/// minus those of an earlier resume.
fn relaunch(id: &str) -> ! {
    let program = crate::labs::installed_binary();
    let mut args: Vec<String> = Vec::new();
    let mut original = std::env::args().skip(1);
    while let Some(arg) = original.next() {
        match arg.as_str() {
            "--resume" => {
                original.next();
            }
            "-c" | "--continue" => {}
            _ => args.push(arg),
        }
    }
    args.push("--resume".into());
    args.push(id.to_owned());
    let started = std::time::Instant::now();
    match std::process::Command::new(&program).args(&args).status() {
        // A build from Po Labs that dies at once goes back to the binary it replaced: the user
        // keeps a working Po and the session, and the model can fix its change.
        Ok(status) if !status.success() && started.elapsed() < std::time::Duration::from_secs(10) => {
            match crate::labs::previous_binary() {
                Some(previous) if previous.is_file() => {
                    eprintln!("{}", po_i18n::t!("chat.restart.rollback"));
                    let broken = program.with_extension("broken");
                    let _ = std::fs::remove_file(&broken);
                    let _ = std::fs::rename(&program, &broken);
                    if std::fs::copy(&previous, &program).is_ok() {
                        let status = std::process::Command::new(&program).args(&args).status();
                        std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
                    }
                    std::process::exit(status.code().unwrap_or(1))
                }
                _ => std::process::exit(status.code().unwrap_or(1)),
            }
        }
        Ok(status) => std::process::exit(status.code().unwrap_or(0)),
        Err(err) => {
            eprintln!(
                "{}",
                po_i18n::t!("chat.restart.failed", program = program.display(), err = err, id = id)
            );
            std::process::exit(1)
        }
    }
}

fn persist(
    store: &SessionStore,
    agent: &Agent,
    workspace: &std::path::Path,
    mode: BudgetMode,
    palette: &Palette,
    id: &mut Option<String>,
) {
    match save_session(store, agent, workspace, mode, id) {
        Some(Ok(id)) => println!("{}  {}{}", palette.dim, po_i18n::t!("chat.session.saved", id = id), palette.reset),
        Some(Err(err)) => {
            eprintln!("{}  {}{}", palette.warn, po_i18n::t!("chat.session.not_saved", err = err), palette.reset)
        }
        None => {}
    }
}

const SYSTEM_PROMPT: &str = "\
You are Po, a development agent running in the terminal. You have tools to read, write and \
search the workspace, and to run commands.

Work through concrete actions rather than descriptions: if information is missing, fetch it with a \
tool — but first ask yourself where it lives: already in your context (environment, user memory \
below), in the machine's state (processes, ports, services: a shell command), or in the project's \
code (glob, grep). Choose the most targeted search; never scan a whole disk or a whole home folder. \
When a task is self-contained and well delimited — exploring the code, planning, implementing, \
verifying, reviewing — you can hand it to a subagent with the `task` tool, describing the whole \
task: it does not see the conversation.

Answer in the language the user writes in, and keep it concise. Never use emojis.";

/// Simultaneous sessions of each local server, as it announces them (`/props`,
/// `total_slots`, the llama.cpp convention adopted by Strata). This is the reference value for
/// sub-agent parallelism; a silent server counts as one slot.
async fn probe_slots(models: &[ResolvedModel]) -> Vec<(String, usize)> {
    let mut seen: Vec<(String, String)> = Vec::new();
    for model in models {
        if let po_core::Origin::Local { endpoint, .. } = &model.spec.origin
            && !seen.iter().any(|(e, _)| e == endpoint)
        {
            seen.push((endpoint.clone(), model.base_url.clone()));
        }
    }
    let probes = seen.into_iter().map(|(endpoint, base)| async move {
        server_slots_at(&base).await.map(|n| (endpoint, n))
    });
    futures_util::future::join_all(probes).await.into_iter().flatten().collect()
}

/// Slots announced by a llama.cpp-compatible server.
pub async fn server_slots_at(base_url: &str) -> Option<usize> {
    let root = base_url.trim_end_matches('/').trim_end_matches("/v1");
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_millis(600)).build().ok()?;
    let props: Value = client.get(format!("{root}/props")).send().await.ok()?.json().await.ok()?;
    props["total_slots"].as_u64().map(|n| n.max(1) as usize)
}

/// Restores the settings of a saved session. An environment variable given at launch takes
/// precedence: it is the user's most recent gesture.
fn restore_settings(agent: &mut Agent, saved: &po_agent::SessionSettings, palette: &Palette) {
    let from_env = agent.settings.reasoning_effort.clone();
    agent.settings = saved.clone();
    if let Some(mode) = saved.work_mode.as_deref().and_then(po_agent::work_mode::WorkMode::parse) {
        agent.work_mode().set(mode);
    }
    if from_env.is_some() {
        agent.settings.reasoning_effort = from_env;
    }
    let mut parts = Vec::new();
    if let Some(effort) = &agent.settings.reasoning_effort {
        parts.push(po_i18n::t!("chat.settings.reasoning", effort = effort));
    }
    if let Some(t) = agent.settings.temperature {
        parts.push(po_i18n::t!("chat.settings.temperature", t = t));
    }
    if !agent.settings.criteria.is_empty() {
        parts.push(po_i18n::t!("chat.settings.criteria", n = agent.settings.criteria.len()));
    }
    if !parts.is_empty() {
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!("chat.settings.restored", parts = parts.join(" · ")),
            palette.reset
        );
    }
}

/// Startup timing, shown with `PO_TIMINGS=1`: a slow startup is diagnosed by measuring, not by
/// intuition.
struct Stopwatch {
    start: std::time::Instant,
    last: std::cell::Cell<std::time::Instant>,
    on: bool,
}

impl Stopwatch {
    fn start() -> Self {
        let now = std::time::Instant::now();
        Self { start: now, last: std::cell::Cell::new(now), on: std::env::var_os("PO_TIMINGS").is_some() }
    }

    fn lap(&self, label: &str) {
        if self.on {
            let now = std::time::Instant::now();
            eprintln!(
                "po: {}",
                po_i18n::t!(
                    "chat.timing.line",
                    label = format!("{label:<30}"),
                    ms = format!("{:>6.0}", (now - self.last.get()).as_secs_f64() * 1000.0),
                    total = format!("{:.0}", (now - self.start).as_secs_f64() * 1000.0)
                )
            );
            self.last.set(now);
        }
    }
}

/// "1 model", "2 models": a singular count reads badly in the plural. Both forms come from the
/// catalog and each carries its own `{n}`.
fn count_unit(n: usize, singular: &str, plural: &str) -> String {
    (if n > 1 { plural } else { singular }).replace("{n}", &n.to_string())
}

/// System prompt completed with an environment block.
///
/// Without it, the model does not know where its commands start from: on Strata, it began with
/// `cd /` and could no longer find the files (measured on the edit bench). The block contains
/// only facts that are stable across the whole session — no date or time — so that the server
/// reuses the cached prompt from one turn to the next.
pub fn system_prompt(workspace: &std::path::Path, serving: &str) -> String {
    let mut text = format!("{SYSTEM_PROMPT}\n\n{}", environment(workspace));
    if !serving.is_empty() {
        text.push_str(&format!("\n- model running you: {serving}"));
    }
    text
}

/// What Po knows about the server running the model. Telling the model spares it from looking:
/// observed, asked "do you see Strata?", the model searched the disk although it was Strata
/// itself answering.
fn serving_description(models: &[ResolvedModel]) -> String {
    models
        .iter()
        .filter_map(|m| match &m.spec.origin {
            po_core::Origin::Local { backend, endpoint } => {
                Some(format!("{}, served locally by {backend} ({endpoint}, {} tokens of context)", m.spec.id, m.spec.context_window))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// What the model must know about the machine: given to the main agent and the sub-agents.
pub fn environment(workspace: &std::path::Path) -> String {
    let os = match std::env::consts::OS {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    format!(
        "Environment:\n\
         - workspace: {}\n\
         - system: {os}\n\
         - `bash` tool: {}\n\
         - commands already run at the root of the workspace: do not use `cd` to go there, and give \
         the tools relative paths.{}",
        workspace.display(),
        po_tools::shell::detected_shell().description,
        if is_home(workspace) {
            "\n- warning: the workspace is the user's home folder, not a project. Do not run a recursive \
             search (glob, grep, find, ls -R) over the whole folder: target a specific subfolder, or \
             query the system directly."
        } else {
            ""
        },
    )
}

/// Is the workspace the home folder itself?
fn is_home(workspace: &std::path::Path) -> bool {
    let canon = |p: &std::path::Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    dirs::home_dir().is_some_and(|home| canon(&home) == canon(workspace))
}

/// Colors, reduced to what the terminal actually announced.
struct Palette {
    dim: &'static str,
    accent: &'static str,
    warn: &'static str,
    error: &'static str,
    reset: &'static str,
}

impl Palette {
    fn for_terminal(terminal: &Terminal) -> Self {
        if terminal.caps().color == ColorDepth::None {
            return Self { dim: "", accent: "", warn: "", error: "", reset: "" };
        }
        Self {
            dim: "\x1b[2m",
            accent: "\x1b[36m",
            warn: "\x1b[33m",
            error: "\x1b[31m",
            reset: "\x1b[0m",
        }
    }
}

/// Asks the model's questions on standard output: numbered answers, or free text.
struct TerminalAsker {
    non_interactive: bool,
}

#[async_trait]
impl po_agent::question::Asker for TerminalAsker {
    async fn ask(&self, question: &po_agent::question::Question) -> Option<po_agent::question::Answer> {
        use po_agent::question::Answer;
        if self.non_interactive {
            return None;
        }
        let question = question.clone();
        tokio::task::spawn_blocking(move || {
            let clean = po_term::width::sanitize_line;
            println!("\n  ? {}", clean(&question.question));
            for (i, option) in question.options.iter().enumerate() {
                if option.description.is_empty() {
                    println!("    {}. {}", i + 1, clean(&option.label));
                } else {
                    println!("    {}. {} — {}", i + 1, clean(&option.label), clean(&option.description));
                }
            }
            print!("{}", po_i18n::t!("chat.ask.prompt"));
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_err() {
                return None;
            }
            let line = line.trim();
            Some(match line.parse::<usize>() {
                Ok(n) if (1..=question.options.len()).contains(&n) => Answer::Choice(n - 1),
                _ if line.is_empty() => Answer::Declined,
                _ => Answer::Text(line.to_owned()),
            })
        })
        .await
        .unwrap_or(None)
    }
}

/// Asks for the user's approval on standard output.
struct TerminalApprover {
    /// True when input is not interactive: there is then nobody to ask.
    non_interactive: bool,
    assume_yes: bool,
}

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, tool: &str, input: &Value) -> bool {
        if self.assume_yes {
            return true;
        }
        if self.non_interactive {
            // With nobody to ask, refusing is the only safe choice: accepting would mean running
            // unsupervised writes in a script or CI.
            eprintln!("po: {}", po_i18n::t!("chat.approve.non_interactive", tool = tool));
            return false;
        }

        let summary = input
            .get("command")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| input.to_string());
        // Both values are copied before moving to a dedicated thread: the closure must outlive
        // the borrows received by the method.
        let tool = tool.to_owned();

        // Reading blocks a dedicated thread: the async loop must not be frozen.
        tokio::task::spawn_blocking(move || {
            print!("\n{}", po_i18n::t!("chat.approve.prompt", tool = tool, summary = summary));
            let _ = std::io::stdout().flush();
            let mut answer = String::new();
            if std::io::stdin().read_line(&mut answer).is_err() {
                return false;
            }
            matches!(answer.trim().to_ascii_lowercase().as_str(), "o" | "oui" | "y" | "yes")
        })
        .await
        .unwrap_or(false)
    }
}

/// Builds a provider for a resolved model.
fn build_provider(model: &ResolvedModel) -> Arc<dyn Provider> {
    match model.kind.as_str() {
        "anthropic" => Arc::new(AnthropicProvider::with_base_url(
            model.provider_name.clone(),
            model.base_url.clone(),
            model.api_key.clone().unwrap_or_default(),
        )),
        _ => {
            let provider = OpenAiProvider::new(
                model.provider_name.clone(),
                model.base_url.clone(),
                model.api_key.clone(),
            );
            // The reasoning level only applies to local servers: a remote provider that does not
            // know the field may reject the whole request.
            if model.spec.origin.is_local() {
                let kwargs = model.spec.origin.label() == "llama.cpp";
                Arc::new(provider.for_local_server().with_effort_in_template_kwargs(kwargs))
            } else {
                Arc::new(provider)
            }
        }
    }
}

/// `PO_REASONING_EFFORT`: `none`, `low`, `medium` or `high`. An unknown value is reported then
/// ignored, rather than forwarded to a server that would reject it.
fn reasoning_effort_from_env() -> Option<String> {
    let raw = std::env::var("PO_REASONING_EFFORT").ok()?;
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "" => None,
        "none" | "low" | "medium" | "high" => Some(value),
        _ => {
            eprintln!("po: {}", po_i18n::t!("chat.reasoning_ignored", raw = raw));
            None
        }
    }
}

pub fn parse_budget(raw: &str) -> Option<BudgetMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "max-quality" | "qualite" | "quality" => Some(BudgetMode::MaxQuality),
        "balanced" | "equilibre" => Some(BudgetMode::Balanced),
        "save" | "save-max-tokens" | "economie" => Some(BudgetMode::SaveMaxTokens),
        "offline" | "hors-ligne" => Some(BudgetMode::Offline),
        _ => None,
    }
}

/// Can the session open?
///
/// A reachable model is obviously enough. Otherwise, there remains the case that matters:
/// weights on disk and the means to serve them. Po can then start the server itself, and
/// refusing to open would send the user to another terminal for a command that `/local` runs
/// better than they do. A single prompt, however, has nobody to type it.
fn can_open_session(reachable_model: bool, startable: bool, single_prompt: bool) -> bool {
    reachable_model || (startable && !single_prompt)
}

pub async fn run(
    terminal: &mut Terminal,
    budget_override: Option<BudgetMode>,
    assume_yes: bool,
    single_prompt: Option<String>,
    resume: Resume,
) -> Result<(), String> {
    let palette = Palette::for_terminal(terminal);
    let (config, config_path) = Config::load();
    let stopwatch = Stopwatch::start();
    // The hardware (nvidia-smi, ~200 ms) is measured while the ports are probed.
    let hardware = tokio::task::spawn_blocking(LocalManager::detect_lazy);
    let resolved = config::resolve(&config).await;
    stopwatch.lap(po_i18n::t!("chat.timing.models"));

    // Local model management: catalog of the weights present and, if the binary is there, the
    // ability to start a server on demand. Detected before even knowing whether a reachable
    // model exists, because that answer decides whether the session can open.
    let mut local = match hardware.await {
        Ok(mut local) => {
            // With no reachable model, the disk is the only resource: scan it right away.
            if resolved.is_empty() {
                local.refresh();
            }
            local
        }
        Err(_) => LocalManager::detect(),
    };
    stopwatch.lap(po_i18n::t!("chat.timing.hardware"));

    // No reachable model does not mean no model. Measured from an Android emulator over SSH:
    // six GGUF files on disk, `llama-server` installed, and Po refused to open the session,
    // advising to "start a server" — although it can do so itself, through `/local <n>`.
    // Sending the user to another terminal for something the harness does better than they do
    // is exactly the friction we claim to remove. The session therefore opens empty, on the
    // sole condition that there is something to fill it — and never for a single prompt, which
    // has nobody to type `/local`.
    let startable = !local.catalog.is_empty() && local.has_server_binary();
    if !can_open_session(!resolved.is_empty(), startable, single_prompt.is_some()) {
        let hint = if startable {
            po_i18n::t!("chat.no_model.startable")
        } else {
            po_i18n::t!("chat.no_model.start_server")
        };
        return Err(po_i18n::t!("chat.no_model.text", hint = hint));
    }

    let mode = budget_override
        .or_else(|| config.budget.as_deref().and_then(parse_budget))
        .unwrap_or(BudgetMode::Balanced);

    let mut workspace = Workspace::new(std::env::current_dir().map_err(|e| e.to_string())?);
    // The memory notes cited in the prompt must be readable — read only.
    if let Some(dir) = crate::instructions::memory_dir(workspace.root()) {
        workspace.allow_read(dir);
    }
    let ctx = ToolContext::new(workspace);
    let specs: Vec<_> = resolved.iter().map(|m| m.spec.clone()).collect();
    let router = Router::new(specs, mode);

    let non_interactive = !terminal.caps().is_tty;
    // The TUI only makes sense in the rich profile and in an interactive session; everywhere
    // else — pipe, CI, flaky SSH, Termux — stream output remains the nominal mode.
    let use_tui = single_prompt.is_none()
        && !non_interactive
        && terminal.profile() == RenderProfile::Rich;

    // The user's approval goes through the TUI keyboard or the command line, depending on the
    // interface actually displayed.
    let (approver, approvals): (Arc<dyn po_agent::Approver>, _) = if use_tui && !assume_yes {
        let (approver, receiver) = po_tui::approval_channel();
        (approver, Some(receiver))
    } else {
        (Arc::new(TerminalApprover { non_interactive, assume_yes }), None)
    };

    // Declared MCP servers are launched before the session: their tools join the registry and
    // become, for the model, tools like any other.
    let mut registry = Registry::with_builtins();
    if !config.mcp.is_empty() {
        let (tools, failures) = po_mcp::connect_all(&config.mcp).await;
        let count = tools.len();
        for tool in tools {
            registry.register(tool);
        }
        if count > 0 {
            println!(
                "{}  {}{}",
                palette.dim,
                po_i18n::t!("chat.mcp.connected", count = count, servers = config.mcp.len() - failures.len()),
                palette.reset
            );
        }
        for (name, error) in failures {
            eprintln!(
                "{}  {}{}",
                palette.warn,
                po_i18n::t!("chat.mcp.unavailable", name = name, error = error),
                palette.reset
            );
        }
    }

    let workspace_dir = std::env::current_dir().unwrap_or_default();
    let mut extras = Extras { workspace: workspace_dir.clone(), ..Default::default() };
    for server in &config.mcp {
        let tools = registry.names().iter().filter(|n| n.starts_with(&po_mcp::tool_prefix(&server.name))).count();
        if tools > 0 {
            extras.mcp.push(commands::McpEntry { config: server.clone(), tools });
        }
    }
    // The servers from the project's `.mcp.json` are offered, not launched: it is third-party
    // code that came with the repository, and the user must have wanted it.
    if let Ok(text) = std::fs::read_to_string(workspace_dir.join(".mcp.json")) {
        extras.mcp_available = po_mcp::read_mcp_json(&text)
            .into_iter()
            .filter(|c| !extras.mcp.iter().any(|e| e.config.name == c.name))
            .collect();
    }
    stopwatch.lap("MCP");
    extras.skills = po_tools::skills::discover(&workspace_dir);
    if !extras.skills.is_empty() {
        registry.register(Arc::new(po_tools::skills::SkillTool::new(extras.skills.clone())));
    }

    let instructions = crate::instructions::load(&workspace_dir);
    extras.instructions = instructions.files.clone();
    let mut system = system_prompt(&workspace_dir, &serving_description(&resolved));
    system.push_str(&instructions.text);
    let labs_approver = approver.clone();
    let mut agent = Agent::new(router, registry, ctx, system, approver);
    let mut labs_asker: Option<Arc<dyn po_agent::question::Asker>> = None;
    // The model's questions go through the interface actually displayed, like approvals.
    let questions = if approvals.is_some() {
        let (asker, receiver) = po_tui::question::channel();
        labs_asker = Some(asker.clone());
        agent.set_asker(asker);
        Some(receiver)
    } else {
        agent.set_asker(Arc::new(TerminalAsker { non_interactive: non_interactive || single_prompt.is_some() }));
        None
    };
    agent.settings.reasoning_effort = reasoning_effort_from_env();
    stopwatch.lap(po_i18n::t!("chat.timing.skills"));
    let slots = probe_slots(&resolved).await;
    let mut parking_endpoint: Option<String> = None;
    for (endpoint, count) in &slots {
        agent.set_server_slots(endpoint.clone(), *count);
    }
    // A single-slot local server loses a conversation's cache to any other one (a subagent, a
    // later session): it is parked on disk instead, when the server can save its slots.
    if let Some((endpoint, base)) = resolved.iter().find_map(|m| match &m.spec.origin {
        po_core::Origin::Local { endpoint, .. } if slots.iter().any(|(e, n)| e == endpoint && *n == 1) => {
            Some((endpoint.clone(), m.base_url.clone()))
        }
        _ => None,
    }) {
        crate::kv::setup(&mut agent, &endpoint, &base);
        parking_endpoint = Some(endpoint);
    }
    stopwatch.lap(po_i18n::t!("chat.timing.probe"));
    agent.set_profiles(po_agent::subagents::discover(&workspace_dir, po_agent::subagents::global_dir().as_deref()));
    agent.subagent_settings = po_agent::subagents::SubagentSettings::load();
    agent.set_subagent_preamble(environment(&workspace_dir));
    crate::labs::register_tools(&mut agent);
    if crate::labs::enabled() {
        crate::labs::activate(&mut agent, true, true);
    }
    for model in &resolved {
        agent.attach(model.spec.id.clone(), build_provider(model));
    }

    // Resume a previous session, if requested, before any exchange.
    let store = SessionStore::default_location();
    // Reserved from the start: the parking of the server cache keys the conversation with it,
    // and a resumed session keeps its own instead of being saved again as a copy.
    let mut session_id: Option<String> = Some(po_agent::store::generate_id());
    let workspace_path = std::env::current_dir().map_err(|e| e.to_string())?;
    match &resume {
        Resume::No => {}
        Resume::Last => match store.latest_for(&workspace_path) {
            Some(saved) => {
                println!(
                    "{}  {}{}",
                    palette.dim,
                    po_i18n::t!(
                        "chat.resume.last",
                        title = saved.title,
                        exchanges = saved.exchange_count()
                    ),
                    palette.reset
                );
                restore_settings(&mut agent, &saved.settings, &palette);
                agent.title = Some(saved.title.clone());
                session_id = Some(saved.id.clone());
                let paused = saved.paused.clone();
                agent.session = saved.into_session();
                if let Some(subagents) = paused {
                    agent.restore_paused(subagents);
                }
            }
            None => println!("{}  {}{}", palette.warn, po_i18n::t!("chat.resume.none"), palette.reset),
        },
        Resume::Id(id) => match store.load(id) {
            Ok(saved) => {
                println!("{}  {}{}", palette.dim, po_i18n::t!("chat.resume.id", title = saved.title), palette.reset);
                restore_settings(&mut agent, &saved.settings, &palette);
                agent.title = Some(saved.title.clone());
                session_id = Some(saved.id.clone());
                let paused = saved.paused.clone();
                agent.session = saved.into_session();
                if let Some(subagents) = paused {
                    agent.restore_paused(subagents);
                }
            }
            Err(err) => println!("{}  {err}{}", palette.warn, palette.reset),
        },
    }
    agent.session_key = session_id.clone();
    if !matches!(resume, Resume::No) && crate::kv::unpark_on_resume(&agent).await {
        println!("{}  {}{}", palette.dim, po_i18n::t!("chat.resume.cache_restored"), palette.reset);
    }
    // A resumed session keeps its own system prompt; Po Labs, on since, must still be in it (the
    // restart after `po_apply` resumes the session).
    // Its guide then comes as a note: changing the saved system prompt would make the server
    // reread the whole conversation (measured: 6,283 tokens out of 6,283 on a resumed session).
    let has_guide = agent.session.system.contains(crate::labs::GUIDE_TITLE)
        || agent.session.messages.iter().any(|m| m.visible_text().contains(crate::labs::GUIDE_TITLE));
    if crate::labs::enabled() && !has_guide {
        let guide = crate::labs::guide(crate::labs::source_root().as_deref());
        agent.add_context(format!("[Po Labs is enabled]\n{guide}"));
    }
    // A change of Po Labs just installed: the session knows what the user is trying, to sort what
    // they say about it.
    if crate::labs::enabled() {
        let book = po_tui::labs::Notebook::load();
        crate::labs::clean_old_binaries(book.trial.as_ref().and_then(|t| t.previous.as_deref()));
        // Told once, to the first session opened on it.
        let told = book.trial.as_ref().filter(|t| t.announced != t.head).and_then(|_| {
            po_tui::labs::Notebook::modify(|book| {
                let trial = book.trial.as_mut().filter(|t| t.announced != t.head)?;
                trial.announced = trial.head.clone();
                Some(trial.summary.clone())
            })
            .ok()
            .and_then(|(_, summary)| summary)
        });
        if let Some(summary) = told {
            agent.add_context(format!(
                "[Po Labs] Po restarted with this change on trial, the user is trying it: {summary}. What they say \
                 about it is a revise for po_labs; the next requests wait for their verdict."
            ));
        }
    }

    // Header: what Po found on its own. It is the first thing the user reads, and it must
    // answer "what will answer me, and how much does it cost".
    println!(
        "{}po{} — {}",
        palette.accent,
        palette.reset,
        po_i18n::t!(
            "chat.start.models",
            models = count_unit(
                resolved.len(),
                po_i18n::t!("chat.start.model_one"),
                po_i18n::t!("chat.start.model_many")
            ),
            mode = mode.label()
        )
    );
    let (locals, cloud): (Vec<_>, Vec<_>) = resolved.iter().partition(|m| m.spec.is_local());
    if !locals.is_empty() {
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!(
                "chat.start.local",
                list = locals
                    .iter()
                    .map(|m| format!("{} ({} · {} tokens)", m.spec.id, m.spec.origin.label(), m.spec.context_window))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            palette.reset
        );
    }
    if !cloud.is_empty() {
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!("chat.start.cloud", list = cloud.iter().map(|m| m.spec.id.as_str()).collect::<Vec<_>>().join(", ")),
            palette.reset
        );
    }
    if let Some(path) = config_path {
        println!("{}  {}{}", palette.dim, po_i18n::t!("chat.start.config", path = path.display()), palette.reset);
    }
    if resolved.is_empty() {
        println!("{}  {}{}", palette.warn, po_i18n::t!("chat.start.no_server"), palette.reset);
    }
    if !local.catalog.is_empty() {
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!(
                "chat.start.on_disk",
                models = count_unit(
                    local.catalog.len(),
                    po_i18n::t!("chat.start.gguf_one"),
                    po_i18n::t!("chat.start.gguf_many")
                )
            ),
            palette.reset
        );
    }
    if !extras.skills.is_empty() {
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!(
                "chat.start.skills",
                skills = count_unit(
                    extras.skills.len(),
                    po_i18n::t!("chat.start.skill_one"),
                    po_i18n::t!("chat.start.skill_many")
                )
            ),
            palette.reset
        );
    }
    if !extras.mcp_available.is_empty() {
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!(
                "chat.start.mcp_declared",
                servers = count_unit(
                    extras.mcp_available.len(),
                    po_i18n::t!("chat.start.mcp_one"),
                    po_i18n::t!("chat.start.mcp_many")
                )
            ),
            palette.reset
        );
    }
    if !extras.instructions.is_empty() {
        let names: Vec<String> = extras
            .instructions
            .iter()
            .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!("chat.start.instructions", names = names.join(", ")),
            palette.reset
        );
    }
    if let Some(model) = agent.session_model() {
        let slots = agent.parallel_limit(&model);
        let custom = agent.profiles().iter().filter(|p| p.source != po_agent::subagents::Source::Builtin).count();
        println!(
            "{}  {}{}",
            palette.dim,
            po_i18n::t!(
                "chat.start.subagents",
                profiles = count_unit(
                    agent.profiles().len(),
                    po_i18n::t!("chat.start.profile_one"),
                    po_i18n::t!("chat.start.profile_many")
                ),
                custom = if custom > 0 { po_i18n::t!("chat.start.custom", n = custom) } else { String::new() },
                parallel = if slots > 1 {
                    po_i18n::t!("chat.start.parallel", n = slots)
                } else {
                    po_i18n::t!("chat.start.sequential").to_owned()
                }
            ),
            palette.reset
        );
    }
    println!("{}  {}{}\n", palette.dim, po_i18n::t!("chat.start.help_hint"), palette.reset);

    if let (Some(approvals), Some(questions)) = (approvals, questions) {
        let mut host = PoHost {
            local,
            extras,
            autosave: Autosave { store: SessionStore::default_location(), workspace: workspace_path.clone(), mode, id: session_id.clone() },
            labs: LabsFactory {
                resolved: resolved.clone(),
                mode,
                approver: labs_approver,
                asker: labs_asker,
                slots: slots.clone(),
                parking: parking_endpoint.clone(),
            },
        };
        let shown = po_tui::run(terminal, &mut agent, &mut host, approvals, questions)
            .await
            .map_err(|err| err.to_string())?;
        if shown {
            if po_tui::take_restart() {
                // In-place restart: the session is saved, then the installed binary (perhaps just
                // rebuilt) resumes it. A server launched by Po keeps running: the new instance
                // finds it again by probing the ports.
                let a = &mut host.autosave;
                persist(&a.store, &agent, &a.workspace, a.mode, &palette, &mut a.id);
                crate::kv::park_on_exit(&agent).await;
                // `/resume` switches to another session; otherwise this one goes on.
                let Some(id) = crate::kv::take_resume_target().or_else(|| a.id.clone()) else {
                    return Err(po_i18n::t!("chat.restart.not_saved").to_owned());
                };
                relaunch(&id);
            }
            host.local.deactivate(&mut agent).await;
            crate::kv::park_on_exit(&agent).await;
            let a = &mut host.autosave;
            persist(&a.store, &agent, &a.workspace, a.mode, &palette, &mut a.id);
            let ledger = &agent.session.ledger;
            if ledger.total_tokens() > 0 {
                println!("{}", po_i18n::t!("chat.session.ledger", ledger = ledger));
            }
            return Ok(());
        }
        // Full screen could not be established: carry on in stream mode rather than abandoning
        // the session. Questions then go through the command line.
        agent.set_asker(Arc::new(TerminalAsker { non_interactive }));
        println!("{}  {}{}", palette.dim, po_i18n::t!("chat.fullscreen_unavailable"), palette.reset);
        local = host.local;
        extras = host.extras;
    }

    // Non-interactive mode: a single request, then exit. This is what makes Po usable in a
    // script or a pipe.
    if let Some(prompt) = single_prompt {
        let result = turn(&mut agent, &prompt, &palette, false).await;
        local.deactivate(&mut agent).await;
        crate::kv::park_on_exit(&agent).await;
        persist(&store, &agent, &workspace_path, mode, &palette, &mut session_id);
        return result;
    }

    let mut editor = LineEditor::new();
    let prompt = format!("{}›{} ", palette.accent, palette.reset);
    let unicode = terminal.caps().unicode;
    let interactive = !non_interactive;
    loop {
        let read = po_term::input::read_line(
            &mut editor,
            &prompt,
            PROMPT_WIDTH,
            interactive,
            &SHORTCUTS,
            unicode,
        );
        let line = match read {
            Input::Eof => {
                println!();
                break; // end of input: Ctrl+D, or pipe exhausted
            }
            // Ctrl+C abandons the line, not the session: losing an entire conversation because
            // one wanted to erase a sentence would be a disproportionate punishment.
            Input::Interrupt => {
                println!("{}  {}{}", palette.dim, po_i18n::t!("chat.line.abandoned"), palette.reset);
                continue;
            }
            Input::BackTab => {
                let mode = agent.work_mode().cycle();
                println!(
                    "\r{}  {}{}",
                    palette.accent,
                    po_i18n::t!("chat.line.work_mode", mode = mode.label()),
                    palette.reset
                );
                continue;
            }
            Input::Shortcut(letter) => {
                shortcut(letter, &mut agent, &mut local, &mut editor, &palette);
                continue;
            }
            Input::Line(line) => line,
        };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        editor.remember(&line);
        let line = line.as_str();

        if line.starts_with('/') || line.starts_with('!') {
            let reply = commands::execute(&mut agent, &mut local, &mut extras, line).await;
            for (tone, text) in &reply.lines {
                let (a, b) = commands::halves(text);
                let (color, shown) = match tone {
                    Tone::Plain => ("", text.clone()),
                    Tone::Dim => (palette.dim, text.clone()),
                    Tone::Accent | Tone::Section => (palette.accent, text.clone()),
                    Tone::Warn => (palette.warn, text.clone()),
                    Tone::Item if b.is_empty() => ("", format!("  {a}")),
                    Tone::Item => ("", format!("  {a} — {b}")),
                    Tone::Field => ("", format!("  {a}: {b}")),
                };
                let reset = if color.is_empty() { "" } else { palette.reset };
                println!("{color}  {}{reset}", po_term::width::sanitize_line(&shown));
            }
            if matches!(line.split_whitespace().next(), Some("/aide" | "/help" | "/?")) {
                println!("\n{}", shortcuts_help());
            }
            if reply.quit {
                break;
            }
            if let Some(target) = crate::kv::take_resume_target() {
                persist(&store, &agent, &workspace_path, mode, &palette, &mut session_id);
                crate::kv::park_on_exit(&agent).await;
                relaunch(&target);
            }
            continue;
        }

        local.touch();
        if let Err(err) = turn(&mut agent, line, &palette, !non_interactive).await {
            eprintln!("{}{}{}", palette.error, po_i18n::t!("chat.line.error", err = err), palette.reset);
        }
        // Saved every turn: a killed session loses at most the current turn.
        let _ = save_session(&store, &agent, &workspace_path, mode, &mut session_id);
        // A forgotten server keeps VRAM locked up; free it as soon as it has waited long enough.
        if let Some(notice) = local.reap_idle(&mut agent).await {
            println!("{}  · {notice}{}", palette.dim, palette.reset);
        }
    }

    // A server launched by Po stops with Po: leaving 10 GB of VRAM occupied after exit would be
    // a bad surprise.
    local.deactivate(&mut agent).await;
    crate::kv::park_on_exit(&agent).await;
    persist(&store, &agent, &workspace_path, mode, &palette, &mut session_id);

    let ledger = &agent.session.ledger;
    if ledger.total_tokens() > 0 {
        println!("{}{}{}", palette.dim, po_i18n::t!("chat.session.ledger", ledger = ledger), palette.reset);
    }
    Ok(())
}

/// Help text of the session. Extracted into a function so that a test can verify that no
/// shortcut is missing from it: an undocumented shortcut does not exist.
fn shortcuts_help() -> &'static str {
    po_i18n::t!("chat.shortcuts")
}

/// Shortcuts active while typing, as `Ctrl+<letter>`: the letters reserved for shortcuts,
/// outside line editing.
///
/// The choice of letters is not free: Ctrl+A/E/U/K/W/D have always belonged to line editing,
/// and commandeering them would break everyone's reflexes. What remains are lightly used
/// combinations that clash with neither bash, nor tmux (prefix Ctrl+B *followed* by a key), nor
/// the user's multiplexer.
///
/// Neither `n` nor `t`: measured from Termius on Android, `Ctrl+N` and `Ctrl+T` never reach the
/// application — the client keeps them for "new session" and "new tab". A shortcut the client
/// intercepts does not exist, however central it is.
const SHORTCUTS: [char; 5] = ['g', 'l', 'p', 'r', 'y'];

/// Visible width of the "›" prompt followed by a space. Color codes do not count.
const PROMPT_WIDTH: usize = 2;

/// Runs a shortcut without leaving the conversation.
///
/// No shortcut triggers a heavy action on its own: switching a budget mode is instantaneous and
/// reversible, but swapping a multi-gigabyte model must never start from a key pressed by
/// mistake. Those *prepare* the command and let the user confirm.
fn shortcut(
    letter: char,
    agent: &mut Agent,
    local: &mut LocalManager,
    editor: &mut LineEditor,
    palette: &Palette,
) {
    match letter {
        // Ctrl+G — the product's central gesture: switch from quality to token savings
        // mid-sentence, without losing what was being typed.
        'g' => {
            let mode = agent.router_mut().cycle_mode();
            println!(
                "\r{}  {}{}",
                palette.accent,
                po_i18n::t!("chat.shortcut.budget_mode", mode = mode.label()),
                palette.reset
            );
        }
        // Ctrl+L — the local models, with the server state, then the pre-filled command.
        'l' => {
            local.refresh();
            if local.catalog.is_empty() {
                println!("\r{}  {}{}", palette.warn, po_i18n::t!("chat.shortcut.no_gguf"), palette.reset);
                return;
            }
            for entry in local.listing() {
                println!("\r{entry}");
            }
            println!("{}  {}{}", palette.dim, po_i18n::t!("chat.shortcut.local_hint"), palette.reset);
            editor.prefill("/local ");
        }
        // Ctrl+P — releases the pinned model, or prepares pinning another.
        'p' => {
            if agent.router().pinned().is_some() {
                agent.router_mut().pin(None);
                println!("\r{}  {}{}", palette.dim, po_i18n::t!("chat.shortcut.auto_routing"), palette.reset);
            } else {
                for model in agent.router().models() {
                    let origin = if model.is_local() { "local" } else { "cloud" };
                    println!("\r  {:<40} {origin}", model.id);
                }
                editor.prefill("/modele ");
            }
        }
        // Ctrl+R — give the VRAM back without leaving the session.
        'r' => {
            editor.prefill("/local stop");
        }
        // Ctrl+Y — the spending, the very question one asks before delegating.
        'y' => {
            println!("\r{}  {}{}", palette.dim, agent.session.ledger, palette.reset);
        }
        _ => {}
    }
}

async fn turn(
    agent: &mut Agent,
    input: &str,
    palette: &Palette,
    interactive: bool,
) -> Result<(), String> {
    use tokio::sync::mpsc;

    // Events go through a channel rather than a direct call: this is what lets a waiting
    // indicator spin *while* the model works. A busy local model easily takes half a minute to
    // answer, and without the slightest sign of life the user concludes the program is stuck.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut render_state = RenderState::new(interactive);
    let mut progress = Progress::new(interactive, palette);

    let result = {
        let mut sink = move |event| {
            let _ = tx.send(event);
        };
        let turn = agent.turn(input, &mut sink);
        tokio::pin!(turn);

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(400));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                outcome = &mut turn => break outcome,
                Some(event) = rx.recv() => {
                    progress.clear();
                    let waiting = matches!(
                        event,
                        AgentEvent::ModelSelected { .. }
                            | AgentEvent::ToolStarted { .. }
                            | AgentEvent::Delegated { .. }
                    );
                    render(event, palette, &mut render_state);
                    if waiting {
                        progress.start();
                    }
                }
                _ = ticker.tick() => progress.tick(),
            }
        }
    };

    progress.clear();
    // The last events may arrive after the future has finished.
    while let Ok(event) = rx.try_recv() {
        render(event, palette, &mut render_state);
    }

    result.map_err(|err| err.to_string())
}

/// Waiting indicator, erased before each write so as never to dirty the transcript.
///
/// It only appears in an interactive session: in a pipe, a log file or CI, a line that rewrites
/// itself in a loop would make no sense.
struct Progress {
    active: bool,
    running: bool,
    shown: bool,
    started: std::time::Instant,
    frame: usize,
    dim: &'static str,
    reset: &'static str,
}

/// ASCII animation: it must stay readable on a terminal without Unicode.
const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];

impl Progress {
    fn new(active: bool, palette: &Palette) -> Self {
        Self {
            active,
            running: false,
            shown: false,
            started: std::time::Instant::now(),
            frame: 0,
            dim: palette.dim,
            reset: palette.reset,
        }
    }

    /// Starts the count after an event that announces a wait.
    fn start(&mut self) {
        if !self.active {
            return;
        }
        self.running = true;
        self.started = std::time::Instant::now();
        self.frame = 0;
    }

    fn tick(&mut self) {
        if !self.active || !self.running {
            return;
        }
        self.frame = (self.frame + 1) % FRAMES.len();
        let seconds = self.started.elapsed().as_secs();
        // Stay discreet during the first second: most answers arrive before.
        if seconds == 0 && self.frame % 2 == 1 {
            return;
        }
        print!("\r{}  {} {seconds} s{}", self.dim, FRAMES[self.frame], self.reset);
        let _ = std::io::stdout().flush();
        self.shown = true;
    }

    fn clear(&mut self) {
        self.running = false;
        if self.shown {
            // Erase the whole line: the counter may be longer than what follows.
            print!("\r\x1b[2K");
            let _ = std::io::stdout().flush();
            self.shown = false;
        }
    }
}

/// State of a turn's stream display.
struct RenderState {
    /// Fragments were written without a final line break.
    printed: bool,
    /// Terminal width, when the output is one. In a pipe, the text stays raw: a script must not
    /// receive line breaks the model did not write.
    width: Option<usize>,
    wrapper: Option<po_term::wrap::StreamWrapper>,
    /// The text being written is reasoning, not the answer.
    thinking: bool,
}

impl RenderState {
    fn new(interactive: bool) -> Self {
        let width = interactive.then(|| po_term::terminal_size().0 as usize).filter(|w| *w >= 20);
        Self { printed: false, width, wrapper: width.map(new_wrapper), thinking: false }
    }

    /// Writes a report line, wrapped at the " · " separators if the screen is narrow.
    fn line(&self, color: &str, reset: &str, prefix: &str, text: &str) {
        let text = po_term::width::sanitize_line(text);
        let Some(width) = self.width else {
            println!("{color}{prefix}{text}{reset}");
            return;
        };
        let indent = " ".repeat(po_term::width::display_width(prefix, po_term::UnicodeLevel::Full));
        let room = width.saturating_sub(indent.len()).max(10);
        for (i, piece) in po_term::wrap::wrap_segments(&text, room, "", po_term::UnicodeLevel::Full)
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { prefix } else { indent.as_str() };
            println!("{color}{lead}{piece}{reset}");
        }
    }
}

fn new_wrapper(width: usize) -> po_term::wrap::StreamWrapper {
    po_term::wrap::StreamWrapper::new(width, po_term::UnicodeLevel::Full)
}

fn render(event: AgentEvent, palette: &Palette, state: &mut RenderState) {
    match event {
        AgentEvent::ModelSelected { model, class, reason } => {
            let suffix = if reason.is_noteworthy() {
                format!(" ({})", reason.explain())
            } else {
                String::new()
            };
            let announcement = format!("{model} · {}{suffix}", class.label());
            // Each tool round trip reselects the model: repeating it every time drowned the
            // conversation. It is only announced when it changes.
            static LAST_ANNOUNCED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
            let mut last = LAST_ANNOUNCED.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_deref() != Some(announcement.as_str()) {
                println!("{}▸ {announcement}{}", palette.dim, palette.reset);
                *last = Some(announcement);
            }
        }
        AgentEvent::ThinkingDelta(_) => {}
        AgentEvent::ThinkingText(text) => {
            // Reasoning is written dimmed, as it flows, and gives way to the answer.
            let text = po_term::width::sanitize_line(&text);
            let out = match state.wrapper.as_mut() {
                Some(wrapper) => wrapper.push(&text),
                None => text,
            };
            print!("{}{out}{}", palette.dim, palette.reset);
            let _ = std::io::stdout().flush();
            state.printed = true;
            state.thinking = true;
        }
        AgentEvent::Queued => {
            println!("{}  ⏳ {}{}", palette.dim, po_i18n::t!("chat.event.queued"), palette.reset);
        }
        AgentEvent::CallStats(stats) => {
            if let Some(line) = stats.summary() {
                state.line(palette.dim, palette.reset, "  ↳ ", &line);
            }
        }
        AgentEvent::Thinking(_) => {
            // Reasoning is not displayed by default: it is long, and showing it would drown the
            // answer. It stays in the history and will be viewable.
        }
        // When streaming, text is written as it arrives, without line breaks: this is what makes
        // a local model at a few tokens per second bearable.
        AgentEvent::TextDelta(fragment) => {
            if state.thinking {
                // End of the displayed reasoning: the answer starts on its own line.
                if let Some(wrapper) = state.wrapper.as_mut() {
                    print!("{}", wrapper.finish());
                }
                println!("\n");
                state.wrapper = state.width.map(new_wrapper);
                state.thinking = false;
            }
            // The text comes from the model: it must not be able to drive the terminal.
            let text = po_term::width::strip_emoji(&po_term::width::sanitize_line(&fragment));
            match state.wrapper.as_mut() {
                Some(wrapper) => print!("{}", wrapper.push(&text)),
                None => print!("{text}"),
            }
            let _ = std::io::stdout().flush();
            state.printed = true;
        }
        AgentEvent::Text(text) => {
            // If the fragments already wrote everything, simply close the line.
            if state.printed {
                if let Some(wrapper) = state.wrapper.as_mut() {
                    print!("{}", wrapper.finish());
                }
                println!();
                state.printed = false;
            } else {
                match state.width {
                    Some(width) => {
                        let mut wrapper = new_wrapper(width);
                        let mut out = wrapper.push(&po_term::width::strip_emoji(&po_term::width::sanitize_line(&text)));
                        out.push_str(&wrapper.finish());
                        println!("{out}");
                    }
                    None => println!("{}", po_term::width::strip_emoji(&po_term::width::sanitize_line(&text))),
                }
            }
            // The next text starts again from column zero.
            state.wrapper = state.width.map(new_wrapper);
        }
        AgentEvent::ToolStarted { name, summary } => {
            println!("{}  ⋯ {name} {}{}", palette.dim, po_term::width::sanitize_line(&summary), palette.reset);
        }
        AgentEvent::ToolFinished { name, summary, is_error } => {
            let color = if is_error { palette.warn } else { palette.dim };
            println!("{color}  {} {name} {}{}", if is_error { "✗" } else { "✓" }, po_term::width::sanitize_line(&summary), palette.reset);
        }
        // In line mode, only the counts: the full listing would bury the conversation.
        AgentEvent::ToolDiff { diff } => {
            if let Some((added, removed)) = po_tools::diff::counts(&diff) {
                println!("{}    +{added}{} {}-{removed}{}", palette.accent, palette.reset, palette.error, palette.reset);
            }
        }
        AgentEvent::ToolDenied { name, summary } => {
            println!(
                "{}  ✗ {}{}",
                palette.warn,
                po_i18n::t!("chat.event.denied", name = name, summary = po_term::width::sanitize_line(&summary)),
                palette.reset
            );
        }
        AgentEvent::Delegated { model, agent, task, background } => {
            let short = po_term::width::sanitize_line(&task.lines().next().unwrap_or("").chars().take(60).collect::<String>());
            println!(
                "{}  → {agent} · {model}{} — {short}{}",
                palette.accent,
                if background { po_i18n::t!("chat.event.in_background") } else { "" },
                palette.reset
            );
        }
        AgentEvent::Retrying { model, attempt, error } => {
            println!(
                "{}  ↻ {}{}",
                palette.warn,
                po_i18n::t!(
                    "chat.event.retrying",
                    model = model,
                    attempt = attempt,
                    error = po_term::width::sanitize_line(&error)
                ),
                palette.reset
            );
        }
        AgentEvent::ModelDisabled { model, reason } => {
            println!(
                "{}  ✗ {}{}",
                palette.error,
                po_i18n::t!("chat.event.model_disabled", model = model, reason = po_term::width::sanitize_line(&reason)),
                palette.reset
            );
        }
        AgentEvent::Notice(text) => {
            println!("{}  · {}{}", palette.dim, po_term::width::sanitize_line(&text), palette.reset);
        }
        AgentEvent::Asked { answer, .. } => {
            println!("{}  → {}{}", palette.accent, po_term::width::sanitize_line(&answer), palette.reset);
        }
        AgentEvent::TurnCompleted { usage, cost, local_share, local_seconds } => {
            // Local compute time only appears when there is some: on an entirely cloud session,
            // showing it at zero would be noise.
            let mut parts = vec![po_i18n::t!("chat.event.tokens", n = usage.total())];
            if cost > 0.0 {
                parts.push(format!("${cost:.4}"));
            }
            if local_seconds >= 0.05 {
                parts.push(po_i18n::t!("chat.event.local_compute", secs = format!("{local_seconds:.1}")));
                if local_share < 0.999 {
                    parts.push(po_i18n::t!("chat.event.local_share", pct = format!("{:.0}", local_share * 100.0)));
                }
            }
            state.line(palette.dim, palette.reset, "  ── ", &parts.join(" · "));
        }
    }
}

/// Links the TUI to the harness: it is through this bridge that keyboard shortcuts act on the
/// router and on the local servers.
struct PoHost {
    local: LocalManager,
    extras: Extras,
    autosave: Autosave,
    labs: LabsFactory,
}

/// What it takes to open the Po Labs session: the same models, approvals and questions as the
/// user's session, but its own conversation, in Po's source.
struct LabsFactory {
    resolved: Vec<ResolvedModel>,
    mode: BudgetMode,
    approver: Arc<dyn po_agent::Approver>,
    asker: Option<Arc<dyn po_agent::question::Asker>>,
    slots: Vec<(String, usize)>,
    parking: Option<String>,
}

impl LabsFactory {
    fn build(&self, main: &Agent) -> Option<Agent> {
        // Its own copy of Po's source, kept in git: the user's checkout is never touched.
        let source = crate::labs::workspace()?;
        let book = po_tui::labs::Notebook::load();
        let router = Router::new(self.resolved.iter().map(|m| m.spec.clone()).collect(), self.mode);
        let mut registry = Registry::with_builtins();
        registry.register(Arc::new(crate::labs::ApplyTool::for_workspace(source.clone())));
        let skills = po_tools::skills::discover(&source);
        if !skills.is_empty() {
            registry.register(Arc::new(po_tools::skills::SkillTool::new(skills)));
        }
        let mut system = system_prompt(&source, &serving_description(&self.resolved));
        system.push_str("\n\n");
        system.push_str(&crate::labs::implementation_guide(&source, &book.trial_text(), &book.pending_text()));
        let ctx = ToolContext::new(Workspace::new(&source));
        // Its requests say who asks: working alongside, they show in the user's session.
        let approver: Arc<dyn Approver> = Arc::new(LabsApprover(self.approver.clone()));
        let mut agent = Agent::new(router, registry, ctx, system, approver);
        let handle = agent.pause_handle();
        agent.registry_mut().register(Arc::new(crate::labs::PauseTool { handle }));
        if let Some(asker) = &self.asker {
            agent.set_asker(Arc::new(LabsAsker(asker.clone())));
        }
        agent.settings = main.settings.clone();
        // Exploring, changing and building take more steps than a conversation turn.
        agent.max_steps = 150;
        // The work mode the user chose (plan, builder, bypass) applies to Po Labs too.
        agent.work_mode().set(main.work_mode().get());
        for (endpoint, count) in &self.slots {
            agent.set_server_slots(endpoint.clone(), *count);
        }
        agent.set_profiles(po_agent::subagents::discover(&source, po_agent::subagents::global_dir().as_deref()));
        agent.subagent_settings = po_agent::subagents::SubagentSettings::load();
        agent.set_subagent_preamble(environment(&source));
        for model in &self.resolved {
            agent.attach(model.spec.id.clone(), build_provider(model));
        }
        // Its whole workspace is Po's source: changes there need no approval.
        agent.open_for_changes(&source);
        if let Some(endpoint) = &self.parking {
            crate::kv::attach(&mut agent, endpoint);
        }
        agent.title = Some("Po Labs".into());
        // The session left paused (or interrupted by a restart) is taken up where it was.
        let store = SessionStore::default_location();
        match book.session.as_ref().and_then(|id| store.load(id).ok()) {
            Some(saved) => {
                agent.session_key = Some(saved.id.clone());
                let paused = saved.paused.clone();
                agent.session = saved.into_session();
                if let Some(subagents) = paused {
                    agent.restore_paused(subagents);
                }
            }
            None => {
                let id = po_agent::store::generate_id();
                agent.session_key = Some(id.clone());
                let _ = po_tui::labs::Notebook::modify(|book| book.session = Some(id));
            }
        }
        Some(agent)
    }
}

/// The user's approver, for Po Labs: its requests are marked as its own.
struct LabsApprover(Arc<dyn Approver>);

#[async_trait]
impl Approver for LabsApprover {
    async fn approve(&self, tool: &str, input: &Value) -> bool {
        self.0.approve(&format!("Po Labs · {tool}"), input).await
    }
}

/// The user's asker, for Po Labs: its questions are marked as its own.
struct LabsAsker(Arc<dyn po_agent::question::Asker>);

#[async_trait]
impl po_agent::question::Asker for LabsAsker {
    async fn ask(&self, question: &po_agent::question::Question) -> Option<po_agent::question::Answer> {
        let mut marked = question.clone();
        marked.question = format!("Po Labs · {}", question.question);
        self.0.ask(&marked).await
    }
}

/// What is needed to save the session again at the end of every turn, under the same identifier.
struct Autosave {
    store: SessionStore,
    workspace: std::path::PathBuf,
    mode: BudgetMode,
    id: Option<String>,
}

#[async_trait::async_trait(?Send)]
impl po_tui::Host for PoHost {
    fn completions(&self) -> Vec<(String, String)> {
        po_tui::complete::commands_from_help(commands::help_text())
    }

    fn workspace(&self) -> Option<std::path::PathBuf> {
        Some(self.extras.workspace.clone())
    }

    fn after_turn(&mut self, agent: &Agent) {
        let a = &mut self.autosave;
        let mut id = a.id.clone();
        let _ = save_session(&a.store, agent, &a.workspace, a.mode, &mut id);
    }

    fn labs_agent(&mut self, main: &Agent) -> Option<Agent> {
        self.labs.build(main)
    }

    async fn labs_undo(&mut self, trial: &po_tui::labs::Trial) -> Result<(), String> {
        crate::labs::undo_trial(trial).await
    }

    fn settings(&mut self, agent: &Agent) -> Vec<po_tui::settings::Tab> {
        crate::settings_panel::tabs(agent, &self.extras)
    }

    async fn command(&mut self, line: &str, agent: &mut Agent) -> po_tui::CommandOutput {
        let reply = commands::execute(agent, &mut self.local, &mut self.extras, line).await;
        po_tui::CommandOutput {
            lines: reply
                .lines
                .into_iter()
                .map(|(tone, text)| {
                    let (a, b) = commands::halves(&text);
                    match tone {
                        Tone::Plain | Tone::Accent => po_tui::CardLine::Plain(text.trim_start().to_owned()),
                        Tone::Dim => po_tui::CardLine::Dim(text.trim_start().to_owned()),
                        Tone::Warn => po_tui::CardLine::Warn(text.clone()),
                        Tone::Section => po_tui::CardLine::Section(text.clone()),
                        Tone::Item => po_tui::CardLine::Item(a.to_owned(), b.to_owned()),
                        Tone::Field => po_tui::CardLine::Field(a.to_owned(), b.to_owned()),
                    }
                })
                .collect(),
            quit: reply.quit,
        }
    }

    fn cycle_budget(&mut self, agent: &mut Agent) -> String {
        agent.router_mut().cycle_mode().label().to_owned()
    }

    fn local_listing(&mut self) -> Vec<String> {
        self.local.refresh();
        if self.local.catalog.is_empty() {
            return vec![po_i18n::t!("chat.host.no_gguf").to_owned()];
        }
        let mut lines = vec![self.local.status()];
        lines.extend(self.local.listing());
        lines
    }

    fn toggle_pin(&mut self, agent: &mut Agent) -> String {
        // The shortcut alternates between "let the router decide" and "pin the current local
        // model", the two frequent gestures during a session.
        match self.local.active_model_id.clone() {
            Some(id) if agent.router().get(&id).is_some() => {
                agent.router_mut().pin(Some(id.clone()));
                po_i18n::t!("chat.host.pinned", id = id)
            }
            _ => {
                agent.router_mut().pin(None);
                po_i18n::t!("chat.shortcut.auto_routing").to_owned()
            }
        }
    }

    fn local_models(&mut self) -> Vec<po_tui::state::LocalModel> {
        // The catalog is re-read on every opening: plans depend on free VRAM, which changes
        // during the session — a browser being closed, a game being quit.
        self.local.refresh();
        let active = self.local.active_model_id.clone();
        self.local
            .catalog
            .iter()
            .map(|entry| {
                let label = entry.label();
                po_tui::state::LocalModel {
                    detail: format!(
                        "{:>6} {} · {}",
                        entry.size_mib(),
                        po_i18n::t!("chat.unit_mib"),
                        if entry.plan.viable {
                            entry.plan.summary()
                        } else {
                            po_i18n::t!("chat.local.does_not_fit").to_owned()
                        }
                    ),
                    loaded: active.as_deref() == Some(label.as_str()),
                    viable: entry.plan.viable,
                    label,
                }
            })
            .collect()
    }

    fn server_state(&mut self) -> String {
        self.local.server_state()
    }

    async fn activate_local(&mut self, index: usize, agent: &mut Agent) -> String {
        // The picker counts from zero, `activate` from one: the conversion is done here rather
        // than in the interface, which does not need to know this convention.
        match self.local.activate(index + 1, agent).await {
            Ok(message) | Err(message) => message,
        }
    }

    async fn stop_local(&mut self, agent: &mut Agent) -> String {
        self.local.deactivate(agent).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_opens_empty_if_a_local_model_is_startable() {
        // Measured from an Android emulator over SSH: six GGUF files on disk, `llama-server`
        // installed, no server listening — and Po refused to open, advising to start a server by
        // hand. That is precisely what `/local` does.
        assert!(can_open_session(false, true, false));
    }

    #[test]
    fn with_nothing_to_start_the_session_refuses_to_open() {
        // Opening a session that no command can save would help nobody.
        assert!(!can_open_session(false, false, false));
    }

    #[test]
    fn a_single_prompt_requires_a_reachable_model() {
        // Nobody is there to type `/local`: better to say so right away.
        assert!(!can_open_session(false, true, true));
        assert!(can_open_session(true, false, true));
    }

    #[test]
    fn both_interfaces_share_the_same_keyboard() {
        // Po has two faces: the full-screen TUI in a rich terminal, the line mode everywhere
        // else — flaky SSH, Termux, pipe, `--yes`. Two different sets of shortcuts for the same
        // product is exactly the friction this project refuses.
        for (key, _) in po_tui::shortcuts() {
            let Some(letter) = key.strip_prefix("Ctrl+").and_then(|s| s.chars().next()) else {
                continue;
            };
            let letter = letter.to_ascii_lowercase();
            // Ctrl+C and Ctrl+D belong to line editing, not to control; Ctrl+O (expand) and
            // Ctrl+K (palette) only make sense on a laid-out screen.
            if matches!(letter, 'c' | 'd' | 'o' | 'k') {
                continue;
            }
            assert!(SHORTCUTS.contains(&letter), "Ctrl+{letter} exists in the TUI but not in line mode");
        }
    }

    #[test]
    fn shortcuts_do_not_steal_the_editing_keys() {
        // Commandeering Ctrl+A, Ctrl+E, Ctrl+U, Ctrl+K, Ctrl+W, Ctrl+C or Ctrl+D would break
        // forty-year-old reflexes. This test prevents a future shortcut from taking them.
        const RESERVED: [char; 7] = ['a', 'c', 'd', 'e', 'k', 'u', 'w'];
        for letter in SHORTCUTS {
            assert!(!RESERVED.contains(&letter), "Ctrl+{letter} belongs to line editing");
        }
    }

    #[test]
    fn every_shortcut_is_documented_in_the_help() {
        // A shortcut nobody knows about does not exist.
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let help = shortcuts_help();
        for letter in SHORTCUTS {
            let expected = format!("Ctrl+{}", letter.to_ascii_uppercase());
            assert!(help.contains(&expected), "{expected} missing from /aide");
        }
    }

    #[test]
    fn shortcuts_are_distinct() {
        let mut seen = SHORTCUTS.to_vec();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), SHORTCUTS.len(), "two shortcuts for the same key");
    }

    #[test]
    fn budget_modes_parse_in_french_and_english() {
        assert_eq!(parse_budget("balanced"), Some(BudgetMode::Balanced));
        assert_eq!(parse_budget("equilibre"), Some(BudgetMode::Balanced));
        assert_eq!(parse_budget("save-max-tokens"), Some(BudgetMode::SaveMaxTokens));
        assert_eq!(parse_budget("economie"), Some(BudgetMode::SaveMaxTokens));
        assert_eq!(parse_budget("OFFLINE"), Some(BudgetMode::Offline));
        assert_eq!(parse_budget("  balanced  "), Some(BudgetMode::Balanced));
        assert_eq!(parse_budget("whatever"), None);
    }

    #[test]
    fn without_color_the_palette_is_empty() {
        // A colorless terminal must receive no sequence: this is what makes the output usable
        // in a pipe or a log file.
        let palette = Palette { dim: "", accent: "", warn: "", error: "", reset: "" };
        assert!(palette.accent.is_empty() && palette.reset.is_empty());
    }
}
