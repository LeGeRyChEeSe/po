//! Driving a llama.cpp server.
//!
//! Since 2026, `llama-server` can run as a *router*: several models, each in its own process,
//! loaded and unloaded on the fly, with requests dispatched by the `model` field. Po builds on
//! this mechanism instead of reimplementing it, and adds what it lacks - starting the server
//! with hardware-derived parameters, and a memory retention policy.
//!
//! As elsewhere in this crate, response parsing is kept separate from transport: the logic is
//! testable without a server, and the day the format moves, only one place changes.

use std::collections::HashMap;

use serde::Deserialize;

/// State of a model from the router's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelStatus {
    /// Loaded in memory, ready to answer immediately.
    Loaded,
    /// Loading in progress.
    Loading,
    /// Known to the router but not in memory.
    Unloaded,
    /// Loaded but asleep: memory is still reserved, resuming is fast.
    Sleeping,
    /// The last load failed; retrying without fixing the cause will fail again.
    Failed,
}

impl ModelStatus {
    pub fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "loaded" | "ready" => Self::Loaded,
            "loading" | "starting" => Self::Loading,
            "sleeping" | "idle" => Self::Sleeping,
            "failed" | "error" => Self::Failed,
            // An unknown state is treated as "not in memory": at worst we request a load that
            // will be a no-op, and we never believe available what is not.
            _ => Self::Unloaded,
        }
    }

    /// Can answer a request without waiting for a load.
    pub fn is_ready(self) -> bool {
        self == Self::Loaded
    }

    /// Occupies memory, and can therefore be unloaded to free some.
    pub fn holds_memory(self) -> bool {
        matches!(self, Self::Loaded | Self::Loading | Self::Sleeping)
    }
}

#[derive(Debug, Clone)]
pub struct RemoteModel {
    pub id: String,
    pub status: ModelStatus,
    /// Context window announced by the server, when it gives one.
    pub context_length: Option<u32>,
}

/// Shape of the entries returned by `/v1/models`. Fields vary with the server mode (router,
/// single-model, or strict OpenAI legacy), hence options everywhere.
#[derive(Debug, Deserialize)]
struct RawModel {
    id: String,
    #[serde(default)]
    status: Option<RawStatus>,
    #[serde(default)]
    state: Option<RawStatus>,
    #[serde(default)]
    meta: Option<RawMeta>,
}

/// A model's state takes two forms depending on the server version: a plain string
/// (`"loaded"`) on the oldest implementations, a structured object
/// (`{"value": "unloaded", "args": [...]}`) on llama.cpp's router. Accepting both avoids
/// treating a whole fleet as "unloaded" because of a mere change of shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawStatus {
    Text(String),
    Structured {
        #[serde(default)]
        value: Option<String>,
    },
}

impl RawStatus {
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Structured { value } => value.as_deref(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawMeta {
    #[serde(default, alias = "n_ctx_train", alias = "context_length")]
    n_ctx: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct ModelsEnvelope {
    data: Vec<RawModel>,
}

/// Parses the response of `GET /v1/models`.
///
/// A single-model server answers without a state field: in that mode only one model exists,
/// and it is necessarily loaded - treating it as "unloaded" would make selection fail.
pub fn parse_models(body: &str) -> Result<Vec<RemoteModel>, serde_json::Error> {
    let envelope: ModelsEnvelope = serde_json::from_str(body)?;
    Ok(envelope
        .data
        .into_iter()
        .map(|raw| {
            let status = raw
                .status
                .or(raw.state)
                .and_then(|s| s.as_str().map(ModelStatus::parse))
                .unwrap_or(ModelStatus::Loaded);
            RemoteModel {
                id: raw.id,
                status,
                context_length: raw.meta.and_then(|m| m.n_ctx),
            }
        })
        .collect())
}

/// Operating mode inferred from what the server exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerMode {
    /// Several models, loaded and unloaded on the fly.
    Router,
    /// A single model, fixed for the lifetime of the process: unloading does not exist.
    Single,
}

impl ServerMode {
    /// Infers the mode from the model list. Unloading only makes sense in router mode;
    /// offering it elsewhere would produce a command that fails silently.
    pub fn infer(models: &[RemoteModel]) -> Self {
        if models.len() > 1 || models.iter().any(|m| m.status != ModelStatus::Loaded) {
            Self::Router
        } else {
            Self::Single
        }
    }

    pub fn supports_unload(self) -> bool {
        self == Self::Router
    }
}

/// Retention policy: what to unload when memory runs short.
///
/// The rule is least-recently-used eviction, with two safeguards: we never unload the model we
/// need, nor a model that is loading - interrupting it would waste the work already done
/// without immediately freeing memory.
pub fn eviction_order(models: &[RemoteModel], last_used: &HashMap<String, u64>, keep: &str) -> Vec<String> {
    let mut candidates: Vec<_> = models
        .iter()
        .filter(|m| m.id != keep && m.status.holds_memory() && m.status != ModelStatus::Loading)
        .map(|m| (m.id.clone(), last_used.get(&m.id).copied().unwrap_or(0)))
        .collect();

    // The least recently used goes first; never used (0) goes before everything.
    candidates.sort_by_key(|(_, when)| *when);
    candidates.into_iter().map(|(id, _)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_router_response() {
        let body = r#"{"data":[
            {"id":"qwen3-27b","status":"loaded","meta":{"n_ctx":32768}},
            {"id":"llama-8b","status":"unloaded"},
            {"id":"phi-4","status":"loading"}
        ]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].status, ModelStatus::Loaded);
        assert_eq!(models[0].context_length, Some(32768));
        assert_eq!(models[1].status, ModelStatus::Unloaded);
        assert_eq!(models[2].status, ModelStatus::Loading);
    }

    #[test]
    fn llama_cpp_router_structured_state_is_understood() {
        // Shape actually returned by `llama-server --models-dir`: the state is an object, not a
        // string. Misreading it would make the whole fleet look "unloaded".
        let body = r#"{"data":[
            {"id":"copie-b","object":"model","status":{"value":"unloaded","args":["--model","x"]}},
            {"id":"qwen","object":"model","status":{"value":"loaded","args":[]}}
        ]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models[0].status, ModelStatus::Unloaded);
        assert_eq!(models[1].status, ModelStatus::Loaded);
        assert_eq!(ServerMode::infer(&models), ServerMode::Router);
    }

    #[test]
    fn both_state_forms_coexist() {
        // A fleet mixing two server versions must stay readable.
        let body = r#"{"data":[
            {"id":"old","status":"loaded"},
            {"id":"recent","status":{"value":"loading"}}
        ]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models[0].status, ModelStatus::Loaded);
        assert_eq!(models[1].status, ModelStatus::Loading);
    }

    #[test]
    fn a_single_model_server_is_considered_loaded() {
        // Classic OpenAI response: neither `status` nor `state`.
        let body = r#"{"data":[{"id":"only-model","object":"model"}]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(models[0].status, ModelStatus::Loaded);
        assert_eq!(ServerMode::infer(&models), ServerMode::Single);
        assert!(!ServerMode::infer(&models).supports_unload());
    }

    #[test]
    fn several_models_imply_router_mode() {
        let body = r#"{"data":[{"id":"a"},{"id":"b"}]}"#;
        let models = parse_models(body).unwrap();
        assert_eq!(ServerMode::infer(&models), ServerMode::Router);
        assert!(ServerMode::infer(&models).supports_unload());
    }

    #[test]
    fn an_unknown_state_is_never_taken_for_ready() {
        assert_eq!(ModelStatus::parse("something"), ModelStatus::Unloaded);
        assert!(!ModelStatus::parse("something").is_ready());
    }

    #[test]
    fn context_aliases_are_accepted() {
        let body = r#"{"data":[{"id":"m","meta":{"n_ctx_train":8192}}]}"#;
        assert_eq!(parse_models(body).unwrap()[0].context_length, Some(8192));
    }

    #[test]
    fn an_unreadable_response_raises_an_error() {
        assert!(parse_models("not json").is_err());
        assert!(parse_models(r#"{"models":[]}"#).is_err(), "`data` field is mandatory");
    }

    fn model(id: &str, status: ModelStatus) -> RemoteModel {
        RemoteModel { id: id.into(), status, context_length: None }
    }

    #[test]
    fn eviction_starts_with_the_oldest() {
        let models = vec![
            model("recent", ModelStatus::Loaded),
            model("old", ModelStatus::Loaded),
            model("asleep", ModelStatus::Sleeping),
        ];
        let last_used = HashMap::from([
            ("recent".to_string(), 100),
            ("old".to_string(), 10),
            ("asleep".to_string(), 50),
        ]);
        let order = eviction_order(&models, &last_used, "none");
        assert_eq!(order, ["old", "asleep", "recent"]);
    }

    #[test]
    fn neither_the_target_nor_a_loading_model_is_unloaded() {
        let models = vec![
            model("target", ModelStatus::Loaded),
            model("in-progress", ModelStatus::Loading),
            model("other", ModelStatus::Loaded),
        ];
        let order = eviction_order(&models, &HashMap::new(), "target");
        assert_eq!(order, ["other"], "interrupting a load frees nothing right away");
    }

    #[test]
    fn an_already_unloaded_model_is_not_a_candidate() {
        let models = vec![model("cold", ModelStatus::Unloaded), model("hot", ModelStatus::Loaded)];
        assert_eq!(eviction_order(&models, &HashMap::new(), ""), ["hot"]);
    }
}
