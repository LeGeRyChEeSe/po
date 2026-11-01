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

fn choices(key: &str) -> Vec<&'static str> {
    match key {
        "reasoning" => vec!["auto", "off", "on", "low", "medium", "high"],
        "temperature" => vec!["0", "0.2", "0.6", "0.7", "1.0"],
        "top_p" => vec!["0.8", "0.9", "0.95", "1.0"],
        "top_k" => vec!["0", "20", "40", "64"],
        "min_p" => vec!["0", "0.05", "0.1"],
        "repeat_penalty" => vec!["1.0", "1.05", "1.1", "1.2"],
        "max_output" => vec!["2k", "4k", "8k", "16k", "32k"],
        "context" => vec!["8k", "16k", "32k", "64k", "128k", "256k"],
        "cache" => vec!["f16", "q8_0", "q4_0", "int8", "k8v4"],
        _ => Vec::new(),
    }
}

/// The model the settings are for: the one pinned, else the one answering, else the first.
fn current(agent: &Agent) -> Option<(String, bool)> {
    let id = agent.router().pinned().map(str::to_owned).or_else(|| agent.session_model().map(|m| m.id)).or_else(|| agent.router().models().first().map(|m| m.id.clone()))?;
    let local = agent.router().get(&id).is_some_and(|m| m.origin.is_local());
    Some((id, local))
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
            if matches!(*key, "context" | "contexte" | "cache" | "kv") {
                reply.dim(t!("engines.profile.when"));
            }
        }
    }
}

pub fn menu(cmd: &str, words: &[&str], agent: &Agent) -> Option<Menu> {
    let (model, local) = current(agent)?;
    let profile = LocalPrefs::load().model(&model);
    match words {
        [] => {
            let mut items = Vec::new();
            for (key, label, launch) in keys() {
                if launch && !local {
                    continue;
                }
                let shown = value(&profile, key).unwrap_or_else(|| t!("engines.profile.auto").to_owned());
                let item = if key == "system" {
                    // Free text: the input takes it, the current one shown.
                    Item::fill(label, format!("{cmd} system ")).detail(shown)
                } else {
                    Item::run(label, format!("{cmd} {key}")).detail(shown)
                };
                items.push(item);
            }
            // Where it runs: the setup's cards screen, which starts it there.
            if local {
                items.push(Item::run(t!("profile.key.cards"), format!("{} {} {model}", w("/configurer", "/setup"), w("cartes", "cards"))).detail(cards(&profile)));
            }
            if !profile.is_empty() {
                items.push(Item::run(t!("engines.model.reset"), format!("{cmd} {}", w("defaut", "default"))).then(cmd.to_owned()));
            }
            Some(Menu::new(t!("profile.title", model = model), items).note(t!("profile.note")))
        }
        [key] if *key != "system" && !choices(key).is_empty() => {
            let now = value(&profile, key);
            let back = cmd.to_owned();
            let mut items = vec![Item::run(t!("engines.profile.auto"), format!("{cmd} {key} {}", w("defaut", "default"))).current(now.is_none()).then(back.clone())];
            for choice in choices(key) {
                items.push(Item::run(choice, format!("{cmd} {key} {choice}")).current(now.as_deref() == Some(choice)).then(back.clone()));
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
}
