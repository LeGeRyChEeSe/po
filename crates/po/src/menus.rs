//! The menus of commands, for the full-screen interface: a command that needs a choice, typed
//! without it (or with one it does not know), offers its choices to walk with the arrows rather
//! than explaining which parameter to type. Line mode keeps the commands as they are.

use po_agent::Agent;
use po_i18n::t;
use po_tui::menu::{Item, Menu};

use crate::commands::Extras;
use crate::local_ui::LocalManager;

/// The word of the interface's language: every subcommand exists in both.
fn w(fr: &'static str, en: &'static str) -> &'static str {
    if po_i18n::lang() == po_i18n::Lang::Fr { fr } else { en }
}

/// The menu of `line`, if it needs a choice it does not have.
pub async fn menu(line: &str, agent: &mut Agent, local: &mut LocalManager, extras: &Extras) -> Option<Menu> {
    // What the background detection found is put in the session first: the menus show it.
    crate::setup::absorb(local);
    let words: Vec<&str> = line.split_whitespace().collect();
    let cmd = *words.first()?;
    let arg = words.get(1).copied();
    let rest = words.get(2..).unwrap_or(&[]);
    match cmd {
        // The settings panel, as menus for a screen without one (the terminal opens its panel).
        c if po_tui::settings::PANEL_COMMANDS.contains(&c) => {
            let tabs = crate::settings_panel::tabs(agent, extras, &crate::engines_ui::summary(local));
            po_tui::settings::menu(&tabs, &words[1..])
        }
        "/agents" | "/agent" | "/sous-agents" if arg.is_none() => {
            let tabs = crate::settings_panel::tabs(agent, extras, &crate::engines_ui::summary(local));
            let at = tabs.iter().position(|t| t.title.eq_ignore_ascii_case(t!("cmd.panel.tab.subagents")))?;
            po_tui::settings::menu(&tabs, &[&(at + 1).to_string()])
        }
        "/profil" | "/profile" => crate::roles::menu(cmd, &words[1..], agent, &extras.workspace),
        "/modele" | "/model" | "/modèle" if matches!(arg, Some("reglages" | "réglages" | "settings")) => {
            crate::profile::menu(&format!("{cmd} {}", arg.unwrap_or_default()), rest, agent)
        }
        "/configurer" | "/setup" => crate::setup::menu(cmd, &words[1..], local),
        "/labs" => labs(cmd, arg, rest),
        "/frictions" | "/accrocs" => crate::friction::menu(cmd, arg, rest),
        "/rc" | "/remote-control" | "/telecommande" | "/télécommande" => crate::rc::menu(cmd, arg, rest).await,
        "/langue" | "/language" | "/lang" => choose(arg, po_i18n::Lang::parse(arg.unwrap_or("")).is_some(), || {
            let now = po_i18n::lang();
            Menu::new(
                t!("menu.language"),
                [po_i18n::Lang::Fr, po_i18n::Lang::En].into_iter().map(|l| Item::run(l.name(), format!("{cmd} {}", l.code())).current(l == now)).collect(),
            )
        }),
        "/reprendre" | "/resume" => resume(cmd, arg, agent, extras),
        "/raccourci" | "/shortcut" => shortcut(cmd, arg, rest),
        "/statusline" | "/barre" => arg.is_none().then(|| {
            Menu::new(
                t!("menu.statusline"),
                vec![
                    Item::fill(t!("menu.statusline.describe"), format!("{cmd} ")).detail(t!("menu.statusline.describe_hint")),
                    Item::show(t!("menu.statusline.show"), cmd),
                    Item::run(t!("menu.statusline.reset"), format!("{cmd} {}", w("defaut", "default"))),
                ],
            )
        }),
        "/verbosite" | "/verbosité" | "/verbosity" => {
            use po_tui::render::Verbosity;
            choose(arg, arg.and_then(Verbosity::parse).is_some(), || {
                let now = Verbosity::current();
                Menu::new(
                    t!("menu.verbosity"),
                    Verbosity::ALL.iter().map(|v| Item::run(v.label(), format!("{cmd} {}", v.as_str())).current(*v == now)).collect(),
                )
            })
        }
        "/rigueur" | "/rigor" => {
            use po_agent::review::Rigor;
            choose(arg, arg.and_then(Rigor::parse).is_some(), || {
                let now = Rigor::current();
                Menu::new(
                    t!("menu.rigor"),
                    Rigor::ALL.iter().map(|r| Item::run(crate::commands::rigor_label(*r), format!("{cmd} {}", r.as_str())).current(*r == now)).collect(),
                )
            })
        }
        "/theme" | "/thème" | "/themes" | "/thèmes" => {
            use po_tui::theme::Theme;
            choose(arg, arg.and_then(Theme::parse).is_some(), || {
                let now = Theme::current();
                // Each choice comes back here: the theme is seen at once, and another tried.
                Menu::new(
                    t!("menu.theme"),
                    Theme::ALL
                        .iter()
                        .map(|th| Item::run(th.label(), format!("{cmd} {}", th.as_str())).current(*th == now).then(cmd.to_owned()))
                        .collect(),
                )
                .note(t!("menu.theme_note"))
            })
        }
        "/reflexion" | "/réflexion" | "/effort" => effort(cmd, arg, rest, agent),
        "/temperature" | "/température" => {
            let known = arg.is_some_and(|a| matches!(a, "defaut" | "défaut" | "default" | "auto") || a.replace(',', ".").parse::<f32>().is_ok_and(|v| (0.0..=2.0).contains(&v)));
            choose(arg, known, || {
                let now = agent.settings.temperature;
                let mut items = vec![Item::run(t!("menu.server_default"), format!("{cmd} {}", w("defaut", "default"))).current(now.is_none())];
                for value in [0.0f32, 0.2, 0.4, 0.6, 0.7, 0.8, 1.0] {
                    items.push(Item::run(format!("{value}"), format!("{cmd} {value}")).current(now.is_some_and(|n| (n - value).abs() < 1e-3)));
                }
                items.push(Item::fill(t!("menu.other_value"), format!("{cmd} ")));
                Menu::new(t!("menu.temperature"), items).note(t!("menu.temperature_note"))
            })
        }
        "/raisonnement" | "/thinking" => choose(arg, matches!(arg, Some("on" | "oui" | "off" | "non")), || {
            let shown = agent.settings.show_thinking;
            Menu::new(
                t!("menu.thinking"),
                vec![
                    Item::run(t!("menu.thinking.show"), format!("{cmd} on")).current(shown),
                    Item::run(t!("menu.thinking.hide"), format!("{cmd} off")).current(!shown),
                ],
            )
        }),
        "/delegation" | "/délégation" => choose(arg, matches!(arg, Some("on" | "oui" | "off" | "non")), || {
            let on = agent.delegation_enabled();
            Menu::new(
                t!("menu.delegation"),
                vec![Item::run(t!("menu.allow"), format!("{cmd} on")).current(on), Item::run(t!("menu.forbid"), format!("{cmd} off")).current(!on)],
            )
        }),
        "/cache" => cache(cmd, arg, rest),
        "/criteres" | "/critères" | "/criteria" => criteria(cmd, arg, rest, agent),
        "/mode" => choose(arg, arg.and_then(crate::chat::parse_budget).is_some(), || {
            use po_router::BudgetMode;
            let now = agent.router().mode();
            let modes = [
                (BudgetMode::MaxQuality, w("qualite", "quality")),
                (BudgetMode::Balanced, w("equilibre", "balanced")),
                (BudgetMode::SaveMaxTokens, w("economie", "save")),
                (BudgetMode::Offline, w("hors-ligne", "offline")),
            ];
            Menu::new(t!("menu.mode"), modes.into_iter().map(|(m, word)| Item::run(m.label(), format!("{cmd} {word}")).current(m == now)).collect())
        }),
        "/modele" | "/model" | "/modèle" => {
            // Unloading is a command, not a choice in the list: it goes through as typed.
            let unload = matches!(arg, Some("decharger" | "décharger" | "unload" | "stop" | "annuler" | "cancel"));
            let known = unload || arg.is_some_and(|a| a == "auto" || agent.router().get(a).is_some());
            choose(arg, known, || {
                let pinned = agent.router().pinned().map(str::to_owned);
                let mut items = vec![Item::run(t!("menu.model.auto"), format!("{cmd} auto")).current(pinned.is_none())];
                for model in agent.router().models() {
                    items.push(
                        Item::run(&model.id, format!("{cmd} {}", model.id))
                            .detail(model.origin.label())
                            .current(pinned.as_deref() == Some(model.id.as_str())),
                    );
                }
                items.push(Item::run(t!("menu.model.settings"), format!("{cmd} {}", w("reglages", "settings"))).detail(t!("menu.model.settings_detail")));
                let mut menu = Menu::new(t!("menu.model"), items);
                if let Some(typed) = arg {
                    menu = menu.note(t!("menu.unknown", value = typed));
                }
                menu
            })
        }
        "/local" => local_menu(cmd, arg, rest, local),
        "/moteurs" | "/moteur" | "/engines" | "/engine" => {
            let words: Vec<&str> = arg.into_iter().chain(rest.iter().copied()).collect();
            crate::engines_ui::menu(cmd, &words, agent, local).await
        }
        "/serveur" | "/server" => arg.is_none().then(|| {
            Menu::new(
                t!("menu.server"),
                vec![
                    Item::show(t!("menu.server.state"), cmd),
                    Item::run(t!("menu.server.unload"), format!("{cmd} {}", w("decharger", "unload"))).detail(t!("menu.server.unload_hint")),
                    Item::run(t!("menu.server.load"), format!("{cmd} {}", w("charger", "load"))),
                ],
            )
        }),
        "/agents" | "/agent" | "/sous-agents" => agents(cmd, arg, rest, agent),
        "/mcp" => mcp(cmd, arg, rest, extras),
        "/skills" | "/competences" | "/compétences" => arg.is_none().then(|| {
            Menu::new(
                t!("menu.skills"),
                vec![
                    Item::show(t!("menu.skills.show"), cmd).detail(t!("menu.count", n = extras.skills.len())),
                    Item::run(t!("menu.skills.reload"), format!("{cmd} {}", w("recharger", "reload"))),
                ],
            )
        }),
        "/voix" | "/voice" | "/dictee" | "/dictée" => voice(cmd, arg, rest).await,
        _ => None,
    }
}

/// The commands whose menu only reads the settings, prepared at the start of a turn: typed
/// during it, they still offer their menu (the agent is busy then). In both languages.
const DURING_A_TURN: &[&str] = &[
    "/raisonnement", "/thinking", "/reflexion", "/réflexion", "/effort", "/reflexion budget", "/effort budget",
    "/temperature", "/température", "/verbosite", "/verbosité", "/verbosity", "/rigueur", "/rigor", "/theme", "/thème", "/mode", "/modele", "/model", "/modèle",
    "/delegation", "/délégation", "/langue", "/language", "/lang", "/labs", "/labs minuteur", "/labs timer",
    "/criteres", "/critères", "/criteria", "/criteres retirer", "/criteria remove", "/raccourci", "/shortcut",
    "/raccourci labs", "/raccourci pause", "/shortcut labs", "/shortcut pause", "/cache", "/cache limite",
    "/cache jours", "/cache limit", "/cache days", "/statusline", "/barre", "/serveur", "/server", "/skills",
    "/agents voir", "/agents show", "/agents modele", "/agents model", "/agents parallele", "/agents parallel",
    "/agents retirer", "/agents remove", "/agents creer", "/agents create", "/mcp", "/mcp activer", "/mcp enable",
    "/mcp garder", "/mcp keep", "/mcp retirer", "/mcp remove", "/voix", "/voice", "/voix moteur", "/voice engine",
    "/voix modele", "/voice model", "/voix langue", "/voice language", "/profil", "/profile", "/profil gerer",
    "/profile manage",
];

/// The menus of [`DURING_A_TURN`] (and each subagent's model), by the line that opens them.
pub async fn turn_menus(agent: &mut Agent, local: &mut LocalManager, extras: &Extras) -> Vec<(String, Menu)> {
    let mut lines: Vec<String> = DURING_A_TURN.iter().map(|l| (*l).to_owned()).collect();
    lines.push("/configurer".into());
    lines.push("/setup".into());
    for key in ["", " reasoning", " temperature", " top_p", " top_k", " min_p", " repeat_penalty", " max_output", " context", " cache"] {
        lines.push(format!("/modele reglages{key}"));
        lines.push(format!("/model settings{key}"));
    }
    // The settings, walked as menus from a phone during a turn too.
    let tabs = crate::settings_panel::tabs(agent, extras, &crate::engines_ui::summary(local));
    lines.push("/settings".into());
    for (t, tab) in tabs.iter().enumerate() {
        lines.push(format!("/settings {}", t + 1));
        for (r, row) in tab.rows.iter().enumerate() {
            if matches!(row.action, po_tui::settings::Action::Choice { .. }) {
                lines.push(format!("/settings {} {}", t + 1, r + 1));
            }
        }
    }
    for profile in agent.profiles() {
        lines.push(format!("/agents modele {}", profile.name));
        lines.push(format!("/agents model {}", profile.name));
    }
    let mut out = Vec::new();
    for line in lines {
        if let Some(menu) = menu(&line, agent, local, extras).await {
            out.push((line, menu));
        }
    }
    out
}

/// A menu when the argument is missing or unknown, saying so in the latter case.
fn choose(arg: Option<&str>, known: bool, menu: impl FnOnce() -> Menu) -> Option<Menu> {
    if known {
        return None;
    }
    let built = menu();
    Some(match arg {
        Some(typed) if built.note.is_none() => built.note(t!("menu.unknown", value = typed)),
        _ => built,
    })
}

fn labs(cmd: &str, arg: Option<&str>, rest: &[&str]) -> Option<Menu> {
    match arg {
        None => {
            let on = crate::labs::enabled();
            let mut items = vec![
                Item::run(t!("menu.labs.on"), format!("{cmd} on")).current(on),
                Item::run(t!("menu.labs.off"), format!("{cmd} off")).current(!on),
            ];
            if on {
                items.push(Item::run(t!("menu.labs.go"), format!("{cmd} {}", w("lancer", "start"))));
            }
            items.push(Item::show(t!("menu.labs.state"), cmd));
            items.push(Item::run(t!("menu.labs.timer"), format!("{cmd} {}", w("minuteur", "timer"))).detail(timer_label(po_tui::labs::idle_minutes())));
            items.push(Item::run(t!("menu.labs.contributions"), "/contributions"));
            Some(Menu::new("Po Labs", items))
        }
        Some("minuteur" | "timer") if rest.is_empty() => {
            let now = po_tui::labs::idle_minutes();
            let mut items: Vec<Item> = [0u64, 5, 10, 15, 30, 60]
                .into_iter()
                .map(|n| Item::run(timer_label(n), format!("{cmd} {} {n}", arg.unwrap_or_default())).current(n == now))
                .collect();
            items.push(Item::fill(t!("menu.other_value"), format!("{cmd} {} ", arg.unwrap_or_default())));
            Some(Menu::new(t!("menu.labs.timer"), items).note(t!("menu.labs.timer_note")))
        }
        _ => None,
    }
}

fn timer_label(minutes: u64) -> String {
    if minutes == 0 { t!("menu.labs.timer_off").to_owned() } else { t!("menu.minutes", n = minutes) }
}

fn resume(cmd: &str, arg: Option<&str>, agent: &Agent, extras: &Extras) -> Option<Menu> {
    if arg.is_some() {
        return None;
    }
    let sessions: Vec<_> = po_agent::SessionStore::default_location()
        .list_for(&extras.workspace)
        .into_iter()
        .filter(|s| Some(&s.id) != agent.session_key.as_ref())
        .take(30)
        .collect();
    if sessions.is_empty() {
        return None;
    }
    let items = sessions
        .iter()
        .enumerate()
        .map(|(i, saved)| {
            let date = saved.saved_at.get(..16).unwrap_or(&saved.saved_at).replace('T', " ");
            Item::run(&saved.title, format!("{cmd} {}", i + 1)).detail(format!("{date} · {}", t!("cmd.resume.exchanges", n = saved.exchange_count())))
        })
        .collect();
    Some(Menu::new(t!("menu.resume"), items))
}

fn shortcut(cmd: &str, arg: Option<&str>, rest: &[&str]) -> Option<Menu> {
    use po_tui::labs::{ACTIONS, shortcut};
    let action = arg.filter(|a| ACTIONS.iter().any(|(name, _)| name == a));
    match action {
        None => Some(Menu::new(
            t!("menu.shortcut"),
            ACTIONS
                .iter()
                .map(|(name, _)| {
                    let what = if *name == "labs" { t!("menu.shortcut.labs") } else { t!("menu.shortcut.pause") };
                    Item::run(what, format!("{cmd} {name}")).detail(shortcut(name).label())
                })
                .collect(),
        )),
        Some(action) if rest.is_empty() => Some(
            Menu::new(
                t!("menu.shortcut"),
                vec![
                    Item::fill(t!("menu.shortcut.keys"), format!("{cmd} {action} ")).detail(t!("menu.shortcut.keys_hint")),
                    Item::run(t!("menu.shortcut.default"), format!("{cmd} {action} {}", w("defaut", "default"))),
                ],
            )
            .note(t!("menu.shortcut.now", keys = shortcut(action).label())),
        ),
        Some(_) => None,
    }
}

fn effort(cmd: &str, arg: Option<&str>, rest: &[&str], agent: &Agent) -> Option<Menu> {
    let level = agent.settings.reasoning_effort.clone();
    match arg.map(str::to_ascii_lowercase).as_deref() {
        Some("budget") if rest.is_empty() => {
            let now = agent.settings.thinking_budget;
            let mut items = vec![
                Item::run(t!("menu.effort.budget_auto"), format!("{cmd} budget auto")).current(now.is_none()),
                Item::run(t!("menu.effort.budget_off"), format!("{cmd} budget off")).current(now == Some(0)),
            ];
            for n in [1024u32, 2048, 4096, 8192, 16384] {
                items.push(Item::run(t!("menu.tokens", n = n), format!("{cmd} budget {n}")).current(now == Some(n)));
            }
            items.push(Item::fill(t!("menu.other_value"), format!("{cmd} budget ")));
            Some(Menu::new(t!("menu.effort.budget"), items))
        }
        Some(a) if a.starts_with("budget") => None,
        Some("defaut" | "défaut" | "default" | "auto" | "none" | "low" | "medium" | "high") => None,
        typed => {
            let mut items = vec![Item::run(t!("menu.server_default"), format!("{cmd} {}", w("defaut", "default"))).current(level.is_none())];
            for (value, label) in [("none", t!("menu.effort.none")), ("low", t!("menu.effort.low")), ("medium", t!("menu.effort.medium")), ("high", t!("menu.effort.high"))] {
                items.push(Item::run(label, format!("{cmd} {value}")).detail(value).current(level.as_deref() == Some(value)));
            }
            items.push(Item::run(t!("menu.effort.budget"), format!("{cmd} budget")));
            let menu = Menu::new(t!("menu.effort"), items);
            Some(match typed {
                Some(typed) => menu.note(t!("menu.unknown", value = typed)),
                None => menu,
            })
        }
    }
}

fn cache(cmd: &str, arg: Option<&str>, rest: &[&str]) -> Option<Menu> {
    let pref = |key: &str| po_i18n::prefs::get_str(key).and_then(|v| v.parse::<u64>().ok());
    match arg {
        None => Some(Menu::new(
            t!("menu.cache"),
            vec![
                Item::show(t!("menu.cache.show"), cmd),
                Item::run(t!("menu.cache.clear"), format!("{cmd} {}", w("vider", "clear"))).detail(t!("menu.cache.clear_hint")),
                Item::run(t!("menu.cache.limit"), format!("{cmd} {}", w("limite", "limit"))).detail(pref("kv_quota_gb").map(|n| t!("menu.gb", n = n)).unwrap_or_default()),
                Item::run(t!("menu.cache.days"), format!("{cmd} {}", w("jours", "days"))).detail(pref("kv_max_days").map(|n| t!("menu.days", n = n)).unwrap_or_default()),
            ],
        )),
        Some(which @ ("limite" | "limit" | "jours" | "days")) if rest.is_empty() => {
            let quota = matches!(which, "limite" | "limit");
            let now = pref(if quota { "kv_quota_gb" } else { "kv_max_days" });
            let values: &[u64] = if quota { &[10, 20, 50, 100, 200] } else { &[1, 3, 7, 14, 30] };
            let mut items: Vec<Item> = values
                .iter()
                .map(|n| Item::run(if quota { t!("menu.gb", n = n) } else { t!("menu.days", n = n) }, format!("{cmd} {which} {n}")).current(now == Some(*n)))
                .collect();
            items.push(Item::fill(t!("menu.other_value"), format!("{cmd} {which} ")));
            Some(Menu::new(if quota { t!("menu.cache.limit") } else { t!("menu.cache.days") }, items))
        }
        _ => None,
    }
}

fn criteria(cmd: &str, arg: Option<&str>, rest: &[&str], agent: &Agent) -> Option<Menu> {
    let list = &agent.settings.criteria;
    let remove_items = |items: &mut Vec<Item>| {
        for (i, text) in list.iter().enumerate() {
            items.push(Item::run(t!("menu.criteria.remove_one", text = text), format!("{cmd} {} {}", w("retirer", "remove"), i + 1)));
        }
    };
    match arg {
        None => {
            let mut items = vec![Item::fill(t!("menu.criteria.add"), format!("{cmd} {} ", w("ajouter", "add"))).detail(t!("menu.criteria.add_hint"))];
            if !list.is_empty() {
                items.push(Item::show(t!("menu.criteria.show"), cmd).detail(t!("menu.count", n = list.len())));
                remove_items(&mut items);
                items.push(Item::run(t!("menu.criteria.clear"), format!("{cmd} {}", w("vider", "clear"))));
            }
            Some(Menu::new(t!("menu.criteria"), items).note(t!("menu.criteria_note")))
        }
        Some("ajouter" | "add" | "+") if rest.is_empty() => Some(Menu::new(
            t!("menu.criteria"),
            vec![Item::fill(t!("menu.criteria.add"), format!("{cmd} {} ", w("ajouter", "add"))).detail(t!("menu.criteria.add_hint"))],
        )),
        Some("retirer" | "remove") if rest.first().and_then(|n| n.parse::<usize>().ok()).is_none_or(|n| n == 0 || n > list.len()) => {
            if list.is_empty() {
                return None;
            }
            let mut items = Vec::new();
            remove_items(&mut items);
            Some(Menu::new(t!("menu.criteria"), items))
        }
        _ => None,
    }
}

fn local_menu(cmd: &str, arg: Option<&str>, rest: &[&str], local: &mut LocalManager) -> Option<Menu> {
    let unload = matches!(arg, Some("decharge" | "décharge" | "unload"));
    if arg.is_some() && !(unload && rest.is_empty()) {
        return None;
    }
    local.refresh();
    let active = local.active_model_id.clone();
    let models: Vec<(usize, String, bool)> = local.catalog.iter().enumerate().map(|(i, e)| (i + 1, e.label(), e.plan.viable)).collect();
    if unload {
        let items = models
            .into_iter()
            .map(|(n, label, _)| {
                let current = active.as_deref() == Some(label.as_str());
                Item::run(label, format!("{cmd} {} {n}", arg.unwrap_or_default())).current(current)
            })
            .collect();
        return Some(Menu::new(t!("menu.local.unload"), items));
    }
    let mut items: Vec<Item> = models
        .into_iter()
        .map(|(n, label, viable)| {
            let current = active.as_deref() == Some(label.as_str());
            let item = Item::run(label, format!("{cmd} {n}")).current(current);
            if viable || current { item } else { item.detail(t!("chat.local.does_not_fit")) }
        })
        .collect();
    items.push(Item::run(t!("menu.local.stop"), format!("{cmd} stop")));
    items.push(Item::show(t!("menu.local.state"), format!("{cmd} {}", w("etat", "status"))));
    if !local.catalog.is_empty() {
        items.push(Item::run(t!("menu.local.unload"), format!("{cmd} {}", w("decharge", "unload"))));
    }
    items.push(Item::fill(t!("menu.local.folder"), format!("{cmd} {} ", w("dossier", "dir"))));
    items.push(Item::fill(t!("menu.local.binary"), format!("{cmd} {} ", w("serveur", "server"))));
    let menu = Menu::new(t!("menu.local"), items);
    Some(if local.catalog.is_empty() { menu.note(t!("cmd.local.none")) } else { menu })
}

fn agents(cmd: &str, arg: Option<&str>, rest: &[&str], agent: &Agent) -> Option<Menu> {
    use po_agent::subagents::Source;
    let profiles = agent.profiles();
    let pick = |sub: &str, title: &str, only_custom: bool| -> Option<Menu> {
        let items: Vec<Item> = profiles
            .iter()
            .filter(|p| !only_custom || !matches!(p.source, Source::Builtin))
            .map(|p| Item::run(&p.name, format!("{cmd} {sub} {}", p.name)).detail(agent.model_choice(p)))
            .collect();
        (!items.is_empty()).then(|| Menu::new(title, items))
    };
    match arg {
        // The panel of subagents opens without an argument.
        None | Some("liste" | "list" | "recharger" | "reload") => None,
        Some(sub @ ("voir" | "show")) if rest.is_empty() => pick(sub, t!("menu.agents.show"), false),
        Some(sub @ ("retirer" | "supprimer" | "remove")) if rest.is_empty() => pick(sub, t!("menu.agents.remove"), true),
        Some(sub @ ("modele" | "modèle" | "model")) => match rest {
            [] => pick(sub, t!("menu.agents.model"), false),
            [name] => {
                let set = agent.subagent_settings.models.get(*name).cloned();
                let mut items = vec![
                    Item::run(t!("menu.agents.model_session"), format!("{cmd} {sub} {name} session")).current(set.is_none()),
                    Item::run(t!("menu.model.auto"), format!("{cmd} {sub} {name} auto")).current(set.as_deref() == Some("auto")),
                ];
                for model in agent.router().models() {
                    items.push(
                        Item::run(&model.id, format!("{cmd} {sub} {name} {}", model.id))
                            .detail(model.origin.label())
                            .current(set.as_deref() == Some(model.id.as_str())),
                    );
                }
                Some(Menu::new(t!("menu.agents.model_of", name = name), items))
            }
            _ => None,
        },
        Some(sub @ ("parallele" | "parallèle" | "parallel")) if rest.is_empty() => {
            let now = agent.subagent_settings.max_parallel;
            let mut items = vec![Item::run(t!("menu.agents.parallel_auto"), format!("{cmd} {sub} auto")).current(now.is_none())];
            for n in [1usize, 2, 3, 4, 6, 8] {
                items.push(Item::run(n.to_string(), format!("{cmd} {sub} {n}")).current(now == Some(n)));
            }
            Some(Menu::new(t!("menu.agents.parallel"), items))
        }
        Some(sub @ ("creer" | "créer" | "create" | "nouveau")) if rest.iter().all(|w| matches!(*w, "global" | "permanent" | "projet" | "project")) => Some(Menu::new(
            t!("menu.agents.create"),
            vec![
                Item::fill(t!("menu.agents.create_project"), format!("{cmd} {sub} {} ", w("projet", "project"))).detail(t!("menu.agents.create_hint")),
                Item::fill(t!("menu.agents.create_global"), format!("{cmd} {sub} global ")).detail(t!("menu.agents.create_hint")),
            ],
        )),
        Some(other) if !matches!(other, "voir" | "show" | "retirer" | "supprimer" | "remove" | "parallele" | "parallèle" | "parallel" | "creer" | "créer" | "create" | "nouveau") => Some(
            Menu::new(
                t!("menu.agents"),
                vec![
                    Item::show(t!("menu.agents.list"), format!("{cmd} {}", w("liste", "list"))),
                    Item::run(t!("menu.agents.show"), format!("{cmd} {}", w("voir", "show"))),
                    Item::run(t!("menu.agents.model"), format!("{cmd} {}", w("modele", "model"))),
                    Item::run(t!("menu.agents.parallel"), format!("{cmd} {}", w("parallele", "parallel"))),
                    Item::run(t!("menu.agents.create"), format!("{cmd} {}", w("creer", "create"))),
                    Item::run(t!("menu.agents.remove"), format!("{cmd} {}", w("retirer", "remove"))),
                    Item::run(t!("menu.agents.reload"), format!("{cmd} {}", w("recharger", "reload"))),
                ],
            )
            .note(t!("menu.unknown", value = other)),
        ),
        _ => None,
    }
}

fn mcp(cmd: &str, arg: Option<&str>, rest: &[&str], extras: &Extras) -> Option<Menu> {
    let connected: Vec<&str> = extras.mcp.iter().map(|e| e.config.name.as_str()).collect();
    let declared: Vec<&str> = extras.mcp_available.iter().map(|c| c.name.as_str()).collect();
    let names = |sub: &str, title: &str, list: &[&str]| -> Option<Menu> {
        (!list.is_empty()).then(|| Menu::new(title, list.iter().map(|n| Item::run(*n, format!("{cmd} {sub} {n}"))).collect()))
    };
    let add = || Item::fill(t!("menu.mcp.add"), format!("{cmd} {} ", w("ajouter", "add"))).detail(t!("menu.mcp.add_hint"));
    match arg {
        None => {
            let mut items = vec![Item::show(t!("menu.mcp.list"), cmd).detail(t!("menu.mcp.counts", on = connected.len(), off = declared.len()))];
            if !declared.is_empty() {
                items.push(Item::run(t!("menu.mcp.enable"), format!("{cmd} {}", w("activer", "enable"))));
            }
            items.push(add());
            if !connected.is_empty() {
                items.push(Item::run(t!("menu.mcp.keep"), format!("{cmd} {}", w("garder", "keep"))));
                items.push(Item::run(t!("menu.mcp.remove"), format!("{cmd} {}", w("retirer", "remove"))));
            }
            Some(Menu::new(t!("menu.mcp"), items))
        }
        Some(sub @ ("activer" | "enable" | "lancer")) if rest.is_empty() => names(sub, t!("menu.mcp.enable"), &declared),
        Some(sub @ ("garder" | "keep" | "save")) if rest.is_empty() => names(sub, t!("menu.mcp.keep"), &connected),
        Some(sub @ ("retirer" | "remove")) if rest.is_empty() => names(sub, t!("menu.mcp.remove"), &connected),
        Some("ajouter" | "add") if rest.len() < 2 => Some(Menu::new(t!("menu.mcp"), vec![add()])),
        _ => None,
    }
}

async fn voice(cmd: &str, arg: Option<&str>, rest: &[&str]) -> Option<Menu> {
    let settings = po_tui::voice::Settings::load();
    match arg {
        None => {
            let on = settings.enabled;
            Some(
                Menu::new(
                    t!("menu.voice"),
                    vec![
                        Item::run(t!("menu.voice.on"), format!("{cmd} on")).current(on),
                        Item::run(t!("menu.voice.off"), format!("{cmd} off")).current(!on),
                        Item::show(t!("menu.voice.state"), cmd),
                        Item::run(t!("menu.voice.install"), format!("{cmd} {}", w("installer", "install"))),
                        Item::run(t!("menu.voice.engine"), format!("{cmd} {}", w("moteur", "engine")))
                            .detail(settings.gpu.map_or_else(|| "CPU".to_owned(), |g| format!("GPU {g}"))),
                        Item::run(t!("menu.voice.model"), format!("{cmd} {}", w("modele", "model"))).detail(&settings.model),
                        Item::run(t!("menu.voice.mic"), format!("{cmd} micro")).detail(settings.microphone.clone().unwrap_or_else(|| t!("cmd.voice.automatic").to_owned())),
                        Item::run(t!("menu.voice.language"), format!("{cmd} {}", w("langue", "language"))).detail(&settings.language),
                        Item::fill(t!("menu.voice.test"), format!("{cmd} {} ", w("tester", "test"))).detail(t!("menu.voice.test_hint")),
                    ],
                ),
            )
        }
        Some(sub @ ("moteur" | "engine")) if rest.is_empty() || !matches!(rest[0], "cpu" | "processeur" | "processor" | "gpu") => Some(Menu::new(
            t!("menu.voice.engine"),
            vec![
                Item::run(t!("menu.voice.cpu"), format!("{cmd} {sub} cpu")).current(settings.gpu.is_none()),
                Item::run(t!("menu.voice.gpu_auto"), format!("{cmd} {sub} gpu")).current(settings.gpu.is_some()).detail(settings.gpu.map(|g| format!("GPU {g}")).unwrap_or_default()),
                Item::fill(t!("menu.voice.gpu_n"), format!("{cmd} {sub} gpu ")),
            ],
        )),
        Some(sub @ ("modele" | "modèle" | "model")) if !matches!(rest.first(), Some(&("small" | "base" | "turbo"))) => Some(Menu::new(
            t!("menu.voice.model"),
            [("small", t!("menu.voice.small")), ("base", t!("menu.voice.base")), ("turbo", t!("menu.voice.turbo"))]
                .into_iter()
                .map(|(m, what)| Item::run(m, format!("{cmd} {sub} {m}")).detail(what).current(settings.model == m))
                .collect(),
        )),
        Some(sub @ ("langue" | "language")) if rest.is_empty() => Some(Menu::new(
            t!("menu.voice.language"),
            [("fr", "Français"), ("en", "English"), ("auto", t!("menu.voice.lang_auto"))]
                .into_iter()
                .map(|(code, name)| Item::run(name, format!("{cmd} {sub} {code}")).current(settings.language == code))
                .collect(),
        )),
        Some("micro") if rest.is_empty() => {
            let mics = po_tui::voice::microphones().await;
            let mut items = vec![Item::run(t!("cmd.voice.automatic"), format!("{cmd} micro auto")).current(settings.microphone.is_none())];
            for mic in mics {
                let current = settings.microphone.as_deref() == Some(mic.as_str());
                items.push(Item::run(&mic, format!("{cmd} micro {mic}")).current(current));
            }
            Some(Menu::new(t!("menu.voice.mic"), items))
        }
        Some(sub @ ("tester" | "test")) if rest.is_empty() => Some(Menu::new(t!("menu.voice"), vec![Item::fill(t!("menu.voice.test"), format!("{cmd} {sub} ")).detail(t!("menu.voice.test_hint"))])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_given_and_known_opens_no_menu() {
        assert!(choose(Some("on"), true, || Menu::new("t", Vec::new())).is_none());
    }

    #[test]
    fn a_missing_choice_opens_the_menu_and_an_unknown_one_says_so() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        assert!(choose(None, false, || Menu::new("t", Vec::new())).unwrap().note.is_none());
        let menu = choose(Some("zzz"), false, || Menu::new("t", Vec::new())).unwrap();
        assert!(menu.note.unwrap().contains("zzz"));
    }

    #[test]
    fn labs_offers_on_off_and_its_timer() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let timer = labs("/labs", Some("minuteur"), &[]).unwrap();
        assert!(timer.items.iter().any(|i| i.action == po_tui::menu::Action::Run("/labs minuteur 15".into())));
        assert!(labs("/labs", Some("on"), &[]).is_none());
        assert!(labs("/labs", Some("minuteur"), &["10"]).is_none());
    }

    #[test]
    fn a_shortcut_offers_its_actions_then_its_keys() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let first = shortcut("/raccourci", None, &[]).unwrap();
        assert_eq!(first.items.len(), po_tui::labs::ACTIONS.len());
        let second = shortcut("/raccourci", Some("labs"), &[]).unwrap();
        assert_eq!(second.items[0].action, po_tui::menu::Action::Fill("/raccourci labs ".into()));
        assert!(shortcut("/raccourci", Some("labs"), &["ctrl+e"]).is_none());
    }

    #[test]
    fn the_cache_limits_offer_values_and_a_free_one() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let menu = cache("/cache", Some("limit"), &[]).unwrap();
        assert!(matches!(menu.items.last().unwrap().action, po_tui::menu::Action::Fill(_)));
        assert!(cache("/cache", Some("limit"), &["50"]).is_none());
        assert!(cache("/cache", Some("clear"), &[]).is_none());
    }
}
