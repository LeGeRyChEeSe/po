//! Every model loaded on this PC, whatever serves it: Strata, llama.cpp, Ollama, LM Studio, vLLM.
//!
//! Po can use several at once (a session's model on one card, a subagent's on another, a model
//! the user loaded by hand). This is the one list the terminal (`/models`), the session (the
//! models it can switch to) and the phone (the hardware sheet) all show, with what each can be
//! unloaded by.

use std::time::Duration;

use po_wire::LoadedModel;

/// A server to ask: its engine as discovery names it, its OpenAI-compatible base URL, its port.
struct Endpoint {
    engine: String,
    base: String,
    port: u16,
}

/// Po's own llama.cpp port (`po load`, `/local`).
const PO_LLAMA_PORT: u16 = 18080;

fn endpoints(extra: &[String]) -> Vec<Endpoint> {
    let mut list: Vec<Endpoint> = crate::config::LOCAL_ENDPOINTS
        .iter()
        .map(|(engine, base, port)| Endpoint { engine: (*engine).to_owned(), base: (*base).to_owned(), port: *port })
        .collect();
    list.push(Endpoint { engine: "llama.cpp".into(), base: format!("http://127.0.0.1:{PO_LLAMA_PORT}/v1"), port: PO_LLAMA_PORT });
    if let Some(bg) = po_local::BackgroundServerInfo::load() {
        list.push(Endpoint { engine: bg.engine, base: bg.endpoint, port: bg.port });
    }
    // The local servers the configuration names (`po.toml`), on any port.
    let (config, _) = crate::config::Config::load();
    for provider in config.provider {
        let url = provider.base_url.trim_end_matches('/').to_owned();
        let local = url.contains("127.0.0.1") || url.contains("localhost");
        if let (true, Some(port)) = (local, port_of(&url)) {
            list.push(Endpoint { engine: provider.name, base: url, port });
        }
    }
    // The open sessions' servers, wherever they are.
    for url in extra {
        if let Some(port) = port_of(url) {
            list.push(Endpoint { engine: "local".into(), base: url.trim_end_matches('/').to_owned(), port });
        }
    }
    let mut seen = std::collections::HashSet::new();
    list.retain(|e| seen.insert(e.port));
    list
}

/// A llama-server started without an alias names its model by the file's path: its name then.
fn shown_name(id: &str) -> String {
    let file = id.rsplit(['/', '\\']).next().unwrap_or(id);
    file.strip_suffix(".gguf").unwrap_or(file).to_owned()
}

/// `http://127.0.0.1:18190/v1` → 18190.
pub fn port_of(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1)?;
    let host = rest.split('/').next()?;
    host.rsplit(':').next()?.parse().ok()
}

fn client() -> Option<reqwest::Client> {
    // A closed port must cost milliseconds, not the two seconds Windows takes to refuse.
    reqwest::Client::builder().connect_timeout(Duration::from_millis(300)).timeout(Duration::from_secs(3)).build().ok()
}

async fn json(client: &reqwest::Client, url: String) -> Option<serde_json::Value> {
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

/// The models one server holds in memory.
async fn loaded_on(client: &reqwest::Client, endpoint: &Endpoint) -> Vec<LoadedModel> {
    let root = endpoint.base.trim_end_matches("/v1").to_owned();
    let port = endpoint.port;
    let model = |id: &str, engine: &str, unloadable: bool| LoadedModel { id: shown_name(id), engine: engine.to_owned(), port, unloadable };
    // Strata says itself whether its model is loaded (its model list names it even unloaded).
    if let Some(status) = json(client, format!("{root}/v1/status")).await
        && status["service"].as_str() == Some("strata")
    {
        return match (status["loaded"].as_bool(), status["model"].as_str()) {
            (Some(true), Some(id)) => vec![model(id, "Strata", true)],
            _ => Vec::new(),
        };
    }
    // Ollama lists every model it has; `/api/ps` the ones in memory.
    if let Some(ps) = json(client, format!("{root}/api/ps")).await
        && let Some(running) = ps["models"].as_array()
    {
        return running.iter().filter_map(|m| m["name"].as_str()).map(|id| model(id, "Ollama", true)).collect();
    }
    // LM Studio says each model's state.
    if let Some(all) = json(client, format!("{root}/api/v0/models")).await
        && let Some(data) = all["data"].as_array()
    {
        return data
            .iter()
            .filter(|m| m["state"].as_str() == Some("loaded"))
            .filter_map(|m| m["id"].as_str())
            .map(|id| model(id, "LM Studio", false))
            .collect();
    }
    // llama.cpp and vLLM serve what they hold. Po stops the llama.cpp it started itself.
    let Some(models) = json(client, format!("{root}/v1/models")).await else { return Vec::new() };
    let engine = if endpoint.engine == "vllm" { "vLLM" } else { "llama.cpp" };
    let ours = port == PO_LLAMA_PORT || po_local::BackgroundServerInfo::load().is_some_and(|bg| bg.port == port);
    crate::config::parse_served_models(&models.to_string()).iter().map(|m| model(&m.id, engine, engine == "llama.cpp" && ours)).collect()
}

/// Every model loaded on this PC now. The servers are asked in parallel.
pub async fn loaded() -> Vec<LoadedModel> {
    loaded_with(&[]).await
}

/// The same, asking also `extra` servers (the open sessions' own, see the service).
pub async fn loaded_with(extra: &[String]) -> Vec<LoadedModel> {
    let Some(client) = client() else { return Vec::new() };
    let endpoints = endpoints(extra);
    let all = futures_util::future::join_all(endpoints.iter().map(|e| loaded_on(&client, e))).await;
    all.into_iter().flatten().collect()
}

/// Unloads one model: Strata empties its memory, Ollama lets the model go, the llama.cpp Po
/// started stops. What it says to the user.
pub async fn unload(port: u16, id: &str) -> Result<String, String> {
    // Asked on its own port too: it may be one only a session knows.
    let here = [format!("http://127.0.0.1:{port}/v1")];
    let model = loaded_with(&here).await.into_iter().find(|m| m.port == port && (id.is_empty() || m.id == id));
    let Some(model) = model else { return Err(po_i18n::t!("running.not_loaded", id = id).to_owned()) };
    if !model.unloadable {
        return Err(po_i18n::t!("running.cannot_unload", id = model.id, engine = model.engine).to_owned());
    }
    let client = client().ok_or("no HTTP client")?;
    match model.engine.as_str() {
        "Strata" => {
            client.post(format!("http://127.0.0.1:{port}/v1/unload")).json(&serde_json::json!({})).send().await.map_err(|e| e.to_string())?;
        }
        "Ollama" => {
            client
                .post(format!("http://127.0.0.1:{port}/api/generate"))
                .json(&serde_json::json!({"model": model.id, "keep_alive": 0}))
                .send()
                .await
                .map_err(|e| e.to_string())?;
        }
        _ => crate::background::stop_llama_on(port),
    }
    Ok(po_i18n::t!("running.unloaded", id = model.id, engine = model.engine).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_is_read_from_a_base_url() {
        assert_eq!(port_of("http://127.0.0.1:18190/v1"), Some(18190));
        assert_eq!(port_of("http://localhost:8080"), Some(8080));
        assert_eq!(port_of("https://api.example.com/v1"), None);
    }

    #[test]
    fn a_model_known_by_its_path_shows_its_file_name() {
        assert_eq!(shown_name(r"E:\models\Qwen3.5-9B-Q4_K_M.gguf"), "Qwen3.5-9B-Q4_K_M");
        assert_eq!(shown_name("/models/a.gguf"), "a");
        assert_eq!(shown_name("qwen3.8-flash"), "qwen3.8-flash");
    }
}
