//! Contents of the settings panel (F2): each row is bound to the `/…` command that applies
//! it, so the panel, the palette and typed input all go through the same code (`commands.rs`).

use po_agent::Agent;
use po_core::Origin;
use po_router::BudgetMode;
use po_tui::settings::{Row, Tab};

use crate::commands::Extras;
use po_i18n::t;

fn options(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(label, command)| ((*label).to_owned(), (*command).to_owned())).collect()
}

fn position(options: &[(String, String)], label: &str) -> usize {
    options.iter().position(|(l, _)| l == label).unwrap_or(0)
}

/// `engines`: the one-line summary of the engines (`/moteurs`), known to the caller.
pub fn tabs(agent: &Agent, extras: &Extras, engines: &str) -> Vec<Tab> {
    vec![session(agent), subagents(agent), models(agent, engines), tools(agent, extras)]
}

fn session(agent: &Agent) -> Tab {
    let s = &agent.settings;
    let labs_on = crate::labs::enabled();
    let featured = vec![
        Row::choice(
            po_i18n::t!("labs.name"),
            vec![
                (po_i18n::t!("labs.state_off").to_owned(), "/labs off".into()),
                (po_i18n::t!("labs.state_on").to_owned(), "/labs on".into()),
            ],
            usize::from(labs_on),
        )
        .hint(po_i18n::t!("labs.setting_hint"))
        .featured(),
        Row::choice(
            po_i18n::t!("labs.timer_label"),
            ["0", "10", "30", "60", "120"]
                .iter()
                .map(|m| {
                    let label = if *m == "0" { po_i18n::t!("labs.timer_never").to_owned() } else { format!("{m} min") };
                    (label, format!("/labs minuteur {m}"))
                })
                .collect(),
            ["0", "10", "30", "60", "120"].iter().position(|m| m.parse::<u64>().ok() == Some(po_tui::labs::idle_minutes())).unwrap_or(2),
        )
        .hint(po_i18n::t!("labs.timer_hint")),
        Row::prompt(
            format!("{} · {}", po_i18n::t!("labs.shortcut_label"), po_tui::labs::shortcut("labs").label()),
            "/raccourci labs ",
        )
        .hint(po_i18n::t!("labs.shortcut_hint")),
        Row::prompt(
            format!("{} · {}", po_i18n::t!("labs.pause_label"), po_tui::labs::shortcut("pause").label()),
            "/raccourci pause ",
        )
        .hint(po_i18n::t!("labs.shortcut_hint")),
        Row::choice(
            po_i18n::t!("common.language.label"),
            vec![("Français".into(), "/langue fr".into()), ("English".into(), "/language en".into())],
            usize::from(po_i18n::lang() == po_i18n::Lang::En),
        )
        .hint(po_i18n::t!("common.language.hint")),
    ];
    let server = t!("cmd.panel.server");
    let effort = options(&[
        (server, "/reflexion defaut"),
        ("none", "/reflexion none"),
        ("low", "/reflexion low"),
        ("medium", "/reflexion medium"),
        ("high", "/reflexion high"),
    ]);
    let current = position(&effort, s.reasoning_effort.as_deref().unwrap_or(server));

    let thinking_budget = options(&[
        ("auto", "/reflexion budget auto"),
        ("2k", "/reflexion budget 2k"),
        ("4k", "/reflexion budget 4k"),
        ("8k", "/reflexion budget 8k"),
        ("16k", "/reflexion budget 16k"),
        (t!("cmd.panel.thinking_budget.server"), "/reflexion budget off"),
    ]);
    let current_budget = match s.thinking_budget {
        None => 0,
        Some(0) => thinking_budget.len() - 1,
        Some(n) => position(&thinking_budget, &format!("{}k", n / 1024)),
    };

    use po_tui::render::Verbosity;
    let verbosity: Vec<(String, String)> =
        Verbosity::ALL.iter().map(|v| (v.label().to_owned(), format!("/verbosity {}", v.as_str()))).collect();
    let current_verbosity = Verbosity::ALL.iter().position(|v| *v == Verbosity::current()).unwrap_or(1);

    let mut temperature = options(&[
        (server, "/temperature defaut"),
        ("0.2", "/temperature 0.2"),
        ("0.6", "/temperature 0.6"),
        ("1.0", "/temperature 1.0"),
    ]);
    let current_t = match s.temperature {
        None => 0,
        Some(t) => {
            let label = format!("{t:.1}");
            match temperature.iter().position(|(l, _)| *l == label) {
                Some(i) => i,
                None => {
                    temperature.push((format!("{t}"), format!("/temperature {t}")));
                    temperature.len() - 1
                }
            }
        }
    };

    let modes = [BudgetMode::MaxQuality, BudgetMode::Balanced, BudgetMode::SaveMaxTokens, BudgetMode::Offline];
    let names = ["max-quality", "balanced", "save-max-tokens", "offline"];
    let budget: Vec<(String, String)> =
        modes.iter().zip(names).map(|(m, n)| (m.label().to_owned(), format!("/mode {n}"))).collect();
    let current_mode = modes.iter().position(|m| *m == agent.router().mode()).unwrap_or(0);

    let modes = options(&[("plan", "/plan"), ("builder", "/builder"), ("bypass", "/bypass")]);
    let current_work = position(&modes, agent.work_mode().get().label());
    let mut rows = vec![
        Row::choice(t!("cmd.panel.work_mode"), modes, current_work).hint(t!("cmd.panel.work_mode.hint")),
        Row::choice(t!("cmd.panel.verbosity"), verbosity, current_verbosity).hint(t!("cmd.panel.verbosity.hint")),
        Row::choice(t!("cmd.panel.effort"), effort, current).hint(t!("cmd.panel.effort.hint")),
        Row::choice(t!("cmd.panel.thinking_budget"), thinking_budget, current_budget)
            .hint(t!("cmd.panel.thinking_budget.hint")),
        Row::choice(t!("cmd.panel.temperature"), temperature, current_t).hint(t!("cmd.panel.temperature.hint")),
        Row::choice(
            t!("cmd.panel.thinking"),
            options(&[(t!("cmd.panel.thinking.folded"), "/raisonnement off"), (t!("cmd.panel.thinking.streamed"), "/raisonnement on")]),
            usize::from(s.show_thinking),
        )
        .hint(t!("cmd.panel.thinking.hint")),
        Row::choice(t!("cmd.panel.budget"), budget, current_mode).hint(t!("cmd.panel.budget.hint")),
        Row::prompt(t!("cmd.panel.add_criterion"), "/criteres ajouter ").hint(t!("cmd.panel.add_criterion.hint")),
    ];
    for (i, criterion) in s.criteria.iter().enumerate() {
        rows.push(
            Row::run(t!("cmd.panel.criterion", n = i + 1), criterion.clone(), format!("/criteres retirer {}", i + 1))
                .hint(t!("cmd.panel.criterion.hint")),
        );
    }
    rows.push(Row::run(t!("cmd.panel.compact"), "", "/compacter").hint(t!("cmd.panel.compact.hint")));
    rows.push(Row::run(t!("cmd.panel.new"), "", "/nouveau").hint(t!("cmd.panel.new.hint")));
    rows.splice(0..0, featured);
    Tab { title: t!("cmd.panel.tab.session").into(), rows }
}

fn subagents(agent: &Agent) -> Tab {
    let mut rows = vec![Row::choice(
        t!("cmd.panel.delegation"),
        options(&[(t!("cmd.panel.enabled_f"), "/delegation on"), (t!("cmd.panel.disabled_f"), "/delegation off")]),
        usize::from(!agent.delegation_enabled()),
    )
    .hint(t!("cmd.panel.delegation.hint"))];

    if let Some(model) = agent.session_model() {
        let detected = agent.detected_slots(&model);
        let mut parallel = vec![(format!("auto ({detected})"), "/agents parallele auto".to_owned())];
        for n in 1..=4 {
            parallel.push((n.to_string(), format!("/agents parallele {n}")));
        }
        let current = agent.subagent_settings.max_parallel.map(|n| n.min(4)).unwrap_or(0);
        rows.push(
            Row::choice(t!("cmd.panel.parallelism"), parallel, current)
                .hint(t!("cmd.panel.parallelism.hint", model = model.display_name, detected = detected)),
        );
    }

    let mut choices = vec![("session".to_owned(), String::new()), ("auto".to_owned(), String::new())];
    choices.extend(agent.router().models().iter().map(|m| (m.id.clone(), String::new())));
    for profile in agent.profiles() {
        let options: Vec<(String, String)> = choices
            .iter()
            .map(|(id, _)| (id.clone(), format!("/agents modele {} {id}", profile.name)))
            .collect();
        let current = position(&options, &agent.model_choice(profile));
        rows.push(Row::choice(format!("{} · {}", profile.name, profile.tools.label()), options, current).hint(t!(
            "cmd.panel.agent.hint",
            description = profile.description,
            source = profile.source.label()
        )));
    }
    rows.push(Row::prompt(t!("cmd.panel.agent_new"), "/agents creer ").hint(t!("cmd.panel.agent_new.hint")));
    rows.push(Row::prompt(t!("cmd.panel.agent_global"), "/agents creer global ").hint(t!("cmd.panel.agent_global.hint")));
    rows.push(Row::run(t!("cmd.panel.reload"), "", "/agents recharger").hint(t!("cmd.panel.reload.hint")));
    Tab { title: t!("cmd.panel.tab.subagents").into(), rows }
}

fn models(agent: &Agent, engines: &str) -> Tab {
    let router = agent.router();
    let mut choice = vec![(t!("cmd.panel.automatic").to_owned(), "/modele auto".to_owned())];
    choice.extend(router.models().iter().map(|m| (m.id.clone(), format!("/modele {}", m.id))));
    let current = router.pinned().map(|id| position(&choice, id)).unwrap_or(0);
    let mut rows = vec![Row::choice(t!("cmd.panel.session_model"), choice, current).hint(t!("cmd.panel.session_model.hint"))];
    for model in router.models() {
        let origin = match &model.origin {
            Origin::Local { .. } => t!("cmd.panel.local"),
            _ => t!("cmd.panel.cloud"),
        };
        let state = if router.is_disabled(&model.id) { t!("cmd.panel.excluded") } else { "" };
        rows.push(Row::info(
            format!("  {}", model.id),
            format!("{origin} · {} k tokens{state}", model.context_window / 1024),
        ));
    }
    // The engines first: for a beginner, the way in to everything local.
    let engines_cmd = if po_i18n::lang() == po_i18n::Lang::Fr { "/moteurs" } else { "/engines" };
    rows.push(Row::open(t!("cmd.panel.engines"), engines, engines_cmd).hint(t!("cmd.panel.engines.hint")));
    rows.push(Row::run(t!("cmd.panel.server_row"), t!("cmd.panel.server_row.value"), "/serveur").hint(t!("cmd.panel.server_row.hint")));
    rows.push(Row::run(t!("cmd.panel.gguf"), "", "/local").hint(t!("cmd.panel.gguf.hint")));
    // The conversations parked on disk: their limits.
    let pref = |key: &str, default: u64| {
        po_i18n::prefs::get_str(key).and_then(|v| v.trim().parse::<u64>().ok()).filter(|n| *n > 0).unwrap_or(default)
    };
    let quotas = [5u64, 10, 20, 50, 100];
    let quota = pref("kv_quota_gb", po_local::kvstore::DEFAULT_QUOTA_GB);
    rows.push(
        Row::choice(
            t!("cmd.panel.kv_quota"),
            quotas.iter().map(|g| (format!("{g} Go"), format!("/cache limite {g}"))).collect(),
            quotas.iter().position(|g| *g == quota).unwrap_or(2),
        )
        .hint(t!("cmd.panel.kv_quota.hint")),
    );
    let ages = [1u64, 3, 7, 30];
    let age = pref("kv_max_days", po_local::kvstore::DEFAULT_MAX_DAYS);
    rows.push(
        Row::choice(
            t!("cmd.panel.kv_days"),
            ages.iter().map(|d| (t!("cmd.panel.kv_days.value", n = d), format!("/cache jours {d}"))).collect(),
            ages.iter().position(|d| *d == age).unwrap_or(2),
        )
        .hint(t!("cmd.panel.kv_days.hint")),
    );
    rows.push(Row::run(t!("cmd.panel.kv_clear"), "", "/cache vider").hint(t!("cmd.panel.kv_clear.hint")));
    Tab { title: t!("cmd.panel.tab.models").into(), rows }
}

fn tools(agent: &Agent, extras: &Extras) -> Tab {
    let mut rows = Vec::new();
    for entry in &extras.mcp {
        rows.push(
            Row::run(format!("MCP {}", entry.config.name), t!("cmd.panel.mcp_on", n = entry.tools), format!("/mcp retirer {}", entry.config.name))
                .hint(t!("cmd.panel.mcp_on.hint")),
        );
    }
    for config in &extras.mcp_available {
        rows.push(
            Row::run(format!("MCP {}", config.name), t!("cmd.panel.mcp_off"), format!("/mcp activer {}", config.name))
                .hint(t!("cmd.panel.mcp_off.hint")),
        );
    }
    rows.push(Row::prompt(t!("cmd.panel.mcp_add"), "/mcp ajouter ").hint(t!("cmd.panel.mcp_add.hint")));
    rows.push(Row::run(t!("cmd.panel.skills"), format!("{}", extras.skills.len()), "/skills").hint(t!("cmd.panel.skills.hint")));
    rows.push(Row::info(t!("cmd.panel.instructions"), t!("cmd.panel.instructions.value", n = extras.instructions.len())));
    rows.push(Row::run(t!("cmd.panel.visible_tools"), format!("{}", agent.registry().names().len()), "/outils"));
    let voice = po_tui::voice::Settings::load();
    rows.push(
        Row::choice(
            t!("cmd.panel.voice"),
            vec![(t!("cmd.panel.disabled_f").into(), "/voix off".into()), (t!("cmd.panel.enabled_f").into(), "/voix on".into())],
            usize::from(voice.enabled),
        )
        .hint(t!("cmd.panel.voice.hint")),
    );
    rows.push(
        Row::choice(
            t!("cmd.panel.voice_engine"),
            vec![(t!("cmd.voice.cpu").into(), "/voix moteur cpu".into()), (t!("cmd.panel.gpu").into(), "/voix moteur gpu".into())],
            usize::from(voice.gpu.is_some()),
        )
        .hint(t!("cmd.panel.voice_engine.hint")),
    );
    let models = ["small", "base", "turbo"];
    rows.push(
        Row::choice(
            t!("cmd.panel.voice_model"),
            models.iter().map(|m| (m.to_string(), format!("/voix modele {m}"))).collect(),
            models.iter().position(|m| *m == voice.model).unwrap_or(0),
        )
        .hint(t!("cmd.panel.voice_model.hint")),
    );
    rows.push(
        Row::run(
            t!("cmd.panel.voice_install"),
            voice.missing().map(|_| t!("cmd.panel.todo")).unwrap_or(t!("cmd.panel.ready")),
            "/voix installer",
        )
        .hint(t!("cmd.panel.voice_install.hint")),
    );
    Tab { title: t!("cmd.panel.tab.tools").into(), rows }
}
