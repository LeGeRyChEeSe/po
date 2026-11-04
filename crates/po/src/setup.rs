//! The local models, set up without friction (`/configurer`, `/setup`).
//!
//! At the start of a session, Po looks for the engines and their models in the background. The
//! first time - or when no model answers - it opens this screen by itself: the search under way
//! (cancellable at any moment), then what it found, the model it recommends for this machine
//! among those already on disk, and the one worth downloading if a better one fits. The model
//! chosen becomes the default of every session to come, and is started when a message needs it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use po_agent::Agent;
use po_i18n::t;
use po_local::Hardware;
use po_local::placement::{self, Placement};
use po_local::prefs::LocalPrefs;
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::engines_ops::Model;
use crate::engines_ui::{Engines, Scanned, w};
use crate::local_ui::LocalManager;

/// Where the background search stands.
enum Stage {
    Idle,
    Running { since: Instant, cancel: Arc<AtomicBool> },
    /// Found, waiting to be put in the session (the next look at the menu does it).
    Ready(Box<Scanned>),
    Cancelled,
}

static STAGE: Mutex<Stage> = Mutex::new(Stage::Idle);

fn stage() -> std::sync::MutexGuard<'static, Stage> {
    STAGE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Starts the search in the background, unless one runs already.
pub fn start(ours: Option<u16>) {
    let cancel = Arc::new(AtomicBool::new(false));
    po_tui::clear_load_cancel();
    {
        let mut stage = stage();
        if matches!(*stage, Stage::Running { .. }) {
            return;
        }
        *stage = Stage::Running { since: Instant::now(), cancel: cancel.clone() };
    }
    let prefs = LocalPrefs::load();
    if let Some(ref def) = prefs.default_model {
        let mirror = po_tui::control::mirror();
        let since_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let stage_name = po_i18n::t!("setup.starting_now", model = def.clone());
        mirror.loading(Some(po_wire::Loading {
            model: def.clone(),
            stage: stage_name,
            since_ms,
        }));
    }
    // Without a runtime (a test), nothing runs in the background.
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        *stage() = Stage::Idle;
        return;
    };
    runtime.spawn(async move {
        let found = crate::engines_ui::scan(ours).await;
        let found = match found {
            Some(scanned) => Some(try_auto_start(scanned, cancel.clone()).await),
            None => None,
        };
        let mut stage = stage();
        // Cancelled meanwhile: what it found is dropped.
        if cancel.load(Ordering::Relaxed) || !matches!(*stage, Stage::Running { .. }) {
            drop(stage);
            clear_loading();
            return;
        }
        // Nothing was actually started: the indicator must not stay lit (a stale "loading model"
        // would sit on the terminal and on every phone until the user presses Escape).
        if found.as_ref().and_then(|s| s.auto_started.as_ref()).is_none() {
            clear_loading();
        }
        *stage = match found {
            Some(found) => Stage::Ready(Box::new(found)),
            None => Stage::Cancelled,
        };
    });
}

pub fn cancel() {
    // The shared flag the launches watch, so a load in progress ends at its next checkpoint and
    // stops its own server rather than being dropped mid-launch.
    po_tui::request_load_cancel();
    let mut stage = stage();
    if let Stage::Running { cancel, .. } = &*stage {
        cancel.store(true, Ordering::Relaxed);
        *stage = Stage::Cancelled;
    }
    drop(stage);
    clear_loading();
}

/// The indicator says "a model is loading": with nothing loading, it must be off.
fn clear_loading() {
    po_tui::control::mirror().loading(None);
    po_tui::set_progress(None);
}

fn running() -> Option<Duration> {
    match &*stage() {
        Stage::Running { since, .. } => Some(since.elapsed()),
        _ => None,
    }
}

pub fn is_running() -> bool {
    running().is_some()
}

/// Puts a finished search in the session.
pub fn absorb(local: &mut LocalManager) {
    let ready = {
        let mut stage = stage();
        match std::mem::replace(&mut *stage, Stage::Idle) {
            Stage::Ready(found) => Some(found),
            other => {
                *stage = other;
                None
            }
        }
    };
    if let Some(found) = ready {
        let mut engines = std::mem::take(&mut local.engines);
        crate::engines_ui::apply(local, &mut engines, *found);
        local.engines = engines;
    }
}

/// Tells the phones following the session that this screen opened by itself and waits for a
/// choice (`true`), or that it no longer does: they open it too, once.
pub fn offer(on: bool) {
    po_tui::control::mirror().offer(on.then(|| w("/configurer", "/setup").to_owned()));
}

/// Should the screen open by itself at the start of a session? The first time, and whenever no
/// model answers.
pub fn wanted_at_start(no_model: bool) -> bool {
    let prefs = LocalPrefs::load();
    if prefs.default_model.is_some() {
        return false;
    }
    no_model || !prefs.setup_seen
}

fn mark_seen() {
    let mut prefs = LocalPrefs::load();
    if !prefs.setup_seen {
        prefs.setup_seen = true;
        let _ = prefs.save();
    }
}

/// A found model's file name, to match it with a recommendation.
fn file_of(model: &Model, local: &LocalManager, engines: &Engines) -> Option<String> {
    let path = match model {
        Model::Llama(index, _) => local.catalog.get(*index).map(|e| e.path.clone()),
        Model::Strata(index, _) => engines.report.as_ref()?.strata.install.as_ref()?.configs.get(*index)?.model_files.first().cloned(),
    }?;
    Some(path.file_name()?.to_string_lossy().into_owned())
}

fn size_of(model: &Model, local: &LocalManager, engines: &Engines) -> Option<f64> {
    let bytes = match model {
        Model::Llama(index, _) => std::fs::metadata(&local.catalog.get(*index)?.path).ok()?.len(),
        Model::Strata(index, _) => engines.report.as_ref()?.strata.install.as_ref()?.configs.get(*index)?.shard1_bytes,
    };
    Some(bytes as f64 / 1e9)
}

/// The model the recommendations favour among those on disk, by name.
fn recommended(local: &LocalManager, engines: &Engines) -> Option<String> {
    let report = engines.report.as_ref()?;
    let advice = crate::engines_ui::advice_for(local, report, None);
    let models = crate::engines_ops::all_models(local, Some(report));
    for item in advice.iter().filter(|a| a.fit.usable() && a.chosen_installed) {
        if let Some(model) = models.iter().find(|m| file_of(m, local, engines).is_some_and(|f| item.installed.contains(&f))) {
            return Some(model.name().to_owned());
        }
    }
    None
}

pub fn menu(cmd: &str, words: &[&str], local: &mut LocalManager) -> Option<Menu> {
    absorb(local);
    if let [first, name @ ..] = words
        && matches!(*first, "cartes" | "cards")
        && !name.is_empty()
    {
        return cards_menu(cmd, &name.join(" "), local);
    }
    if !words.is_empty() {
        return None;
    }
    let detect = format!("{cmd} {}", w("detecter", "detect"));
    if let Some(elapsed) = running() {
        let items = vec![Item::run(t!("setup.cancel"), format!("{cmd} {}", w("annuler", "cancel"))).then(cmd.to_owned())];
        return Some(Menu::new(t!("setup.title"), items).note(format!("~ {}", t!("setup.searching", secs = elapsed.as_secs()))).live().busy(true));
    }
    let engines = &local.engines;
    let Some(report) = engines.report.as_ref() else {
        let cancelled = matches!(*stage(), Stage::Cancelled);
        let items = vec![Item::run(t!("setup.search"), detect).then(cmd.to_owned()), Item::close(t!("rc.close"))];
        return Some(Menu::new(t!("setup.title"), items).note(if cancelled { t!("setup.cancelled") } else { t!("setup.intro") }));
    };
    let prefs = LocalPrefs::load();
    let models = crate::engines_ops::all_models(local, Some(report));
    let best = recommended(local, engines);
    let use_word = w("utiliser", "use");
    let cards_word = w("cartes", "cards");
    let hardware = report.hardware.clone();
    let mut items = Vec::new();
    let default_name = prefs.default_model.as_deref();
    // The default model first at the very top, then recommended, then the others as found.
    let mut ordered: Vec<&Model> = Vec::new();
    if let Some(def) = default_name {
        ordered.extend(models.iter().filter(|m| m.name() == def));
    }
    ordered.extend(models.iter().filter(|m| Some(m.name()) == best.as_deref() && Some(m.name()) != default_name));
    ordered.extend(models.iter().filter(|m| Some(m.name()) != default_name && Some(m.name()) != best.as_deref()));
    for model in ordered {
        let engine = match model.kind() {
            crate::engines_ui::Kind::Llama => "llama.cpp",
            crate::engines_ui::Kind::Strata => "Strata",
        };
        let mut detail = engine.to_owned();
        if let Some(gb) = size_of(model, local, engines) {
            detail.push_str(&format!(" · {gb:.1} GB"));
        }
        let serving = report.strata_live.iter().any(|l| l.model == model.name()) || local.active_model_id.as_deref() == Some(model.name());
        if serving {
            detail = format!("{} · {detail}", t!("setup.serving"));
        }
        let star = Some(model.name()) == best.as_deref();
        let chosen = default_name == Some(model.name());
        if chosen {
            detail = format!("{} · {detail}", t!("setup.default_tag"));
        } else if star {
            detail = format!("{} · {detail}", t!("setup.recommended"));
        }
        let label = if chosen {
            format!("★ {} ({})", model.name(), t!("setup.default_tag"))
        } else if star {
            format!("★ {}", model.name())
        } else {
            model.name().to_owned()
        };
        // Several ways to place it on the cards: the next screen asks where, before it starts.
        let item = if placement_options(model, local, &hardware).len() > 1 {
            if let Some(place) = prefs.models.get(model.name()).and_then(|p| p.placement.as_ref()) {
                detail.push_str(&format!(" · {}", place.describe(&hardware)));
            }
            Item::run(label, format!("{cmd} {cards_word} {}", model.name())).detail(detail).current(chosen)
        } else {
            Item::run(label, format!("{cmd} {use_word} {}", model.name())).detail(detail).current(chosen).then(cmd.to_owned())
        };
        items.push(item);
    }
    // Worth downloading: the best the machine can run, when it is not on disk.
    let advice = crate::engines_ui::advice_for(local, report, None);
    if let Some(item) = advice.iter().find(|a| a.fit.usable() && !a.chosen_installed) {
        items.push(
            Item::show(format!("⬇ {} · {}", item.card.name, item.quant.name), format!("{} {}", w("/moteurs", "/engines"), w("conseils", "advice")))
                .detail(t!("setup.download", fit = item.fit.label())),
        );
    }
    items.push(Item::run(t!("setup.search_again"), detect).then(cmd.to_owned()));
    items.push(Item::close(t!("rc.close")));
    let found = models.len();
    let engines_found = usize::from(report.llama.located.is_some()) + usize::from(report.strata.install.is_some());
    let mut note = t!("setup.found", engines = engines_found, models = found);
    note.push('\n');
    note.push_str(&crate::engines_ui::hardware_line(&report.hardware));
    if found == 0 {
        note.push('\n');
        note.push_str(t!("setup.none"));
    } else if prefs.default_model.is_none() {
        note.push('\n');
        note.push_str(t!("setup.choose"));
    }
    mark_seen();
    Some(Menu::new(t!("setup.title"), items).note(note).at(0))
}

/// The ways `model` can be placed on this machine's cards.
fn placement_options(model: &Model, local: &LocalManager, hardware: &Hardware) -> Vec<Placement> {
    let (engine, draft) = match model {
        Model::Llama(index, _) => (placement::Engine::Llama, local.catalog.get(*index).is_some_and(|e| e.info.draft_layers > 0)),
        Model::Strata(index, _) => (placement::Engine::Strata, strata_config(local, *index).is_some_and(|c| c.mtp)),
    };
    placement::choices(hardware, engine, draft)
}

fn strata_config(local: &LocalManager, index: usize) -> Option<&po_local::strata::RunConfig> {
    local.engines.report.as_ref()?.strata.install.as_ref()?.configs.get(index)
}

/// How a placement would go, in a few words: for llama.cpp the plan it gives (on the card, how
/// much of the model, the window), for Strata the cards' free memory.
fn placement_detail(model: &Model, local: &LocalManager, hardware: &Hardware, placement: &Placement) -> String {
    match model {
        Model::Llama(index, _) => {
            let Some(entry) = local.catalog.get(*index) else { return String::new() };
            let profile = LocalPrefs::load().model(model.name());
            let request = po_local::PlanRequest { desired_context: profile.context.unwrap_or(po_local::PlanRequest::default().desired_context), ..Default::default() };
            let plan = po_local::plan::plan_placed(&entry.info, hardware, &request, placement);
            if !plan.viable {
                t!("setup.cards_no_fit").to_owned()
            } else if !plan.is_fully_offloaded() {
                format!("{} · {}", plan.summary(), t!("setup.cards_partly"))
            } else {
                plan.summary()
            }
        }
        Model::Strata(index, _) => {
            if placement.is_auto() {
                let gpus = strata_config(local, *index).map(|c| c.gpus.clone()).unwrap_or_default();
                return if gpus.is_empty() { String::new() } else { t!("setup.cards_strata_own", place = Placement { gpus, ..Default::default() }.describe(hardware)) };
            }
            let free: u64 = placement.gpus.iter().chain(placement.experts.iter()).filter_map(|c| hardware.gpus.iter().find(|g| g.index == Some(*c))).map(|g| g.vram_free_mib).sum();
            t!("setup.cards_free", gb = format!("{:.1}", free as f64 / 1024.0))
        }
    }
}

/// One line per card: its number, name and free memory.
fn card_lines(hardware: &Hardware) -> Vec<String> {
    hardware
        .gpus
        .iter()
        .filter_map(|gpu| {
            let index = gpu.index?;
            Some(t!("setup.cards_card", index = index, name = placement::short_name(&gpu.name), free = format!("{:.1}", gpu.vram_free_mib as f64 / 1024.0), total = format!("{:.0}", gpu.vram_total_mib as f64 / 1024.0)))
        })
        .collect()
}

/// The screen that asks where `name` runs, before it starts: automatic, each card, the cards
/// together, the draft head elsewhere. The choice is kept for the model.
fn cards_menu(cmd: &str, name: &str, local: &LocalManager) -> Option<Menu> {
    let report = local.engines.report.as_ref()?;
    let models = crate::engines_ops::all_models(local, Some(report));
    let model = crate::engines_ops::find(&models, name).into_iter().next()?;
    // Measured now: the free memory is what decides.
    let hardware = Hardware::detect();
    let current = LocalPrefs::load().model(model.name()).placement.unwrap_or_default();
    let place_word = w("placer", "place");
    let mut items = vec![Item::run(t!("setup.cards_auto"), format!("{cmd} {place_word} auto {}", model.name())).detail(placement_detail(&model, local, &hardware, &Placement::default())).current(current.is_auto())];
    let options = placement_options(&model, local, &hardware);
    for option in &options {
        let mut detail = placement_detail(&model, local, &hardware, option);
        // Measured: the head on another card or the processor halves the words per second.
        if matches!(option.draft, Some(placement::Draft::Cpu)) || matches!(option.draft, Some(placement::Draft::Card(c)) if !option.gpus.contains(&c)) {
            detail.push_str(&format!(" · {}", t!("setup.cards_slower")));
        } else if matches!(model, Model::Strata(..)) && (option.gpus.len() > 1 || option.experts.is_some()) {
            detail.push_str(&format!(" · {}", t!("setup.cards_strata_split")));
        }
        items.push(Item::run(option.describe(&hardware), format!("{cmd} {place_word} {option} {}", model.name())).detail(detail).current(*option == current));
    }
    let at = items.iter().position(|i| i.current).unwrap_or(0);
    items.push(Item::close(t!("rc.close")));
    let mut note = model.name().to_owned();
    for line in card_lines(&hardware) {
        note.push('\n');
        note.push_str(&line);
    }
    if options.iter().any(|o| o.draft.is_some()) {
        note.push('\n');
        note.push_str(t!("setup.cards_mtp"));
    }
    if options.iter().any(|o| o.gpus.len() > 1) {
        note.push('\n');
        note.push_str(t!("setup.cards_lead"));
    }
    // Strata: the settings measured best and the chat template, at hand in the profile.
    if let Model::Strata(index, _) = &model {
        let profile = LocalPrefs::load().model(model.name());
        let preset_template = profile.preset.as_ref().and_then(|n| po_local::strata::presets().into_iter().find(|(m, _)| m == n)).and_then(|(_, p)| p.template);
        let template = match (&profile.template.clone().or(preset_template), strata_config(local, *index)) {
            (Some(choice), Some(config)) => po_local::strata::template_file(choice, config.path.parent().unwrap_or(std::path::Path::new("."))).map(|f| po_local::strata::template_name(&f).unwrap_or_else(|| f.display().to_string())).unwrap_or_else(|| choice.clone()),
            _ => t!("setup.cards_template_own").to_owned(),
        };
        let preset = profile.preset.clone().unwrap_or_else(|| t!("setup.preset_standard").to_owned());
        note.push('\n');
        note.push_str(&t!("setup.cards_tuned", preset = preset, template = template));
    }
    Some(Menu::new(t!("setup.cards_title"), items).note(note).back(cmd.to_owned()).at(at))
}

/// `/configurer …`, typed or chosen.
pub async fn command(agent: &mut Agent, local: &mut LocalManager, words: &[&str], reply: &mut Reply) {
    absorb(local);
    match words.first().copied() {
        Some("detecter" | "détecter" | "detect" | "rescan") => {
            start(local.engines.strata_server.as_ref().map(|s| s.port));
            reply.dim(t!("setup.started"));
        }
        Some("annuler" | "cancel") => {
            cancel();
            mark_seen();
            offer(false);
            reply.dim(t!("setup.cancelled"));
        }
        Some("utiliser" | "use") => {
            let name = words[1..].join(" ");
            choose(agent, local, &name, reply).await;
        }
        Some("placer" | "place") if words.len() >= 3 => {
            place(agent, local, words[1], &words[2..].join(" "), reply).await;
        }
        Some("cartes" | "cards") => {
            for line in card_lines(&Hardware::detect()) {
                reply.dim(line);
            }
            reply.dim(t!("setup.cards_how", cmd = w("/configurer", "/setup")));
        }
        _ => {
            if local.engines.report.is_none() {
                let mut engines = std::mem::take(&mut local.engines);
                crate::engines_ui::detect(local, &mut engines).await;
                local.engines = engines;
            }
            let prefs = LocalPrefs::load();
            reply.field(t!("setup.default"), prefs.default_model.unwrap_or_else(|| t!("setup.no_default").to_owned()));
            if let Some(best) = recommended(local, &local.engines) {
                reply.field(t!("setup.recommended"), best);
            }
            reply.dim(t!("setup.how", cmd = w("/configurer", "/setup")));
        }
    }
}

/// Makes `name` the default model of every session, and starts it now.
async fn choose(agent: &mut Agent, local: &mut LocalManager, name: &str, reply: &mut Reply) {
    if local.engines.report.is_none() {
        let mut engines = std::mem::take(&mut local.engines);
        crate::engines_ui::detect(local, &mut engines).await;
        local.engines = engines;
    }
    let models = crate::engines_ops::all_models(local, local.engines.report.as_ref());
    let Some(model) = crate::engines_ops::find(&models, name).into_iter().next() else {
        reply.warn(t!("setup.unknown", name = name));
        return;
    };
    let mut prefs = LocalPrefs::load();
    prefs.default_model = Some(model.name().to_owned());
    prefs.setup_seen = true;
    if let Err(err) = prefs.save() {
        reply.warn(t!("cmd.local.pref_failed", err = err));
        return;
    }
    reply.accent(t!("setup.chosen", model = model.name()));
    offer(false);
    // Chosen now, by hand: it wins over a model pinned before.
    if agent.router().pinned().is_some_and(|p| p != model.name()) {
        agent.router_mut().pin(None);
    }
    start_default(agent, local, false, reply).await;
}

/// Keeps where `name` runs (`spec`: `auto`, `1`, `0,1`, `1/mtp=0`…), then makes it the default
/// and starts it there. Running elsewhere in a server Po started: started again where asked.
async fn place(agent: &mut Agent, local: &mut LocalManager, spec: &str, name: &str, reply: &mut Reply) {
    let Some(placement) = Placement::parse(spec) else {
        reply.warn(t!("setup.cards_bad", spec = spec));
        return;
    };
    if local.engines.report.is_none() {
        let mut engines = std::mem::take(&mut local.engines);
        crate::engines_ui::detect(local, &mut engines).await;
        local.engines = engines;
    }
    let models = crate::engines_ops::all_models(local, local.engines.report.as_ref());
    let Some(model) = crate::engines_ops::find(&models, name).into_iter().next() else {
        reply.warn(t!("setup.unknown", name = name));
        return;
    };
    let hardware = Hardware::detect();
    if let Err(why) = placement.check(&hardware) {
        reply.warn(why);
        return;
    }
    let mut prefs = LocalPrefs::load();
    let before = prefs.model(model.name()).placement.unwrap_or_default();
    prefs.model_mut(model.name()).placement = (!placement.is_auto()).then(|| placement.clone());
    if let Err(err) = prefs.save() {
        reply.warn(t!("cmd.local.pref_failed", err = err));
        return;
    }
    reply.dim(t!("setup.cards_saved", model = model.name(), place = placement.describe(&hardware)));
    if before != placement {
        let ours = match model.kind() {
            crate::engines_ui::Kind::Llama => local.active_model_id.as_deref() == Some(model.name()) && local.supervisor.as_ref().is_some_and(|s| s.running().is_some()),
            crate::engines_ui::Kind::Strata => local.engines.strata_server.as_ref().is_some_and(|s| s.config.model_name == model.name()),
        };
        let outside = !ours && local.engines.report.as_ref().is_some_and(|r| r.strata_live.iter().any(|l| l.model == model.name()));
        if ours {
            let mut engines = std::mem::take(&mut local.engines);
            crate::engines_ui::stop(model.kind(), agent, local, &mut engines, reply).await;
            local.engines = engines;
        } else if outside {
            reply.warn(t!("setup.cards_outside", model = model.name()));
        }
    }
    choose(agent, local, model.name(), reply).await;
}

/// The default model, ready for a message: nothing when it already serves the session (or the
/// user chose another), else started - once the machine can hold it - and the session switched
/// to it. A model another Po is loading is waited for. True when the session can go on.
pub async fn start_default(agent: &mut Agent, local: &mut LocalManager, for_message: bool, reply: &mut Reply) -> bool {
    let prefs = LocalPrefs::load();
    let Some(name) = prefs.default_model.clone() else { return true };
    if agent.router().get(&name).is_some() {
        return true;
    }
    // A model the user pinned by hand stays theirs.
    if agent.router().pinned().is_some_and(|p| agent.router().get(p).is_some()) {
        return true;
    }
    let mut engines = std::mem::take(&mut local.engines);
    if engines.report.is_none() {
        crate::engines_ui::detect(local, &mut engines).await;
    }
    let models = crate::engines_ops::all_models(local, engines.report.as_ref());
    let Some(model) = crate::engines_ops::find(&models, &name).into_iter().next() else {
        local.engines = engines;
        reply.warn(t!("setup.gone", model = name));
        return !agent.router().models().is_empty();
    };
    // Room for it now, as the launch plan measures it (the cards' free memory, the RAM).
    if let Model::Llama(index, _) = &model
        && local.catalog.get(*index).is_some_and(|e| !e.plan.viable)
    {
        local.engines = engines;
        reply.warn(t!("setup.no_room", model = name));
        return !agent.router().models().is_empty();
    }
    reply.dim(if for_message { t!("setup.starting", model = name) } else { t!("setup.starting_now", model = name) });
    crate::engines_ui::start_model(&model, agent, local, &mut engines, false, reply).await;
    // Served by a Strata still loading (started by another session): waited for, and said so -
    // this wait belongs to no `Progress` guard of ours, the other session is the one logging.
    if model.kind() == crate::engines_ui::Kind::Strata {
        let mirror = po_tui::control::mirror();
        let since_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let stage = t!("setup.waiting_other").to_owned();
        po_tui::set_progress(Some(t!("engines.progress.line", model = name, stage = stage.clone(), secs = 0)));
        mirror.loading(Some(po_wire::Loading { model: name.clone(), stage, since_ms }));
        let deadline = Instant::now() + po_local::strata::READY_TIMEOUT;
        while Instant::now() < deadline {
            let live = po_local::strata::probe(&[po_local::strata::DEFAULT_PORT]).await;
            if live.iter().any(|l| l.model == name && l.loaded) || !live.iter().any(|l| l.model == name) {
                break;
            }
            if po_tui::load_cancel_requested() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        mirror.loading(None);
    }
    local.engines = engines;
    if agent.router().get(&name).is_some() {
        agent.router_mut().pin(Some(name.clone()));
        true
    } else {
        !agent.router().models().is_empty()
    }
}

/// Tries to auto-start the default model in the background if viable.
async fn try_auto_start(mut scanned: Scanned, cancel: Arc<AtomicBool>) -> Scanned {
    if cancel.load(Ordering::Relaxed) {
        return scanned;
    }
    let Some(default_name) = scanned.prefs.default_model.clone() else {
        return scanned;
    };
    if scanned.strata_live.iter().any(|l| l.model == default_name && l.loaded) {
        return scanned;
    }
    let is_strata = scanned.strata.install.as_ref().is_some_and(|inst| {
        inst.configs.iter().any(|c| c.model_name == default_name)
    });
    let is_llama = scanned.catalog.iter().any(|e| e.label() == default_name);

    if is_strata {
        let Some(install) = scanned.strata.install.clone() else { return scanned; };
        if !install.launchable() {
            return scanned;
        }
        let Some(config) = install.configs.iter().find(|c| c.model_name == default_name).cloned() else {
            return scanned;
        };
        if config.model_files.is_empty() || !config.model_files.iter().all(|f| f.exists()) {
            return scanned;
        }
        let profile = scanned.prefs.model(&default_name);
        if profile.placement.as_ref().is_some_and(|place| place.check(&scanned.hardware).is_err()) {
            return scanned;
        }
        // Safety check: ensure no outside Strata is running
        if let Some(_other) = scanned.strata_live.iter().find(|l| l.model != default_name) {
            return scanned;
        }
        let port = if po_local::supervisor::port_is_free(config.port) {
            config.port
        } else {
            match po_local::supervisor::free_port() {
                Some(p) => p,
                None => return scanned,
            }
        };

        let mirror = po_tui::control::mirror();
        let since_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let stage_name = po_i18n::t!("setup.starting_now", model = default_name.clone());
        po_tui::set_progress(Some(po_i18n::t!("engines.progress.line", model = default_name.clone(), stage = stage_name.clone(), secs = 0)));
        mirror.loading(Some(po_wire::Loading { model: default_name.clone(), stage: stage_name, since_ms }));

        let overrides = crate::engines_ops::strata_overrides(&profile, scanned.prefs.game_mode);
        // `cancel` and the terminal's Escape write into the same shared flag (see `cancel`),
        // so the launch stops itself through the ordinary path.
        let cancel_watch = po_tui::load_cancel_cell();
        // The cancel flag is handed to the launch itself: a cancelled wait stops the server
        // through the ordinary path. Dropping the launch future instead would kill only the
        // Python launcher and leave the engine it had started holding the card.
        match po_local::strata::start_watch(&install, &config, port, po_local::strata::READY_TIMEOUT, &overrides, Some(cancel_watch)).await {
            Ok(server) => {
                mirror.loading(None);
                po_tui::set_progress(None);
                scanned.auto_started = Some(crate::engines_ui::StartedServer {
                    model_name: default_name,
                    kind: crate::engines_ui::Kind::Strata,
                    port,
                    strata: Some(server),
                    llama: None,
                });
            }
            Err(_) => {
                mirror.loading(None);
                po_tui::set_progress(None);
            }
        }
    } else if is_llama {
        let Some(located) = scanned.llama.located.clone() else { return scanned; };
        let Some(entry) = scanned.catalog.iter().find(|e| e.label() == default_name).cloned() else { return scanned; };
        if !entry.path.exists() {
            return scanned;
        }
        let profile = scanned.prefs.model(&default_name);
        let placement = profile.placement.clone().unwrap_or_default();
        if placement.check(&scanned.hardware).is_err() {
            return scanned;
        }
        let request = po_local::PlanRequest {
            desired_context: profile.context.unwrap_or(po_local::PlanRequest::default().desired_context),
            ..Default::default()
        };
        let plan = po_local::plan::plan_placed(&entry.info, &scanned.hardware, &request, &placement);
        if !plan.viable {
            return scanned;
        }

        let mirror = po_tui::control::mirror();
        let since_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let stage_name = po_i18n::t!("setup.starting_now", model = default_name.clone());
        po_tui::set_progress(Some(po_i18n::t!("engines.progress.line", model = default_name.clone(), stage = stage_name.clone(), secs = 0)));
        mirror.loading(Some(po_wire::Loading { model: default_name.clone(), stage: stage_name, since_ms }));

        let mut supervisor = po_local::Supervisor::with_binary(located.path.clone());
        let threads = scanned.hardware.cpu_threads;
        let pin = plan.device_env(&scanned.hardware);
        supervisor.set_env(pin);
        let options = po_local::supervisor::LaunchOptions {
            overrides: po_local::supervisor::profile_overrides(&profile, scanned.prefs.game_mode, threads),
            low_priority: scanned.prefs.game_mode,
            ..Default::default()
        };
        let cancel_wait = cancel.clone();
        tokio::select! {
            result = supervisor.start_with(&entry.path, &plan, &options) => {
                mirror.loading(None);
                po_tui::set_progress(None);
                if let Ok(port) = result {
                    if cancel.load(Ordering::Relaxed) {
                        supervisor.stop().await;
                    } else {
                        scanned.auto_started = Some(crate::engines_ui::StartedServer {
                            model_name: default_name,
                            kind: crate::engines_ui::Kind::Llama,
                            port,
                            strata: None,
                            llama: Some(supervisor),
                        });
                    }
                }
            }
            _ = async {
                while !cancel_wait.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => {
                supervisor.stop().await;
                mirror.loading(None);
                po_tui::set_progress(None);
                return scanned;
            }
        }
    }

    scanned
}

/// Registers the default model into the agent if already running or auto-started.
pub async fn register_if_ready(agent: &mut Agent, local: &mut LocalManager) -> bool {
    absorb(local);
    let prefs = LocalPrefs::load();
    let Some(ref default_name) = prefs.default_model else { return false; };
    if agent.router().get(default_name).is_some() {
        if agent.router().pinned() != Some(default_name.as_str()) {
            agent.router_mut().pin(Some(default_name.clone()));
            return true;
        }
        return false;
    }
    let strata_port = local.engines.strata_server.as_ref().filter(|s| s.config.model_name == *default_name).map(|s| s.port);
    if let Some(port) = strata_port {
        crate::engines_ui::register_strata(agent, &mut local.engines, default_name, port).await;
        agent.router_mut().pin(Some(default_name.clone()));
        return true;
    }
    let live_port = local.engines.report.as_ref().and_then(|r| r.strata_live.iter().find(|l| l.model == *default_name && l.loaded).map(|l| l.port));
    if let Some(port) = live_port {
        crate::engines_ui::register_strata(agent, &mut local.engines, default_name, port).await;
        agent.router_mut().pin(Some(default_name.clone()));
        return true;
    }
    if let Some(supervisor) = &local.supervisor
        && let Some(running) = supervisor.running()
        && local.active_model_id.as_deref() == Some(default_name)
    {
        let base_url = running.base_url();
        let profile = prefs.model(default_name);
        let context_window = profile.context.unwrap_or(32_768);
        agent.router_mut().add_model(po_core::ModelSpec {
            id: default_name.clone(),
            display_name: format!("{default_name} (local)"),
            origin: po_core::Origin::Local { backend: "llama.cpp".into(), endpoint: base_url.clone() },
            tier: crate::config::guess_tier(default_name),
            capabilities: po_core::Capabilities { tools: true, ..Default::default() },
            context_window,
            max_output_tokens: 4096,
            pricing: po_core::Pricing::FREE,
        });
        agent.attach(default_name.clone(), std::sync::Arc::new(po_provider::OpenAiProvider::local("llama.cpp", base_url).with_effort_in_template_kwargs(true)));
        agent.settings.model_defaults.insert(default_name.clone(), crate::engines_ops::session_defaults(&profile));
        agent.router_mut().pin(Some(default_name.clone()));
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wanted_at_start_with_and_without_default() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        let path = temp.path().to_string_lossy().to_string();
        unsafe { std::env::set_var("PO_LOCAL_PREFS", &path); }

        // Case 1: no default model, setup not seen
        let mut prefs = LocalPrefs::default();
        prefs.save().expect("save prefs");

        assert!(wanted_at_start(false));
        assert!(wanted_at_start(true));

        // Case 2: no default model, setup seen, but no model running
        prefs.setup_seen = true;
        prefs.save().expect("save prefs");
        assert!(wanted_at_start(true));
        assert!(!wanted_at_start(false));

        // Case 3: default model set! (setup MUST NOT be proposed even on fresh start with no running model)
        prefs.default_model = Some("test-model-default".to_string());
        prefs.save().expect("save prefs");
        assert!(!wanted_at_start(true));
        assert!(!wanted_at_start(false));

        unsafe { std::env::remove_var("PO_LOCAL_PREFS"); }
    }

    #[test]
    fn test_menu_default_model_ordering() {
        let best = Some("recommended-model".to_string());
        let default_name = Some("my-default-model");

        let m1 = "other-model-1";
        let m2 = "recommended-model";
        let m3 = "my-default-model";
        let m4 = "other-model-2";
        let models = [m1, m2, m3, m4];

        let mut ordered: Vec<&&str> = Vec::new();
        if let Some(def) = default_name {
            ordered.extend(models.iter().filter(|m| **m == def));
        }
        ordered.extend(models.iter().filter(|m| Some(**m) == best.as_deref() && Some(**m) != default_name));
        ordered.extend(models.iter().filter(|m| Some(**m) != default_name && Some(**m) != best.as_deref()));

        let ordered_names: Vec<&str> = ordered.into_iter().copied().collect();
        assert_eq!(ordered_names[0], "my-default-model", "Default model must be at index 0");
        assert_eq!(ordered_names[1], "recommended-model", "Recommended model must be at index 1");
        assert!(ordered_names.contains(&"other-model-1"));
        assert!(ordered_names.contains(&"other-model-2"));
    }
}
