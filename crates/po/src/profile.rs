//! `/profil` (`/profile`): the settings of the model the session uses, kept for that model —
//! its own instructions added to the system prompt, its sampling, the length of its answers,
//! and for a model Po starts, its window and cache. Each model keeps its own: switching model
//! switches settings.

use po_agent::Agent;
use po_i18n::t;
use po_local::prefs::{LocalPrefs, ModelProfile};
use po_tui::menu::{Item, Menu};

use crate::commands::Reply;
use crate::engines_ui::w;

/// The settings, in the menu's order: key, label, and whether only a model Po starts reads it.
fn keys() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        ("system", t!("profile.key.system"), false),
        ("reasoning", t!("engines.profile.reasoning"), false),
        ("temperature", t!("profile.key.temperature"), false),
        ("top_p", "top_p", false),
        ("top_k", "top_k", false),
        ("min_p", "min_p", false),
        ("repeat_penalty", t!("profile.key.repeat"), false),
        ("max_output", t!("profile.key.max_output"), false),
        ("context", t!("engines.profile.context"), true),
        ("cache", t!("engines.profile.cache"), true),
        ("preset", t!("profile.key.preset"), true),
        ("template", t!("profile.key.template"), true),
    ]
}

fn value(profile: &ModelProfile, key: &str) -> Option<String> {
    match key {
        "system" => profile.system.clone(),
        "reasoning" => profile.reasoning.clone(),
        "temperature" => profile.temperature.map(|v| v.to_string()),
        "top_p" => profile.top_p.map(|v| v.to_string()),
        "top_k" => profile.top_k.map(|v| v.to_string()),
        "min_p" => profile.min_p.map(|v| v.to_string()),
        "repeat_penalty" => profile.repeat_penalty.map(|v| v.to_string()),
        "max_output" => profile.max_output.map(|v| format!("{}k", v / 1024)),
        "context" => profile.context.map(|c| format!("{}k", c / 1024)),
        "cache" => profile.cache.clone(),
        "preset" => profile.preset.clone(),
        "template" => profile.template.as_deref().map(|t| if t == "depot" { w("dépôt", "repo").to_owned() } else { t.to_owned() }),
        _ => None,
    }
}

/// The cards it runs on, with their names.
fn cards(profile: &ModelProfile) -> String {
    match &profile.placement {
        Some(placement) => placement.describe(&po_local::Hardware::detect()),
        None => t!("engines.profile.auto").to_owned(),
    }
}

fn choices(key: &str) -> Vec<String> {
    if key == "preset" {
        // The user's own files: none, nothing to choose but standard.
        return po_local::strata::presets().into_iter().map(|(name, _)| name).collect();
    }
    let fixed: Vec<&'static str> = match key {
        "reasoning" => vec!["auto", "off", "on", "low", "medium", "high"],
        "temperature" => vec!["0", "0.2", "0.6", "0.7", "1.0"],
        "top_p" => vec!["0.8", "0.9", "0.95", "1.0"],
        "top_k" => vec!["0", "20", "40", "64"],
        "min_p" => vec!["0", "0.05", "0.1"],
        "repeat_penalty" => vec!["1.0", "1.05", "1.1", "1.2"],
        "max_output" => vec!["2k", "4k", "8k", "16k", "32k"],
        "context" => vec!["8k", "16k", "32k", "64k", "128k", "256k"],
        "cache" => vec!["f16", "q8_0", "q4_0", "int8", "k8v4"],
        "template" => vec![w("dépôt", "repo")],
        _ => Vec::new(),
    };
    fixed.into_iter().map(str::to_owned).collect()
}

/// The model the settings are for: the one pinned, else the one answering, else the first.
fn current(agent: &Agent) -> Option<(String, bool)> {
    let id = agent.router().pinned().map(str::to_owned).or_else(|| agent.session_model().map(|m| m.id)).or_else(|| agent.router().models().first().map(|m| m.id.clone()))?;
    let local = agent.router().get(&id).is_some_and(|m| m.origin.is_local());
    Some((id, local))
}


fn find_profile<'a>(prefs: &'a LocalPrefs, model: &str, target: &str) -> Option<(&'a String, &'a ModelProfile)> {
    let lower_target = target.to_lowercase();
    // 1. Exact match in prefs.models
    if let Some((k, p)) = prefs.models.get_key_value(target) {
        return Some((k, p));
    }
    // 2. Exact match with model prefix: format!("{model}-{target}")
    let model_target = format!("{model}-{target}");
    if let Some((k, p)) = prefs.models.get_key_value(&model_target) {
        return Some((k, p));
    }
    // 3. Suffix match on model: e.g. "dev" -> "{model}-dev", "retro" -> "{model}-retro-ingenieur"
    let model_prefix = format!("{}-", model.to_lowercase());
    for (k, p) in &prefs.models {
        let kl = k.to_lowercase();
        if kl.starts_with(&model_prefix) {
            let suffix = &kl[model_prefix.len()..];
            if suffix == lower_target || suffix.starts_with(&lower_target) {
                return Some((k, p));
            }
        }
    }
    // 4. Any key in prefs.models containing target
    for (k, p) in &prefs.models {
        let kl = k.to_lowercase();
        if kl == lower_target || kl.ends_with(&format!("-{lower_target}")) || kl.contains(&lower_target) {
            return Some((k, p));
        }
    }
    None
}

fn list_available_profiles(prefs: &LocalPrefs, model: &str) -> Vec<(String, String)> {
    let mut list = Vec::new();
    let model_prefix = format!("{}-", model.to_lowercase());
    for (name, prof) in &prefs.models {
        if name == model {
            continue;
        }
        let label = if name.to_lowercase().starts_with(&model_prefix) {
            name[model_prefix.len()..].to_string()
        } else {
            name.clone()
        };
        let mut desc = Vec::new();
        if let Some(r) = &prof.reasoning {
            desc.push(format!("reasoning: {r}"));
        }
        if let Some(t) = prof.temperature {
            desc.push(format!("temp: {t}"));
        }
        if let Some(m) = prof.max_output {
            desc.push(format!("max: {}k", m / 1024));
        }
        if prof.system.is_some() {
            desc.push("system custom".to_string());
        }
        list.push((label, if desc.is_empty() { "-".into() } else { desc.join(", ") }));
    }
    list
}

pub fn command(agent: &mut Agent, words: &[&str], reply: &mut Reply) {
    let Some((model, local)) = current(agent) else {
        reply.warn(t!("profile.no_model"));
        return;
    };
    match words {
        [] => {
            let profile = LocalPrefs::load().model(&model);
            reply.section(t!("profile.title", model = model));
            for (key, label, launch) in keys() {
                if launch && !local {
                    continue;
                }
                reply.field(label, value(&profile, key).unwrap_or_else(|| t!("engines.profile.auto").to_owned()));
            }
            if local {
                reply.field(t!("profile.key.cards"), cards(&profile));
            }
            reply.dim(t!("profile.how"));
        }
        [action] if matches!(*action, "liste" | "list" | "profils" | "profiles") => {
            let prefs = LocalPrefs::load();
            let available = list_available_profiles(&prefs, &model);
            if available.is_empty() {
                reply.dim(t!("profile.list_empty"));
                return;
            }
            reply.section(t!("profile.list_title", n = available.len()));
            for (name, desc) in available {
                reply.item(&name, &desc);
            }
            reply.dim(t!("profile.how"));
        }
        [action, name] if matches!(*action, "charger" | "load" | "appliquer" | "apply") => {
            let mut prefs = LocalPrefs::load();
            if let Some((full_name, profile)) = find_profile(&prefs, &model, name) {
                let full_name = full_name.clone();
                let loaded = profile.clone();
                *prefs.model_mut(&model) = loaded.clone();
                if let Err(err) = prefs.save() {
                    reply.warn(t!("cmd.local.pref_failed", err = err));
                    return;
                }
                crate::engines_ops::apply_session_defaults(agent, &prefs);
                reply.accent(t!("profile.loaded", name = full_name, model = model));
                if let Some(r) = &loaded.reasoning { reply.field("reasoning", r); }
                if let Some(t) = loaded.temperature { reply.field("temperature", &t.to_string()); }
                if let Some(m) = loaded.max_output { reply.field("max_output", &format!("{}k", m / 1024)); }
                if let Some(s) = &loaded.system {
                    let first_line = s.lines().next().unwrap_or("").trim();
                    reply.field("system", first_line);
                }
            } else {
                reply.warn(t!("profile.not_found", name = name));
            }
        }
        [action, name] if matches!(*action, "sauver" | "save" | "sauvegarder") => {
            let mut prefs = LocalPrefs::load();
            let current_profile = prefs.model(&model);
            let save_key = if name.starts_with(&model) {
                name.to_string()
            } else {
                format!("{model}-{name}")
            };
            prefs.models.insert(save_key.clone(), current_profile);
            if let Err(err) = prefs.save() {
                reply.warn(t!("cmd.local.pref_failed", err = err));
                return;
            }
            reply.accent(t!("profile.saved_as", name = save_key));
        }
        [reset] if matches!(*reset, "defaut" | "défaut" | "default" | "reset") => {
            let mut prefs = LocalPrefs::load();
            prefs.models.remove(&model);
            if let Err(err) = prefs.save() {
                reply.warn(t!("cmd.local.pref_failed", err = err));
                return;
            }
            crate::engines_ops::apply_session_defaults(agent, &prefs);
            reply.accent(t!("engines.profile.reset", model = model));
        }
        [single] if !keys().iter().any(|(k, ..)| k == single) => {
            let mut prefs = LocalPrefs::load();
            if let Some((full_name, profile)) = find_profile(&prefs, &model, single) {
                let full_name = full_name.clone();
                let loaded = profile.clone();
                *prefs.model_mut(&model) = loaded.clone();
                if let Err(err) = prefs.save() {
                    reply.warn(t!("cmd.local.pref_failed", err = err));
                    return;
                }
                crate::engines_ops::apply_session_defaults(agent, &prefs);
                reply.accent(t!("profile.loaded", name = full_name, model = model));
                if let Some(r) = &loaded.reasoning { reply.field("reasoning", r); }
                if let Some(t) = loaded.temperature { reply.field("temperature", &t.to_string()); }
                if let Some(m) = loaded.max_output { reply.field("max_output", &format!("{}k", m / 1024)); }
                if let Some(s) = &loaded.system {
                    let first_line = s.lines().next().unwrap_or("").trim();
                    reply.field("system", first_line);
                }
                return;
            }
            let text = "";
            let mut profile = prefs.model(&model);
            if !profile.set(single, text) {
                reply.warn(t!("engines.profile.refused", key = single, value = text));
                return;
            }
            *prefs.model_mut(&model) = profile;
            if let Err(err) = prefs.save() {
                reply.warn(t!("cmd.local.pref_failed", err = err));
                return;
            }
            crate::engines_ops::apply_session_defaults(agent, &prefs);
            reply.accent(t!("profile.saved", model = model));
        }
        [key, rest @ ..] => {
            let text = rest.join(" ");
            let mut prefs = LocalPrefs::load();
            let mut profile = prefs.model(&model);
            if !profile.set(key, &text) {
                reply.warn(t!("engines.profile.refused", key = key, value = text));
                return;
            }
            *prefs.model_mut(&model) = profile;
            if let Err(err) = prefs.save() {
                reply.warn(t!("cmd.local.pref_failed", err = err));
                return;
            }
            crate::engines_ops::apply_session_defaults(agent, &prefs);
            reply.accent(t!("profile.saved", model = model));
            if matches!(*key, "context" | "contexte" | "cache" | "kv" | "preset" | "préréglage" | "template") {
                reply.dim(t!("engines.profile.when"));
            }
        }
    }
}

pub fn menu(cmd: &str, words: &[&str], agent: &Agent) -> Option<Menu> {
    let (model, local) = current(agent)?;
    let prefs = LocalPrefs::load();
    let profile = prefs.model(&model);
    match words {
        [] => {
            let mut items = Vec::new();
            let available = list_available_profiles(&prefs, &model);
            if !available.is_empty() {
                items.push(Item::run(t!("profile.load_menu"), format!("{cmd} {}", w("charger", "load"))).detail(format!("{} disponibles", available.len())));
            }
            items.push(Item::fill(t!("profile.save_menu"), format!("{cmd} {} ", w("sauver", "save"))));
            for (key, label, launch) in keys() {
                if launch && !local {
                    continue;
                }
                let shown = value(&profile, key).unwrap_or_else(|| if key == "preset" { t!("setup.preset_standard") } else { t!("engines.profile.auto") }.to_owned());
                let item = if key == "system" {
                    Item::fill(label, format!("{cmd} system ")).detail(shown)
                } else {
                    Item::run(label, format!("{cmd} {key}")).detail(shown)
                };
                items.push(item);
            }
            if local {
                items.push(Item::run(t!("profile.key.cards"), format!("{} {} {model}", w("/configurer", "/setup"), w("cartes", "cards"))).detail(cards(&profile)));
            }
            if !profile.is_empty() {
                items.push(Item::run(t!("engines.model.reset"), format!("{cmd} {}", w("defaut", "default"))).then(cmd.to_owned()));
            }
            Some(Menu::new(t!("profile.title", model = model), items).note(t!("profile.note")))
        }
        [action] if matches!(*action, "charger" | "load") => {
            let available = list_available_profiles(&prefs, &model);
            let back = cmd.to_owned();
            let mut items = Vec::new();
            for (name, desc) in available {
                items.push(Item::run(name.clone(), format!("{cmd} charger {name}")).detail(desc).then(back.clone()));
            }
            Some(Menu::new(t!("profile.load_menu"), items).back(back))
        }
        [key] if *key != "system" && (*key == "preset" || !choices(key).is_empty()) => {
            let now = value(&profile, key);
            let back = cmd.to_owned();
            let none = if *key == "preset" { t!("setup.preset_standard") } else { t!("engines.profile.auto") };
            let mut items = vec![Item::run(none, format!("{cmd} {key} {}", w("defaut", "default"))).current(now.is_none()).then(back.clone())];
            for choice in choices(key) {
                let current = now.as_deref() == Some(choice.as_str());
                items.push(Item::run(choice.clone(), format!("{cmd} {key} {choice}")).current(current).then(back.clone()));
            }
            items.push(Item::fill(t!("menu.other_value"), format!("{cmd} {key} ")));
            let label = keys().into_iter().find(|(k, ..)| k == key).map(|(_, l, _)| l).unwrap_or(key);
            Some(Menu::new(format!("{model} · {label}"), items).back(back))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_keeps_its_own_instructions_and_sampling() {
        let mut profile = ModelProfile::default();
        assert!(profile.set("system", "Answer in French, briefly."));
        assert!(profile.set("repeat_penalty", "1.1"));
        assert!(profile.set("max_output", "8k"));
        assert!(!profile.set("max_output", "12"), "too small");
        assert_eq!(value(&profile, "system").as_deref(), Some("Answer in French, briefly."));
        assert_eq!(value(&profile, "max_output").as_deref(), Some("8k"));
        let defaults = crate::engines_ops::session_defaults(&profile);
        assert_eq!(defaults.system.as_deref(), Some("Answer in French, briefly."));
        assert_eq!(defaults.max_output_tokens, Some(8192));
        assert!(profile.set("system", "defaut"));
        assert!(profile.system.is_none());
    }

    #[test]
    fn find_profile_matches_exact_prefixed_and_suffix() {
        let mut prefs = LocalPrefs::default();
        let mut dev_prof = ModelProfile::default();
        dev_prof.reasoning = Some("low".into());
        let mut retro_prof = ModelProfile::default();
        retro_prof.reasoning = Some("high".into());

        prefs.models.insert("qwen-dev".into(), dev_prof.clone());
        prefs.models.insert("qwen-retro-ingenieur".into(), retro_prof.clone());

        // 1. Suffix match
        let (name1, p1) = find_profile(&prefs, "qwen", "dev").expect("should match dev");
        assert_eq!(name1, "qwen-dev");
        assert_eq!(p1.reasoning.as_deref(), Some("low"));

        // 2. Partial suffix match
        let (name2, p2) = find_profile(&prefs, "qwen", "retro").expect("should match retro");
        assert_eq!(name2, "qwen-retro-ingenieur");
        assert_eq!(p2.reasoning.as_deref(), Some("high"));

        // 3. Exact full match
        let (name3, _) = find_profile(&prefs, "qwen", "qwen-retro-ingenieur").expect("exact match");
        assert_eq!(name3, "qwen-retro-ingenieur");

        // 4. Missing
        assert!(find_profile(&prefs, "qwen", "nonexistent").is_none());

        // 5. List available profiles
        let list = list_available_profiles(&prefs, "qwen");
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|(name, _)| name == "dev"));
        assert!(list.iter().any(|(name, _)| name == "retro-ingenieur"));
    }
}
