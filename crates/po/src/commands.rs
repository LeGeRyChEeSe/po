//! `/…` commands, shared by the full-screen interface and line mode.
//!
//! A command is never addressed to the model: it acts on the session — settings, models,
//! tools, cache — and returns a report. Rendering is left to the interface: line mode adds
//! colors, the TUI files the lines into the conversation. Writing directly to the terminal
//! from here would break the full-screen display.

use std::path::{Path, PathBuf};
use std::time::Duration;

use po_agent::Agent;
use po_core::{Origin, Tier};
use po_mcp::ServerConfig;
use po_tools::skills::{Skill, SkillTool};
use serde_json::Value;

use crate::chat::parse_budget;
use crate::local_ui::LocalManager;

/// Tone of a report line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Dim,
    Accent,
    Warn,
    /// Subheading.
    Section,
    /// List item: "name SEP description".
    Item,
    /// Field: "key SEP value".
    Field,
}

/// Separates the name from the description of an item, the key from the value of a field.
pub const SEP: char = '\u{1f}';

/// The two halves of an item or a field.
pub fn halves(text: &str) -> (&str, &str) {
    text.split_once(SEP).unwrap_or((text, ""))
}

#[derive(Debug, Default)]
pub struct Reply {
    pub lines: Vec<(Tone, String)>,
    pub quit: bool,
}

impl Reply {
    fn push(&mut self, tone: Tone, text: impl Into<String>) {
        self.lines.push((tone, text.into()));
    }
    pub(crate) fn plain(&mut self, text: impl Into<String>) {
        self.push(Tone::Plain, text);
    }
    pub(crate) fn dim(&mut self, text: impl Into<String>) {
        self.push(Tone::Dim, text);
    }
    fn accent(&mut self, text: impl Into<String>) {
        self.push(Tone::Accent, text);
    }
    pub(crate) fn warn(&mut self, text: impl Into<String>) {
        self.push(Tone::Warn, text);
    }
    /// Subheading of a report.
    pub(crate) fn section(&mut self, text: impl Into<String>) {
        self.push(Tone::Section, text);
    }
    /// List item: name and description (wrapped if long).
    pub(crate) fn item(&mut self, name: impl AsRef<str>, detail: impl AsRef<str>) {
        self.push(Tone::Item, format!("{}{SEP}{}", name.as_ref(), detail.as_ref()));
    }
    /// Field: key and value, aligned in columns.
    pub(crate) fn field(&mut self, key: impl AsRef<str>, value: impl AsRef<str>) {
        self.push(Tone::Field, format!("{}{SEP}{}", key.as_ref(), value.as_ref()));
    }
}

/// An MCP server plugged into the session.
#[derive(Debug, Clone)]
pub struct McpEntry {
    pub config: ServerConfig,
    pub tools: usize,
}

/// Session state that the agent does not carry itself.
#[derive(Debug, Default)]
pub struct Extras {
    pub workspace: PathBuf,
    pub mcp: Vec<McpEntry>,
    /// Servers declared in `.mcp.json` but not yet launched: starting third-party code
    /// requires a deliberate action from the user.
    pub mcp_available: Vec<ServerConfig>,
    pub skills: Vec<Skill>,
    /// Instruction files read at startup (AGENTS.md, CLAUDE.md, PO.md).
    pub instructions: Vec<PathBuf>,
}

/// The help in the current language (same line format in both languages: `help()` and
/// completion parse it).
pub fn help_text() -> &'static str {
    po_i18n::t!("cmd.help.text")
}

/// `/labs`: state, activation, recorded requests.
fn labs(agent: &mut Agent, argument: Option<&str>, rest: &str, reply: &mut Reply) {
    use po_i18n::t;
    match argument {
        Some("go" | "lancer" | "start") => {
            po_tui::request_labs(true);
            reply.accent(t!("labs.going"));
            return;
        }
        Some("retour" | "back") => {
            po_tui::request_labs(false);
            return;
        }
        Some("minuteur" | "timer") => {
            let value = rest.trim().parse::<u64>().ok();
            match value {
                Some(minutes) => {
                    if let Err(err) = po_i18n::prefs::set_str("labs_idle_minutes", &minutes.to_string()) {
                        reply.warn(err.to_string());
                    }
                    reply.accent(if minutes == 0 { t!("labs.timer_off").to_owned() } else { t!("labs.timer_set", n = minutes) });
                }
                None => reply.plain(t!("labs.timer_state", n = po_tui::labs::idle_minutes())),
            }
            return;
        }
        _ => {}
    }
    let turn_on = match argument {
        Some("on" | "oui" | "yes") => Some(true),
        Some("off" | "non" | "no") => Some(false),
        _ => None,
    };
    if let Some(on) = turn_on {
        if let Err(err) = crate::labs::set_enabled(on) {
            reply.warn(err.to_string());
        }
        crate::labs::activate(agent, on, false);
        if on {
            reply.section(t!("labs.enabled"));
            match crate::labs::source_root() {
                Some(source) => reply.dim(t!("labs.source", path = source.display().to_string())),
                None => reply.warn(t!("labs.no_source")),
            }
        } else {
            reply.section(t!("labs.disabled"));
            return;
        }
    } else {
        reply.section(t!("labs.name"));
        reply.field(
            "",
            if crate::labs::enabled() { t!("labs.state_on") } else { t!("labs.state_off") },
        );
    }
    reply.plain(t!("labs.tagline"));
    reply.item("1", t!("labs.how.talk"));
    reply.item("2", t!("labs.how.split"));
    reply.item("3", t!("labs.how.next"));
    let book = po_tui::labs::Notebook::load();
    if let Some(trial) = &book.trial {
        reply.section(t!("labs.trial"));
        let ids: Vec<String> = trial.ids.iter().map(|id| format!("#{id}")).collect();
        reply.item(format!("{} · {}", ids.join(" "), trial.date), trial.summary.clone());
        let files = crate::labs_git::workspace_dir()
            .and_then(|dir| crate::labs_git::changed_files(&dir, &trial.base, &trial.head).ok())
            .unwrap_or_default();
        if !files.is_empty() {
            reply.dim(t!("labs.trial_files", files = files.join(", ")));
        }
        for remark in &trial.revisions {
            reply.dim(format!("→ {remark}"));
        }
        if book.awaiting_verdict() {
            reply.dim(t!("labs.trial_how"));
        }
    }
    let pending = book.pending();
    reply.section(t!("labs.requests"));
    if pending.is_empty() {
        reply.dim(t!("labs.none"));
    }
    for request in pending {
        let kind = match request.kind.as_str() {
            "bug" => t!("labs.kind.bug"),
            "improvement" => t!("labs.kind.improvement"),
            _ => t!("labs.kind.idea"),
        };
        let core = if request.core { format!(" · {}", t!("labs.core")) } else { String::new() };
        reply.item(format!("#{} · {} · {kind}{core}", request.id, request.date), request.summary.clone());
    }
    reply.dim(t!("labs.how.toggle"));
}

/// The help, in sections and items: the command, then what it does.
fn help(reply: &mut Reply) {
    for line in help_text().lines() {
        if !line.starts_with(' ') {
            // "Saisie : …" / "Input: …" is a remark, not a section.
            if line.contains(": ") {
                reply.dim(line);
            } else {
                reply.section(line);
            }
            continue;
        }
        let entry = line.trim();
        match entry.find("  ") {
            Some(at) => reply.item(entry[..at].trim(), entry[at..].trim()),
            None => reply.item(entry, ""),
        }
    }
}

pub async fn execute(agent: &mut Agent, local: &mut LocalManager, extras: &mut Extras, line: &str) -> Reply {
    use po_i18n::t;
    let mut reply = Reply::default();
    if let Some(command) = line.strip_prefix('!') {
        user_command(agent, command.trim(), &mut reply).await;
        return reply;
    }
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or("");
    let argument = parts.next();
    let rest: Vec<&str> = parts.collect();

    match command {
        "/quitter" | "/quit" | "/q" | "/exit" => reply.quit = true,
        // The full-screen interface opens the panel itself; line mode has none.
        "/parametres" | "/paramètres" | "/reglages" | "/réglages" | "/settings" => {
            reply.plain(t!("cmd.settings.line_mode"));
        }
        "/controle" | "/contrôle" | "/control" | "/redemarrer" | "/redémarrer" | "/restart" => {
            reply.plain(t!("cmd.fullscreen_only"));
        }
        "/aide" | "/help" | "/?" => help(&mut reply),
        "/rc" | "/remote-control" | "/telecommande" | "/télécommande" => reply = crate::rc::command(argument, &rest).await,
        "/labs" => labs(agent, argument, &rest.join(" "), &mut reply),
        "/langue" | "/language" | "/lang" => match argument.and_then(po_i18n::Lang::parse) {
            Some(lang) => {
                if let Err(err) = po_i18n::choose(lang) {
                    reply.warn(err.to_string());
                }
                reply.accent(po_i18n::t!("common.language.changed"));
            }
            None => {
                reply.field(po_i18n::t!("common.language.label"), po_i18n::lang().name());
                reply.dim(po_i18n::t!("common.language.usage"));
            }
        },
        "/plan" | "/builder" | "/build" | "/bypass" => {
            let mode = po_agent::work_mode::WorkMode::parse(&command[1..]).unwrap_or(po_agent::work_mode::WorkMode::Build);
            agent.work_mode().set(mode);
            reply.accent(t!("cmd.workmode.set", mode = mode.label()));
        }
        "/reflexion" | "/réflexion" | "/effort" => effort(agent, argument, rest.first().copied(), &mut reply),
        "/temperature" | "/température" => temperature(agent, argument, &mut reply),
        "/verbosite" | "/verbosité" | "/verbosity" => verbosity(argument, &mut reply),
        "/statusline" | "/barre" => statusline(agent, argument, &mut reply).await,
        "/raccourci" | "/shortcut" => shortcut(argument, &rest.join(" "), &mut reply),
        "/raisonnement" | "/thinking" => {
            agent.settings.show_thinking = match argument {
                Some("on" | "oui") => true,
                Some("off" | "non") => false,
                _ => !agent.settings.show_thinking,
            };
            reply.accent(if agent.settings.show_thinking { t!("cmd.thinking.shown") } else { t!("cmd.thinking.hidden") });
        }
        "/contexte" | "/context" => context(agent, &mut reply),
        "/cache" => match argument {
            Some(limit @ ("limite" | "limit" | "jours" | "days")) => {
                let key = if matches!(limit, "limite" | "limit") { "kv_quota_gb" } else { "kv_max_days" };
                match rest.first().and_then(|v| v.parse::<u64>().ok()).filter(|n| *n > 0) {
                    Some(n) => {
                        if let Err(err) = po_i18n::prefs::set_str(key, &n.to_string()) {
                            reply.warn(err.to_string());
                        }
                        reply.accent(if key == "kv_quota_gb" { t!("cmd.cache.quota_set", n = n) } else { t!("cmd.cache.days_set", n = n) });
                    }
                    None => reply.warn(t!("cmd.cache.limit_usage")),
                }
            }
            _ => cache(agent, argument, &mut reply).await,
        },
        "/reprendre" | "/resume" => resume(agent, extras, argument, &rest.join(" "), &mut reply),
        "/nouveau" | "/clear" | "/new" => {
            let messages = agent.session.messages.len();
            agent.session.messages.clear();
            agent.session.archive.clear();
            if let (Some(parking), Some(key)) = (agent.parking(), &agent.session_key) {
                parking.forget(key);
            }
            let stopped = agent.cancel_background();
            reply.accent(t!("cmd.clear.done", n = messages));
            if stopped > 0 {
                reply.dim(t!("cmd.clear.stopped", n = stopped));
            }
        }
        "/delegation" | "/délégation" => match argument {
            Some("on" | "oui") => {
                agent.set_delegation(true);
                reply.accent(t!("cmd.delegation.on"));
            }
            Some("off" | "non") => {
                agent.set_delegation(false);
                reply.accent(t!("cmd.delegation.off"));
            }
            _ => reply.plain(if agent.delegation_enabled() { t!("cmd.delegation.state_on") } else { t!("cmd.delegation.state_off") }),
        },
        "/criteres" | "/critères" | "/criteria" => criteria(agent, argument, &rest, &mut reply),
        "/serveur" | "/server" => server(agent, argument, &mut reply).await,
        "/instructions" => {
            if extras.instructions.is_empty() {
                reply.plain(t!("cmd.instructions.none"));
            }
            for file in &extras.instructions {
                let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                reply.item(name, file.parent().map(|p| p.display().to_string()).unwrap_or_default());
            }
            reply.dim(t!("cmd.instructions.note"));
        }
        "/compacter" | "/compact" => {
            reply.dim(t!("cmd.compact.running"));
            let (before, after, by_model) = agent.compact_now().await;
            if before == after {
                reply.plain(t!("cmd.compact.already"));
            } else {
                let how = if by_model { t!("cmd.compact.by_model") } else { t!("cmd.compact.truncated") };
                reply.accent(t!("cmd.compact.done", how = how, before = before, after = after));
                reply.dim(t!("cmd.compact.cache_note"));
            }
        }
        "/outils" | "/tools" => {
            let names = agent.registry().names();
            reply.section(t!("cmd.tools.count", n = names.len() + usize::from(agent.delegation_enabled())));
            for name in names {
                let description = agent.registry().get(&name).map(|t| t.spec().description).unwrap_or_default();
                reply.item(&name, description.split(". ").next().unwrap_or(""));
            }
            if agent.delegation_enabled() {
                reply.item("task", t!("cmd.tools.task_desc"));
            }
        }
        "/mcp" => mcp(agent, extras, argument, &rest, &mut reply).await,
        "/skills" | "/competences" | "/compétences" => skills(agent, extras, argument, &mut reply),
        "/local" => local_command(agent, local, argument, &rest, &mut reply).await,
        "/agents" | "/agent" | "/sous-agents" => subagents(agent, extras, argument, &rest, &mut reply).await,
        "/catalogue" | "/catalog" => catalog(agent, extras, argument, &rest, &mut reply).await,
        "/voix" | "/voice" | "/dictee" | "/dictée" => crate::voice_cmd::command(argument, &rest, &mut reply).await,
        "/modeles" | "/models" => {
            // The remaining headroom says more than the window size: it tells what the session
            // can still absorb on *this* model, here and now.
            let used = agent.session.estimated_prompt_tokens();
            for model in agent.router().models() {
                let origin = model.origin.label().to_owned();
                let tier = match model.tier {
                    Tier::Frontier => "frontier",
                    Tier::Strong => "strong",
                    Tier::Small => "small",
                };
                let state = if agent.router().is_disabled(&model.id) {
                    t!("cmd.models.excluded").to_owned()
                } else {
                    t!("cmd.models.headroom", n = model.headroom(used))
                };
                reply.item(&model.id, t!("cmd.models.line", origin = origin, tier = tier, window = model.context_window, state = state));
            }
        }
        "/mode" => match argument {
            Some(raw) => match parse_budget(raw) {
                Some(mode) => {
                    agent.router_mut().set_mode(mode);
                    reply.accent(t!("cmd.mode.changed", mode = mode.label()));
                }
                None => reply.warn(t!("cmd.mode.unknown")),
            },
            None => {
                let mode = agent.router_mut().cycle_mode();
                reply.accent(t!("cmd.mode.changed", mode = mode.label()));
            }
        },
        "/modele" | "/model" | "/modèle" => match argument {
            Some("auto") | None => {
                agent.router_mut().pin(None);
                reply.dim(t!("cmd.model.auto"));
            }
            Some(id) => {
                if agent.router().get(id).is_some() {
                    agent.router_mut().pin(Some(id.to_owned()));
                    reply.accent(t!("cmd.model.pinned", id = id));
                } else {
                    reply.warn(t!("cmd.model.unknown", id = id));
                }
            }
        },
        "/cout" | "/cost" | "/coût" => {
            let ledger = &agent.session.ledger;
            for (model, usage, cost) in &ledger.cloud {
                reply.field(model, t!("cmd.cost.cloud", tokens = usage.total(), cost = format!("{cost:.4}")));
            }
            for (model, usage, seconds) in &ledger.local {
                reply.field(model, t!("cmd.cost.local", tokens = usage.total(), seconds = format!("{seconds:.1}")));
            }
            reply.field(t!("cmd.cost.total"), ledger.to_string());
        }
        other => reply.warn(t!("cmd.unknown", command = other)),
    }
    reply
}

/// Endpoints of the session's local servers, without duplicates.
fn local_endpoints(agent: &Agent) -> Vec<String> {
    let mut endpoints: Vec<String> = agent
        .router()
        .models()
        .iter()
        .filter_map(|m| match &m.origin {
            Origin::Local { endpoint, .. } => Some(endpoint.trim_end_matches('/').trim_end_matches("/v1").to_owned()),
            _ => None,
        })
        .collect();
    endpoints.sort();
    endpoints.dedup();
    endpoints
}

/// State and control of the local servers Po did not launch itself (Strata first).
async fn server(agent: &Agent, argument: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    // Reloading Strata's model took 123 s on the development machine: the timeout must cover a
    // slower disk, not just the measured case.
    let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(600)).build() else { return };
    let endpoints = local_endpoints(agent);
    if endpoints.is_empty() {
        reply.plain(t!("cmd.server.none"));
        return;
    }
    for root in endpoints {
        let status: Option<Value> = async {
            client
                .get(format!("{root}/v1/status"))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .ok()?
                .json()
                .await
                .ok()
        }
        .await;
        let is_strata = status.as_ref().and_then(|s| s["service"].as_str()) == Some("strata");
        match argument {
            Some(action @ ("decharger" | "décharger" | "unload" | "charger" | "load")) => {
                if !is_strata {
                    reply.warn(t!("cmd.server.no_hot", root = root));
                    continue;
                }
                let unload = matches!(action, "decharger" | "décharger" | "unload");
                if unload {
                    reply.dim(t!("cmd.server.unloading"));
                } else {
                    reply.dim(t!("cmd.server.loading"));
                }
                let path = if unload { "/v1/unload" } else { "/v1/load" };
                let started = std::time::Instant::now();
                match client.post(format!("{root}{path}")).json(&serde_json::json!({})).send().await {
                    Ok(response) if response.status().is_success() => {
                        let secs = format!("{:.0}", started.elapsed().as_secs_f64());
                        reply.accent(if unload { t!("cmd.server.unloaded", secs = secs) } else { t!("cmd.server.loaded", secs = secs) })
                    }
                    Ok(response) if response.status().as_u16() == 409 => {
                        reply.warn(t!("cmd.server.busy"))
                    }
                    Ok(response) => reply.warn(t!("cmd.server.refused", status = response.status())),
                    Err(err) => reply.warn(t!("cmd.server.unreachable", root = root, err = err)),
                }
            }
            _ => match status {
                Some(s) if is_strata => {
                    let loaded = s["loaded"].as_bool().unwrap_or(false);
                    reply.section(t!("cmd.server.strata", engine = s["engine"].as_str().unwrap_or("?"), root = root));
                    reply.field(
                        t!("cmd.server.model_label"),
                        format!(
                            "{} · {}",
                            s["model"].as_str().unwrap_or("?"),
                            if loaded { t!("cmd.server.model_loaded") } else { t!("cmd.server.model_unloaded") }
                        ),
                    );
                    let gpu = &s["machine"]["gpu"];
                    if let (Some(used), Some(total)) = (gpu["used_mib"].as_f64(), gpu["total_mib"].as_f64()) {
                        reply.field(
                            "GPU",
                            t!(
                                "cmd.server.gpu",
                                name = gpu["name"].as_str().unwrap_or("?"),
                                used = format!("{used:.0}"),
                                total = format!("{total:.0}"),
                                util = gpu["util_pct"].as_f64().map(|u| format!("{u:.0} %")).unwrap_or_default()
                            ),
                        );
                    }
                    let t = &s["last_timings"];
                    if let Some(decode) = t["predicted_per_second"].as_f64() {
                        let mut line = t!(
                            "cmd.server.last_line",
                            decode = format!("{decode:.1}"),
                            prefill = format!("{:.0}", t["prompt_per_second"].as_f64().unwrap_or(0.0)),
                            cached = t["cache_n"].as_u64().unwrap_or(0)
                        );
                        if let (Some(n), Some(ok)) = (t["draft_n"].as_u64(), t["draft_n_accepted"].as_u64())
                            && n > 0
                        {
                            line.push_str(&t!("cmd.server.drafts", pct = format!("{:.0}", ok as f64 / n as f64 * 100.0)));
                        }
                        reply.field(t!("cmd.server.last_label"), line);
                    }
                    let activity = &s["activity"];
                    reply.field(
                        t!("cmd.server.activity_label"),
                        t!(
                            "cmd.server.activity",
                            requests = activity["requests"].as_u64().unwrap_or(0),
                            in_flight = activity["in_flight"].as_u64().unwrap_or(0),
                            window = s["context"]["native"].as_u64().unwrap_or(0)
                        ),
                    );
                    reply.dim(t!("cmd.server.unload_hint"));
                }
                _ => {
                    let slots = server_slots(&root).await;
                    reply.plain(format!(
                        "{root} · {}",
                        if slots.is_some() { t!("cmd.server.kind_llama") } else { t!("cmd.server.kind_openai") }
                    ));
                    for slot in slots.unwrap_or_default() {
                        reply.plain(format!("  {slot}"));
                    }
                }
            },
        }
    }
}

fn criteria(agent: &mut Agent, argument: Option<&str>, rest: &[&str], reply: &mut Reply) {
    use po_i18n::t;
    match argument {
        Some("ajouter" | "add" | "+") => {
            let text = rest.join(" ");
            if text.trim().is_empty() {
                reply.warn(t!("cmd.criteria.usage_add"));
                return;
            }
            agent.settings.criteria.push(text.trim().to_owned());
            reply.accent(t!("cmd.criteria.added", n = agent.settings.criteria.len()));
        }
        Some("vider" | "clear") => {
            let n = agent.settings.criteria.len();
            agent.settings.criteria.clear();
            reply.accent(t!("cmd.criteria.cleared", n = n));
        }
        Some("retirer" | "remove") => match rest.first().and_then(|n| n.parse::<usize>().ok()) {
            Some(n) if n >= 1 && n <= agent.settings.criteria.len() => {
                let removed = agent.settings.criteria.remove(n - 1);
                reply.accent(t!("cmd.criteria.removed", text = removed));
            }
            _ => reply.warn(t!("cmd.criteria.usage_remove")),
        },
        _ => {
            if agent.settings.criteria.is_empty() {
                reply.plain(t!("cmd.criteria.none"));
                reply.dim(t!("cmd.criteria.none_hint"));
            }
            for (i, c) in agent.settings.criteria.iter().enumerate() {
                reply.field(format!("{}.", i + 1), c);
            }
        }
    }
}

/// Output lines shown on screen; the model, for its part, receives everything (within the
/// tool's limit).
const SHOWN_LINES: usize = 30;

async fn user_command(agent: &mut Agent, command: &str, reply: &mut Reply) {
    use po_i18n::t;
    if command.is_empty() {
        reply.plain(t!("cmd.bang.usage"));
        return;
    }
    let outcome = agent.run_user_command(command).await;
    let lines: Vec<&str> = outcome.content.lines().collect();
    let skipped = lines.len().saturating_sub(SHOWN_LINES);
    if skipped > 0 {
        reply.dim(t!("cmd.bang.skipped", n = skipped));
    }
    for line in &lines[skipped..] {
        reply.push(if outcome.is_error { Tone::Warn } else { Tone::Plain }, (*line).to_owned());
    }
    reply.dim(t!("cmd.bang.attached"));
}

fn effort(agent: &mut Agent, argument: Option<&str>, value: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    let argument = argument.map(str::to_ascii_lowercase);
    // `/effort budget 4096`, and `/effort budget4096` written in one word.
    if let Some(glued) = argument.as_deref().and_then(|a| a.strip_prefix("budget")) {
        let value = if glued.is_empty() { value.unwrap_or("").to_ascii_lowercase() } else { glued.to_owned() };
        return thinking_budget(agent, value.trim(), reply);
    }
    match argument.as_deref() {
        None => {
            reply.plain(t!(
                "cmd.effort.state",
                level = agent.settings.reasoning_effort.as_deref().unwrap_or(t!("cmd.server_setting"))
            ));
            reply.dim(budget_state(agent));
        }
        Some("defaut" | "défaut" | "default" | "auto") => {
            agent.settings.reasoning_effort = None;
            reply.accent(t!("cmd.effort.default"));
        }
        Some(level @ ("none" | "low" | "medium" | "high")) => {
            let previous = agent.settings.reasoning_effort.clone();
            agent.settings.reasoning_effort = Some(level.to_owned());
            reply.accent(t!("cmd.effort.set", level = level));
            // Only switching thinking on or off changes the template; the other levels go through
            // the thinking budget, which leaves the cache alone.
            let template_changes = (level == "none") != (previous.as_deref() == Some("none"))
                || agent.settings.thinking_budget == Some(0);
            if previous.as_deref() != Some(level) && template_changes && !agent.session.messages.is_empty() {
                // Measured on Strata: the prompt template changes with the level.
                reply.dim(t!("cmd.effort.invalidated"));
            }
        }
        Some(other) => reply.warn(t!("cmd.effort.unknown", level = other)),
    }
}

/// `/shortcut <action> <keys>`: changes a shortcut (`labs`: accept Po Labs or resume, `pause`:
/// pause the agent at work), from one to four keys at once; `default` restores it.
fn shortcut(argument: Option<&str>, rest: &str, reply: &mut Reply) {
    use po_i18n::t;
    use po_tui::labs::{ACTIONS, Shortcut, shortcut};
    let Some(action) = argument.filter(|a| ACTIONS.iter().any(|(name, _)| name == a)) else {
        reply.section(t!("cmd.shortcut.title"));
        for (name, _) in ACTIONS {
            reply.field(*name, shortcut(name).label());
        }
        reply.dim(t!("cmd.shortcut.usage"));
        return;
    };
    let keys = rest.trim();
    if matches!(keys, "default" | "defaut" | "défaut") {
        let _ = po_i18n::prefs::set_str(&format!("keys.{action}"), "");
        reply.accent(t!("cmd.shortcut.set", action = action, keys = shortcut(action).label()));
        return;
    }
    match Shortcut::parse(keys) {
        Some(parsed) => {
            if let Err(err) = po_i18n::prefs::set_str(&format!("keys.{action}"), keys) {
                reply.warn(err.to_string());
            }
            reply.accent(t!("cmd.shortcut.set", action = action, keys = parsed.label()));
        }
        None => reply.warn(t!("cmd.shortcut.invalid", keys = keys)),
    }
}

/// `/statusline <request>`: the model rewrites the status line from a request in plain words;
/// `/statusline default` restores the original line. Applied right away, kept for next time.
async fn statusline(agent: &mut Agent, argument: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    use po_tui::statusline;
    let describe = |reply: &mut Reply| {
        reply.section(t!("cmd.statusline.current"));
        for segment in statusline::current() {
            let value = match segment.id.as_str() {
                "text" => format!("« {} »", segment.text.clone().unwrap_or_default()),
                "command" => format!("$ {}", segment.command.clone().unwrap_or_default()),
                id => segment.text.as_ref().map_or(id.to_owned(), |label| format!("{label} {id}")),
            };
            reply.item(value, segment.color.clone().unwrap_or_default());
        }
    };
    match argument.map(str::trim) {
        None | Some("") => {
            describe(reply);
            reply.section(t!("cmd.statusline.available"));
            for (id, what) in statusline::BUILTINS {
                reply.item(*id, *what);
            }
            reply.dim(t!("cmd.statusline.usage"));
        }
        Some("default" | "defaut" | "défaut" | "reset") => {
            if let Err(err) = statusline::set(None) {
                reply.warn(t!("cmd.verbosity.not_saved", error = err.to_string()));
            }
            reply.accent(t!("cmd.statusline.reset"));
        }
        Some(request) => {
            let current = statusline::current();
            let (system, mut prompt) = statusline::editing_prompt(&current, request);
            // A small local model sometimes answers off format: it gets one more try, told why.
            let mut result = Err(String::new());
            for _ in 0..2 {
                result = match agent.complete_once(&system, &prompt).await {
                    Ok(answer) => statusline::parse_answer(&answer),
                    Err(err) => Err(err.to_string()),
                };
                match &result {
                    Ok(_) => break,
                    Err(why) => prompt.push_str(&format!("\n\nYour previous answer was rejected: {why}. Answer with the JSON array only.")),
                }
            }
            match result {
                Ok(segments) => {
                    if let Err(err) = statusline::set(Some(segments)) {
                        reply.warn(t!("cmd.verbosity.not_saved", error = err.to_string()));
                    }
                    reply.accent(t!("cmd.statusline.updated"));
                    describe(reply);
                    reply.dim(t!("cmd.statusline.undo"));
                }
                Err(why) => reply.warn(t!("cmd.statusline.failed", error = why)),
            }
        }
    }
}

/// `/verbosity [quiet|normal|verbose]`: how much of a turn's work the conversation shows.
/// Without an argument, moves to the next level.
fn verbosity(argument: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    use po_tui::render::Verbosity;
    let level = match argument {
        None => {
            let current = Verbosity::current();
            let next = Verbosity::ALL.iter().position(|v| *v == current).map_or(0, |i| (i + 1) % Verbosity::ALL.len());
            Verbosity::ALL[next]
        }
        Some(raw) => match Verbosity::parse(raw) {
            Some(level) => level,
            None => return reply.warn(t!("cmd.verbosity.unknown", level = raw)),
        },
    };
    if let Err(err) = level.set() {
        reply.warn(t!("cmd.verbosity.not_saved", error = err.to_string()));
    }
    reply.accent(t!("cmd.verbosity.set", level = level.label()));
    reply.dim(match level {
        Verbosity::Quiet => t!("cmd.verbosity.quiet_hint"),
        Verbosity::Normal => t!("cmd.verbosity.normal_hint"),
        Verbosity::Verbose => t!("cmd.verbosity.verbose_hint"),
    });
}

/// The thinking budget in force, as shown by `/effort`.
fn budget_state(agent: &Agent) -> String {
    use po_i18n::t;
    let window = agent.router().models().iter().filter(|m| m.origin.is_local()).map(|m| m.context_window).max().unwrap_or(0);
    match agent.settings.thinking_budget(window) {
        Some(tokens) if agent.settings.thinking_budget.is_some() => t!("cmd.effort.budget_fixed", n = tokens),
        Some(tokens) => t!("cmd.effort.budget_auto", n = tokens),
        None => t!("cmd.effort.budget_none").to_owned(),
    }
}

fn thinking_budget(agent: &mut Agent, value: &str, reply: &mut Reply) {
    use po_i18n::t;
    agent.settings.thinking_budget = match value {
        "" => {
            reply.plain(budget_state(agent));
            return;
        }
        "auto" | "defaut" | "défaut" | "default" => None,
        "off" | "none" | "0" => Some(0),
        number => match number.trim_end_matches('k').parse::<u32>() {
            Ok(n) if number.ends_with('k') => Some(n * 1024),
            Ok(n) => Some(n),
            Err(_) => {
                reply.warn(t!("cmd.effort.budget_unknown", value = number));
                return;
            }
        },
    };
    reply.accent(budget_state(agent));
}

fn temperature(agent: &mut Agent, argument: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    match argument {
        None => reply.plain(t!(
            "cmd.temperature.state",
            value = agent.settings.temperature.map(|v| format!("{v}")).unwrap_or_else(|| t!("cmd.server_setting").into())
        )),
        Some("defaut" | "défaut" | "default" | "auto") => {
            agent.settings.temperature = None;
            reply.accent(t!("cmd.temperature.default"));
        }
        Some(raw) => match raw.replace(',', ".").parse::<f32>() {
            Ok(value) if (0.0..=2.0).contains(&value) => {
                agent.settings.temperature = Some(value);
                reply.accent(t!("cmd.temperature.set", value = value));
            }
            _ => reply.warn(t!("cmd.temperature.range")),
        },
    }
}

fn context(agent: &Agent, reply: &mut Reply) {
    use po_i18n::t;
    let used = agent.estimated_request_tokens();
    reply.field(t!("cmd.context.conversation"), t!("cmd.context.messages", n = agent.session.messages.len()));
    reply.field(t!("cmd.context.per_request"), t!("cmd.context.tokens", n = used));
    for model in agent.router().models() {
        let window = model.context_window.max(1);
        let pct = (used as f64 / window as f64 * 100.0).min(999.0);
        reply.field(&model.id, t!("cmd.context.model_pct", pct = format!("{pct:.0}"), window = window));
    }
    reply.dim(t!("cmd.context.note"));
}

async fn cache(agent: &Agent, argument: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    let gb = |bytes: u64| format!("{:.1}", bytes as f64 / 1e9).replace('.', po_i18n::t!("view.meta.decimal"));
    if matches!(argument, Some("vider" | "clear" | "purge")) {
        match crate::kv::current() {
            Some(store) => {
                let freed = store.clear();
                reply.accent(t!("cmd.cache.disk_cleared", size = gb(freed.bytes), n = freed.files));
            }
            None => reply.dim(t!("cmd.cache.disk_off")),
        }
        return;
    }
    match agent.last_call().and_then(|s| s.summary().map(|line| (s, line))) {
        Some((stats, line)) => {
            reply.field(t!("cmd.cache.last_call"), line);
            if stats.prefill_tok_s > 0.0 {
                reply.field(t!("cmd.cache.uncached_read"), format!("{:.0} tok/s", stats.prefill_tok_s));
            }
        }
        None => reply.dim(t!("cmd.cache.none")),
    }
    // The conversations parked on disk (single-slot servers): what they take, out of the limit.
    match crate::kv::current() {
        Some(store) => {
            let usage = store.usage();
            reply.field(
                t!("cmd.cache.disk"),
                t!("cmd.cache.disk_usage", size = gb(usage.bytes), quota = gb(usage.quota_bytes), n = usage.files),
            );
            reply.dim(t!("cmd.cache.disk_where", path = store.dir().display().to_string()));
        }
        None => reply.dim(t!("cmd.cache.disk_off")),
    }
    let mut endpoints: Vec<String> = agent
        .router()
        .models()
        .iter()
        .filter_map(|m| match &m.origin {
            Origin::Local { endpoint, .. } => Some(endpoint.clone()),
            _ => None,
        })
        .collect();
    endpoints.sort();
    endpoints.dedup();
    for endpoint in endpoints {
        match server_slots(&endpoint).await {
            Some(slots) => {
                reply.section(&endpoint);
                for slot in slots {
                    let (name, state) = slot.split_once(": ").unwrap_or((slot.as_str(), ""));
                    reply.field(name.trim_end_matches(' '), state);
                }
            }
            None => reply.dim(t!("cmd.cache.no_slots", endpoint = endpoint)),
        }
    }
}

/// `/resume`: the sessions of this project, most recent first; `/resume <n|words of the title>`
/// switches to one — Po restarts on it, the current one being saved first.
fn resume(agent: &Agent, extras: &Extras, argument: Option<&str>, rest: &str, reply: &mut Reply) {
    use po_i18n::t;
    let store = po_agent::SessionStore::default_location();
    let sessions: Vec<_> = store
        .list_for(&extras.workspace)
        .into_iter()
        .filter(|s| Some(&s.id) != agent.session_key.as_ref())
        .collect();
    if sessions.is_empty() {
        reply.dim(t!("cmd.resume.none"));
        return;
    }
    let wanted = argument.map(|first| format!("{first} {rest}").trim().to_lowercase());
    let Some(wanted) = wanted else {
        reply.section(t!("cmd.resume.title"));
        for (i, saved) in sessions.iter().enumerate().take(20) {
            let date = saved.saved_at.get(..16).unwrap_or(&saved.saved_at).replace('T', " ");
            let cached = crate::kv::current().is_some_and(|s| s.holds(&saved.id));
            reply.item(
                format!("{} · {}", i + 1, saved.title),
                format!(
                    "{date} · {}{}",
                    t!("cmd.resume.exchanges", n = saved.exchange_count()),
                    if cached { format!(" · {}", t!("cmd.resume.instant")) } else { String::new() }
                ),
            );
        }
        reply.dim(t!("cmd.resume.usage"));
        return;
    };
    let chosen = wanted
        .parse::<usize>()
        .ok()
        .and_then(|n| sessions.get(n.wrapping_sub(1)))
        .or_else(|| sessions.iter().find(|s| s.title.to_lowercase().contains(&wanted)));
    match chosen {
        Some(saved) => {
            crate::kv::set_resume_target(saved.id.clone());
            po_tui::request_restart();
            reply.accent(t!("cmd.resume.switching", title = saved.title));
        }
        None => reply.warn(t!("cmd.resume.not_found", query = wanted)),
    }
}

/// State of a local server's slots (`/slots`, llama.cpp and Strata).
pub async fn server_slots(endpoint: &str) -> Option<Vec<String>> {
    let root = endpoint.trim_end_matches('/').trim_end_matches("/v1");
    let client = reqwest::Client::builder().timeout(Duration::from_millis(800)).build().ok()?;
    let body: Value = client.get(format!("{root}/slots")).send().await.ok()?.json().await.ok()?;
    let slots = body.as_array()?;
    Some(
        slots
            .iter()
            .map(|slot| {
                let id = slot["id"].as_u64().unwrap_or(0);
                let busy = slot["is_processing"].as_bool().unwrap_or(false);
                let held = slot["n_prompt_tokens"].as_u64().or_else(|| slot["n_past"].as_u64()).unwrap_or(0);
                let ctx = slot["n_ctx"].as_u64().unwrap_or(0);
                po_i18n::t!(
                    "cmd.slot.line",
                    id = id,
                    state = if busy { po_i18n::t!("cmd.slot.busy") } else { po_i18n::t!("cmd.slot.free") },
                    held = held,
                    ctx = ctx
                )
            })
            .collect(),
    )
}

/// `/agents`: available sub-agents, each one's model, parallelism, creation and removal.
async fn subagents(agent: &mut Agent, extras: &mut Extras, argument: Option<&str>, rest: &[&str], reply: &mut Reply) {
    use po_agent::subagents::{self, Source};
    use po_i18n::t;
    match argument {
        None | Some("liste" | "list") => {}
        Some("recharger" | "reload") => {
            agent.set_profiles(subagents::discover(&extras.workspace, subagents::global_dir().as_deref()));
            reply.accent(t!("cmd.agents.reloaded", n = agent.profiles().len()));
        }
        Some("voir" | "show") => {
            let Some(profile) = rest.first().and_then(|n| agent.profiles().iter().find(|p| p.name == *n)) else {
                reply.warn(t!("cmd.agents.usage_show"));
                return;
            };
            reply.accent(format!("{} — {}", profile.name, profile.description));
            reply.dim(t!(
                "cmd.agents.detail",
                tools = profile.tools.label(),
                model = agent.model_choice(profile),
                source = match profile.source.path() {
                    Some(path) => format!("{} ({})", profile.source.label(), path.display()),
                    None => profile.source.label().to_owned(),
                }
            ));
            for line in profile.rules.lines() {
                reply.plain(format!("  {line}"));
            }
            return;
        }
        Some("modele" | "modèle" | "model") => {
            let (Some(name), Some(choice)) = (rest.first(), rest.get(1)) else {
                reply.warn(t!("cmd.agents.usage_model"));
                return;
            };
            if !agent.profiles().iter().any(|p| p.name == *name) {
                reply.warn(t!("cmd.agents.unknown_list", name = name));
                return;
            }
            let choice = match *choice {
                "session" | "défaut" | "defaut" => "session".to_owned(),
                "auto" => "auto".to_owned(),
                id => match agent.router().models().iter().find(|m| m.id == id || m.display_name == id) {
                    Some(model) => model.id.clone(),
                    None => {
                        reply.warn(t!("cmd.agents.unknown_model", id = id));
                        return;
                    }
                },
            };
            if choice == "session" {
                agent.subagent_settings.models.remove(*name);
            } else {
                agent.subagent_settings.models.insert((*name).to_owned(), choice.clone());
            }
            save_subagent_settings(agent, reply);
            reply.accent(t!("cmd.agents.model_set", name = name, choice = choice));
        }
        Some("parallele" | "parallèle" | "parallel") => {
            let value = rest.first().copied().unwrap_or("");
            agent.subagent_settings.max_parallel = match value {
                "auto" | "defaut" | "défaut" => None,
                n => match n.parse::<usize>() {
                    Ok(n) if (1..=16).contains(&n) => Some(n),
                    _ => {
                        reply.warn(t!("cmd.agents.usage_parallel"));
                        return;
                    }
                },
            };
            save_subagent_settings(agent, reply);
        }
        Some("creer" | "créer" | "create" | "nouveau") => {
            let (global, words) = match rest.first() {
                Some(&("global" | "permanent")) => (true, &rest[1..]),
                Some(&("projet" | "project")) => (false, &rest[1..]),
                _ => (false, rest),
            };
            let description = words.join(" ");
            if description.trim().is_empty() {
                reply.warn(t!("cmd.agents.usage_create"));
                return;
            }
            create_subagent(agent, extras, global, &description, reply).await;
            return;
        }
        Some("retirer" | "supprimer" | "remove") => {
            let Some(name) = rest.first() else {
                reply.warn(t!("cmd.agents.usage_remove"));
                return;
            };
            match agent.profiles().iter().find(|p| p.name == *name).map(|p| p.source.clone()) {
                None => reply.warn(t!("cmd.agents.unknown", name = name)),
                Some(Source::Builtin) => reply.warn(t!("cmd.agents.builtin", name = name)),
                Some(source) => {
                    let path = source.path().map(Path::to_path_buf).unwrap_or_default();
                    match std::fs::remove_file(&path) {
                        Ok(()) => {
                            agent.set_profiles(subagents::discover(&extras.workspace, subagents::global_dir().as_deref()));
                            reply.accent(t!("cmd.agents.removed", name = name, path = path.display()));
                        }
                        Err(err) => reply.warn(format!("{}: {err}", path.display())),
                    }
                }
            }
            return;
        }
        Some(other) => {
            reply.warn(t!("cmd.agents.unknown_sub", sub = other));
            return;
        }
    }

    // The server may have been restarted with more slots since startup: read it again.
    if let Some(model) = agent.session_model() {
        if let Origin::Local { endpoint, .. } = &model.origin
            && let Some(slots) = crate::chat::server_slots_at(endpoint).await
        {
            agent.set_server_slots(endpoint.clone(), slots);
        }
        let detected = agent.detected_slots(&model);
        let limit = agent.parallel_limit(&model);
        let slots = count_unit(detected, t!("cmd.unit.slot"), t!("cmd.unit.slots"));
        reply.plain(t!(
            "cmd.agents.parallelism",
            model = model.display_name,
            limit = limit,
            origin = match agent.subagent_settings.max_parallel {
                Some(_) => t!("cmd.agents.parallel_forced", slots = slots),
                None => t!("cmd.agents.parallel_announced", slots = slots),
            }
        ));
        reply.dim(if limit > 1 {
            t!("cmd.agents.parallel_many", limit = limit, rest = count_unit(limit - 1, t!("cmd.unit.subagent"), t!("cmd.unit.subagents")))
        } else {
            t!("cmd.agents.parallel_one").to_owned()
        });
    }
    if !agent.delegation_enabled() {
        reply.warn(t!("cmd.agents.delegation_off"));
    }
    if let Some(model) = agent.session_model() {
        reply.field(t!("cmd.agents.session_field"), &model.display_name);
    }
    reply.section(t!("cmd.agents.title"));
    for profile in agent.profiles() {
        reply.item(
            &profile.name,
            t!(
                "cmd.agents.entry",
                description = profile.description,
                tools = profile.tools.label(),
                model = agent.model_choice(profile),
                source = profile.source.label()
            ),
        );
    }
    if agent.background_count() > 0 {
        reply.dim(t!("cmd.agents.background", count = count_unit(agent.background_count(), t!("cmd.unit.subagent"), t!("cmd.unit.subagents"))));
    }
    reply.dim(t!("cmd.agents.footer"));
}

/// "1 slot", "3 slots": both forms come from the catalog, in the current language.
fn count_unit(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n > 1 { plural } else { singular })
}

fn save_subagent_settings(agent: &Agent, reply: &mut Reply) {
    use po_i18n::t;
    match agent.subagent_settings.save() {
        Ok(path) => reply.dim(t!("cmd.agents.saved", path = path.display())),
        Err(err) => reply.warn(t!("cmd.agents.not_saved", err = err)),
    }
    if let Some(model) = agent.session_model() {
        reply.accent(t!("cmd.agents.parallelism_set", limit = agent.parallel_limit(&model)));
    }
}

/// Has the session's model write a sub-agent from a description, then saves it. The model
/// chooses the name, description and rules; Po checks the format and never overwrites an
/// existing sub-agent.
async fn create_subagent(agent: &mut Agent, extras: &mut Extras, global: bool, description: &str, reply: &mut Reply) {
    use po_agent::subagents::{self, Source};
    use po_i18n::t;
    let dir = if global {
        match subagents::global_dir() {
            Some(dir) => dir,
            None => {
                reply.warn(t!("cmd.agents.no_config_dir"));
                return;
            }
        }
    } else {
        subagents::project_dir(&extras.workspace)
    };
    let prompt = format!("Description of the wanted sub-agent, by the user:\n{description}");
    let mut profile = None;
    // Two attempts: a local model sometimes misses the format the first time.
    for _ in 0..2 {
        let text = match agent.complete_once(subagents::GENERATION_SYSTEM, &prompt).await {
            Ok(text) => text,
            Err(err) => {
                reply.warn(t!("cmd.agents.draft_failed", err = err));
                return;
            }
        };
        let source = |path| if global { Source::Global(path) } else { Source::Project(path) };
        if let Some(p) = subagents::parse_generated(&text, source(PathBuf::new())) {
            profile = Some(p);
            break;
        }
    }
    let Some(mut profile) = profile else {
        reply.warn(t!("cmd.agents.unreadable"));
        reply.dim(t!("cmd.agents.format", dir = dir.display()));
        return;
    };
    // A name already taken gets a suffix: overwriting an existing sub-agent — or masking a
    // built-in role — would be a surprise.
    let base = profile.name.clone();
    let mut n = 2;
    while agent.profiles().iter().any(|p| p.name == profile.name) || dir.join(format!("{}.md", profile.name)).exists() {
        profile.name = format!("{base}-{n}");
        n += 1;
    }
    let path = dir.join(format!("{}.md", profile.name));
    profile.source = if global { Source::Global(path.clone()) } else { Source::Project(path.clone()) };
    if let Err(err) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, profile.render())) {
        reply.warn(format!("{}: {err}", path.display()));
        return;
    }
    agent.set_profiles(subagents::discover(&extras.workspace, subagents::global_dir().as_deref()));
    reply.accent(t!("cmd.agents.created", name = profile.name, source = profile.source.label(), path = path.display()));
    reply.dim(t!("cmd.agents.created_detail", description = profile.description, tools = profile.tools.label()));
    for line in profile.rules.lines() {
        reply.plain(format!("  {line}"));
    }
    reply.dim(t!("cmd.agents.created_note"));
}

async fn mcp(agent: &mut Agent, extras: &mut Extras, argument: Option<&str>, rest: &[&str], reply: &mut Reply) {
    use po_i18n::t;
    match argument {
        None | Some("liste" | "list") => {
            if extras.mcp.is_empty() && extras.mcp_available.is_empty() {
                reply.plain(t!("cmd.mcp.none"));
            }
            for entry in &extras.mcp {
                reply.item(format!("● {}", entry.config.name), t!("cmd.mcp.connected", n = entry.tools, command = command_line(&entry.config)));
            }
            for config in &extras.mcp_available {
                reply.item(format!("○ {}", config.name), t!("cmd.mcp.declared", command = command_line(config)));
            }
            if !extras.mcp_available.is_empty() {
                reply.dim(t!("cmd.mcp.not_started"));
            }
        }
        Some("activer" | "enable" | "lancer") => {
            let Some(name) = rest.first() else {
                reply.warn(t!("cmd.mcp.usage_enable"));
                return;
            };
            let Some(index) = extras.mcp_available.iter().position(|c| c.name == *name) else {
                reply.warn(t!("cmd.mcp.not_declared", name = name));
                return;
            };
            let config = extras.mcp_available.remove(index);
            if let Err(config) = connect(agent, extras, config, reply).await {
                extras.mcp_available.insert(index, config);
            }
        }
        Some("ajouter" | "add") => {
            let (Some(name), Some(command)) = (rest.first(), rest.get(1)) else {
                reply.warn(t!("cmd.mcp.usage_add"));
                return;
            };
            if extras.mcp.iter().any(|e| e.config.name == *name) {
                reply.warn(t!("cmd.mcp.already", name = name));
                return;
            }
            // An address designates a remote server (HTTP transport), a command a local server
            // (stdio).
            let config = if command.starts_with("http://") || command.starts_with("https://") {
                ServerConfig::http(name, command)
            } else {
                ServerConfig::stdio(name, command, rest[2..].iter().map(|s| (*s).to_owned()).collect())
            };
            let _ = connect(agent, extras, config, reply).await;
        }
        Some("retirer" | "remove") => {
            let Some(name) = rest.first() else {
                reply.warn(t!("cmd.mcp.usage_remove"));
                return;
            };
            match extras.mcp.iter().position(|e| e.config.name == *name) {
                Some(index) => {
                    let removed = agent.registry_mut().remove_prefixed(&po_mcp::tool_prefix(name));
                    extras.mcp.remove(index);
                    // The last tool removed frees the server: its process stops.
                    reply.accent(t!("cmd.mcp.removed", name = name, n = removed));
                }
                None => reply.warn(t!("cmd.mcp.not_connected", name = name)),
            }
        }
        Some("garder" | "keep" | "save") => {
            let Some(name) = rest.first() else {
                reply.warn(t!("cmd.mcp.usage_keep"));
                return;
            };
            let Some(entry) = extras.mcp.iter().find(|e| e.config.name == *name) else {
                reply.warn(t!("cmd.mcp.not_connected", name = name));
                return;
            };
            let path = extras.workspace.join(".mcp.json");
            match save_mcp_json(&path, &entry.config) {
                Ok(()) => reply.accent(t!("cmd.mcp.saved", name = name, path = path.display())),
                Err(err) => reply.warn(t!("cmd.mcp.write_failed", path = path.display(), err = err)),
            }
        }
        Some(other) => reply.warn(t!("cmd.mcp.unknown_sub", sub = other)),
    }
}

/// Plugs in a server. Returns the configuration on failure, so that it stays on offer.
async fn connect(agent: &mut Agent, extras: &mut Extras, config: ServerConfig, reply: &mut Reply) -> Result<(), ServerConfig> {
    use po_i18n::t;
    match po_mcp::connect_one(&config).await {
        Ok(tools) => {
            let count = tools.len();
            for tool in tools {
                agent.registry_mut().register(tool);
            }
            reply.accent(t!("cmd.mcp.plugged", name = config.name, n = count));
            reply.dim(t!("cmd.mcp.tools_changed"));
            extras.mcp.push(McpEntry { config, tools: count });
            Ok(())
        }
        Err(err) => {
            reply.warn(format!("{}: {err}", config.name));
            reply.dim(t!("cmd.mcp.log", path = po_mcp::log_path(&config.name).display()));
            Err(config)
        }
    }
}

fn command_line(config: &ServerConfig) -> String {
    if let Some(url) = &config.url {
        return url.clone();
    }
    let mut text = config.command.clone();
    for arg in &config.args {
        text.push(' ');
        text.push_str(arg);
    }
    text
}

/// Adds or replaces a server in `.mcp.json`, keeping the rest of the file.
pub fn save_mcp_json(path: &Path, config: &ServerConfig) -> std::io::Result<()> {
    // A file that does not read as a JSON object is left alone: rewriting it would lose the
    // servers it holds (other tools read `.mcp.json` too).
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| std::io::Error::other(po_i18n::t!("cmd.mcp.unreadable_file")))?,
        _ => serde_json::json!({}),
    };
    let servers = root
        .as_object_mut()
        .expect("object")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    let mut entry = match &config.url {
        Some(url) => serde_json::json!({"type": "http", "url": url}),
        None => serde_json::json!({"command": config.command, "args": config.args}),
    };
    if !config.env.is_empty() {
        entry["env"] = serde_json::to_value(&config.env).unwrap_or_default();
    }
    if !config.headers.is_empty() {
        entry["headers"] = serde_json::to_value(&config.headers).unwrap_or_default();
    }
    servers.as_object_mut().expect("object").insert(config.name.clone(), entry);
    let text = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    std::fs::write(path, text + "\n")
}

/// Skills and subagents read again: an extension installed or removed applies at once.
fn reload_extensions(agent: &mut Agent, extras: &mut Extras) {
    extras.skills = po_tools::skills::discover(&extras.workspace);
    agent.registry_mut().remove_prefixed("skill");
    if !extras.skills.is_empty() {
        agent.registry_mut().register(std::sync::Arc::new(SkillTool::new(extras.skills.clone())));
    }
    agent.set_profiles(po_agent::subagents::discover(&extras.workspace, po_agent::subagents::global_dir().as_deref()));
}

/// The catalog for its browser: by category, then as listed (certified first, then by votes).
pub async fn catalog_entries() -> Result<Vec<po_tui::catalog::Entry>, String> {
    use crate::catalog;
    let (_, list) = catalog::list(&crate::github::Config::from_env()).await.map_err(|err| po_i18n::t!("cmd.catalog.unreachable", err = err))?;
    let installed = catalog::installed();
    let mut entries = Vec::new();
    for category in catalog::CATEGORIES {
        for extension in list.iter().filter(|e| e.manifest.category == *category) {
            let m = &extension.manifest;
            entries.push(po_tui::catalog::Entry {
                name: extension.name.clone(),
                title: m.title.clone(),
                description: m.description.clone(),
                category: m.category.clone(),
                author: m.author.clone(),
                certified: m.certified,
                votes: extension.votes,
                installed: installed.contains_key(&extension.name),
            });
        }
    }
    Ok(entries)
}

/// The files of extension `name`, to read before installing it.
pub async fn catalog_files(name: &str) -> Result<Vec<(String, String)>, String> {
    use crate::catalog;
    let config = crate::github::Config::from_env();
    let (branch, list) = catalog::list(&config).await.map_err(|err| po_i18n::t!("cmd.catalog.unreachable", err = err))?;
    let extension = list.iter().find(|e| e.name == name).ok_or_else(|| po_i18n::t!("cmd.catalog.unknown", name = name))?;
    Ok(catalog::contents(&config, extension, &branch).await)
}

/// Installs, removes or votes for extension `name`, from the catalog's browser.
pub async fn catalog_act(agent: &mut Agent, extras: &mut Extras, act: po_tui::catalog::Act, name: &str) -> Result<po_tui::catalog::Done, String> {
    use crate::catalog::{self, Vote};
    use po_i18n::t;
    use po_tui::catalog::{Act, Done};
    let config = crate::github::Config::from_env();
    match act {
        Act::Install => {
            let record = catalog::install(&config, name).await?;
            reload_extensions(agent, extras);
            let mut message = t!("cmd.catalog.installed", title = record.title);
            if record.statusline_before.is_some() {
                message = format!("{message}\n{}", t!("cmd.catalog.statusline_set"));
            }
            Ok(Done { message, installed: Some(true), votes: None })
        }
        Act::Remove => {
            let record = catalog::remove(name)?;
            reload_extensions(agent, extras);
            Ok(Done { message: t!("cmd.catalog.removed", title = record.title), installed: Some(false), votes: None })
        }
        Act::Vote => Ok(match catalog::vote(&config, name).await? {
            Vote::Given(n) => Done { message: t!("cmd.catalog.voted", name = name, n = n), installed: None, votes: Some(n) },
            Vote::Withdrawn(n) => Done { message: t!("cmd.catalog.unvoted", name = name, n = n), installed: None, votes: Some(n) },
            Vote::Login { url, code } => Done {
                message: format!("{}\n{url}\n{} {code}\n{}", t!("cmd.catalog.login"), t!("cmd.catalog.code"), t!("cmd.catalog.login_then")),
                installed: None,
                votes: None,
            },
        }),
    }
}

/// `/catalogue`: the community's extensions, by category; see one, install it, remove it, vote.
async fn catalog(agent: &mut Agent, extras: &mut Extras, argument: Option<&str>, rest: &[&str], reply: &mut Reply) {
    use crate::catalog::{self, Vote};
    use po_i18n::t;
    let config = crate::github::Config::from_env();
    let name = rest.first().copied().unwrap_or("");
    match argument {
        Some("installer" | "install") if !name.is_empty() => match catalog::install(&config, name).await {
            Ok(record) => {
                reload_extensions(agent, extras);
                reply.accent(t!("cmd.catalog.installed", title = record.title));
                for path in &record.files {
                    reply.dim(format!("  {}", path.display()));
                }
                if record.statusline_before.is_some() {
                    reply.dim(t!("cmd.catalog.statusline_set"));
                }
            }
            Err(err) => reply.warn(err),
        },
        Some("retirer" | "remove" | "desinstaller" | "désinstaller" | "uninstall") if !name.is_empty() => match catalog::remove(name) {
            Ok(record) => {
                reload_extensions(agent, extras);
                reply.accent(t!("cmd.catalog.removed", title = record.title));
            }
            Err(err) => reply.warn(err),
        },
        Some("voter" | "vote") if !name.is_empty() => match catalog::vote(&config, name).await {
            Ok(Vote::Given(n)) => reply.accent(t!("cmd.catalog.voted", name = name, n = n)),
            Ok(Vote::Withdrawn(n)) => reply.accent(t!("cmd.catalog.unvoted", name = name, n = n)),
            Ok(Vote::Login { url, code }) => {
                reply.accent(t!("cmd.catalog.login"));
                reply.plain(url);
                reply.field(t!("cmd.catalog.code"), code);
                reply.dim(t!("cmd.catalog.login_then"));
            }
            Err(err) => reply.warn(err),
        },
        Some("voir" | "show") if !name.is_empty() => {
            let (branch, list) = match catalog::list(&config).await {
                Ok(found) => found,
                Err(err) => return reply.warn(t!("cmd.catalog.unreachable", err = err)),
            };
            let Some(extension) = list.iter().find(|e| e.name == name) else {
                return reply.warn(t!("cmd.catalog.unknown", name = name));
            };
            let m = &extension.manifest;
            reply.section(format!("{} — {}", extension.name, m.title));
            reply.plain(m.description.trim());
            reply.field(t!("cmd.catalog.category"), po_tui::labs::category_label(&m.category));
            reply.field(t!("cmd.catalog.author"), &m.author);
            reply.field(t!("cmd.catalog.votes"), extension.votes.to_string());
            reply.field(t!("cmd.catalog.status"), if m.certified { t!("cmd.catalog.certified") } else { t!("cmd.catalog.not_certified") });
            if catalog::installed().contains_key(name) {
                reply.field(t!("cmd.catalog.here"), t!("cmd.catalog.installed_mark"));
            }
            // What it contains, to read before installing.
            for (file, text) in catalog::contents(&config, extension, &branch).await {
                reply.section(file);
                let lines: Vec<&str> = text.lines().collect();
                for line in lines.iter().take(20) {
                    reply.dim(format!("  {line}"));
                }
                if lines.len() > 20 {
                    reply.dim(t!("cmd.catalog.more_lines", n = lines.len() - 20));
                }
            }
            reply.dim(t!("cmd.catalog.show_hint", name = name));
        }
        filter => {
            // The list, by category; a category given shows that one only.
            let (_, list) = match catalog::list(&config).await {
                Ok(found) => found,
                Err(err) => return reply.warn(t!("cmd.catalog.unreachable", err = err)),
            };
            let installed = catalog::installed();
            let wanted = filter.and_then(|f| catalog::CATEGORIES.iter().find(|c| **c == f || po_tui::labs::category_label(c).to_lowercase() == f.to_lowercase()));
            if filter.is_some() && wanted.is_none() {
                reply.warn(t!("cmd.catalog.usage"));
                return;
            }
            if list.is_empty() {
                reply.plain(t!("cmd.catalog.empty"));
            }
            for category in catalog::CATEGORIES.iter().filter(|c| wanted.is_none_or(|w| w == *c)) {
                let here: Vec<_> = list.iter().filter(|e| e.manifest.category == *category).collect();
                if here.is_empty() {
                    continue;
                }
                reply.section(po_tui::labs::category_label(category));
                for extension in here {
                    let mut marks = vec![format!("👍 {}", extension.votes)];
                    if extension.manifest.certified {
                        marks.insert(0, t!("cmd.catalog.certified_mark").to_owned());
                    }
                    if installed.contains_key(&extension.name) {
                        marks.push(t!("cmd.catalog.installed_mark").to_owned());
                    }
                    reply.item(&extension.name, format!("{} · {}", extension.manifest.title, marks.join(" · ")));
                }
            }
            reply.dim(t!("cmd.catalog.hint"));
        }
    }
}

fn skills(agent: &mut Agent, extras: &mut Extras, argument: Option<&str>, reply: &mut Reply) {
    use po_i18n::t;
    if matches!(argument, Some("recharger" | "reload")) {
        extras.skills = po_tools::skills::discover(&extras.workspace);
        agent.registry_mut().remove_prefixed("skill");
        if !extras.skills.is_empty() {
            agent.registry_mut().register(std::sync::Arc::new(SkillTool::new(extras.skills.clone())));
        }
        reply.accent(t!("cmd.skills.reloaded", n = extras.skills.len()));
    }
    if extras.skills.is_empty() {
        reply.plain(t!("cmd.skills.none"));
        reply.dim(t!("cmd.skills.template"));
        for root in po_tools::skills::skill_roots(&extras.workspace) {
            reply.dim(t!("cmd.skills.scanned", path = root.display()));
        }
        return;
    }
    reply.section(t!("cmd.skills.title", n = extras.skills.len()));
    for skill in &extras.skills {
        reply.item(&skill.name, &skill.description);
    }
    reply.dim(t!("cmd.skills.note"));
}

async fn local_command(
    agent: &mut Agent,
    local: &mut LocalManager,
    argument: Option<&str>,
    rest: &[&str],
    reply: &mut Reply,
) {
    use po_i18n::t;
    let index = rest.first().copied();
    match argument {
        Some("dossier" | "dir") => {
            let dir = rest.join(" ");
            if dir.trim().is_empty() || !std::path::Path::new(dir.trim().trim_matches('"')).is_dir() {
                reply.warn(t!("cmd.local.usage_dir"));
                return;
            }
            let mut prefs = po_local::prefs::LocalPrefs::load();
            prefs.add_models_dir(&dir);
            match prefs.save() {
                Ok(path) => reply.dim(t!("cmd.local.saved", path = path.display())),
                Err(err) => reply.warn(t!("cmd.local.pref_failed", err = err)),
            }
            local.refresh();
            // A native llama-server stored next to these models becomes the right candidate —
            // unless a server is already running: we do not drop a process midway.
            let running = local.supervisor.as_ref().is_some_and(|s| s.running().is_some());
            if !running && let Ok(found) = po_local::Supervisor::discover() {
                reply.dim(t!("cmd.local.server_found", path = found.binary().display()));
                local.supervisor = Some(found);
            }
            reply.accent(t!("cmd.local.found", n = local.catalog.len()));
            return;
        }
        Some("serveur" | "server") => {
            let path = rest.join(" ");
            let path = path.trim().trim_matches('"');
            if !std::path::Path::new(path).is_file() {
                reply.warn(t!("cmd.local.usage_server"));
                return;
            }
            let mut prefs = po_local::prefs::LocalPrefs::load();
            prefs.llama_server = Some(path.to_owned());
            if let Err(err) = prefs.save() {
                reply.warn(t!("cmd.local.pref_failed", err = err));
            }
            local.supervisor = po_local::Supervisor::discover().ok();
            reply.accent(t!("cmd.local.server_set", path = path));
            return;
        }
        _ => {}
    }
    // The disk scan is deferred at startup: the first command that designates a model by its
    // number must do it itself, otherwise "/local 1" finds nothing.
    if local.catalog.is_empty() && argument.is_some_and(|a| a != "stop" && a != "arret" && a != "arrêt") {
        local.refresh();
    }
    match argument {
        None => {
            local.refresh();
            if local.catalog.is_empty() {
                reply.plain(t!("cmd.local.none"));
            } else {
                for entry in local.listing() {
                    reply.plain(entry);
                }
                if !local.has_server_binary() {
                    reply.warn(t!("cmd.local.no_binary"));
                }
            }
        }
        Some("stop" | "arret" | "arrêt") => reply.dim(local.deactivate(agent).await),
        Some("decharge" | "décharge" | "unload") => match index.and_then(|n| n.parse::<usize>().ok()) {
            Some(index) => match local.catalog.get(index.wrapping_sub(1)) {
                Some(entry) => {
                    let label = entry.label();
                    match local.unload(&label, agent).await {
                        Ok(message) => reply.accent(message),
                        Err(err) => reply.warn(err),
                    }
                }
                None => reply.warn(t!("cmd.local.no_model", n = index)),
            },
            None => reply.plain(t!("cmd.local.usage_unload")),
        },
        Some("etat" | "état" | "status") => {
            local.refresh();
            reply.plain(local.status());
        }
        Some(raw) => match raw.parse::<usize>() {
            Ok(index) => match local.activate(index, agent).await {
                Ok(message) => reply.accent(message),
                Err(err) => reply.warn(err),
            },
            Err(_) => reply.plain(t!("cmd.local.usage")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeping_a_server_preserves_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, r#"{"other": 1, "mcpServers": {"old": {"command": "x"}}}"#).unwrap();
        let config = ServerConfig::stdio("new", "npx", vec!["-y".into()]);
        save_mcp_json(&path, &config).unwrap();
        let reread = po_mcp::read_mcp_json(&std::fs::read_to_string(&path).unwrap());
        let names: Vec<_> = reread.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["new", "old"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"other\": 1"));
    }

    #[test]
    fn a_file_that_does_not_read_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let broken = "{\"mcpServers\": {\"old\": {\"command\": \"x\"},}}";
        std::fs::write(&path, broken).unwrap();
        let config = ServerConfig::stdio("new", "npx", vec![]);
        assert!(save_mcp_json(&path, &config).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn the_help_mentions_every_recognized_command() {
        use po_i18n::Lang;
        let cases = [
            (Lang::Fr, ["/statusline", "/verbosite", "/reflexion", "/temperature", "/contexte", "/cache", "/nouveau", "/mcp", "/skills", "/outils", "/delegation", "/agents", "/local", "/mode", "/modele", "/cout", "/quitter", "/voix", "/criteres", "/serveur"]),
            (Lang::En, ["/statusline", "/verbosity", "/effort", "/temperature", "/context", "/cache", "/new", "/mcp", "/skills", "/tools", "/delegation", "/agents", "/local", "/mode", "/model", "/cost", "/quit", "/voice", "/criteria", "/server"]),
        ];
        for (lang, commands) in cases {
            let help = po_i18n::text_in("cmd.help.text", lang).unwrap();
            for command in commands {
                assert!(help.contains(command), "{command} missing from the help ({})", lang.code());
            }
        }
    }
}
