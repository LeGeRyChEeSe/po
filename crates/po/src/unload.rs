//! What `/model unload` frees: the model the session is serving, whichever server started it.
//!
//! The old behaviour only knew Po's own llama.cpp server: a Strata launched by the user (the
//! usual case - Po does not start the machine's production server) was never unloaded, while the
//! phone was told "model unloaded". Here the session's servers are asked, one by one, and what is
//! answered is what is reported.

use std::time::Duration;

use po_agent::Agent;
use po_core::Origin;
use po_i18n::t;
use po_local::strata;

use crate::commands::Reply;
use crate::local_ui::LocalManager;

const UNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// A local server the session serves its models from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Server {
    /// Its base URL, `/v1` cut (`http://127.0.0.1:8080`).
    pub root: String,
    pub port: u16,
}

/// The local servers of the session's models, in port order, without duplicates.
pub(crate) fn servers(agent: &Agent) -> Vec<Server> {
    let mut found: Vec<Server> = agent
        .router()
        .models()
        .iter()
        .filter_map(|m| match &m.origin {
            Origin::Local { endpoint, .. } => {
                let root = endpoint.trim_end_matches("/v1").trim_end_matches('/');
                let port = root.rsplit(':').next()?.parse::<u16>().ok()?;
                Some(Server { root: root.to_owned(), port })
            }
            _ => None,
        })
        .collect();
    found.sort_by_key(|s| s.port);
    found.dedup_by(|a, b| a.port == b.port);
    found
}

/// Frees the model the session is serving: what Po started (its llama.cpp server, its Strata) is
/// stopped - it is Po's to stop; a server launched outside Po only has its model taken out of
/// memory, which gives the card back and keeps the server reachable.
pub(crate) async fn run(agent: &mut Agent, local: &mut LocalManager, reply: &mut Reply) {
    crate::setup::cancel();
    po_tui::control::mirror().loading(None);
    po_tui::set_progress(None);

    // The conversation is in the server's memory: saved first, it comes back in seconds when the
    // model loads again.
    crate::kv::park_on_exit(agent).await;

    // Po's llama.cpp server: hot unloading does not exist for it, so it is stopped whole.
    let ours_llama = local.supervisor.as_ref().and_then(|s| s.running()).map(|s| s.port);
    if ours_llama.is_some() {
        reply.accent(local.deactivate(agent).await);
    }

    // Po's Strata: Po's to stop as well, the card given back whole.
    let ours_strata = local.engines.strata_server.as_ref().map(|s| s.port);
    if ours_strata.is_some() {
        let mut engines = std::mem::take(&mut local.engines);
        crate::engines_ui::stop(crate::engines_ui::Kind::Strata, agent, local, &mut engines, reply).await;
        local.engines = engines;
    }

    let ours = [ours_llama, ours_strata];
    let mut freed = ours_llama.is_some() || ours_strata.is_some();
    for server in servers(agent) {
        // What Po started was already stopped above: its port has nothing left to say.
        if ours.contains(&Some(server.port)) {
            continue;
        }
        match strata::probe(&[server.port]).await.first().cloned() {
            // A Strata answers: its model is asked to leave the memory, the server stays up.
            Some(live) if live.loaded => match unload(&server.root).await {
                Ok(secs) => {
                    freed = true;
                    reply.accent(t!("cmd.server.unloaded", secs = secs));
                }
                Err(why) => reply.warn(why),
            },
            Some(_) => {
                freed = true;
                reply.dim(t!("cmd.server.model_unloaded"));
            }
            // Nothing of Strata answers there.
            None => {
                // Something does answer on that port: it is not Po's to touch, and its model
                // was not freed - said as such, without pretending.
                if listening(server.port).await {
                    reply.warn(t!("cmd.model.unload.foreign", port = server.port));
                }
            }
        }
    }

    if !freed && reply.lines.is_empty() {
        reply.dim(t!("cmd.model.unload.nothing"));
    }

    // Only what was actually freed lets go its hold: a refused unload (a turn running on that
    // server) leaves the session as it was - its pin, its default model, and the next message
    // still served by the same model.
    if freed {
        agent.router_mut().pin(None);
        let mut prefs = po_local::prefs::LocalPrefs::load();
        prefs.default_model = None;
        let _ = prefs.save();
    }
}

/// Something listens on that port: a bare TCP connection, which changes nothing there.
async fn listening(port: u16) -> bool {
    tokio::time::timeout(Duration::from_millis(300), tokio::net::TcpStream::connect(("127.0.0.1", port)))
        .await
        .is_ok()
}

/// Asks a Strata to take its model out of memory: `Ok` is the time it took (seconds), for the
/// report; `Err` is what the refusal means.
async fn unload(root: &str) -> Result<String, String> {
    let Ok(client) = reqwest::Client::builder().timeout(UNLOAD_TIMEOUT).build() else {
        return Err(t!("cmd.server.unreachable", root = root, err = "no client"));
    };
    let started = std::time::Instant::now();
    match client.post(format!("{root}/v1/unload")).json(&serde_json::json!({})).send().await {
        Ok(response) if response.status().is_success() => Ok(format!("{:.0}", started.elapsed().as_secs_f64())),
        Ok(response) if response.status().as_u16() == 409 => Err(t!("cmd.server.busy").to_string()),
        Ok(response) => Err(t!("cmd.server.refused", status = response.status())),
        Err(err) => Err(t!("cmd.server.unreachable", root = root, err = err)),
    }
}

#[cfg(test)]
// The lock only serialises the tests that share the prefs file; they run on one thread.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::commands::Tone;
    use po_core::{Capabilities, ModelSpec, Pricing, Tier};
    use po_router::{BudgetMode, Router};
    use tokio::net::TcpListener;

    /// A stand-in for a local server: it answers `/v1/status` and `/v1/unload`, and remembers
    /// what it was asked. Nothing here talks to the machine's real servers.
    struct Mock {
        port: u16,
        seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Mock {
        async fn start(status: &'static str, unload_code: u16) -> Mock {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let shared = seen.clone();
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let seen = shared.clone();
                    tokio::spawn(async move {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let mut text = String::new();
                        let mut buf = [0u8; 512];
                        while let Ok(n) = stream.read(&mut buf[..]).await {
                            if n == 0 {
                                break;
                            }
                            text.push_str(&String::from_utf8_lossy(&buf[..n]));
                            if text.contains("\r\n\r\n") {
                                break;
                            }
                        }
                        let line = text.lines().next().unwrap_or_default().to_owned();
                        seen.lock().unwrap().push(line.clone());
                        let (code, body) = if line.starts_with("POST") {
                            (unload_code, "{}".to_owned())
                        } else {
                            (200, status.to_owned())
                        };
                        let answer = format!(
                            "HTTP/1.1 {code} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(answer.as_bytes()).await;
                    });
                }
            });
            Mock { port, seen }
        }

        fn asked(&self, method: &str) -> bool {
            self.seen.lock().unwrap().iter().any(|r| r.starts_with(method))
        }
    }

    /// A session serving one local model, in a temporary folder nothing else shares.
    fn agent_with(endpoint: &str, dir: &std::path::Path) -> Agent {
        let mut router = Router::new(Vec::new(), BudgetMode::Balanced);
        router.add_model(ModelSpec {
            id: "mock-model".into(),
            display_name: "mock-model (strata)".into(),
            origin: Origin::Local { backend: "strata".into(), endpoint: endpoint.to_owned() },
            tier: Tier::Strong,
            capabilities: Capabilities { tools: true, ..Default::default() },
            context_window: 4096,
            max_output_tokens: 1024,
            pricing: Pricing::FREE,
        });
        Agent::new(
            router,
            po_tools::Registry::with_builtins(),
            po_tools::ToolContext::new(po_tools::Workspace::new(dir)),
            "system",
            std::sync::Arc::new(po_agent::ReadOnly),
        )
    }

    fn said(reply: &Reply, tone: Tone) -> Vec<String> {
        reply.lines.iter().filter(|(t, _)| *t == tone).map(|(_, text)| text.clone()).collect()
    }

    #[test]
    fn the_servers_of_a_session_are_found_by_port() {
        let mut router = Router::new(Vec::new(), BudgetMode::Balanced);
        let local = |n: &str| ModelSpec {
            id: n.into(),
            display_name: n.into(),
            origin: Origin::Local { backend: "strata".into(), endpoint: n.into() },
            tier: Tier::Strong,
            capabilities: Capabilities::default(),
            context_window: 4096,
            max_output_tokens: 1024,
            pricing: Pricing::FREE,
        };
        router.add_model(local("http://127.0.0.1:18123/v1"));
        router.add_model(local("http://127.0.0.1:18122/v1"));
        let agent = Agent::new(
            router,
            po_tools::Registry::with_builtins(),
            po_tools::ToolContext::new(po_tools::Workspace::new(std::path::Path::new("."))),
            "system",
            std::sync::Arc::new(po_agent::ReadOnly),
        );
        let found = servers(&agent);
        assert_eq!(found.len(), 2, "one entry per server");
        // In port order: the same address twice is one server, not two.
        assert_eq!(found[0].port, 18122);
        assert_eq!(found[1].root, "http://127.0.0.1:18123", "`/v1` cut off the address");
    }

    #[tokio::test]
    async fn an_outside_strata_is_asked_to_free_its_model() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PO_LOCAL_PREFS", dir.path().join("local.json")) };
        // A Strata Po did not start, with its model in memory: the phone's button must free it.
        let mock = Mock::start(r#"{"service":"strata","model":"m","loaded":true}"#, 200).await;
        let mut agent = agent_with(&format!("http://127.0.0.1:{}/v1", mock.port), dir.path());
        let mut local = LocalManager::empty();
        let mut reply = Reply::default();
        run(&mut agent, &mut local, &mut reply).await;
        assert!(mock.asked("POST"), "the model was asked to leave the memory");
        assert!(said(&reply, Tone::Accent).iter().any(|t| t.contains("d")), "{:?}", reply.lines);
        assert_eq!(agent.router().pinned(), None, "the session lets go what it was serving");
    }

    #[tokio::test]
    async fn a_busy_outside_server_is_said_busy_not_unloaded() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PO_LOCAL_PREFS", dir.path().join("local.json")) };
        // The server refuses (a turn in progress on it): the report must not say "unloaded".
        let mock = Mock::start(r#"{"service":"strata","model":"m","loaded":true}"#, 409).await;
        let mut agent = agent_with(&format!("http://127.0.0.1:{}/v1", mock.port), dir.path());
        agent.router_mut().pin(Some("mock-model".into()));
        let mut local = LocalManager::empty();
        let mut reply = Reply::default();
        run(&mut agent, &mut local, &mut reply).await;
        assert!(mock.asked("POST"));
        assert!(!said(&reply, Tone::Accent).iter().any(|t| t.contains("d")), "no success over a refusal: {:?}", reply.lines);
        assert!(said(&reply, Tone::Warn).iter().any(|t| t.contains("d")), "{:?}", reply.lines);
        // A refusal changes nothing on the PC's side either: the next message is served by the same model.
        assert_eq!(agent.router().pinned(), Some("mock-model"), "a refusal leaves the session as it was");
    }

    #[tokio::test]
    async fn a_server_that_is_not_strata_is_left_alone() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PO_LOCAL_PREFS", dir.path().join("local.json")) };
        // `/v1/status` answers, but is not Strata: Po never touches a server it did not start.
        let mock = Mock::start(r#"{"service":"other"}"#, 200).await;
        let mut agent = agent_with(&format!("http://127.0.0.1:{}/v1", mock.port), dir.path());
        let mut local = LocalManager::empty();
        let mut reply = Reply::default();
        run(&mut agent, &mut local, &mut reply).await;
        assert!(!mock.asked("POST"), "nothing was asked to a server Po does not own");
        assert!(said(&reply, Tone::Warn).iter().any(|t| t.contains("Po")), "{:?}", reply.lines);
        assert!(!said(&reply, Tone::Accent).iter().any(|t| t.contains("d")), "{:?}", reply.lines);
    }

    #[tokio::test]
    async fn a_model_already_out_of_memory_is_said_simply() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PO_LOCAL_PREFS", dir.path().join("local.json")) };
        let mock = Mock::start(r#"{"service":"strata","model":"m","loaded":false}"#, 200).await;
        let mut agent = agent_with(&format!("http://127.0.0.1:{}/v1", mock.port), dir.path());
        let mut local = LocalManager::empty();
        let mut reply = Reply::default();
        run(&mut agent, &mut local, &mut reply).await;
        assert!(!mock.asked("POST"), "nothing to free was asked");
        assert!(said(&reply, Tone::Dim).iter().any(|t| t.contains("d")), "{:?}", reply.lines);
    }

    #[tokio::test]
    async fn nothing_running_says_there_is_nothing_to_free() {
        let _guard = crate::PREFS_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("PO_LOCAL_PREFS", dir.path().join("local.json")) };
        // Port 1 answers on nothing: no server there.
        let mut agent = agent_with("http://127.0.0.1:1/v1", dir.path());
        agent.router_mut().pin(Some("mock-model".into()));
        let mut local = LocalManager::empty();
        let mut reply = Reply::default();
        run(&mut agent, &mut local, &mut reply).await;
        assert!(reply.lines.iter().all(|(tone, _)| *tone != Tone::Accent), "nothing freed: nothing claimed: {:?}", reply.lines);
        assert!(!said(&reply, Tone::Warn).iter().any(|t| t.contains("requ")), "no busy talk with nothing running: {:?}", reply.lines);
        // Nothing was freed: the session keeps what it was serving, its default model with it.
        assert_eq!(agent.router().pinned(), Some("mock-model"), "{:?}", reply.lines);
    }
}