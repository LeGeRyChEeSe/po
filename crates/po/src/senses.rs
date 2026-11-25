//! What the session's host perceives that the agent cannot: where the user is, whether a phone is
//! looking, which other models sit in this PC's memory. See `po_agent::awareness`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use po_agent::awareness::{Interface, Senses};

/// How long the list of loaded models is trusted: asking every server at each turn would delay
/// the model, and what is loaded rarely changes within half a minute.
const MODELS_TTL: Duration = Duration::from_secs(30);
/// The servers answer in a few hundred milliseconds or not at all; a turn never waits longer.
const MODELS_PATIENCE: Duration = Duration::from_millis(1500);

pub struct HostSenses {
    interface: Interface,
    models: Mutex<Option<(Instant, Vec<String>)>>,
    hints: Mutex<std::collections::HashMap<String, Vec<String>>>,
}

impl HostSenses {
    pub fn new(interface: Interface) -> Self {
        Self { interface, models: Mutex::new(None), hints: Mutex::default() }
    }

    fn cached(&self) -> Option<Vec<String>> {
        let known = self.models.lock().ok()?;
        known.as_ref().filter(|(at, _)| at.elapsed() < MODELS_TTL).map(|(_, list)| list.clone())
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

    async fn other_models(&self, own: &str) -> Vec<String> {
        let list = match self.cached() {
            Some(list) => list,
            None => {
                let fetched = tokio::time::timeout(MODELS_PATIENCE, crate::running::loaded())
                    .await
                    .map(|models| models.into_iter().map(|m| m.id).collect::<Vec<_>>())
                    // Slow or silent servers: the last list stays, better than nothing.
                    .ok();
                let mut known = self.models.lock().unwrap_or_else(|e| e.into_inner());
                let list = fetched.or_else(|| known.as_ref().map(|(_, l)| l.clone())).unwrap_or_default();
                *known = Some((Instant::now(), list.clone()));
                list
            }
        };
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
    fn the_list_of_models_is_kept_for_a_while() {
        let senses = HostSenses::new(Interface::Terminal);
        assert!(senses.cached().is_none());
        *senses.models.lock().unwrap() = Some((Instant::now(), vec!["a".into()]));
        assert_eq!(senses.cached(), Some(vec!["a".to_owned()]));
        *senses.models.lock().unwrap() = Some((Instant::now() - MODELS_TTL - Duration::from_secs(1), vec!["a".into()]));
        assert!(senses.cached().is_none(), "stale");
    }

    #[test]
    fn the_interface_is_what_the_host_was_given() {
        assert_eq!(HostSenses::new(Interface::Phone).interface(), Interface::Phone);
    }
}
