//! Configuration and automatic discovery.
//!
//! The principle: Po must be usable **without configuration**. At startup, it looks for local
//! inference servers on the usual ports and reads API keys from the environment. A `po.toml`
//! file only serves to correct or complete what was found.

use std::path::PathBuf;
use std::time::Duration;

use po_core::{Capabilities, ModelSpec, Origin, Pricing, Tier};
use serde::{Deserialize, Serialize};

/// Local servers probed at startup, with the default port of each implementation.
const LOCAL_ENDPOINTS: &[(&str, &str, u16)] = &[
    ("llama.cpp", "http://127.0.0.1:8080/v1", 8080),
    ("ollama", "http://127.0.0.1:11434/v1", 11434),
    ("lmstudio", "http://127.0.0.1:1234/v1", 1234),
    ("vllm", "http://127.0.0.1:8000/v1", 8000),
];

/// The probe must stay imperceptible at startup: an absent server should cost a few
/// milliseconds, not a second. On Windows, a connection to a closed local port takes about two
/// seconds to fail (measured: 2,070 ms); a listening server accepts in under a millisecond. So
/// the *connection* is bounded very short, while the HTTP response is given the time a server
/// busy generating needs.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_millis(150);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub provider: Vec<ProviderConfig>,
    #[serde(default)]
    pub model: Vec<ModelConfig>,
    /// Budget mode at startup.
    #[serde(default)]
    pub budget: Option<String>,
    /// MCP servers to launch at session startup.
    #[serde(default)]
    pub mcp: Vec<po_mcp::ServerConfig>,
    /// `false`: no API keys and no probing of local ports, only the models declared here
    /// (sandbox, tests).
    #[serde(default)]
    pub discovery: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    /// `anthropic` or `openai` — the wire format, not the brand: a local server speaks
    /// `openai`, just like Groq or DeepSeek.
    pub kind: String,
    pub base_url: String,
    /// Name of the environment variable holding the key, rather than the key itself: a
    /// configuration file ends up in a repository, an environment does not.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default = "default_context")]
    pub context_window: u32,
    #[serde(default = "default_max_output")]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub input_per_mtok: f64,
    #[serde(default)]
    pub output_per_mtok: f64,
}

fn default_tier() -> String {
    "strong".into()
}
fn default_context() -> u32 {
    32_768
}
fn default_max_output() -> u32 {
    4096
}

pub fn parse_tier(raw: &str) -> Tier {
    match raw.to_ascii_lowercase().as_str() {
        "frontier" => Tier::Frontier,
        "small" => Tier::Small,
        _ => Tier::Strong,
    }
}

impl Config {
    /// Locations of the configuration file, from the most specific to the most general.
    pub fn search_paths() -> Vec<PathBuf> {
        let mut paths = vec![PathBuf::from("po.toml"), PathBuf::from(".po/po.toml")];
        if let Some(home) = dirs::config_dir() {
            paths.push(home.join("po").join("po.toml"));
        }
        paths
    }

    pub fn load() -> (Self, Option<PathBuf>) {
        for path in Self::search_paths() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                match toml::from_str::<Self>(&text) {
                    Ok(config) => return (config, Some(path)),
                    Err(err) => {
                        eprintln!(
                            "po: {}",
                            po_i18n::t!("chat.config.unreadable", path = path.display(), err = err)
                        );
                    }
                }
            }
        }
        (Self::default(), None)
    }
}

/// A ready-to-use model, with the provider that knows how to reach it.
pub struct ResolvedModel {
    pub spec: ModelSpec,
    pub provider_name: String,
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

/// Models known to the cloud providers, with their public prices.
/// Used when the matching API key is present and no configuration describes them — so that
/// Po works without a file.
fn well_known_cloud() -> Vec<(&'static str, &'static str, &'static str, Tier, u32, f64, f64)> {
    vec![
        // (env, kind, id, tier, context, input $/Mtok, output $/Mtok)
        ("ANTHROPIC_API_KEY", "anthropic", "claude-opus-5", Tier::Frontier, 200_000, 15.0, 75.0),
        ("ANTHROPIC_API_KEY", "anthropic", "claude-sonnet-5", Tier::Strong, 200_000, 3.0, 15.0),
        ("ANTHROPIC_API_KEY", "anthropic", "claude-haiku-4-5-20251001", Tier::Small, 200_000, 0.8, 4.0),
        ("OPENAI_API_KEY", "openai", "gpt-5.2", Tier::Frontier, 400_000, 1.25, 10.0),
        ("OPENROUTER_API_KEY", "openai", "openrouter/auto", Tier::Strong, 128_000, 1.0, 3.0),
        ("GROQ_API_KEY", "openai", "llama-3.3-70b-versatile", Tier::Strong, 128_000, 0.6, 0.8),
        ("DEEPSEEK_API_KEY", "openai", "deepseek-chat", Tier::Strong, 128_000, 0.3, 1.2),
    ]
}

fn base_url_for(kind: &str, env_var: &str) -> &'static str {
    match (kind, env_var) {
        ("anthropic", _) => "https://api.anthropic.com/v1",
        (_, "OPENAI_API_KEY") => "https://api.openai.com/v1",
        (_, "OPENROUTER_API_KEY") => "https://openrouter.ai/api/v1",
        (_, "GROQ_API_KEY") => "https://api.groq.com/openai/v1",
        (_, "DEEPSEEK_API_KEY") => "https://api.deepseek.com/v1",
        _ => "https://api.openai.com/v1",
    }
}

/// Builds the model fleet: cloud inferred from the keys present, local discovered by probing,
/// then everything the configuration adds or replaces.
pub async fn resolve(config: &Config) -> Vec<ResolvedModel> {
    let mut models = Vec::new();
    let discovery = config.discovery != Some(false);

    // 1. Cloud, if the keys are there.
    for (env_var, kind, id, tier, context, input, output) in well_known_cloud().into_iter().filter(|_| discovery) {
        let Ok(key) = std::env::var(env_var) else { continue };
        if key.trim().is_empty() {
            continue;
        }
        models.push(ResolvedModel {
            spec: ModelSpec {
                id: id.into(),
                display_name: id.into(),
                origin: Origin::Cloud { provider: kind.into() },
                tier,
                capabilities: Capabilities { tools: true, parallel_tools: true, ..Default::default() },
                context_window: context,
                max_output_tokens: 8192,
                pricing: Pricing {
                    input_per_mtok: input,
                    output_per_mtok: output,
                    ..Default::default()
                },
            },
            provider_name: kind.to_owned(),
            kind: kind.to_owned(),
            base_url: base_url_for(kind, env_var).to_owned(),
            api_key: Some(key),
        });
    }

    // 2. Local servers actually reachable.
    if discovery {
        models.extend(discover_local().await);
    }

    // 3. Explicit configuration: it completes, and replaces on duplicate identifiers.
    for model in &config.model {
        let Some(provider) = config.provider.iter().find(|p| p.name == model.provider) else {
            eprintln!(
                "po: {}",
                po_i18n::t!("chat.config.unknown_provider", model = model.id, provider = model.provider)
            );
            continue;
        };
        let api_key = provider
            .api_key_env
            .as_ref()
            .and_then(|var| std::env::var(var).ok());
        let local = provider.base_url.contains("127.0.0.1") || provider.base_url.contains("localhost");

        models.retain(|existing| existing.spec.id != model.id);
        models.push(ResolvedModel {
            spec: ModelSpec {
                id: model.id.clone(),
                display_name: model.display_name.clone().unwrap_or_else(|| model.id.clone()),
                origin: if local {
                    Origin::Local {
                        backend: provider.name.clone(),
                        endpoint: provider.base_url.clone(),
                    }
                } else {
                    Origin::Cloud { provider: provider.name.clone() }
                },
                tier: parse_tier(&model.tier),
                capabilities: Capabilities { tools: true, ..Default::default() },
                context_window: model.context_window,
                max_output_tokens: model.max_output_tokens,
                pricing: Pricing {
                    input_per_mtok: model.input_per_mtok,
                    output_per_mtok: model.output_per_mtok,
                    ..Default::default()
                },
            },
            provider_name: provider.name.clone(),
            kind: provider.kind.clone(),
            base_url: provider.base_url.clone(),
            api_key,
        });
    }

    models
}

/// Probes the usual ports and keeps the servers that answer with a model list.
pub async fn discover_local() -> Vec<ResolvedModel> {
    let client = match reqwest::Client::builder()
        .connect_timeout(PROBE_CONNECT_TIMEOUT)
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };

    // Probes run in parallel: probing four ports one after the other would add two seconds
    // to startup when no server is running.
    let probes = LOCAL_ENDPOINTS.iter().map(|(backend, url, port)| {
        let client = client.clone();
        async move {
            let response = client.get(format!("{url}/models")).send().await.ok()?;
            let body = response.text().await.ok()?;
            let mut served = parse_served_models(&body);
            if served.is_empty() {
                return None;
            }
            // `/v1/models` is silent about media (llama.cpp): `/props` says what the loaded
            // model can read (`modalities`).
            if served.iter().any(|m| !m.declared) {
                let props_url = format!("{}/props", url.trim_end_matches("/v1"));
                // A failing probe must not make us forget the server: no `?` here.
                if let Ok(response) = client.get(props_url).send().await
                    && let Ok(props) = response.text().await
                {
                    apply_props_modalities(&mut served, &props);
                }
            }
            // Port 8080 is llama.cpp's, but other servers settle there: `/v1/status` says who
            // really answers, instead of deducing it from the port number.
            let backend = identify_backend(&client, url).await.unwrap_or(backend);
            Some((backend, *url, *port, served))
        }
    });

    let results = futures_util::future::join_all(probes).await;
    let mut models = Vec::new();

    for (backend, url, _port, served) in results.into_iter().flatten() {
        for ServedModel { id, context, vision, video, audio, .. } in served {
            models.push(ResolvedModel {
                spec: ModelSpec {
                    display_name: format!("{id} ({backend})"),
                    id: id.clone(),
                    origin: Origin::Local {
                        backend: backend.to_owned(),
                        endpoint: url.to_owned(),
                    },
                    // Without information about the model, assume the intermediate tier:
                    // overestimating it would hand orchestration to a model that cannot do
                    // it, underestimating it would make it unusable.
                    tier: guess_tier(&id),
                    capabilities: Capabilities { tools: true, vision, video, audio, ..Default::default() },
                    // Without an answer from the server, the cautious value: overestimating the
                    // window would overflow the server, underestimating it only costs a compaction.
                    context_window: context.unwrap_or(32_768),
                    max_output_tokens: context.map_or(4096, output_budget),
                    pricing: Pricing::FREE,
                },
                provider_name: backend.to_owned(),
                kind: "openai".to_owned(),
                base_url: url.to_owned(),
                api_key: None,
            });
        }
    }
    models
}

/// Asks a local server who it is, via `/v1/status` (Strata). Other servers do not have this
/// route: the absence of an answer leaves the port's default label.
async fn identify_backend(client: &reqwest::Client, base_url: &str) -> Option<&'static str> {
    let body = client.get(format!("{base_url}/status")).send().await.ok()?.text().await.ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&body).ok()?;
    match value["service"].as_str()? {
        "strata" => Some("strata"),
        _ => None,
    }
}

/// Guesses the tier of a local model from its name: imperfect, but a "3b" and a "70b" do not
/// have the same uses, and the user can correct it.
///
/// The count must read a *number*, not a substring: searching for "4b" in the name would
/// classify `Mistral-Small-24B` among the small models, and it would never again be trusted
/// with a code edit. Same trap for all the 14B and 34B models on the market.
pub fn guess_tier(id: &str) -> Tier {
    match billions_of_parameters(id) {
        Some(b) if b >= 45.0 => Tier::Frontier,
        Some(b) if b < 6.0 => Tier::Small,
        Some(_) => Tier::Strong,
        // A name that does not announce its size: the intermediate tier is the cheapest bet —
        // neither excluded from serious tasks, nor promoted to frontier model.
        None => Tier::Strong,
    }
}

/// Number of billions of parameters announced by the name, if any.
///
/// Recognizes `27b`, `1.5b`, `0.5b`, `A3B`, `E4B`. Keeps the largest value found: in
/// `Qwen3-30B-A3B`, it is indeed 30 billion that must fit in memory, not 3.
fn billions_of_parameters(id: &str) -> Option<f32> {
    let bytes = id.as_bytes();
    let mut best: Option<f32> = None;
    let mut i = 0;

    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        // The number must be immediately followed by a "b" that ends the word: "27b-instruct"
        // counts, "2024b0" does not, and "iq2_xxs" does not even have a b after its digit.
        let Some(&b) = bytes.get(i) else { continue };
        if !b.eq_ignore_ascii_case(&b'b') {
            continue;
        }
        let terminated = bytes.get(i + 1).is_none_or(|c| !c.is_ascii_alphanumeric());
        if !terminated {
            continue;
        }
        if let Ok(value) = id[start..i].parse::<f32>() {
            best = Some(best.map_or(value, |m: f32| m.max(value)));
        }
        i += 1;
    }

    best
}

/// What a local server announces about a model in `/v1/models`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedModel {
    pub id: String,
    /// Context window actually served, when the server gives it. The names vary:
    /// `max_model_len` (vLLM, Strata), `context_length` (Strata), `meta.n_ctx` (llama.cpp in
    /// router mode). `n_ctx_train` is not one of them: it is what the model can do, not what
    /// the server allocated.
    pub context: Option<u32>,
    /// Media the model can read, as the server announces them.
    pub vision: bool,
    pub video: bool,
    pub audio: bool,
    /// The server said what it accepts (`input_modalities`); otherwise, `/props` is consulted.
    pub declared: bool,
}

/// Completes the models from `/props` (llama.cpp, Strata): `"modalities": {"vision": true,
/// "audio": false}`. Never removes what `/v1/models` already announced.
pub fn apply_props_modalities(served: &mut [ServedModel], props: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(props) else { return };
    let modalities = &value["modalities"];
    if !modalities.is_object() {
        return;
    }
    for model in served.iter_mut().filter(|m| !m.declared) {
        model.vision |= modalities["vision"].as_bool().unwrap_or(false);
        model.video |= modalities["video"].as_bool().unwrap_or(false);
        model.audio |= modalities["audio"].as_bool().unwrap_or(false);
        model.declared = true;
    }
}

/// Extracts the models and their properties from a `/v1/models` response.
pub fn parse_served_models(body: &str) -> Vec<ServedModel> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    value["data"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item["id"].as_str()?.to_owned();
                    let context = [
                        &item["max_model_len"],
                        &item["context_length"],
                        &item["meta"]["n_ctx"],
                    ]
                    .into_iter()
                    .find_map(|v| v.as_u64())
                    .filter(|n| *n > 0)
                    .map(|n| n.min(u64::from(u32::MAX)) as u32);
                    let modalities = item["architecture"]["input_modalities"].as_array();
                    let has = |kind: &str| modalities.is_some_and(|m| m.iter().any(|v| v == kind));
                    Some(ServedModel {
                        id,
                        context,
                        vision: has("image"),
                        video: has("video"),
                        audio: has("audio"),
                        declared: modalities.is_some(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Output cap derived from the window: a quarter, between 4096 and 32768 tokens.
///
/// A fixed cap of 4096 cuts off a reasoning model before it has answered; with no limit, a
/// looping answer would fill the whole context. The quarter bounds that risk.
pub fn output_budget(context: u32) -> u32 {
    (context / 4).clamp(4096, 32_768)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_identifiers_are_extracted() {
        let body = r#"{"data":[{"id":"qwen3-27b"},{"id":"llama-8b"}]}"#;
        let ids: Vec<_> = parse_served_models(body).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["qwen3-27b", "llama-8b"]);
    }

    #[test]
    fn the_servers_real_window_is_read() {
        // Real Strata 0.2.2 response, captured on the development machine.
        let body = r#"{"object":"list","data":[{"id":"qwen3.8-flash-next-iq3_xxs","object":"model",
            "status":{"value":"loaded"},"max_model_len":131072,"context_length":131072,
            "meta":{"n_ctx":131072},
            "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]}}]}"#;
        let served = parse_served_models(body);
        assert_eq!(served.len(), 1);
        assert_eq!(served[0].context, Some(131_072));
        assert!(served[0].vision);
    }

    #[test]
    fn media_come_from_props_when_v1_models_says_nothing() {
        let mut served = parse_served_models(r#"{"data":[{"id":"m"}]}"#);
        assert!(!served[0].declared);
        // Real shape from Strata 0.1.47 (and llama.cpp).
        apply_props_modalities(&mut served, r#"{"total_slots":1,"modalities":{"vision":true,"audio":false}}"#);
        assert!(served[0].vision && !served[0].audio && !served[0].video);
        // What `/v1/models` announces takes precedence.
        let mut announced = parse_served_models(
            r#"{"data":[{"id":"m","architecture":{"input_modalities":["text","video","audio"]}}]}"#,
        );
        assert!(announced[0].video && announced[0].audio && !announced[0].vision);
        apply_props_modalities(&mut announced, r#"{"modalities":{"vision":true}}"#);
        assert!(!announced[0].vision);
    }

    #[test]
    fn llama_cpp_gives_its_window_in_meta() {
        let body = r#"{"data":[{"id":"m","meta":{"n_ctx":32768,"n_ctx_train":262144}}]}"#;
        assert_eq!(parse_served_models(body)[0].context, Some(32_768));
    }

    #[test]
    fn the_training_window_is_not_the_served_window() {
        let body = r#"{"data":[{"id":"m","meta":{"n_ctx_train":262144}}]}"#;
        assert_eq!(parse_served_models(body)[0].context, None);
    }

    #[test]
    fn a_server_silent_about_its_window_keeps_the_cautious_value() {
        let served = parse_served_models(r#"{"data":[{"id":"m"}]}"#);
        assert_eq!(served[0].context, None);
        assert!(!served[0].vision);
    }

    #[test]
    fn the_output_cap_follows_the_window_without_overflowing() {
        assert_eq!(output_budget(8_192), 4096, "floor");
        assert_eq!(output_budget(32_768), 8192);
        assert_eq!(output_budget(131_072), 32_768, "ceiling");
        assert_eq!(output_budget(1_000_000), 32_768);
    }

    #[test]
    fn an_unexpected_response_yields_no_model() {
        assert!(parse_served_models("not json").is_empty());
        assert!(parse_served_models("{}").is_empty());
        assert!(parse_served_models(r#"{"data":"not an array"}"#).is_empty());
    }

    #[test]
    fn the_tier_is_guessed_from_the_name() {
        assert_eq!(guess_tier("Qwen3-0.5B-Instruct"), Tier::Small);
        assert_eq!(guess_tier("qwen2.5-3b-instruct-q4"), Tier::Small);
        assert_eq!(guess_tier("Qwen3-27B-Q4_K_M.gguf"), Tier::Strong);
        assert_eq!(guess_tier("llama-3.3-70b"), Tier::Frontier);
        assert_eq!(guess_tier("model-without-size"), Tier::Strong);
    }

    #[test]
    fn a_two_digit_size_is_not_mistaken_for_a_small_model() {
        // The substring-search trap: "24b" contains "4b", and so does "14b". Classified as
        // small, these models would never again be trusted with a code edit.
        for name in ["Mistral-Small-24B-Instruct", "phi-4-14B", "codestral-22b", "yi-34b-chat"] {
            assert_eq!(guess_tier(name), Tier::Strong, "{name} is not a small model");
        }
        assert_eq!(guess_tier("Llama-3.1-405B"), Tier::Frontier);
    }

    #[test]
    fn the_machines_real_names_are_classified_correctly() {
        // Two files present on the development machine.
        assert_eq!(guess_tier("Qwen3.8-27B-UD-IQ2_XXS.gguf"), Tier::Strong);
        assert_eq!(guess_tier("gemma-4-E4B-it-Q4_K_M.gguf"), Tier::Small);
    }

    #[test]
    fn a_mixture_of_experts_is_judged_on_its_total_size() {
        // What matters is the memory to find, not the number of active parameters: a 30B-A3B
        // does take thirty billion weights.
        assert_eq!(guess_tier("Qwen3-30B-A3B-Instruct"), Tier::Strong);
        assert_eq!(guess_tier("Mixtral-8x22B"), Tier::Strong);
    }

    #[test]
    fn quantization_labels_are_not_sizes() {
        // "IQ2_XXS", "Q4_K_M", "Q8_0" must never be read as a parameter count, nor a version
        // number stuck to a letter.
        assert_eq!(billions_of_parameters("model-IQ2_XXS-Q4_K_M"), None);
        assert_eq!(billions_of_parameters("build-2024b0"), None);
        assert_eq!(billions_of_parameters("Qwen3.8-27B"), Some(27.0));
        assert_eq!(billions_of_parameters("Qwen3-0.5B"), Some(0.5));
    }

    #[test]
    fn the_configuration_is_read() {
        let text = r#"
budget = "balanced"

[[provider]]
name = "local"
kind = "openai"
base_url = "http://127.0.0.1:8080/v1"

[[model]]
id = "qwen3-27b"
provider = "local"
tier = "strong"
context_window = 32768
"#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.budget.as_deref(), Some("balanced"));
        assert_eq!(config.provider.len(), 1);
        assert_eq!(config.model[0].context_window, 32_768);
        assert_eq!(config.model[0].max_output_tokens, 4096, "default value");
    }

    #[test]
    fn mcp_servers_are_declared() {
        let text = r#"
[[mcp]]
name = "files"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
"#;
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.mcp.len(), 1);
        assert_eq!(config.mcp[0].name, "files");
        assert_eq!(config.mcp[0].args.len(), 3);
    }

    #[test]
    fn an_empty_configuration_is_valid() {
        // This is the normal case: Po must start without a file.
        let config: Config = toml::from_str("").unwrap();
        assert!(config.model.is_empty());
        assert!(config.provider.is_empty());
    }

    #[tokio::test]
    async fn the_configuration_replaces_a_discovered_model() {
        let text = r#"
[[provider]]
name = "local"
kind = "openai"
base_url = "http://127.0.0.1:8080/v1"

[[model]]
id = "custom"
provider = "local"
tier = "frontier"
context_window = 131072
"#;
        let config: Config = toml::from_str(text).unwrap();
        let models = resolve(&config).await;
        let ours = models.iter().find(|m| m.spec.id == "custom").unwrap();
        assert_eq!(ours.spec.tier, Tier::Frontier);
        assert_eq!(ours.spec.context_window, 131_072);
        assert!(ours.spec.is_local(), "a local URL must give a local origin");
    }

    #[test]
    fn base_urls_match_the_providers() {
        assert_eq!(base_url_for("anthropic", "ANTHROPIC_API_KEY"), "https://api.anthropic.com/v1");
        assert!(base_url_for("openai", "GROQ_API_KEY").contains("groq"));
        assert!(base_url_for("openai", "OPENROUTER_API_KEY").contains("openrouter"));
    }
}
