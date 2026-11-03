//! What `/moteurs` does beyond finding the engines: a model's own profile (context, KV cache,
//! reasoning, sampling), game mode, starting a model by a piece of its name with its loading
//! progress on screen, the log of a server, what a GGUF's header says, and a small model on a
//! second graphics card for the subagents.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use po_agent::Agent;
use po_core::{Capabilities, ModelSpec, Origin, Pricing};
use po_i18n::t;
use po_local::prefs::{LocalPrefs, ModelProfile};
use po_local::{Gpu, GpuBackend, Hardware, strata};
use po_provider::OpenAiProvider;
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::engines_ui::{Engines, Kind, Report, w};
use crate::local_ui::LocalManager;

/// The port of the second card's model: the one after Strata's 8080.
pub const WORKER_PORT: u16 = 8081;

/// A model `/moteurs` can start, whichever engine serves it.
#[derive(Debug, Clone, PartialEq)]
pub enum Model {
    /// Index in `local.catalog`, and its name.
    Llama(usize, String),
    /// Index in the Strata install's run configs, and the model's name.
    Strata(usize, String),
}

impl Model {
    /// Its name, which is also the key of its profile.
    pub fn name(&self) -> &str {
        match self {
            Self::Llama(_, name) | Self::Strata(_, name) => name,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Self::Llama(..) => Kind::Llama,
            Self::Strata(..) => Kind::Strata,
        }
    }
}

/// Every model of both engines.
pub fn all_models(local: &LocalManager, report: Option<&Report>) -> Vec<Model> {
    let mut out: Vec<Model> = local.catalog.iter().enumerate().map(|(i, e)| Model::Llama(i, e.label())).collect();
    if let Some(install) = report.and_then(|r| r.strata.install.as_ref()) {
        out.extend(install.configs.iter().enumerate().map(|(i, c)| Model::Strata(i, c.model_name.clone())));
    }
    out
}

/// The models whose name holds `fragment` (any case). An exact name wins over the pieces.
pub fn find(models: &[Model], fragment: &str) -> Vec<Model> {
    let wanted = fragment.trim().to_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    if let Some(exact) = models.iter().find(|m| m.name().to_lowercase() == wanted) {
        return vec![exact.clone()];
    }
    // Every word must be there: "flash iq3" finds qwen3.8-flash-next-iq3_xxs.
    let words: Vec<&str> = wanted.split_whitespace().collect();
    models.iter().filter(|m| words.iter().all(|w| m.name().to_lowercase().contains(w))).cloned().collect()
}

/// A profile in a few words, for a model's line.
pub fn profile_summary(profile: &ModelProfile) -> String {
    let mut parts = Vec::new();
    if let Some(c) = profile.context {
        parts.push(format!("{} {}k", t!("engines.profile.context"), c / 1024));
    }
    if let Some(c) = &profile.cache {
        parts.push(format!("{} {c}", t!("engines.profile.cache")));
    }
    if let Some(r) = &profile.reasoning {
        parts.push(format!("{} {r}", t!("engines.profile.reasoning")));
    }
    if let Some(v) = profile.temperature {
        parts.push(format!("temp {v}"));
    }
    if let Some(v) = profile.top_p {
        parts.push(format!("top_p {v}"));
    }
    if let Some(v) = profile.top_k {
        parts.push(format!("top_k {v}"));
    }
    if let Some(v) = profile.min_p {
        parts.push(format!("min_p {v}"));
    }
    parts.join(" · ")
}

/// A profile as what changes in Strata's run config.
pub fn strata_overrides(profile: &ModelProfile, game: bool) -> strata::Overrides {
    strata::Overrides {
        context: profile.context,
        // llama.cpp's cache types mean nothing to Strata's engine.
        kv: profile.cache.clone().filter(|c| matches!(c.as_str(), "int8" | "q4_0" | "k8v4")),
        reasoning_effort: profile.request_effort(),
        temperature: profile.temperature,
        top_p: profile.top_p,
        top_k: profile.top_k,
        min_p: profile.min_p,
        // Strata's own game mode: two processor workers for the experts, at low priority.
        pool_workers: game.then_some(2),
        low_priority: game,
        placement: profile.placement.clone().filter(|p| !p.is_auto()),
        // Read at the start (a file of the user's), as the cards seen then.
        preset: None,
        cards: Vec::new(),
        template: profile.template.clone(),
    }
}

/// The sampling a session gets from a model's profile.
pub fn session_defaults(profile: &ModelProfile) -> po_agent::ModelDefaults {
    po_agent::ModelDefaults {
        temperature: profile.temperature,
        reasoning_effort: profile.request_effort(),
        top_p: profile.top_p,
        top_k: profile.top_k,
        min_p: profile.min_p,
        repeat_penalty: profile.repeat_penalty,
        max_output_tokens: profile.max_output,
        system: profile.system.clone(),
    }
}

/// Gives every model of the router whose name has a profile its session defaults: the Strata
/// already running when Po opened is used with its profile too.
pub fn apply_session_defaults(agent: &mut Agent, prefs: &LocalPrefs) {
    let ids: Vec<String> = agent.router().models().iter().map(|m| m.id.clone()).collect();
    for id in ids {
        let profile = prefs.model(&id);
        if profile.is_empty() {
            agent.settings.model_defaults.remove(&id);
        } else {
            agent.settings.model_defaults.insert(id, session_defaults(&profile));
        }
    }
}

// ------------------------------------------------------------------------------- progress

/// Shows what a loading server does, every half second, until the returned guard is dropped.
///
/// It speaks to both ends at once: the terminal's progress line (`po_tui::set_progress`), and
/// the session's status (`po_tui::control::Mirror::loading`), which every phone following the
/// session gets. A guard dropped in a panic or an early return clears both.
pub struct Progress {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.task.abort();
        po_tui::set_progress(None);
        po_tui::control::mirror().loading(None);
    }
}

/// The newest `po-llama-*.log` written since `since`: the server being started.
fn newest_llama_log(since: std::time::SystemTime) -> Option<PathBuf> {
    std::fs::read_dir(std::env::temp_dir())
        .ok()?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("po-llama-") && name.ends_with(".log")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(modified, _)| *modified >= since)
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Follows the loading of `model`: `logs` gives the log files to read at each beat.
pub fn follow(model: String, kind: Kind, logs: impl Fn() -> Vec<PathBuf> + Send + 'static) -> Progress {
    let started = Instant::now();
    let since_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mirror = po_tui::control::mirror();
    let task = tokio::spawn(async move {
        // The first beat is immediate: a phone that just sent a message should not wait half a
        // second to learn that something is happening.
        loop {
            let tail: Vec<String> = logs().iter().flat_map(|log| po_local::supervisor::log_tail(log, 40)).collect();
            let stage = match kind {
                Kind::Llama => po_local::supervisor::llama_progress(&tail),
                Kind::Strata => strata::progress(&tail).or_else(|| po_local::supervisor::llama_progress(&tail)),
            }
            .unwrap_or_else(|| t!("engines.progress.starting").to_owned());
            po_tui::set_progress(Some(t!("engines.progress.line", model = model, stage = stage, secs = started.elapsed().as_secs())));
            mirror.loading(Some(po_wire::Loading { model: model.clone(), stage, since_ms }));
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    Progress { task }
}

/// Follows a llama.cpp server being started now.
pub fn follow_llama(model: String) -> Progress {
    let since = std::time::SystemTime::now() - Duration::from_secs(1);
    follow(model, Kind::Llama, move || newest_llama_log(since).into_iter().collect())
}

/// Follows a Strata server being started on `port` from `config`.
pub fn follow_strata(model: String, port: u16, config: &Path) -> Progress {
    let server_log = std::env::temp_dir().join(format!("po-strata-{port}.log"));
    // Po starts Strata from a copy of its config, which logs next to itself.
    let name = config.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let engine_log = Some(strata::work_dir().join(port.to_string()).join(name).with_extension("engine.log"));
    follow(model, Kind::Strata, move || [Some(server_log.clone()), engine_log.clone()].into_iter().flatten().collect())
}

// ------------------------------------------------------------------------------- info, logs

/// What the GGUF header of a model says.
pub fn info_lines(model: &Model, local: &LocalManager, report: Option<&Report>, reply: &mut Reply) {
    let (path, header, plan) = match model {
        Model::Llama(i, _) => {
            let Some(entry) = local.catalog.get(*i) else { return };
            (entry.path.clone(), Ok(entry.info.clone()), Some(entry.plan.summary()))
        }
        Model::Strata(i, _) => {
            let Some(config) = report.and_then(|r| r.strata.install.as_ref()).and_then(|inst| inst.configs.get(*i)) else { return };
            let Some(shard) = config.model_files.first() else { return };
            (shard.clone(), po_local::read_header(shard), None)
        }
    };
    reply.section(t!("engines.info.title", model = model.name()));
    reply.field(t!("engines.info.file"), path.display().to_string());
    match header {
        Ok(info) => {
            reply.field(t!("engines.info.architecture"), &info.architecture);
            if !info.name.is_empty() {
                reply.field(t!("engines.info.name"), &info.name);
            }
            let quant = path.file_name().and_then(|n| po_local::quant::from_name(&n.to_string_lossy())).unwrap_or_else(|| "?".into());
            reply.field(t!("engines.info.quant"), quant);
            reply.field(t!("engines.info.size"), t!("menu.gb", n = format!("{:.2}", info.file_size_bytes as f64 / 1e9)));
            reply.field(t!("engines.info.layers"), info.block_count.to_string());
            reply.field(t!("engines.info.embedding"), info.embedding_length.to_string());
            reply.field(t!("engines.info.heads"), t!("engines.info.heads_value", heads = info.head_count, kv = info.head_count_kv));
            reply.field(t!("engines.info.context"), t!("menu.tokens", n = info.context_length));
            reply.field(t!("engines.info.tensors"), info.tensor_count.to_string());
            if let Some(plan) = plan {
                reply.field(t!("engines.info.plan"), plan);
            }
            if matches!(model, Model::Strata(..)) {
                reply.dim(t!("engines.info.shard"));
            }
        }
        Err(err) => reply.warn(t!("engines.info.unreadable", err = err)),
    }
}

/// The log of a server: the one Po started for that engine, else (Strata) the one its serving
/// config names. Read from its end, never written.
pub fn log_lines(target: &str, local: &LocalManager, engines: &Engines, reply: &mut Reply) {
    const LINES: usize = 30;
    let path: Option<PathBuf> = match target {
        "worker" | "second" | "renfort" => engines.worker.as_ref().and_then(|s| s.running()).map(|r| r.log.clone()),
        "strata" => engines.strata_server.as_ref().map(|s| s.engine_log.clone().filter(|l| l.is_file()).unwrap_or_else(|| s.log.clone())).or_else(|| {
            let report = engines.report.as_ref()?;
            let live = report.strata_live.first()?;
            let config = report.strata.install.as_ref()?.configs.iter().find(|c| c.model_name == live.model)?;
            strata::engine_log_of(&config.path)
        }),
        _ => local.supervisor.as_ref().and_then(|s| s.running()).map(|r| r.log.clone()),
    };
    let Some(path) = path else {
        reply.dim(t!("engines.log.none"));
        return;
    };
    reply.section(t!("engines.log.title", path = path.display()));
    let lines = po_local::supervisor::log_tail(&path, LINES);
    if lines.is_empty() {
        reply.dim(t!("engines.log.empty"));
    }
    for line in lines {
        reply.dim(line);
    }
}

// ------------------------------------------------------------------------------- game mode

pub fn set_game(raw: Option<&str>, reply: &mut Reply) {
    let mut prefs = LocalPrefs::load();
    let on = match raw {
        Some("on" | "oui" | "yes") => true,
        Some("off" | "non" | "no") => false,
        _ => !prefs.game_mode,
    };
    prefs.game_mode = on;
    if prefs.save().is_err() {
        reply.warn(t!("cmd.local.pref_failed", err = "local.json"));
        return;
    }
    reply.accent(if on { t!("engines.game.enabled") } else { t!("engines.game.disabled") });
    reply.dim(t!("engines.game.how"));
}

// ------------------------------------------------------------------------------- profiles

/// The values offered for a profile key, as (label, value).
fn choices(key: &str, kind: Kind) -> Vec<(String, String)> {
    let plain = |values: &[&str]| values.iter().map(|v| ((*v).to_owned(), (*v).to_owned())).collect::<Vec<_>>();
    match key {
        "contexte" | "context" => [8, 16, 32, 64, 128, 256].iter().map(|k| (format!("{k}k"), format!("{k}k"))).collect(),
        "cache" => match kind {
            Kind::Llama => plain(&["f16", "q8_0", "q4_0"]),
            Kind::Strata => plain(&["int8", "q4_0", "k8v4"]),
        },
        "raisonnement" | "reasoning" => plain(&["auto", "off", "on", "low", "medium", "high"]),
        "temperature" => plain(&["0", "0.2", "0.6", "0.7", "1.0"]),
        "top_p" => plain(&["0.8", "0.9", "0.95", "1.0"]),
        "top_k" => plain(&["0", "20", "40", "64"]),
        "min_p" => plain(&["0", "0.05", "0.1"]),
        _ => Vec::new(),
    }
}

/// The profile keys, in the order of the menu, with their label.
fn keys() -> [(&'static str, &'static str); 7] {
    [
        ("context", t!("engines.profile.context")),
        ("cache", t!("engines.profile.cache")),
        ("reasoning", t!("engines.profile.reasoning")),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("top_k", "top_k"),
        ("min_p", "min_p"),
    ]
}

fn current_value(profile: &ModelProfile, key: &str) -> Option<String> {
    match key {
        "context" => profile.context.map(|c| format!("{}k", c / 1024)),
        "cache" => profile.cache.clone(),
        "reasoning" => profile.reasoning.clone(),
        "temperature" => profile.temperature.map(|v| v.to_string()),
        "top_p" => profile.top_p.map(|v| v.to_string()),
        "top_k" => profile.top_k.map(|v| v.to_string()),
        "min_p" => profile.min_p.map(|v| v.to_string()),
        _ => None,
    }
}

/// `/moteurs reglage <model> <key> <value>`: changes and saves one field of a model's profile.
pub fn set_setting(agent: &mut Agent, model: &str, key: &str, value: &str, reply: &mut Reply) {
    let mut prefs = LocalPrefs::load();
    let mut profile = prefs.model(model);
    if !profile.set(key, value) {
        reply.warn(t!("engines.profile.refused", key = key, value = value));
        return;
    }
    *prefs.model_mut(model) = profile.clone();
    if let Err(err) = prefs.save() {
        reply.warn(t!("cmd.local.pref_failed", err = err));
        return;
    }
    apply_session_defaults(agent, &prefs);
    let summary = profile_summary(&profile);
    reply.accent(t!("engines.profile.saved", model = model, summary = if summary.is_empty() { t!("engines.profile.empty").to_owned() } else { summary }));
    reply.dim(t!("engines.profile.when"));
}

/// `/moteurs reglage <model> defaut`: forgets a model's profile.
pub fn reset_profile(agent: &mut Agent, model: &str, reply: &mut Reply) {
    let mut prefs = LocalPrefs::load();
    prefs.models.remove(model);
    if let Err(err) = prefs.save() {
        reply.warn(t!("cmd.local.pref_failed", err = err));
        return;
    }
    apply_session_defaults(agent, &prefs);
    reply.accent(t!("engines.profile.reset", model = model));
}

/// A model's own menu: start it, start it in game mode, its header, and each field of its profile.
pub fn model_menu(cmd: &str, model: &Model, detail: String) -> Menu {
    let name = model.name();
    let profile = LocalPrefs::load().model(name);
    let start = w("demarrer", "start");
    let mut items = vec![
        Item::run(t!("engines.model.start"), format!("{cmd} {start} {name}")).detail(detail),
        Item::run(t!("engines.model.start_game"), format!("{cmd} {start} {name} {}", w("jeu", "game"))).detail(t!("engines.game.short")),
        Item::show(t!("engines.model.info"), format!("{cmd} info {name}")),
    ];
    for (key, label) in keys() {
        let value = current_value(&profile, key).unwrap_or_else(|| t!("engines.profile.auto").to_owned());
        items.push(Item::run(format!("{} : {label}", t!("engines.model.setting")), format!("{cmd} {} {name} {key}", w("reglage", "setting"))).detail(value));
    }
    if !profile.is_empty() {
        items.push(Item::run(t!("engines.model.reset"), format!("{cmd} {} {name} {}", w("reglage", "setting"), w("defaut", "default"))));
    }
    let summary = profile_summary(&profile);
    Menu::new(name, items).note(if summary.is_empty() { t!("engines.profile.none").to_owned() } else { t!("engines.profile.note", summary = summary) })
}

/// The values of one field of a model's profile.
pub fn setting_menu(cmd: &str, model: &Model, key: &str) -> Option<Menu> {
    let name = model.name();
    let (key, label) = keys().into_iter().find(|(k, label)| *k == key || label.eq_ignore_ascii_case(key))?;
    let current = current_value(&LocalPrefs::load().model(name), key);
    let set = w("reglage", "setting");
    let mut items = vec![Item::run(t!("engines.profile.auto"), format!("{cmd} {set} {name} {key} {}", w("defaut", "default"))).current(current.is_none())];
    for (label, value) in choices(key, model.kind()) {
        let here = current.as_deref() == Some(value.as_str());
        items.push(Item::run(label, format!("{cmd} {set} {name} {key} {value}")).current(here));
    }
    items.push(Item::fill(t!("menu.other_value"), format!("{cmd} {set} {name} {key} ")));
    let hint = match key {
        "context" => t!("engines.profile.hint.context"),
        "cache" => t!("engines.profile.hint.cache"),
        "reasoning" => t!("engines.profile.hint.reasoning"),
        _ => t!("engines.profile.hint.sampling"),
    };
    Some(Menu::new(format!("{name} · {label}"), items).note(hint))
}

// ------------------------------------------------------------------------------- worker

/// The card a second model goes on: the one the user chose, else the smaller of two NVIDIA cards
/// (the larger one serves the main model). `None` on a machine with one card.
pub fn worker_gpu(hardware: &Hardware, prefs: &LocalPrefs) -> Option<(Gpu, String)> {
    let cuda: Vec<&Gpu> = hardware.gpus.iter().filter(|g| g.backend == GpuBackend::Cuda).collect();
    if cuda.len() < 2 {
        return None;
    }
    if let Some(chosen) = prefs.worker.as_ref().and_then(|w| w.gpu.clone()) {
        // An index, or a UUID that CUDA takes as it is.
        let gpu = chosen.parse::<u32>().ok().and_then(|i| cuda.iter().find(|g| g.index == Some(i)).copied()).or_else(|| cuda.iter().min_by_key(|g| g.vram_total_mib).copied())?;
        return Some((gpu.clone(), chosen));
    }
    let gpu = cuda.iter().min_by_key(|g| (g.vram_total_mib, std::cmp::Reverse(g.index)))?;
    Some(((*gpu).clone(), gpu.index?.to_string()))
}

/// The machine as the worker's server sees it: that card alone.
fn worker_hardware(hardware: &Hardware, gpu: &Gpu) -> Hardware {
    Hardware { gpus: vec![gpu.clone()], ..hardware.clone() }
}

/// The models that fit entirely on the worker's card now, the most useful first: a small fast
/// one (2 to 8 GB), the largest of those.
pub fn worker_candidates(local: &LocalManager, gpu: &Gpu) -> Vec<(usize, po_local::CatalogEntry)> {
    let hardware = worker_hardware(&local.hardware, gpu);
    let request = po_local::PlanRequest { desired_context: 16_384, ..Default::default() };
    let mut out: Vec<(usize, po_local::CatalogEntry)> = local
        .catalog
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut entry = e.clone();
            entry.plan = po_local::plan::plan(&entry.info, &hardware, &request);
            (i, entry)
        })
        .filter(|(_, e)| e.fits_on_gpu())
        .collect();
    let gb = |e: &po_local::CatalogEntry| e.info.file_size_bytes as f64 / 1e9;
    out.sort_by(|(_, a), (_, b)| {
        let sweet = |e: &po_local::CatalogEntry| (2.0..=8.0).contains(&gb(e));
        sweet(b).cmp(&sweet(a)).then(b.info.file_size_bytes.cmp(&a.info.file_size_bytes))
    });
    out
}

/// Starts a model on the second card, on port 8081 when free, and adds it to the router: the
/// subagents can then be given it (`/agents modele …`) while the main model keeps its card.
pub async fn start_worker(agent: &mut Agent, local: &mut LocalManager, engines: &mut Engines, raw: &str, game: bool, reply: &mut Reply) {
    let prefs = LocalPrefs::load();
    let Some((gpu, visible)) = worker_gpu(&local.hardware, &prefs) else {
        reply.warn(t!("engines.worker.one_card"));
        return;
    };
    let Some(binary) = engines.report.as_ref().and_then(|r| r.llama.located.as_ref()).map(|l| l.path.clone()) else {
        reply.warn(t!("engines.worker.no_llama"));
        return;
    };
    let candidates = worker_candidates(local, &gpu);
    let chosen = if raw.trim().is_empty() {
        let preferred = prefs.worker.as_ref().and_then(|w| w.model.clone());
        preferred.and_then(|p| candidates.iter().find(|(_, e)| e.label() == p || e.path.display().to_string() == p).cloned()).or_else(|| candidates.first().cloned())
    } else {
        let wanted = raw.trim().to_lowercase();
        candidates.iter().find(|(_, e)| e.label().to_lowercase().contains(&wanted)).cloned()
    };
    let Some((_, entry)) = chosen else {
        reply.warn(t!("engines.worker.nothing_fits", gpu = gpu.name));
        return;
    };
    stop_worker(agent, engines).await;
    let label = entry.label();
    let profile = prefs.model(&label);
    let mut supervisor = po_local::Supervisor::with_binary(binary);
    let options = po_local::supervisor::LaunchOptions {
        overrides: po_local::supervisor::profile_overrides(&profile, game, local.hardware.cpu_threads),
        port: Some(WORKER_PORT),
        low_priority: game,
        // The card by its PCI order, as `nvidia-smi` numbers it — or the UUID the user gave.
        env: vec![("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()), ("CUDA_VISIBLE_DEVICES".into(), visible.clone())],
    };
    let progress = follow_llama(label.clone());
    let started = supervisor.start_with(&entry.path, &entry.plan, &options).await;
    drop(progress);
    let port = match started {
        Ok(port) => port,
        Err(err) => {
            reply.warn(format!("{label}: {err}"));
            return;
        }
    };
    // Another name than the main model's, should it be the same file.
    let id = if agent.router().get(&label).is_some() { format!("{label}-gpu{visible}") } else { label.clone() };
    let base_url = format!("http://127.0.0.1:{port}/v1");
    agent.router_mut().add_model(ModelSpec {
        id: id.clone(),
        display_name: format!("{label} ({})", gpu.name.trim_start_matches("NVIDIA ").trim_start_matches("GeForce ")),
        origin: Origin::Local { backend: "llama.cpp".into(), endpoint: base_url.clone() },
        tier: crate::config::guess_tier(&label),
        capabilities: Capabilities { tools: true, ..Default::default() },
        context_window: entry.plan.context,
        max_output_tokens: 4096,
        pricing: Pricing::FREE,
    });
    agent.attach(id.clone(), Arc::new(OpenAiProvider::local("llama.cpp", base_url)));
    if !profile.is_empty() {
        agent.settings.model_defaults.insert(id.clone(), session_defaults(&profile));
    }
    engines.worker = Some(supervisor);
    engines.worker_model = Some(id.clone());
    reply.accent(t!("engines.worker.started", model = label, gpu = gpu.name, port = port, summary = entry.plan.summary()));
    reply.dim(t!("engines.worker.use", cmd = format!("{} {} <agent> {id}", w("/agents", "/agents"), w("modele", "model"))));
}

pub async fn stop_worker(agent: &mut Agent, engines: &mut Engines) -> bool {
    let Some(mut supervisor) = engines.worker.take() else { return false };
    let running = supervisor.running().is_some();
    supervisor.stop().await;
    if let Some(id) = engines.worker_model.take() {
        agent.router_mut().remove_model(&id);
        agent.settings.model_defaults.remove(&id);
    }
    running
}

/// The worker's state, for its menu and the overview.
pub fn worker_state(engines: &Engines) -> String {
    match (engines.worker.as_ref().and_then(|s| s.running()), engines.worker_model.as_ref()) {
        (Some(running), Some(id)) => t!("engines.worker.running", model = id, port = running.port),
        _ => t!("engines.state.stopped").to_owned(),
    }
}

/// The worker's lines in the advice: worth it as soon as there are two cards.
pub fn worker_advice(local: &LocalManager, reply: &mut Reply) {
    let Some((gpu, _)) = worker_gpu(&local.hardware, &LocalPrefs::load()) else { return };
    let best = worker_candidates(local, &gpu).into_iter().next();
    let model = best.map(|(_, e)| e.label()).unwrap_or_else(|| t!("engines.worker.small_model").to_owned());
    reply.item(t!("engines.worker.advice_name"), t!("engines.worker.advice", gpu = gpu.name, model = model));
    reply.dim(format!("  {}", t!("engines.worker.advice_how", cmd = format!("{} worker", w("/moteurs", "/engines")))));
}

/// The worker's menu.
pub fn worker_menu(cmd: &str, local: &LocalManager, engines: &Engines) -> Menu {
    let prefs = LocalPrefs::load();
    let Some((gpu, _)) = worker_gpu(&local.hardware, &prefs) else {
        return Menu::new(t!("engines.worker.title"), Vec::new()).note(t!("engines.worker.one_card"));
    };
    let start = w("demarrer", "start");
    let mut items: Vec<Item> = worker_candidates(local, &gpu)
        .into_iter()
        .take(8)
        .map(|(_, e)| {
            let current = engines.worker_model.as_deref() == Some(e.label().as_str());
            Item::run(e.label(), format!("{cmd} worker {start} {}", e.label())).detail(format!("{} · {}", t!("menu.gb", n = format!("{:.1}", e.info.file_size_bytes as f64 / 1e9)), e.plan.summary())).current(current)
        })
        .collect();
    if engines.worker.as_ref().is_some_and(|s| s.running().is_some()) {
        items.push(Item::run(t!("engines.menu.stop"), format!("{cmd} worker {}", w("arreter", "stop"))));
        items.push(Item::show(t!("engines.menu.log"), format!("{cmd} {} worker", w("journal", "log"))));
    }
    let note = t!("engines.worker.note", gpu = gpu.name, free = format!("{:.1}", gpu.vram_free_mib as f64 / 1024.0), state = worker_state(engines));
    Menu::new(t!("engines.worker.title"), items).note(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(name: &str, total: u64, free: u64, index: u32) -> Gpu {
        Gpu { name: name.into(), backend: GpuBackend::Cuda, vram_total_mib: total, vram_free_mib: free, index: Some(index) }
    }

    fn two_cards() -> Hardware {
        Hardware {
            gpus: vec![gpu("NVIDIA GeForce RTX 3070 Ti", 8192, 5777, 0), gpu("NVIDIA GeForce RTX 4070 SUPER", 12_282, 471, 1)],
            ram_total_mib: 65_430,
            ram_free_mib: 6000,
            cpu_threads: 24,
        }
    }

    #[test]
    fn a_name_fragment_finds_its_model() {
        let models = vec![
            Model::Strata(0, "qwen3.8-flash-next-coder-iq1_m".into()),
            Model::Strata(1, "qwen3.8-flash-next-iq2_xs".into()),
            Model::Strata(2, "qwen3.8-flash-next-iq3_xxs".into()),
            Model::Llama(0, "Qwen3.5-9B-Q4_K_M".into()),
        ];
        assert_eq!(find(&models, "iq3"), vec![models[2].clone()]);
        assert_eq!(find(&models, "IQ3"), vec![models[2].clone()], "any case");
        assert_eq!(find(&models, "flash"), models[..3].to_vec(), "several: the user chooses");
        assert_eq!(find(&models, "flash coder"), vec![models[0].clone()], "every word counts");
        assert_eq!(find(&models, "qwen3.8-flash-next-iq2_xs"), vec![models[1].clone()]);
        assert!(find(&models, "gemma").is_empty());
        assert!(find(&models, "  ").is_empty());
    }

    #[test]
    fn the_worker_goes_on_the_smaller_card_unless_told_otherwise() {
        let machine = two_cards();
        let (card, visible) = worker_gpu(&machine, &LocalPrefs::default()).unwrap();
        assert!(card.name.contains("3070 Ti"));
        assert_eq!(visible, "0");
        let mut prefs = LocalPrefs::default();
        prefs.worker = Some(po_local::prefs::WorkerPrefs { gpu: Some("1".into()), model: None });
        assert!(worker_gpu(&machine, &prefs).unwrap().0.name.contains("4070"));
        prefs.worker = Some(po_local::prefs::WorkerPrefs { gpu: Some("GPU-4aa19fbb-68de".into()), model: None });
        assert_eq!(worker_gpu(&machine, &prefs).unwrap().1, "GPU-4aa19fbb-68de", "a UUID goes to CUDA as it is");
        let one = Hardware { gpus: machine.gpus[..1].to_vec(), ..machine.clone() };
        assert!(worker_gpu(&one, &LocalPrefs::default()).is_none(), "one card: no worker");
    }

    #[test]
    fn a_profile_becomes_strata_s_settings_and_game_mode_its_workers() {
        let mut profile = ModelProfile::default();
        for (k, v) in [("context", "64k"), ("cache", "q8_0"), ("reasoning", "off"), ("temperature", "0.6")] {
            assert!(profile.set(k, v));
        }
        let o = strata_overrides(&profile, true);
        assert_eq!(o.context, Some(65_536));
        assert_eq!(o.kv, None, "a llama.cpp cache type is not sent to Strata");
        assert_eq!(o.reasoning_effort.as_deref(), Some("none"));
        assert_eq!(o.pool_workers, Some(2));
        assert!(o.low_priority);
        assert!(!strata_overrides(&ModelProfile::default(), false).touches_config(), "Strata's config as it is by default");
        let mut mine = ModelProfile::default();
        assert!(mine.set("préréglage", "tuned") && mine.set("template", "dépôt"));
        assert_eq!(mine.preset.as_deref(), Some("tuned"));
        assert_eq!(strata_overrides(&mine, false).template.as_deref(), Some("depot"));
        assert!(!mine.set("préréglage", "../x y"));
        assert!(mine.set("template", "modèle") && mine.template.is_none());
        assert!(mine.set("préréglage", "standard") && mine.preset.is_none());
    }

    #[test]
    fn the_profile_summary_says_only_what_is_set() {
        po_i18n::set_lang(po_i18n::Lang::En);
        assert_eq!(profile_summary(&ModelProfile::default()), "");
        let mut profile = ModelProfile::default();
        profile.set("context", "32k");
        profile.set("temperature", "0.7");
        let summary = profile_summary(&profile);
        assert!(summary.contains("32k") && summary.contains("temp 0.7"), "{summary}");
    }

    #[test]
    fn every_setting_offers_values_and_a_free_one() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let model = Model::Strata(0, "m".into());
        for (key, _) in keys() {
            let menu = setting_menu("/moteurs", &model, key).unwrap();
            assert!(menu.items.len() >= 3, "{key}");
            assert!(matches!(menu.items.last().unwrap().action, po_tui::menu::Action::Fill(_)));
            for item in &menu.items[1..menu.items.len() - 1] {
                if let po_tui::menu::Action::Run(c) = &item.action {
                    let value = c.rsplit(' ').next().unwrap();
                    assert!(ModelProfile::default().set(key, value), "{key} {value} is offered but refused");
                }
            }
        }
        assert!(setting_menu("/moteurs", &model, "couleur").is_none());
    }
}
