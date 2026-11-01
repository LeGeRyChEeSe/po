//! `/moteurs` (`/engines`): the local model servers of the machine, for someone who does not know
//! how they work.
//!
//! Two engines for now: llama.cpp (`llama-server`) and Strata. For each one, what Po found (where,
//! which version, running or not, launched by Po or not), the models it can serve with how they
//! fit this machine, the models worth installing for this hardware, and the few actions a
//! beginner needs: start a model, stop, look in another folder, give the exact executable, add a
//! models folder, detect again.
//!
//! Detection is automatic; every choice the user makes is saved per engine in `local.json`
//! ([`po_local::prefs`]). A server Po did not start is never stopped by Po: Strata on port 8080
//! may be the user's production server, started by hand and serving someone else.

use std::sync::Arc;
use std::time::Duration;

use po_agent::Agent;
use po_core::{Capabilities, ModelSpec, Origin, Pricing};
use po_i18n::t;
use po_local::prefs::{self, LocalPrefs};
use po_local::recommend::{self, Advice, Capacity, Engine};
use po_local::{Hardware, llama, strata};
use po_provider::OpenAiProvider;
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::local_ui::LocalManager;

/// How long `llama-server --version` is given.
const VERSION_TIMEOUT: Duration = Duration::from_secs(4);

/// The two engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Llama,
    Strata,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Strata, Kind::Llama];

    pub fn parse(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "llama" | "llama.cpp" | "llamacpp" | "llama-server" | "llama-cpp" => Some(Self::Llama),
            "strata" => Some(Self::Strata),
            _ => None,
        }
    }

    /// The key of its settings in `local.json`.
    pub fn key(self) -> &'static str {
        match self {
            Self::Llama => prefs::LLAMA_CPP,
            Self::Strata => prefs::STRATA,
        }
    }

    /// The word of the commands (`/moteurs llama …`).
    pub fn word(self) -> &'static str {
        match self {
            Self::Llama => "llama",
            Self::Strata => "strata",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Llama => "llama.cpp",
            Self::Strata => "Strata",
        }
    }

    fn engine(self) -> Engine {
        match self {
            Self::Llama => Engine::LlamaCpp,
            Self::Strata => Engine::Strata,
        }
    }
}

/// The word of the interface's language: every subcommand exists in both.
pub(crate) fn w(fr: &'static str, en: &'static str) -> &'static str {
    if po_i18n::lang() == po_i18n::Lang::Fr { fr } else { en }
}

/// What was found, as of the last detection.
#[derive(Debug, Clone)]
pub struct Report {
    pub prefs: LocalPrefs,
    pub hardware: Hardware,
    pub capacity: Capacity,
    pub llama: llama::Detection,
    pub llama_version: Option<String>,
    pub strata: strata::Detection,
    /// The Strata servers answering, read-only (`/v1/status`).
    pub strata_live: Vec<strata::Live>,
}

impl Report {
    fn strata_install(&self) -> Option<&strata::Install> {
        self.strata.install.as_ref()
    }

    fn strata_configs(&self) -> &[strata::RunConfig] {
        self.strata_install().map(|i| i.configs.as_slice()).unwrap_or(&[])
    }

    fn has_choice(&self, kind: Kind) -> bool {
        let engine = self.prefs.engine(kind.key());
        engine.executable.is_some() || engine.search_dir.is_some() || (kind == Kind::Llama && self.prefs.llama_executable().is_some())
    }
}

/// The state kept over the session.
#[derive(Default)]
pub struct Engines {
    pub report: Option<Report>,
    /// The Strata server Po started itself, the only one Po may stop.
    pub(crate) strata_server: Option<strata::Running>,
    /// Its model's identifier in the router.
    pub(crate) strata_model: Option<String>,
    /// The small model on the second card, for the subagents.
    pub(crate) worker: Option<po_local::Supervisor>,
    pub(crate) worker_model: Option<String>,
}

/// What the user set for Strata, as the detection reads it.
fn strata_choice(prefs: &LocalPrefs) -> strata::Choice {
    let engine = prefs.engine(prefs::STRATA);
    strata::Choice {
        executable: engine.executable.map(Into::into),
        search_dir: engine.search_dir.map(Into::into),
        extra_dirs: engine.models_dirs.into_iter().map(Into::into).collect(),
    }
}

/// Detects everything: the hardware, both engines, the running Strata servers, and refreshes
/// llama.cpp's model catalog and binary in `local`.
///
/// The slow part (processes, disk) runs off the interface's thread: the screen keeps being drawn.
pub async fn detect(local: &mut LocalManager, engines: &mut Engines) {
    let prefs = LocalPrefs::load();
    let for_blocking = prefs.clone();
    let blocking = tokio::task::spawn_blocking(move || {
        let prefs = for_blocking;
        let hardware = Hardware::detect();
        let llama = llama::detect(&llama::Choice::from_prefs(&prefs), &llama::Env::system(&prefs));
        let version = llama.located.as_ref().and_then(|l| llama::version(&l.path, VERSION_TIMEOUT));
        let strata = strata::detect(&strata_choice(&prefs), &strata::Places::system());
        let catalog = po_local::catalog::scan_with(&prefs, &hardware, &po_local::PlanRequest::default());
        (hardware, llama, version, strata, catalog)
    })
    .await;
    let Ok((hardware, llama, llama_version, strata, catalog)) = blocking else { return };
    let mut ports: Vec<u16> = strata.install.iter().flat_map(|i| i.configs.iter().map(|c| c.port)).collect();
    ports.push(strata::DEFAULT_PORT);
    if let Some(server) = &engines.strata_server {
        ports.push(server.port);
    }
    let strata_live = strata::probe(&ports).await;

    // llama.cpp's catalog and binary feed `/local` too: one detection for both.
    let strata_files: Vec<std::path::PathBuf> = strata.install.iter().flat_map(|i| i.configs.iter().flat_map(|c| c.model_files.iter().cloned())).collect();
    local.hardware = hardware.clone();
    local.catalog = catalog.into_iter().filter(|e| !strata_files.contains(&e.path)).collect();
    let running = local.supervisor.as_ref().is_some_and(|s| s.running().is_some());
    if !running {
        local.supervisor = llama.located.as_ref().map(|l| po_local::Supervisor::with_binary(l.path.clone()));
    }
    engines.report = Some(Report { capacity: Capacity::of(&hardware), prefs, hardware, llama, llama_version, strata, strata_live });
}

/// [`detect`], then the profiles of the models the session already knows are applied to it.
pub async fn detect_for(agent: &mut Agent, local: &mut LocalManager, engines: &mut Engines) {
    detect(local, engines).await;
    if let Some(report) = engines.report.as_ref() {
        crate::engines_ops::apply_session_defaults(agent, &report.prefs);
    }
}

/// Detects once, the first time; the menus then reuse it (and the running state is read again).
async fn ensure(local: &mut LocalManager, engines: &mut Engines) {
    if engines.report.is_none() {
        detect(local, engines).await;
    } else {
        refresh_live(engines).await;
    }
}

/// Reads again which Strata answers, cheap and read-only.
async fn refresh_live(engines: &mut Engines) {
    let Some(report) = engines.report.as_mut() else { return };
    let mut ports: Vec<u16> = report.strata_configs().iter().map(|c| c.port).collect();
    ports.push(strata::DEFAULT_PORT);
    if let Some(server) = engines.strata_server.as_mut() {
        if server.exited() {
            engines.strata_server = None;
        } else {
            ports.push(server.port);
        }
    }
    report.strata_live = strata::probe(&ports).await;
}

fn gb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1e9)
}

/// The machine in one line: every card, the RAM.
fn hardware_line(hardware: &Hardware) -> String {
    let gpus: Vec<String> = hardware
        .gpus
        .iter()
        .map(|g| t!("engines.hw.gpu", name = g.name.trim_start_matches("NVIDIA ").trim_start_matches("GeForce "), total = format!("{:.0}", g.vram_total_mib as f64 / 1024.0), free = format!("{:.1}", g.vram_free_mib as f64 / 1024.0)))
        .collect();
    let gpus = if gpus.is_empty() { t!("engines.hw.no_gpu").to_owned() } else { gpus.join(" + ") };
    t!("engines.hw.line", gpus = gpus, ram = format!("{:.0}", hardware.ram_total_mib as f64 / 1024.0))
}

/// How an origin is said.
fn llama_origin(origin: llama::Origin) -> &'static str {
    match origin {
        llama::Origin::UserPath => t!("engines.origin.user_path"),
        llama::Origin::UserFolder => t!("engines.origin.user_folder"),
        llama::Origin::EnvVar => t!("engines.origin.env"),
        llama::Origin::Path => t!("engines.origin.path"),
        llama::Origin::PackageManager => t!("engines.origin.package"),
        llama::Origin::Usual => t!("engines.origin.usual"),
        llama::Origin::NextToModels => t!("engines.origin.next_to_models"),
        llama::Origin::Wsl => t!("engines.origin.wsl"),
    }
}

fn strata_origin(origin: strata::Origin) -> &'static str {
    match origin {
        strata::Origin::UserPath => t!("engines.origin.user_path"),
        strata::Origin::UserFolder => t!("engines.origin.user_folder"),
        strata::Origin::EnvVar => t!("engines.origin.env"),
        strata::Origin::StrataSettings => t!("engines.origin.strata_settings"),
        strata::Origin::App => t!("engines.origin.strata_app"),
    }
}

/// The running state of llama.cpp: started by Po, running outside Po, or stopped.
fn llama_state(local: &LocalManager, agent: &Agent) -> String {
    if let Some(running) = local.supervisor.as_ref().and_then(|s| s.running()) {
        return t!("engines.state.po", port = running.port, model = local.active_model_id.clone().unwrap_or_else(|| running.model_label()));
    }
    let outside: Vec<String> = agent
        .router()
        .models()
        .iter()
        .filter_map(|m| match &m.origin {
            Origin::Local { backend, endpoint } if backend == "llama.cpp" => Some(format!("{} ({})", m.id, endpoint.trim_end_matches("/v1"))),
            _ => None,
        })
        .collect();
    if outside.is_empty() { t!("engines.state.stopped").to_owned() } else { t!("engines.state.outside", what = outside.join(", ")) }
}

/// The running state of Strata.
fn strata_state(report: &Report, engines: &Engines) -> String {
    if report.strata_live.is_empty() {
        return t!("engines.state.stopped").to_owned();
    }
    report
        .strata_live
        .iter()
        .map(|live| {
            let by_po = engines.strata_server.as_ref().is_some_and(|s| s.port == live.port);
            let loaded = if live.loaded { t!("engines.strata.loaded") } else { t!("engines.strata.unloaded") };
            if by_po {
                t!("engines.state.po", port = live.port, model = format!("{} · {loaded}", live.model))
            } else {
                t!("engines.state.outside", what = format!("{} · {loaded} · port {}", live.model, live.port))
            }
        })
        .collect::<Vec<_>>()
        .join(" ; ")
}

/// One line for an engine in the overview: found or not, where, version.
fn engine_where(kind: Kind, report: &Report) -> String {
    match kind {
        Kind::Llama => match &report.llama.located {
            Some(found) => {
                let version = report.llama_version.as_deref().map(|v| format!(" · {v}")).unwrap_or_default();
                format!("{} ({}){version}", found.path.display(), llama_origin(found.origin))
            }
            None => t!("engines.not_found").to_owned(),
        },
        Kind::Strata => match report.strata_install() {
            Some(install) => {
                let version = install.version.as_deref().map(|v| format!(" · {} {v}", t!("engines.strata.engine"))).unwrap_or_default();
                format!("{} ({}){version}", install.dir.display(), strata_origin(install.origin))
            }
            None => t!("engines.not_found").to_owned(),
        },
    }
}

/// The llama.cpp models offered, in the catalog's order, without Strata's files.
fn llama_models(local: &LocalManager) -> Vec<(usize, &po_local::CatalogEntry)> {
    local.catalog.iter().enumerate().collect()
}

/// A llama.cpp model's detail: size, quantization, how it fits now.
fn llama_model_detail(entry: &po_local::CatalogEntry) -> String {
    let name = entry.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let quant = po_local::quant::from_name(&name).map(|q| format!(" · {q}")).unwrap_or_default();
    let fit = if !entry.plan.viable {
        t!("chat.local.does_not_fit").to_owned()
    } else if entry.fits_on_gpu() {
        t!("engines.fit_now.gpu").to_owned()
    } else if entry.plan.n_gpu_layers == 0 {
        t!("engines.fit_now.cpu").to_owned()
    } else {
        t!("engines.fit_now.partial", on = entry.plan.n_gpu_layers, total = entry.info.block_count)
    };
    format!("{}{quant} · {fit}", t!("menu.gb", n = gb(entry.info.file_size_bytes)))
}

/// A Strata model's detail.
fn strata_model_detail(config: &strata::RunConfig, report: &Report) -> String {
    let quant = config.quant.as_deref().map(|q| format!(" · {q}")).unwrap_or_default();
    let fit = if !config.is_complete() {
        t!("engines.strata.incomplete").to_owned()
    } else {
        recommend::strata_config_fit(&report.capacity, config.shard1_bytes, config.quant.as_deref()).label().to_owned()
    };
    let serving = report.strata_live.iter().any(|l| l.model == config.model_name);
    let serving = if serving { format!(" · {}", t!("engines.serving")) } else { String::new() };
    format!("{}{quant} · {fit}{serving}", t!("menu.gb", n = gb(config.size_bytes)))
}

/// A model's detail, with its profile when it has one.
fn with_profile(detail: String, prefs: &LocalPrefs, name: &str) -> String {
    let summary = crate::engines_ops::profile_summary(&prefs.model(name));
    if summary.is_empty() { detail } else { format!("{detail} · {} {summary}", t!("engines.profile.label")) }
}

/// The file names of every model on the machine, for the recommendations.
fn installed_files(local: &LocalManager, report: &Report) -> Vec<String> {
    let mut files: Vec<String> = local.catalog.iter().filter_map(|e| e.path.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
    // A split model is named by its first shard: listing the second too says the same twice.
    files.extend(report.strata_configs().iter().filter_map(|c| c.model_files.first()).filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())));
    files
}

fn advice_for(local: &LocalManager, report: &Report, kind: Option<Kind>) -> Vec<Advice> {
    recommend::advise(&report.capacity, &installed_files(local, report), report.strata.install.is_some())
        .into_iter()
        .filter(|a| kind.is_none_or(|k| a.card.engine == k.engine()))
        .collect()
}

/// The recommendations, as report lines.
fn advice_lines(local: &LocalManager, report: &Report, kind: Option<Kind>, reply: &mut Reply) {
    let advice = advice_for(local, report, kind);
    reply.section(t!("engines.advice.title"));
    reply.dim(hardware_line(&report.hardware));
    if report.hardware.gpus.len() > 1 {
        reply.dim(t!("engines.advice.one_card"));
    }
    if advice.is_empty() {
        reply.dim(t!("engines.advice.none"));
    }
    if kind.is_none_or(|k| k == Kind::Llama) {
        crate::engines_ops::worker_advice(local, reply);
    }
    for item in &advice {
        let mut name = format!("{} · {}", item.card.name, item.quant.name);
        if !item.card.tested {
            name.push_str(&format!(" · {}", t!("engines.untested")));
        }
        let mark = if item.chosen_installed { format!(" · {}", t!("engines.advice.installed")) } else { String::new() };
        reply.item(name, format!("{}{mark}", item.fit.label()));
        reply.dim(format!("  {}", po_i18n::text(item.card.about_key)));
        reply.dim(format!("  {}", item.reason(&report.capacity)));
        if !item.installed.is_empty() && !item.chosen_installed {
            reply.dim(format!("  {}", t!("engines.advice.have", files = item.installed.join(", "))));
        }
        if item.fit.usable() && !item.chosen_installed {
            match item.card.engine {
                Engine::LlamaCpp => {
                    reply.dim(format!("  {}", t!("engines.advice.get_llama", url = item.card.url(), file = item.quant.file)));
                    reply.dim(format!("  {}", t!("engines.advice.get_llama_hf", command = item.card.hf_command(item.quant))));
                }
                Engine::Strata => reply.dim(format!("  {}", t!("engines.advice.get_strata", quant = item.quant.name.to_ascii_lowercase()))),
            }
        }
    }
}

/// Where to get an engine that is missing.
fn install_lines(kind: Kind, report: &Report, reply: &mut Reply) {
    match kind {
        Kind::Llama => {
            reply.section(t!("engines.install.llama.title"));
            let backend = report.capacity.backend;
            let asset = match (cfg!(windows), cfg!(target_os = "macos"), backend) {
                (true, _, Some(po_local::GpuBackend::Cuda)) => t!("engines.install.llama.win_cuda"),
                (true, _, Some(_)) => t!("engines.install.llama.win_vulkan"),
                (true, _, None) => t!("engines.install.llama.win_cpu"),
                (_, true, _) => t!("engines.install.llama.mac"),
                _ => t!("engines.install.llama.linux"),
            };
            reply.plain(t!("engines.install.llama.where"));
            reply.plain(asset);
            reply.dim(t!("engines.install.llama.then"));
        }
        Kind::Strata => {
            reply.section(t!("engines.install.strata.title"));
            reply.plain(t!("engines.install.strata.private"));
            reply.dim(t!("engines.install.strata.then"));
        }
    }
}

/// What went wrong with the user's own choices, said first: a path that no longer leads
/// anywhere must not be silently ignored.
fn choice_warnings(kind: Kind, report: &Report, reply: &mut Reply) {
    let (bad_exe, bad_dir) = match kind {
        Kind::Llama => (report.llama.bad_executable.as_ref(), report.llama.bad_search_dir.as_ref()),
        Kind::Strata => (report.strata.bad_executable.as_ref(), report.strata.bad_search_dir.as_ref()),
    };
    if let Some(path) = bad_exe {
        reply.warn(t!("engines.warn.bad_exe", path = path.display(), engine = kind.name()));
    }
    if let Some(path) = bad_dir {
        reply.warn(t!("engines.warn.bad_dir", path = path.display(), engine = kind.name()));
    }
}

/// One engine, in full: where, state, models, folders.
fn engine_lines(kind: Kind, local: &LocalManager, engines: &Engines, agent: &Agent, reply: &mut Reply) {
    let Some(report) = engines.report.as_ref() else { return };
    reply.section(kind.name());
    choice_warnings(kind, report, reply);
    reply.field(t!("engines.field.where"), engine_where(kind, report));
    let state = match kind {
        Kind::Llama => llama_state(local, agent),
        Kind::Strata => strata_state(report, engines),
    };
    reply.field(t!("engines.field.state"), state);
    let folders = match kind {
        Kind::Llama => report.prefs.llama_models_dirs(),
        Kind::Strata => report.prefs.engine(prefs::STRATA).models_dirs,
    };
    if !folders.is_empty() {
        reply.field(t!("engines.field.folders"), folders.join(" ; "));
    }
    match kind {
        Kind::Llama => {
            let models = llama_models(local);
            if models.is_empty() {
                reply.dim(t!("engines.llama.no_models"));
            }
            for (n, (_, entry)) in models.iter().enumerate() {
                let active = local.active_model_id.as_deref() == Some(entry.label().as_str());
                let mark = if active { " ←" } else { "" };
                reply.item(format!("{}. {}{mark}", n + 1, entry.label()), with_profile(llama_model_detail(entry), &report.prefs, &entry.label()));
            }
            if report.llama.located.is_none() {
                reply.warn(t!("engines.llama.missing_hint", cmd = format!("/{} llama {}", w("moteurs", "engines"), w("installer", "install"))));
            }
        }
        Kind::Strata => {
            if let Some(install) = report.strata_install() {
                if !install.launchable() {
                    reply.warn(t!("engines.strata.not_launchable"));
                }
                if install.configs.is_empty() {
                    reply.dim(t!("engines.strata.no_configs"));
                }
                for (n, config) in install.configs.iter().enumerate() {
                    reply.item(format!("{}. {}", n + 1, config.model_name), with_profile(strata_model_detail(config, report), &report.prefs, &config.model_name));
                }
                if let Some(data) = &install.data_dir {
                    reply.dim(t!("engines.strata.data_dir", path = data.display()));
                }
                for other in &report.strata.others {
                    reply.dim(t!("engines.strata.other", path = other.display()));
                }
            } else if !report.strata_live.is_empty() {
                reply.dim(t!("engines.strata.live_only"));
            }
        }
    }
}

/// `/moteurs` without a menu (line mode, or `etat`): everything.
fn overview_lines(local: &LocalManager, engines: &Engines, agent: &Agent, reply: &mut Reply) {
    let Some(report) = engines.report.as_ref() else { return };
    reply.section(t!("engines.title"));
    reply.dim(hardware_line(&report.hardware));
    for kind in Kind::ALL {
        engine_lines(kind, local, engines, agent, reply);
    }
    if crate::engines_ops::worker_gpu(&local.hardware, &report.prefs).is_some() {
        reply.section(t!("engines.worker.title"));
        reply.field(t!("engines.field.state"), crate::engines_ops::worker_state(engines));
    }
    reply.field(t!("engines.game.label"), if report.prefs.game_mode { t!("engines.game.on") } else { t!("engines.game.off") });
    advice_lines(local, report, None, reply);
    reply.dim(t!("engines.how", cmd = w("/moteurs", "/engines")));
}

/// Puts the engines back into `local` once the work is done, whatever the path taken.
async fn with_engines<T>(local: &mut LocalManager, work: impl AsyncFnOnce(&mut LocalManager, &mut Engines) -> T) -> T {
    let mut engines = std::mem::take(&mut local.engines);
    let out = work(local, &mut engines).await;
    local.engines = engines;
    out
}

/// `/moteurs …`, typed or chosen in a menu.
pub async fn command(agent: &mut Agent, local: &mut LocalManager, words: &[&str], reply: &mut Reply) {
    with_engines(local, async |local, engines| run(agent, local, engines, words, reply).await).await;
    crate::engines_ops::apply_session_defaults(agent, &LocalPrefs::load());
}

async fn run(agent: &mut Agent, local: &mut LocalManager, engines: &mut Engines, words: &[&str], reply: &mut Reply) {
    let first = words.first().copied();
    match first {
        None | Some("etat" | "état" | "status" | "tout" | "all") => {
            ensure(local, engines).await;
            overview_lines(local, engines, agent, reply);
            return;
        }
        Some("detecter" | "détecter" | "detect" | "redetecter" | "rescan") => {
            detect_for(agent, local, engines).await;
            reply.accent(t!("engines.detected"));
            overview_lines(local, engines, agent, reply);
            return;
        }
        Some("conseils" | "conseil" | "advice" | "recommendations" | "recommandations") => {
            ensure(local, engines).await;
            if let Some(report) = engines.report.as_ref() {
                advice_lines(local, report, None, reply);
            }
            return;
        }
        Some("jeu" | "game") => {
            crate::engines_ops::set_game(words.get(1).copied(), reply);
            return;
        }
        Some("journal" | "log" | "logs") => {
            ensure(local, engines).await;
            crate::engines_ops::log_lines(words.get(1).copied().unwrap_or("llama"), local, engines, reply);
            return;
        }
        Some("worker" | "second" | "renfort") => {
            ensure(local, engines).await;
            let (game, rest) = game_word(words.get(2..).unwrap_or(&[]));
            match words.get(1).copied() {
                Some("demarrer" | "démarrer" | "start") => crate::engines_ops::start_worker(agent, local, engines, &rest, game || LocalPrefs::load().game_mode, reply).await,
                Some("arreter" | "arrêter" | "stop") => {
                    if crate::engines_ops::stop_worker(agent, engines).await {
                        reply.accent(t!("engines.worker.stopped"));
                    } else {
                        reply.dim(t!("engines.worker.none"));
                    }
                }
                _ => {
                    reply.section(t!("engines.worker.title"));
                    reply.field(t!("engines.field.state"), crate::engines_ops::worker_state(engines));
                    crate::engines_ops::worker_advice(local, reply);
                }
            }
            return;
        }
        Some(verb @ ("demarrer" | "démarrer" | "start" | "info" | "infos" | "modele" | "modèle" | "model")) => {
            ensure(local, engines).await;
            let (game, fragment) = game_word(&words[1..]);
            let models = crate::engines_ops::all_models(local, engines.report.as_ref());
            let found = crate::engines_ops::find(&models, &fragment);
            match found.as_slice() {
                [] => reply.warn(t!("engines.find.none", fragment = fragment)),
                [model] => match verb {
                    "info" | "infos" => crate::engines_ops::info_lines(model, local, engines.report.as_ref(), reply),
                    "modele" | "modèle" | "model" => engine_lines(model.kind(), local, engines, agent, reply),
                    _ => start_model(model, agent, local, engines, game, reply).await,
                },
                several => {
                    reply.plain(t!("engines.find.several", fragment = fragment));
                    for model in several {
                        reply.item(model.name(), model.kind().name());
                    }
                }
            }
            return;
        }
        Some("reglage" | "réglage" | "setting" | "settings") => {
            match (words.get(1), words.get(2), words.get(3..)) {
                (Some(model), Some(reset), _) if matches!(*reset, "defaut" | "défaut" | "default") && words.len() == 3 => crate::engines_ops::reset_profile(agent, model, reply),
                (Some(model), Some(key), Some(value)) if !value.is_empty() => crate::engines_ops::set_setting(agent, model, key, &value.join(" "), reply),
                _ => reply.warn(t!("engines.profile.usage", cmd = format!("{} {}", w("/moteurs", "/engines"), w("reglage", "setting")))),
            }
            return;
        }
        _ => {}
    }
    let Some(kind) = first.and_then(Kind::parse) else {
        reply.warn(t!("engines.usage", cmd = w("/moteurs", "/engines")));
        return;
    };
    let action = words.get(1).copied();
    let rest = words.get(2..).map(|r| r.join(" ")).unwrap_or_default();
    ensure(local, engines).await;
    match action {
        None | Some("etat" | "état" | "status") => engine_lines(kind, local, engines, agent, reply),
        Some("conseils" | "advice" | "recommendations") => {
            if let Some(report) = engines.report.as_ref() {
                advice_lines(local, report, Some(kind), reply);
            }
        }
        Some("installer" | "install" | "obtenir" | "get") => {
            if let Some(report) = engines.report.as_ref() {
                install_lines(kind, report, reply);
            }
        }
        Some("detecter" | "détecter" | "detect") => {
            detect(local, engines).await;
            reply.accent(t!("engines.detected"));
            engine_lines(kind, local, engines, agent, reply);
        }
        Some("demarrer" | "démarrer" | "start" | "lancer" | "charger" | "load") => {
            let (game, fragment) = game_word(words.get(2..).unwrap_or(&[]));
            start(kind, agent, local, engines, &fragment, game, reply).await
        }
        Some("journal" | "log" | "logs") => crate::engines_ops::log_lines(kind.word(), local, engines, reply),
        Some("arreter" | "arrêter" | "stop") => stop(kind, agent, local, engines, reply).await,
        Some("dossier" | "folder" | "chercher" | "search") => set_folder(kind, local, engines, &rest, reply).await,
        Some("executable" | "exécutable" | "exe" | "chemin" | "path") => set_executable(kind, local, engines, &rest, reply).await,
        Some("modeles" | "modèles" | "models") => add_models_dir(kind, local, engines, &rest, reply).await,
        Some("retirer" | "remove") => remove_models_dir(kind, local, engines, &rest, reply).await,
        Some("auto" | "automatique" | "reset") => {
            let mut prefs = LocalPrefs::load();
            prefs.reset_engine(kind.key());
            save(&prefs, reply);
            detect(local, engines).await;
            reply.accent(t!("engines.reset", engine = kind.name()));
            engine_lines(kind, local, engines, agent, reply);
        }
        Some(other) => reply.warn(t!("engines.unknown_action", action = other, cmd = format!("{} {}", w("/moteurs", "/engines"), kind.word()))),
    }
}

fn save(prefs: &LocalPrefs, reply: &mut Reply) -> bool {
    match prefs.save() {
        Ok(path) => {
            reply.dim(t!("cmd.local.saved", path = path.display()));
            true
        }
        Err(err) => {
            reply.warn(t!("cmd.local.pref_failed", err = err));
            false
        }
    }
}

async fn set_folder(kind: Kind, local: &mut LocalManager, engines: &mut Engines, raw: &str, reply: &mut Reply) {
    let dir = prefs::clean_path(raw);
    if dir.is_empty() || !std::path::Path::new(&dir).is_dir() {
        reply.warn(t!("engines.usage.folder", cmd = format!("{} {} {}", w("/moteurs", "/engines"), kind.word(), w("dossier", "folder"))));
        return;
    }
    // Checked before anything is saved: a folder that does not hold the engine would only
    // leave a warning behind at every detection.
    let holds = match kind {
        Kind::Llama => llama::find_in_dir(std::path::Path::new(&dir), llama::exe_name(), 4).is_some(),
        Kind::Strata => !strata::installs_under(std::path::Path::new(&dir)).is_empty(),
    };
    if !holds {
        reply.warn(t!("engines.folder.not_found", engine = kind.name(), dir = dir));
        return;
    }
    let mut prefs = LocalPrefs::load();
    prefs.set_search_dir(kind.key(), Some(&dir));
    if !save(&prefs, reply) {
        return;
    }
    detect(local, engines).await;
    let found = engines.report.as_ref().map(|r| engine_where(kind, r)).unwrap_or_default();
    reply.accent(t!("engines.folder.found", engine = kind.name(), path = found));
}

/// Is this path one the engine can use as its executable?
fn executable_ok(kind: Kind, path: &std::path::Path) -> bool {
    match kind {
        Kind::Llama => {
            if path.is_dir() {
                return llama::find_in_dir(path, llama::exe_name(), 3).is_some();
            }
            path.is_file() && path.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with("llama-server"))
        }
        Kind::Strata => strata::install_from_path(path).is_some(),
    }
}

async fn set_executable(kind: Kind, local: &mut LocalManager, engines: &mut Engines, raw: &str, reply: &mut Reply) {
    let path = prefs::clean_path(raw);
    let usage = || t!("engines.usage.exe", cmd = format!("{} {} {}", w("/moteurs", "/engines"), kind.word(), "executable"), what = match kind {
        Kind::Llama => t!("engines.exe.llama"),
        Kind::Strata => t!("engines.exe.strata"),
    });
    if path.is_empty() {
        reply.warn(usage());
        return;
    }
    if !executable_ok(kind, std::path::Path::new(&path)) {
        reply.warn(t!("engines.exe.rejected", path = path, engine = kind.name()));
        reply.dim(usage());
        return;
    }
    if kind == Kind::Llama && local.supervisor.as_ref().is_some_and(|s| s.running().is_some()) {
        reply.dim(t!("engines.exe.after_stop"));
    }
    let mut prefs = LocalPrefs::load();
    prefs.set_executable(kind.key(), Some(&path));
    if !save(&prefs, reply) {
        return;
    }
    detect(local, engines).await;
    let found = engines.report.as_ref().map(|r| engine_where(kind, r)).unwrap_or_default();
    reply.accent(t!("engines.exe.set", engine = kind.name(), path = found));
}

async fn add_models_dir(kind: Kind, local: &mut LocalManager, engines: &mut Engines, raw: &str, reply: &mut Reply) {
    let dir = prefs::clean_path(raw);
    if dir.is_empty() || !std::path::Path::new(&dir).is_dir() {
        reply.warn(t!("engines.usage.models", cmd = format!("{} {} {}", w("/moteurs", "/engines"), kind.word(), w("modeles", "models"))));
        return;
    }
    let mut prefs = LocalPrefs::load();
    if !prefs.add_engine_models_dir(kind.key(), &dir) {
        reply.dim(t!("engines.models.already", dir = dir));
    } else if !save(&prefs, reply) {
        return;
    }
    detect(local, engines).await;
    let count = match kind {
        Kind::Llama => local.catalog.len(),
        Kind::Strata => engines.report.as_ref().map(|r| r.strata_configs().len()).unwrap_or(0),
    };
    reply.accent(t!("engines.models.found", engine = kind.name(), n = count));
    if kind == Kind::Strata && count == 0 {
        reply.dim(t!("engines.strata.models_hint"));
    }
}

async fn remove_models_dir(kind: Kind, local: &mut LocalManager, engines: &mut Engines, raw: &str, reply: &mut Reply) {
    let mut prefs = LocalPrefs::load();
    let folders = match kind {
        Kind::Llama => prefs.llama_models_dirs(),
        Kind::Strata => prefs.engine(prefs::STRATA).models_dirs,
    };
    let raw = prefs::clean_path(raw);
    let target = raw.parse::<usize>().ok().and_then(|n| folders.get(n.wrapping_sub(1)).cloned()).unwrap_or(raw);
    if target.is_empty() || !prefs.remove_engine_models_dir(kind.key(), &target) {
        reply.warn(t!("engines.models.not_saved", dir = target));
        return;
    }
    if save(&prefs, reply) {
        detect(local, engines).await;
        reply.accent(t!("engines.models.removed", dir = target));
    }
}

/// The model the user designated: its number in the list, or its name.
fn pick<'a, T>(items: &'a [T], raw: &str, name: impl Fn(&T) -> String) -> Option<&'a T> {
    match raw.parse::<usize>() {
        Ok(n) => items.get(n.wrapping_sub(1)),
        Err(_) => {
            let wanted = raw.to_lowercase();
            items.iter().find(|i| name(i).to_lowercase() == wanted).or_else(|| items.iter().find(|i| !wanted.is_empty() && name(i).to_lowercase().contains(&wanted)))
        }
    }
}

/// A trailing "jeu" / "game" asks for game mode; the rest is the model.
fn game_word(words: &[&str]) -> (bool, String) {
    let game = words.last().is_some_and(|w| matches!(*w, "jeu" | "game" | "--game" | "-g"));
    let words = if game { &words[..words.len() - 1] } else { words };
    (game, words.join(" "))
}

async fn start(kind: Kind, agent: &mut Agent, local: &mut LocalManager, engines: &mut Engines, raw: &str, game: bool, reply: &mut Reply) {
    let model = match kind {
        Kind::Llama => pick(&llama_models(local), raw, |(_, e)| e.label()).map(|(i, e)| crate::engines_ops::Model::Llama(*i, e.label())),
        Kind::Strata => engines.report.as_ref().and_then(|r| r.strata_install()).and_then(|inst| {
            let index = pick(&inst.configs, raw, |c| c.model_name.clone()).and_then(|c| inst.configs.iter().position(|x| x.path == c.path))?;
            Some(crate::engines_ops::Model::Strata(index, inst.configs[index].model_name.clone()))
        }),
    };
    match model {
        Some(model) => start_model(&model, agent, local, engines, game, reply).await,
        None if kind == Kind::Strata && engines.report.as_ref().is_some_and(|r| r.strata_install().is_none()) => reply.warn(t!("engines.strata.missing")),
        None => reply.warn(t!("engines.start.which", cmd = format!("{} {} {}", w("/moteurs", "/engines"), kind.word(), w("demarrer", "start")))),
    }
}

/// Starts a model with its profile, in game mode if asked or set, its loading shown meanwhile.
pub(crate) async fn start_model(model: &crate::engines_ops::Model, agent: &mut Agent, local: &mut LocalManager, engines: &mut Engines, game: bool, reply: &mut Reply) {
    let prefs = LocalPrefs::load();
    let game = game || prefs.game_mode;
    let profile = prefs.model(model.name());
    match model {
        crate::engines_ops::Model::Llama(index, label) => {
            if local.supervisor.is_none() {
                reply.warn(t!("chat.localui.server_missing_install"));
                return;
            }
            let progress = crate::engines_ops::follow_llama(label.clone());
            let result = local.activate_with(index + 1, agent, &profile, game).await;
            drop(progress);
            match result {
                Ok(message) => reply.accent(message),
                Err(err) => reply.warn(err),
            }
        }
        crate::engines_ops::Model::Strata(index, _) => start_strata(agent, engines, &(index + 1).to_string(), &profile, game, reply).await,
    }
}

async fn start_strata(agent: &mut Agent, engines: &mut Engines, raw: &str, profile: &po_local::prefs::ModelProfile, game: bool, reply: &mut Reply) {
    refresh_live(engines).await;
    let Some(report) = engines.report.as_ref() else { return };
    let Some(install) = report.strata_install().cloned() else {
        reply.warn(t!("engines.strata.missing"));
        return;
    };
    let Some(config) = pick(&install.configs, raw, |c| c.model_name.clone()).cloned() else {
        reply.warn(t!("engines.start.which", cmd = format!("{} strata {}", w("/moteurs", "/engines"), w("demarrer", "start"))));
        return;
    };
    // Already served: nothing to start, only to make sure the session knows it.
    if let Some(live) = report.strata_live.iter().find(|l| l.model == config.model_name).cloned() {
        register_strata(agent, engines, &live.model, live.port).await;
        reply.accent(t!("engines.strata.already", model = live.model, port = live.port));
        return;
    }
    // Another model served by a Strata Po did not start: never stopped behind the user's back.
    let ours = engines.strata_server.as_ref().map(|s| s.port);
    if let Some(other) = report.strata_live.iter().find(|l| Some(l.port) != ours) {
        reply.warn(t!("engines.strata.outside_busy", model = other.model, port = other.port));
        return;
    }
    if !install.launchable() {
        reply.warn(t!("engines.strata.not_launchable"));
        return;
    }
    // Switching model: the one Po started gives the card back first.
    if let Some(server) = engines.strata_server.take() {
        reply.dim(t!("engines.strata.switching", model = server.config.model_name));
        unregister_strata(agent, engines);
        server.stop().await;
    }
    let port = if po_local::supervisor::port_is_free(config.port) {
        config.port
    } else {
        match po_local::supervisor::free_port() {
            Some(port) => port,
            None => {
                reply.warn(t!("local.supervisor.err.no_free_port", start = 18080, end = 18180));
                return;
            }
        }
    };
    reply.dim(t!("engines.strata.starting", model = config.model_name, port = port));
    let overrides = crate::engines_ops::strata_overrides(profile, game);
    let progress = crate::engines_ops::follow_strata(config.model_name.clone(), port, &config.path);
    let result = strata::start_with(&install, &config, port, strata::READY_TIMEOUT, &overrides).await;
    drop(progress);
    match result {
        Ok(server) => {
            let secs = server.started_at.elapsed().as_secs();
            engines.strata_server = Some(server);
            register_strata(agent, engines, &config.model_name, port).await;
            refresh_live(engines).await;
            reply.accent(t!("engines.strata.started", model = config.model_name, port = port, secs = secs));
        }
        Err(err) => reply.warn(err.to_string()),
    }
}

/// Makes a Strata model reachable by the session (once).
async fn register_strata(agent: &mut Agent, engines: &mut Engines, model: &str, port: u16) {
    let base_url = format!("http://127.0.0.1:{port}/v1");
    let already = agent.router().get(model).is_some_and(|m| matches!(&m.origin, Origin::Local { endpoint, .. } if endpoint.trim_end_matches('/') == base_url));
    if already {
        return;
    }
    // The window the server really serves: announcing more would overflow it.
    let context = async {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(3)).build().ok()?;
        let body = client.get(format!("{base_url}/models")).send().await.ok()?.text().await.ok()?;
        crate::config::parse_served_models(&body).into_iter().find(|m| m.id == model).and_then(|m| m.context)
    }
    .await
    .unwrap_or(32_768);
    let spec = ModelSpec {
        id: model.to_owned(),
        display_name: format!("{model} (strata)"),
        origin: Origin::Local { backend: "strata".into(), endpoint: base_url.clone() },
        tier: crate::config::guess_tier(model),
        capabilities: Capabilities { tools: true, ..Default::default() },
        context_window: context,
        max_output_tokens: crate::config::output_budget(context),
        pricing: Pricing::FREE,
    };
    agent.router_mut().add_model(spec);
    agent.attach(model.to_owned(), Arc::new(OpenAiProvider::local("strata", base_url)));
    let profile = LocalPrefs::load().model(model);
    if !profile.is_empty() {
        agent.settings.model_defaults.insert(model.to_owned(), crate::engines_ops::session_defaults(&profile));
    }
    if engines.strata_server.as_ref().is_some_and(|s| s.port == port) {
        engines.strata_model = Some(model.to_owned());
    }
}

fn unregister_strata(agent: &mut Agent, engines: &mut Engines) {
    if let Some(model) = engines.strata_model.take() {
        agent.router_mut().remove_model(&model);
    }
}

async fn stop(kind: Kind, agent: &mut Agent, local: &mut LocalManager, engines: &mut Engines, reply: &mut Reply) {
    match kind {
        Kind::Llama => {
            if local.supervisor.as_ref().is_some_and(|s| s.running().is_some()) {
                reply.accent(local.deactivate(agent).await);
            } else if llama_state(local, agent) != t!("engines.state.stopped") {
                reply.warn(t!("engines.stop.outside", engine = "llama.cpp"));
            } else {
                reply.dim(t!("chat.localui.no_managed_server"));
            }
        }
        Kind::Strata => match engines.strata_server.take() {
            Some(server) => {
                let model = server.config.model_name.clone();
                unregister_strata(agent, engines);
                server.stop().await;
                refresh_live(engines).await;
                reply.accent(t!("engines.strata.stopped", model = model));
            }
            None => {
                refresh_live(engines).await;
                if engines.report.as_ref().is_some_and(|r| !r.strata_live.is_empty()) {
                    reply.warn(t!("engines.stop.outside", engine = "Strata"));
                    reply.dim(t!("engines.strata.unload_hint", cmd = w("/serveur decharger", "/server unload")));
                } else {
                    reply.dim(t!("engines.strata.none_running"));
                }
            }
        },
    }
}

/// A one-line summary for the settings panel: how many engines found, how many running.
pub fn summary(local: &LocalManager) -> String {
    let Some(report) = local.engines.report.as_ref() else { return t!("engines.panel.unknown").to_owned() };
    let found = usize::from(report.llama.located.is_some()) + usize::from(report.strata.install.is_some());
    let running = usize::from(local.supervisor.as_ref().is_some_and(|s| s.running().is_some())) + usize::from(!report.strata_live.is_empty());
    t!("engines.panel.value", found = found, running = running)
}

// ----------------------------------------------------------------------------------- menus

/// The menu of `/moteurs …`, when the command needs a choice.
pub async fn menu(cmd: &str, words: &[&str], agent: &Agent, local: &mut LocalManager) -> Option<Menu> {
    let mut engines = std::mem::take(&mut local.engines);
    let menu = build_menu(cmd, words, agent, local, &mut engines).await;
    local.engines = engines;
    menu
}

async fn build_menu(cmd: &str, words: &[&str], agent: &Agent, local: &mut LocalManager, engines: &mut Engines) -> Option<Menu> {
    match words {
        [] => {
            ensure(local, engines).await;
            Some(main_menu(cmd, agent, local, engines, None))
        }
        // Detecting again from the menu comes back to it, with the new findings.
        [again] if matches!(*again, "detecter" | "détecter" | "detect") => {
            detect(local, engines).await;
            Some(main_menu(cmd, agent, local, engines, Some(t!("engines.detected"))))
        }
        [worker] if matches!(*worker, "worker" | "second" | "renfort") => {
            ensure(local, engines).await;
            Some(crate::engines_ops::worker_menu(cmd, local, engines))
        }
        [engine] => {
            let kind = Kind::parse(engine)?;
            ensure(local, engines).await;
            Some(engine_menu(cmd, kind, agent, local, engines, None))
        }
        [engine, again] if matches!(*again, "detecter" | "détecter" | "detect") => {
            let kind = Kind::parse(engine)?;
            detect(local, engines).await;
            Some(engine_menu(cmd, kind, agent, local, engines, Some(t!("engines.detected"))))
        }
        [engine, action] if Kind::parse(engine).is_some() && matches!(*action, "demarrer" | "démarrer" | "start") => {
            let kind = Kind::parse(engine)?;
            ensure(local, engines).await;
            let models: Vec<Item> = crate::engines_ops::all_models(local, engines.report.as_ref())
                .into_iter()
                .filter(|m| m.kind() == kind)
                .map(|m| Item::run(m.name(), format!("{cmd} {} {}", w("demarrer", "start"), m.name())))
                .collect();
            (!models.is_empty()).then(|| Menu::new(t!("engines.menu.pick_model"), models))
        }
        [verb, rest @ ..] if matches!(*verb, "demarrer" | "démarrer" | "start" | "info" | "infos" | "modele" | "modèle" | "model") => {
            ensure(local, engines).await;
            let (_, fragment) = game_word(rest);
            let models = crate::engines_ops::all_models(local, engines.report.as_ref());
            let found = crate::engines_ops::find(&models, &fragment);
            match found.as_slice() {
                // Several match: which one, with the arrows.
                [_, _, ..] => Some(Menu::new(
                    t!("engines.find.several", fragment = fragment),
                    found.iter().map(|m| Item::run(m.name(), format!("{cmd} {verb} {}{}", m.name(), if rest.last().is_some_and(|w| matches!(*w, "jeu" | "game")) { format!(" {}", w("jeu", "game")) } else { String::new() })).detail(m.kind().name())).collect(),
                )),
                [one] if matches!(*verb, "modele" | "modèle" | "model") => {
                    let detail = model_detail(one, local, engines);
                    Some(crate::engines_ops::model_menu(cmd, one, detail))
                }
                _ => None,
            }
        }
        [verb, model, key] if matches!(*verb, "reglage" | "réglage" | "setting") && !matches!(*key, "defaut" | "défaut" | "default") => {
            ensure(local, engines).await;
            let models = crate::engines_ops::all_models(local, engines.report.as_ref());
            let found = crate::engines_ops::find(&models, model);
            crate::engines_ops::setting_menu(cmd, found.first()?, key)
        }
        [engine, action] if matches!(*action, "retirer" | "remove") => {
            let kind = Kind::parse(engine)?;
            let prefs = LocalPrefs::load();
            let folders = match kind {
                Kind::Llama => prefs.llama_models_dirs(),
                Kind::Strata => prefs.engine(prefs::STRATA).models_dirs,
            };
            (!folders.is_empty()).then(|| {
                Menu::new(
                    t!("engines.menu.remove_folder"),
                    folders.iter().enumerate().map(|(i, f)| Item::run(f, format!("{cmd} {} {} {}", kind.word(), w("retirer", "remove"), i + 1))).collect(),
                )
            })
        }
        _ => None,
    }
}

/// A model's detail line, whichever its engine.
fn model_detail(model: &crate::engines_ops::Model, local: &LocalManager, engines: &Engines) -> String {
    match model {
        crate::engines_ops::Model::Llama(i, _) => local.catalog.get(*i).map(llama_model_detail).unwrap_or_default(),
        crate::engines_ops::Model::Strata(i, _) => engines
            .report
            .as_ref()
            .and_then(|r| Some((r, r.strata_install()?.configs.get(*i)?)))
            .map(|(r, c)| strata_model_detail(c, r))
            .unwrap_or_default(),
    }
}

fn main_menu(cmd: &str, agent: &Agent, local: &LocalManager, engines: &Engines, note: Option<&str>) -> Menu {
    let Some(report) = engines.report.as_ref() else { return Menu::new(t!("engines.title"), Vec::new()) };
    let mut items = Vec::new();
    for kind in Kind::ALL {
        let found = match kind {
            Kind::Llama => report.llama.located.is_some(),
            Kind::Strata => report.strata.install.is_some() || !report.strata_live.is_empty(),
        };
        let state = match kind {
            Kind::Llama => llama_state(local, agent),
            Kind::Strata => strata_state(report, engines),
        };
        let running = state != t!("engines.state.stopped");
        let by_po = match kind {
            Kind::Llama => local.supervisor.as_ref().is_some_and(|s| s.running().is_some()),
            Kind::Strata => engines.strata_server.is_some(),
        };
        // Short, to leave room for the location: the full state is in the engine's own menu.
        let short = match (running, by_po) {
            (false, _) => t!("engines.state.stopped"),
            (true, true) => t!("engines.short.po"),
            (true, false) => t!("engines.short.outside"),
        };
        let (label, detail) = if found {
            (format!("{} — {short}", kind.name()), engine_where(kind, report))
        } else {
            (format!("{} — {}", kind.name(), t!("engines.not_found")), t!("engines.short.missing_hint").to_owned())
        };
        items.push(Item::run(label, format!("{cmd} {}", kind.word())).detail(detail).current(running && kind == Kind::Strata));
    }
    if crate::engines_ops::worker_gpu(&local.hardware, &report.prefs).is_some() {
        items.push(Item::run(t!("engines.worker.title"), format!("{cmd} worker")).detail(crate::engines_ops::worker_state(engines)));
    }
    let game = report.prefs.game_mode;
    items.push(
        Item::run(t!("engines.game.label"), format!("{cmd} {} {}", w("jeu", "game"), if game { "off" } else { "on" }))
            .detail(if game { t!("engines.game.on") } else { t!("engines.game.off") }),
    );
    items.push(Item::show(t!("engines.menu.advice"), format!("{cmd} {}", w("conseils", "advice"))).detail(t!("engines.menu.advice_hint")));
    items.push(Item::show(t!("engines.menu.all"), format!("{cmd} {}", w("etat", "status"))));
    items.push(Item::run(t!("engines.menu.detect"), format!("{cmd} {}", w("detecter", "detect"))));
    let base = hardware_line(&report.hardware);
    let note = match note {
        Some(n) => format!("{n} · {base}"),
        None => base,
    };
    Menu::new(t!("engines.title"), items).note(note)
}

fn engine_menu(cmd: &str, kind: Kind, agent: &Agent, local: &LocalManager, engines: &Engines, note: Option<&str>) -> Menu {
    let Some(report) = engines.report.as_ref() else { return Menu::new(kind.name(), Vec::new()) };
    let word = kind.word();
    let mut items = Vec::new();
    let found;
    match kind {
        Kind::Llama => {
            found = report.llama.located.is_some();
            for (_, entry) in llama_models(local).iter() {
                let active = local.active_model_id.as_deref() == Some(entry.label().as_str());
                items.push(Item::run(entry.label(), format!("{cmd} {} {}", w("modele", "model"), entry.label())).detail(with_profile(llama_model_detail(entry), &report.prefs, &entry.label())).current(active));
            }
            if local.supervisor.as_ref().is_some_and(|s| s.running().is_some()) {
                items.push(Item::run(t!("engines.menu.stop"), format!("{cmd} {word} {}", w("arreter", "stop"))));
            }
        }
        Kind::Strata => {
            found = report.strata.install.is_some();
            for config in report.strata_configs() {
                let serving = report.strata_live.iter().any(|l| l.model == config.model_name);
                items.push(Item::run(&config.model_name, format!("{cmd} {} {}", w("modele", "model"), config.model_name)).detail(with_profile(strata_model_detail(config, report), &report.prefs, &config.model_name)).current(serving));
            }
            if engines.strata_server.is_some() {
                items.push(Item::run(t!("engines.menu.stop"), format!("{cmd} {word} {}", w("arreter", "stop"))));
            }
            if let Some(live) = report.strata_live.first() {
                if live.loaded {
                    items.push(Item::run(t!("engines.menu.unload"), w("/serveur decharger", "/server unload")).detail(t!("engines.menu.unload_hint")));
                } else {
                    items.push(Item::run(t!("engines.menu.reload"), w("/serveur charger", "/server load")));
                }
            }
        }
    }
    items.push(Item::show(t!("engines.menu.advice"), format!("{cmd} {word} {}", w("conseils", "advice"))).detail(t!("engines.menu.advice_hint")));
    if !found {
        items.push(Item::show(t!("engines.menu.install"), format!("{cmd} {word} {}", w("installer", "install"))));
    }
    items.push(Item::fill(t!("engines.menu.folder"), format!("{cmd} {word} {} ", w("dossier", "folder"))).detail(t!("engines.menu.folder_hint")));
    items.push(Item::fill(t!("engines.menu.exe"), format!("{cmd} {word} executable ")).detail(match kind {
        Kind::Llama => t!("engines.exe.llama"),
        Kind::Strata => t!("engines.exe.strata"),
    }));
    items.push(Item::fill(t!("engines.menu.models"), format!("{cmd} {word} {} ", w("modeles", "models"))).detail(match kind {
        Kind::Llama => t!("engines.menu.models_hint_llama"),
        Kind::Strata => t!("engines.menu.models_hint_strata"),
    }));
    let folders = match kind {
        Kind::Llama => report.prefs.llama_models_dirs(),
        Kind::Strata => report.prefs.engine(prefs::STRATA).models_dirs,
    };
    if !folders.is_empty() {
        items.push(Item::run(t!("engines.menu.remove_folder"), format!("{cmd} {word} {}", w("retirer", "remove"))).detail(t!("menu.count", n = folders.len())));
    }
    if report.has_choice(kind) {
        items.push(Item::run(t!("engines.menu.auto"), format!("{cmd} {word} auto")).detail(t!("engines.menu.auto_hint")));
    }
    let running = match kind {
        Kind::Llama => local.supervisor.as_ref().is_some_and(|s| s.running().is_some()),
        Kind::Strata => engines.strata_server.is_some() || !report.strata_live.is_empty(),
    };
    if running {
        items.push(Item::show(t!("engines.menu.log"), format!("{cmd} {word} {}", w("journal", "log"))));
    }
    items.push(Item::run(t!("engines.menu.detect"), format!("{cmd} {word} {}", w("detecter", "detect"))));
    items.push(Item::show(t!("engines.menu.details"), format!("{cmd} {word} {}", w("etat", "status"))));

    let state = match kind {
        Kind::Llama => llama_state(local, agent),
        Kind::Strata => strata_state(report, engines),
    };
    let mut lines = Vec::new();
    if let Some(n) = note {
        lines.push(n.to_owned());
    }
    let mut warnings = Reply::default();
    choice_warnings(kind, report, &mut warnings);
    lines.extend(warnings.lines.into_iter().map(|(_, text)| text));
    lines.push(format!("{} · {state}", engine_where(kind, report)));
    let model_word = format!(" {} ", w("modele", "model"));
    if found && items.iter().all(|i| !matches!(&i.action, po_tui::menu::Action::Run(c) if c.contains(&model_word))) {
        lines.push(match kind {
            Kind::Llama => t!("engines.llama.no_models").to_owned(),
            Kind::Strata => t!("engines.strata.no_configs").to_owned(),
        });
    }
    Menu::new(kind.name(), items).note(lines.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_local::hardware::{Gpu, GpuBackend};

    fn report_with(llama_found: Option<std::path::PathBuf>, strata_install: Option<strata::Install>, live: Vec<strata::Live>) -> Report {
        let hardware = Hardware {
            gpus: vec![
                Gpu { name: "NVIDIA GeForce RTX 3070 Ti".into(), backend: GpuBackend::Cuda, vram_total_mib: 8192, vram_free_mib: 5777, index: Some(0) },
                Gpu { name: "NVIDIA GeForce RTX 4070 SUPER".into(), backend: GpuBackend::Cuda, vram_total_mib: 12_282, vram_free_mib: 471, index: Some(1) },
            ],
            ram_total_mib: 65_430,
            ram_free_mib: 6_000,
            cpu_threads: 24,
        };
        Report {
            prefs: LocalPrefs::default(),
            capacity: Capacity::of(&hardware),
            hardware,
            llama: llama::Detection { located: llama_found.map(|path| llama::Located { path, origin: llama::Origin::NextToModels }), ..Default::default() },
            llama_version: Some("0.5.0-dev (build 11342, commit f1cee9941)".into()),
            strata: strata::Detection { install: strata_install, ..Default::default() },
            strata_live: live,
        }
    }

    fn strata_install() -> strata::Install {
        strata::Install {
            dir: "E:\\LLM\\Strata".into(),
            origin: strata::Origin::StrataSettings,
            python: Some("E:\\LLM\\Strata\\.venv\\Scripts\\python.exe".into()),
            server: Some("E:\\LLM\\Strata\\serve\\server.py".into()),
            version: Some("0.1.47".into()),
            configs: vec![strata::RunConfig {
                path: "E:\\LLM\\Strata\\strata-iq3_xxs.json".into(),
                tag: "iq3_xxs".into(),
                model_name: "qwen3.8-flash-next-iq3_xxs".into(),
                port: 8080,
                model_files: vec!["E:\\LLM\\Strata-data\\models\\IQ3_XXS\\Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf".into()],
                missing: vec![],
                shard1_bytes: 47_040_000_000,
                size_bytes: 75_840_000_000,
                quant: Some("IQ3_XXS".into()),
                gpus: vec![1],
                engine_exe: None,
            }],
            data_dir: None,
        }
    }

    fn live() -> strata::Live {
        strata::Live { port: 8080, model: "qwen3.8-flash-next-iq3_xxs".into(), loaded: true, engine: Some("0.1.47".into()), in_flight: 0 }
    }

    #[test]
    fn engine_words_are_read_in_any_spelling() {
        assert_eq!(Kind::parse("llama.cpp"), Some(Kind::Llama));
        assert_eq!(Kind::parse("LLAMA"), Some(Kind::Llama));
        assert_eq!(Kind::parse("Strata"), Some(Kind::Strata));
        assert_eq!(Kind::parse("ollama"), None);
    }

    #[test]
    fn a_strata_started_outside_po_is_said_so() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let report = report_with(None, Some(strata_install()), vec![live()]);
        let state = strata_state(&report, &Engines::default());
        // Other tests change the language at the same time: the checks hold in both.
        assert!(state.contains("Po") && state.contains("8080"), "{state}");
        assert!(state.contains("qwen3.8-flash-next-iq3_xxs"));
        let detail = strata_model_detail(&report.strata_configs()[0], &report);
        assert!(detail.contains("IQ3_XXS") && (detail.contains("en service") || detail.contains("serving")), "{detail}");
    }

    #[test]
    fn the_overview_names_both_engines_and_the_hardware() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let report = report_with(Some("E:\\LLM\\llama-bin\\llama-server.exe".into()), Some(strata_install()), vec![live()]);
        let line = hardware_line(&report.hardware);
        assert!(line.contains("RTX 3070 Ti") && line.contains("RTX 4070 SUPER") && line.contains("64"), "{line}");
        let llama = engine_where(Kind::Llama, &report);
        assert!(llama.contains("llama-server.exe") && llama.contains("build 11342"), "{llama}");
        let strata = engine_where(Kind::Strata, &report);
        assert!(strata.contains("E:\\LLM\\Strata") && strata.contains("0.1.47"), "{strata}");
        assert_eq!(engine_where(Kind::Llama, &report_with(None, None, vec![])), t!("engines.not_found"));
    }

    #[test]
    fn a_wrong_user_path_is_reported() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let mut report = report_with(None, None, vec![]);
        report.llama.bad_executable = Some("C:\\gone\\llama-server.exe".into());
        let mut reply = Reply::default();
        choice_warnings(Kind::Llama, &report, &mut reply);
        assert_eq!(reply.lines.len(), 1);
        assert!(reply.lines[0].1.contains("C:\\gone"));
    }

    #[test]
    fn install_help_never_offers_to_download_strata() {
        for lang in [po_i18n::Lang::Fr, po_i18n::Lang::En] {
            po_i18n::set_lang(lang);
            let report = report_with(None, None, vec![]);
            let mut reply = Reply::default();
            install_lines(Kind::Strata, &report, &mut reply);
            let text: String = reply.lines.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n");
            assert!(!text.contains("http"), "no link to download Strata: {text}");
            let mut reply = Reply::default();
            install_lines(Kind::Llama, &report, &mut reply);
            let text: String = reply.lines.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n");
            assert!(text.contains("github.com/ggml-org/llama.cpp/releases"), "{text}");
        }
    }

    #[test]
    fn a_model_is_picked_by_number_or_name() {
        let names = vec!["qwen3.8-flash-next-iq3_xxs".to_owned(), "qwen3.8-flash-next-iq2_xs".to_owned()];
        assert_eq!(pick(&names, "2", |s| s.clone()), Some(&names[1]));
        assert_eq!(pick(&names, "iq3_xxs", |s| s.clone()), Some(&names[0]));
        assert_eq!(pick(&names, "QWEN3.8-FLASH-NEXT-IQ2_XS", |s| s.clone()), Some(&names[1]));
        assert_eq!(pick(&names, "0", |s| s.clone()), None);
        assert_eq!(pick(&names, "9", |s| s.clone()), None);
        assert_eq!(pick(&names, "", |s| s.clone()), None);
    }

    #[test]
    fn the_executable_given_must_be_the_engine_s() {
        let dir = tempfile::tempdir().unwrap();
        let llama = dir.path().join(llama::exe_name());
        std::fs::write(&llama, "").unwrap();
        let other = dir.path().join("notepad.exe");
        std::fs::write(&other, "").unwrap();
        assert!(executable_ok(Kind::Llama, &llama));
        assert!(executable_ok(Kind::Llama, dir.path()), "a folder that holds it");
        assert!(!executable_ok(Kind::Llama, &other));
        assert!(!executable_ok(Kind::Strata, &other), "not a Strata install");
        assert!(!executable_ok(Kind::Llama, &dir.path().join("absent")));
    }

    fn test_agent() -> Agent {
        let dir = std::env::temp_dir();
        let router = po_router::Router::new(Vec::new(), po_router::BudgetMode::Balanced);
        let ctx = po_tools::ToolContext::new(po_tools::Workspace::new(&dir));
        Agent::new(router, po_tools::Registry::with_builtins(), ctx, "system", Arc::new(po_agent::ReadOnly))
    }

    fn runs(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .map(|i| match &i.action {
                po_tui::menu::Action::Run(c) | po_tui::menu::Action::Show(c) | po_tui::menu::Action::Fill(c) => c.clone(),
            })
            .collect()
    }

    #[test]
    fn the_main_menu_offers_both_engines_and_the_advice() {
        let agent = test_agent();
        let mut local = LocalManager::empty();
        let engines = Engines { report: Some(report_with(Some("E:\\LLM\\llama-bin\\llama-server.exe".into()), Some(strata_install()), vec![live()])), ..Default::default() };
        let menu = main_menu("/moteurs", &agent, &local, &engines, None);
        let commands = runs(&menu);
        assert_eq!(commands[0], "/moteurs strata", "Strata first: it is the one running");
        assert_eq!(commands[1], "/moteurs llama");
        assert_eq!(menu.items.len(), 6, "two engines, game mode, advice, everything, detect again");
        assert!(commands.iter().any(|c| c.ends_with(" on") && (c.contains(" jeu ") || c.contains(" game "))), "game mode is off: the item turns it on");
        // Two cards: the second one is offered for a helper model.
        local.hardware = engines.report.as_ref().unwrap().hardware.clone();
        let menu = main_menu("/moteurs", &agent, &local, &engines, None);
        assert!(runs(&menu).contains(&"/moteurs worker".to_owned()));
        assert!(menu.items[0].current, "the menu opens on the engine running");
        assert!(menu.note.as_deref().unwrap().contains("4070 SUPER"));
    }

    #[test]
    fn strata_s_menu_lists_its_models_and_never_offers_to_stop_a_server_po_did_not_start() {
        let agent = test_agent();
        let local = LocalManager::empty();
        let engines = Engines { report: Some(report_with(None, Some(strata_install()), vec![live()])), ..Default::default() };
        let menu = engine_menu("/moteurs", Kind::Strata, &agent, &local, &engines, None);
        let commands = runs(&menu);
        assert!(commands[0].starts_with("/moteurs mod") && commands[0].ends_with(" qwen3.8-flash-next-iq3_xxs"), "a model opens its own menu: {commands:?}");
        assert!(menu.items[0].current, "the model being served is marked");
        assert!(!commands.iter().any(|c| c.ends_with(" arreter") || c.ends_with(" stop")), "{commands:?}");
        assert!(!commands.iter().any(|c| c.contains(" installer") || c.contains(" install")), "found: nothing to install");
        assert!(commands.iter().any(|c| c.contains("decharger") || c.contains("unload")), "freeing the card stays possible");
    }

    #[test]
    fn a_missing_llama_cpp_offers_where_to_get_it_and_its_folders() {
        let agent = test_agent();
        let local = LocalManager::empty();
        let engines = Engines { report: Some(report_with(None, None, vec![])), ..Default::default() };
        let menu = engine_menu("/engines", Kind::Llama, &agent, &local, &engines, None);
        let commands = runs(&menu);
        assert!(commands.iter().any(|c| c.ends_with(" installer") || c.ends_with(" install")), "{commands:?}");
        assert!(commands.iter().any(|c| c.contains(" executable ")), "the exact path can be given");
        assert!(menu.items.iter().any(|i| matches!(&i.action, po_tui::menu::Action::Fill(c) if c.ends_with(' '))), "free text ends in a space");
        assert!(!commands.iter().any(|c| c.ends_with(" auto")), "nothing to reset: no choice was made");
    }

    #[tokio::test]
    async fn a_strata_model_is_registered_once_and_removed_with_its_server() {
        let mut agent = test_agent();
        let mut engines = Engines::default();
        // Nothing answers on this port: the window falls back to the cautious value.
        register_strata(&mut agent, &mut engines, "fake-flash", 1).await;
        register_strata(&mut agent, &mut engines, "fake-flash", 1).await;
        let models: Vec<_> = agent.router().models().iter().filter(|m| m.id == "fake-flash").collect();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].context_window, 32_768);
        assert!(engines.strata_model.is_none(), "a server Po did not start is not Po's to remove");
        unregister_strata(&mut agent, &mut engines);
        assert!(agent.router().get("fake-flash").is_some());
    }
}
