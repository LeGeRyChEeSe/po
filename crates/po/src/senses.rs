//! What the session's host perceives that the agent cannot: where the user is, whether a phone is
//! looking, which other models sit in this PC's memory. See `po_agent::awareness`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use po_agent::awareness::{Interface, Senses};

/// How long the list of loaded models is trusted. What is loaded rarely changes, and a turn
/// never waits for the servers: the list is refreshed in the background.
const MODELS_TTL: Duration = Duration::from_secs(120);
/// The servers answer in a few hundred milliseconds or not at all.
const MODELS_PATIENCE: Duration = Duration::from_millis(1500);

/// The last list of loaded models that was really read, and whether a look is under way.
#[derive(Default)]
struct ModelsCache {
    list: Vec<String>,
    /// When the list was read; `None`: never.
    at: Option<Instant>,
    refreshing: bool,
}

impl ModelsCache {
    /// A refresh is due, and none is under way: takes the right to make it.
    fn start_refresh(&mut self) -> bool {
        let stale = self.at.is_none_or(|at| at.elapsed() >= MODELS_TTL);
        if stale && !self.refreshing {
            self.refreshing = true;
            return true;
        }
        false
    }

    /// The look ended: `Some(list)` read, `None` failed (the old list stays, and stays stale).
    fn finish_refresh(&mut self, result: Option<Vec<String>>) {
        self.refreshing = false;
        if let Some(list) = result {
            self.list = list;
            self.at = Some(Instant::now());
        }
    }
}

pub struct HostSenses {
    interface: Interface,
    models: Arc<Mutex<ModelsCache>>,
    hints: Mutex<std::collections::HashMap<String, Vec<String>>>,
}

impl HostSenses {
    pub fn new(interface: Interface) -> Self {
        Self { interface, models: Arc::default(), hints: Mutex::default() }
    }
}

/// Whether a loaded model is the session's own: servers name a model by its file or its id, the
/// session by its router id, and one often contains the other.
pub fn is_own(loaded: &str, own: &str) -> bool {
    let (loaded, own) = (loaded.to_lowercase(), own.to_lowercase());
    !own.is_empty() && (loaded == own || loaded.contains(&own) || own.contains(&loaded))
}

#[async_trait]
impl Senses for HostSenses {
    fn interface(&self) -> Interface {
        self.interface
    }

    fn phone_connected(&self) -> bool {
        po_tui::control::mirror().followers() > 0
    }

    fn friction_hints(&self, model: &str) -> Vec<String> {
        let mut known = self.hints.lock().unwrap_or_else(|e| e.into_inner());
        // Read once per model: what recurred across sessions does not change within one.
        known.entry(model.to_owned()).or_insert_with(|| crate::friction::hints(model)).clone()
    }

    /// The last list read, at once: the servers are asked in a task of their own, never on the
    /// way to the model. The first turn may therefore not know yet; a later one tells.
    async fn other_models(&self, own: &str) -> Vec<String> {
        let (list, refresh) = {
            let mut cache = self.models.lock().unwrap_or_else(|e| e.into_inner());
            (cache.list.clone(), cache.start_refresh())
        };
        if refresh {
            let cache = self.models.clone();
            tokio::spawn(async move {
                let read = tokio::time::timeout(MODELS_PATIENCE, crate::running::loaded())
                    .await
                    .ok()
                    .map(|models| models.into_iter().map(|m| m.id).collect::<Vec<_>>());
                cache.lock().unwrap_or_else(|e| e.into_inner()).finish_refresh(read);
            });
        }
        list.into_iter().filter(|id| !is_own(id, own)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sessions_own_model_is_not_another_one() {
        assert!(is_own("Qwen3.5-9B-Q4_K_M", "qwen3.5-9b-q4_k_m"));
        assert!(is_own("Qwen3.5-9B", "Qwen3.5-9B-Q4_K_M"), "a server may drop the quantization");
        assert!(!is_own("Gemma-4B", "Qwen3.5-9B"));
        assert!(!is_own("anything", ""), "no model of its own: every loaded one is another");
    }

    #[test]
    fn the_list_is_refreshed_once_in_the_background_and_a_failed_look_does_not_freshen_it() {
        let mut cache = ModelsCache::default();
        assert!(cache.start_refresh(), "never read: due");
        assert!(!cache.start_refresh(), "one look at a time");
        cache.finish_refresh(None);
        assert!(cache.at.is_none() && cache.list.is_empty());
        assert!(cache.start_refresh(), "a failed look is tried again");
        cache.finish_refresh(Some(vec!["a".into()]));
        assert_eq!(cache.list, ["a"]);
        assert!(!cache.start_refresh(), "fresh");
        cache.at = Some(Instant::now() - MODELS_TTL - Duration::from_secs(1));
        assert!(cache.start_refresh(), "stale");
        cache.finish_refresh(None);
        assert_eq!(cache.list, ["a"], "the old list stays, and is still stale");
        assert!(cache.start_refresh());
    }

    #[tokio::test]
    async fn asking_for_the_models_never_waits_for_the_servers() {
        let senses = HostSenses::new(Interface::Terminal);
        {
            let mut cache = senses.models.lock().unwrap();
            cache.list = vec!["Gemma-4B".into(), "Qwen-9B".into()];
            cache.at = Some(Instant::now());
        }
        // Fresh: answered from the cache, own model left out, nothing started.
        assert_eq!(senses.other_models("qwen-9b").await, ["Gemma-4B"]);
        assert!(!senses.models.lock().unwrap().refreshing);
    }

    #[test]
    fn the_interface_is_what_the_host_was_given() {
        assert_eq!(HostSenses::new(Interface::Phone).interface(), Interface::Phone);
    }
}
