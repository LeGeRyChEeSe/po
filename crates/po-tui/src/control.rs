//! Taking control of a running session from the outside.
//!
//! Every full-screen Po opens a small local MCP server (HTTP on 127.0.0.1, protected by a token
//! in the address). Another Po, Claude Code or any MCP client can then see the screen, read the
//! conversation, change a setting, send a message or restart Po on the same session — which
//! lets Po work on Po, live.
//!
//! The protocol is MCP rather than a home-made format: every agent already speaks it, Po first
//! of all (`/mcp add`). The address of each open session is written to a file in the `live`
//! folder, removed on exit.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

/// What a client asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Session state: title, model, turn in progress, mode, subagents.
    Status,
    /// The screen as displayed.
    Screen,
    /// The last lines of the conversation.
    Transcript { last: usize },
    /// A `/…` command, as typed on the keyboard.
    Command(String),
    /// A user message.
    Prompt(String),
    /// Restart Po (most recently installed binary) on the same session.
    Restart,
}

/// A request and the means to answer it: text, and whether it is an error.
pub struct Request {
    pub action: Action,
    pub reply: oneshot::Sender<(String, bool)>,
}

/// The open server: its address, to give to the MCP client.
pub struct Server {
    pub url: String,
    registry: Option<PathBuf>,
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(path) = &self.registry {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Folder where each open session records its address.
pub fn live_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po").join("live"))
}

/// Random token: `RandomState` is seeded by the system at each creation.
fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for _ in 0..2 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

/// Opens the server and records its address. Requests arrive on `requests`.
pub async fn start(workspace: Option<PathBuf>, requests: mpsc::UnboundedSender<Request>) -> std::io::Result<Server> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let secret = token();
    let url = format!("http://127.0.0.1:{port}/mcp/{secret}");
    let registry = live_dir().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join(format!("{}.json", std::process::id()));
        let entry = json!({
            "pid": std::process::id(),
            "url": url,
            "workspace": workspace.map(|w| w.display().to_string()),
            "started": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        });
        std::fs::write(&path, serde_json::to_string_pretty(&entry).ok()?).ok()?;
        Some(path)
    });
    let path = format!("/mcp/{secret}");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let requests = requests.clone();
            let path = path.clone();
            tokio::spawn(async move {
                let _ = serve(stream, &path, &requests).await;
            });
        }
    });
    Ok(Server { url, registry })
}

/// Beyond this, a request is not read: nothing legitimate comes close.
const MAX_BODY: usize = 1024 * 1024;

async fn serve(mut stream: TcpStream, path: &str, requests: &mpsc::UnboundedSender<Request>) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await.map_err(std::io::Error::other)??;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buffer.len() > MAX_BODY {
            return respond(&mut stream, 413, "").await;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if target != path {
        return respond(&mut stream, 404, "").await;
    }
    match method {
        "POST" => {}
        "DELETE" => return respond(&mut stream, 200, "").await,
        _ => return respond(&mut stream, 405, "").await,
    }
    if length > MAX_BODY {
        return respond(&mut stream, 413, "").await;
    }
    while buffer.len() < header_end + length {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await.map_err(std::io::Error::other)??;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let body = &buffer[header_end..buffer.len().min(header_end + length)];
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return respond(&mut stream, 400, "").await;
    };
    match handle(&message, requests).await {
        Some(reply) => respond(&mut stream, 200, &reply.to_string()).await,
        // A notification calls for no response.
        None => respond(&mut stream, 202, "").await,
    }
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Payload Too Large",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Tools offered to the client.
pub fn tools() -> Value {
    let none = json!({"type": "object", "properties": {}});
    json!([
        {"name": "po_state", "description": "State of the running Po session: title, model, whether a turn is in progress, work mode, context, subagents.", "inputSchema": none},
        {"name": "po_screen", "description": "Po's screen as the user currently sees it, as text.", "inputSchema": none},
        {"name": "po_conversation", "description": "The last lines of the displayed conversation (messages, reasoning, tools).",
         "inputSchema": {"type": "object", "properties": {"derniers": {"type": "integer", "description": "Number of lines (default 80)"}}}},
        {"name": "po_command", "description": "Runs a Po command (/reflexion low, /agents parallele 2, /plan…) in the running session, as if the user typed it. During a turn, it applies at the next safe point.",
         "inputSchema": {"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]}},
        {"name": "po_message", "description": "Sends a message to Po as if the user wrote it. During a turn, it is sent at the end of the turn.",
         "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
        {"name": "po_restart", "description": "Restarts Po on the same session with the most recently installed binary (after `scripts/install.ps1`). Waits for the current turn to finish.", "inputSchema": none}
    ])
}

/// Turns a tool call into an action.
pub fn action_for(name: &str, arguments: &Value) -> Result<Action, String> {
    let text = |key: &str| {
        arguments[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).ok_or(po_i18n::t!("tui.control.missing_arg", key = key))
    };
    Ok(match name {
        "po_state" => Action::Status,
        "po_screen" => Action::Screen,
        "po_conversation" => Action::Transcript { last: arguments["derniers"].as_u64().unwrap_or(80).clamp(1, 2000) as usize },
        "po_command" => {
            let command = text("command")?;
            if !command.starts_with('/') {
                return Err(po_i18n::t!("tui.control.command_slash").into());
            }
            Action::Command(command)
        }
        "po_message" => Action::Prompt(text("text")?),
        "po_restart" => Action::Restart,
        other => return Err(po_i18n::t!("tui.control.unknown_tool", name = other)),
    })
}

/// Answers a JSON-RPC message; `None` for a notification.
async fn handle(message: &Value, requests: &mpsc::UnboundedSender<Request>) -> Option<Value> {
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => json!({
            "protocolVersion": message["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "po-live", "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Running Po session: po_screen to see it, po_command to configure it, po_restart after a reinstall."
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or("");
            let (text, is_error) = match action_for(name, &message["params"]["arguments"]) {
                Err(err) => (err, true),
                Ok(action) => ask(requests, action).await,
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => {
            return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": po_i18n::t!("tui.control.unknown_method", method = method)}}));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Passes an action to the interface and waits for its answer.
async fn ask(requests: &mpsc::UnboundedSender<Request>, action: Action) -> (String, bool) {
    let (reply, answer) = oneshot::channel();
    if requests.send(Request { action, reply }).is_err() {
        return (po_i18n::t!("tui.control.session_closed").into(), true);
    }
    // A long command (model loading) can take a while; beyond that, we hand control back.
    match tokio::time::timeout(Duration::from_secs(120), answer).await {
        Ok(Ok(answer)) => answer,
        _ => (po_i18n::t!("tui.control.timeout").into(), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_calls_become_actions() {
        assert_eq!(action_for("po_screen", &json!({})), Ok(Action::Screen));
        assert_eq!(action_for("po_conversation", &json!({"derniers": 5})), Ok(Action::Transcript { last: 5 }));
        assert_eq!(action_for("po_command", &json!({"command": " /plan "})), Ok(Action::Command("/plan".into())));
        assert!(action_for("po_command", &json!({"command": "rm -rf"})).is_err());
        assert!(action_for("po_message", &json!({})).is_err());
    }

    #[tokio::test]
    async fn the_server_speaks_mcp_and_refuses_without_the_token() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
        let server = start(None, tx).await.unwrap();
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                let _ = request.reply.send((format!("{:?}", request.action), false));
            }
        });
        let post = |url: String, body: Value| async move {
            let mut stream = TcpStream::connect(url.trim_start_matches("http://").split('/').next().unwrap()).await.unwrap();
            let path = format!("/{}", url.trim_start_matches("http://").split_once('/').unwrap().1);
            let body = body.to_string();
            let request = format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut out = String::new();
            stream.read_to_string(&mut out).await.unwrap();
            out
        };
        let listing = post(server.url.clone(), json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).await;
        assert!(listing.starts_with("HTTP/1.1 200") && listing.contains("po_screen"), "{listing}");
        let call = post(
            server.url.clone(),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "po_command", "arguments": {"command": "/plan"}}}),
        )
        .await;
        assert!(call.contains("Command(\\\"/plan\\\")"), "{call}");
        let base = server.url.rsplit_once('/').unwrap().0.to_owned();
        let refused = post(format!("{base}/wrong"), json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})).await;
        assert!(refused.starts_with("HTTP/1.1 404"), "without the right token, nothing");
    }
}
