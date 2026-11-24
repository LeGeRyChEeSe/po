//! Taking control of a running session from the outside.
//!
//! Every full-screen Po opens a small local MCP server (HTTP on 127.0.0.1, protected by a token
//! in the address). Another Po, Claude Code or any MCP client can then see the screen, read the
//! conversation, change a setting, send a message or restart Po on the same session - which
//! lets Po work on Po, live.
//!
//! The protocol is MCP rather than a home-made format: every agent already speaks it, Po first
//! of all (`/mcp add`). The address of each open session is written to a file in the `live`
//! folder, removed on exit.
//!
//! For Po's service (the phone's way in), the same server also gives the session as data: the
//! [`Mirror`] numbers every event of the session ([`po_wire::Event`]) and keeps the conversation
//! they build, `GET …/events` follows them (a snapshot, or the events missed since a number, then
//! the rest as it comes, one JSON per line), and `POST …/input` acts like the keyboard: a message,
//! a command, an approval, an answer, a stop. What the terminal shows and what the phone shows
//! come from the same events.

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
    /// What the phone does: answer an approval or a question, stop the turn (a message or a
    /// command arrive as [`Action::Prompt`] and [`Action::Command`]).
    Input(po_wire::Input),
    /// The menu of a command line, as JSON (`null` when it needs no choice).
    Menu(String),
    /// The commands offered to completion, as JSON.
    Commands,
    /// Ends a session opened without a terminal (the phone closed it).
    Close,
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
    start_as(workspace, requests, "tui").await
}

/// The same, saying what kind of session it is: `tui`, or `headless` (opened from a phone).
pub async fn start_as(workspace: Option<PathBuf>, requests: mpsc::UnboundedSender<Request>, kind: &str) -> std::io::Result<Server> {
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
            "kind": kind,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&entry).ok()?).ok()?;
        Some(path)
    });
    let path = format!("/mcp/{secret}");
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            // Each event is a small write: without this, Nagle and the delayed ACK of Windows can
            // hold a fragment up to 200 ms before the phone sees it.
            let _ = stream.set_nodelay(true);
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
    let (route, query) = target.split_once('?').unwrap_or((target, ""));
    if let Some(sub) = route.strip_prefix(path).filter(|r| r.starts_with('/')) {
        return serve_data(stream, method, sub, query, &buffer[header_end..], length, requests).await;
    }
    if route != path {
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

/// The session as data, for Po's service: its events, and acting on it.
async fn serve_data(
    mut stream: TcpStream,
    method: &str,
    route: &str,
    query: &str,
    already: &[u8],
    length: usize,
    requests: &mpsc::UnboundedSender<Request>,
) -> std::io::Result<()> {
    let param = |name: &str| query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_owned());
    match (method, route) {
        ("GET", "/events") => {
            let since = param("since").and_then(|v| v.parse().ok()).unwrap_or(0);
            follow(stream, param("epoch").filter(|e| !e.is_empty()), since).await
        }
        ("GET", "/info") => respond(&mut stream, 200, &mirror().info().to_string()).await,
        ("GET", "/commands") => {
            let (text, error) = ask(requests, Action::Commands).await;
            respond(&mut stream, if error { 503 } else { 200 }, &text).await
        }
        ("POST", "/input" | "/menu" | "/close") => {
            if length > MAX_BODY {
                return respond(&mut stream, 413, "").await;
            }
            let mut body = already.to_vec();
            let mut chunk = [0u8; 8192];
            while body.len() < length {
                let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await.map_err(std::io::Error::other)??;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(length);
            let action = match route {
                "/close" => Some(Action::Close),
                "/menu" => serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["line"].as_str().map(|l| Action::Menu(l.to_owned()))),
                _ => serde_json::from_slice::<po_wire::Input>(&body).ok().map(|input| match input {
                    po_wire::Input::Prompt { text } => Action::Prompt(text),
                    po_wire::Input::Command { text } => Action::Command(text),
                    other => Action::Input(other),
                }),
            };
            let Some(action) = action else { return respond(&mut stream, 400, "").await };
            let empty = matches!(&action, Action::Prompt(t) | Action::Command(t) if t.trim().is_empty());
            if empty || matches!(&action, Action::Command(c) if !c.trim_start().starts_with('/')) {
                return respond(&mut stream, 400, &json!({"text": po_i18n::t!("tui.control.command_slash"), "error": true}).to_string()).await;
            }
            let menu = matches!(action, Action::Menu(_));
            let (text, error) = ask(requests, action).await;
            if menu && !error {
                return respond(&mut stream, 200, &text).await;
            }
            respond(&mut stream, 200, &json!({"text": text, "error": error}).to_string()).await
        }
        _ => respond(&mut stream, 404, "").await,
    }
}

/// Follows the session: what was missed (or the whole conversation), then each event as it
/// comes, one JSON per line, until the client leaves. A line every few seconds (a ping) tells a
/// dead client from a quiet session.
async fn follow(mut stream: TcpStream, epoch: Option<String>, since: u64) -> std::io::Result<()> {
    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n").await?;
    let (current, catchup, mut live) = mirror().subscribe(epoch.as_deref(), since);
    let mut last = since;
    let mut out = String::new();
    match catchup {
        po_wire::Catchup::Snapshot { seq, conversation } => {
            out.push_str(&serde_json::to_string(&po_wire::Feed::Snapshot { epoch: current, seq, conversation }).unwrap_or_default());
            out.push('\n');
            last = seq;
        }
        po_wire::Catchup::Events(events) => {
            for (seq, event) in events {
                out.push_str(&serde_json::to_string(&po_wire::Feed::Event { seq, event }).unwrap_or_default());
                out.push('\n');
                last = seq;
            }
        }
    }
    stream.write_all(out.as_bytes()).await?;
    let mut ping = tokio::time::interval(Duration::from_secs(5));
    ping.tick().await;
    loop {
        tokio::select! {
            item = live.recv() => match item {
                Ok((seq, event)) if seq > last => {
                    last = seq;
                    let closed = matches!(event, po_wire::Event::Closed);
                    let mut line = serde_json::to_string(&po_wire::Feed::Event { seq, event }).unwrap_or_default();
                    line.push('\n');
                    stream.write_all(line.as_bytes()).await?;
                    if closed {
                        break;
                    }
                }
                Ok(_) => {}
                // Too slow to keep up: the client starts again from a snapshot.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let gone = po_wire::Feed::Gone { reason: "lagged".into() };
                    stream.write_all(format!("{}\n", serde_json::to_string(&gone).unwrap_or_default()).as_bytes()).await?;
                    break;
                }
                Err(_) => break,
            },
            _ = ping.tick() => stream.write_all(b"{\"ping\":true}\n").await?,
        }
    }
    stream.shutdown().await
}

async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Service Unavailable",
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
        {"name": "po_restart", "description": "Restarts Po on the same session with the most recently installed binary (after `scripts/install.ps1`). Waits for the current turn to finish.", "inputSchema": none},
        {"name": "po_approve", "description": "Answers the approval Po is waiting for (a tool asking to run).",
         "inputSchema": {"type": "object", "properties": {"granted": {"type": "boolean"}}, "required": ["granted"]}},
        {"name": "po_interrupt", "description": "Stops the turn in progress, as Esc twice would.", "inputSchema": none}
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
        // The approval on screen, whichever it is: the MCP client sees no ids.
        "po_approve" => Action::Input(po_wire::Input::Approve { id: 0, granted: arguments["granted"].as_bool().unwrap_or(false) }),
        "po_interrupt" => Action::Input(po_wire::Input::Interrupt),
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

/// The session as events, for whoever follows it from outside (Po's service, for the phone).
///
/// One per process: a Po runs one session, and the interface, the approvals and the questions
/// all feed the same mirror without passing it around.
pub struct Mirror {
    state: std::sync::Mutex<MirrorState>,
    live: tokio::sync::broadcast::Sender<(u64, po_wire::Event)>,
}

struct MirrorState {
    journal: po_wire::Journal,
    next_id: u64,
    status: Option<po_wire::Status>,
    /// The turn's end-of-turn line, written when the agent reports its usage.
    summary: Option<String>,
    /// The session the conversation is of, and how many messages it had when last looked at:
    /// fewer, or another session, and the conversation is replaced.
    session: Option<String>,
    messages: usize,
    /// Each subagent's line being written: its text goes out by whole lines, a progress line
    /// ("Progress: …") to its card instead of its conversation.
    partial: std::collections::HashMap<String, String>,
    /// The calls' speeds, averaged over the session (generation, prefill), tokens per second.
    speeds: (f32, f32),
    /// A screen the session opened by itself and that waits for a choice (the setup at the
    /// first start): the phones open it too.
    offer: Option<String>,
    /// A model being loaded for this session, whatever started it (a command, a message,
    /// another Po): the terminal's progress line, as data, so every end shows it.
    loading: Option<po_wire::Loading>,
    /// The local servers this session's models are on: the service asks them too, so a model
    /// on any port shows on the phone.
    endpoints: Vec<String>,
}

static MIRROR: std::sync::OnceLock<Mirror> = std::sync::OnceLock::new();

/// This process's mirror.
pub fn mirror() -> &'static Mirror {
    MIRROR.get_or_init(|| {
        // A run's name: numbers restart with a new process, and must not be taken for the old ones.
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        let epoch = format!("{:x}-{:x}", std::process::id(), hasher.finish());
        Mirror {
            state: std::sync::Mutex::new(MirrorState {
                journal: po_wire::Journal::new(epoch, po_wire::Journal::CAPACITY),
                next_id: 1,
                status: None,
                summary: None,
                session: None,
                messages: 0,
                partial: std::collections::HashMap::new(),
                speeds: (0.0, 0.0),
                offer: None,
                loading: None,
                endpoints: Vec::new(),
            }),
            // Enough room for a burst of fragments while a follower is busy writing.
            live: tokio::sync::broadcast::channel(8192).0,
        }
    })
}

/// The conversation's rows from a saved history: what a resumed session shows.
pub fn items_from_messages(messages: &[po_core::Message]) -> Vec<po_wire::Item> {
    use po_core::{ContentBlock, Role};
    use po_wire::{Item, ToolState};
    let mut items = Vec::new();
    let mut calls: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for message in messages {
        for block in &message.content {
            match (message.role, block) {
                (Role::User, ContentBlock::Text { text }) if po_agent::session::is_summary(text) => {
                    items.push(Item::Notice { text: po_i18n::t!("tui.note.compacted").to_owned(), warn: false });
                }
                (Role::User, ContentBlock::Text { text }) => {
                    let typed = po_agent::session::typed_text(text);
                    if !typed.is_empty() {
                        // The images it attached: their paths, from the session's folder.
                        let images = po_agent::mentions::image_mentions(&typed);
                        items.push(Item::User { text: typed });
                        items.extend(images.into_iter().map(|path| Item::Image { path }));
                    }
                }
                (Role::User, ContentBlock::ToolResult { tool_use_id, content, is_error }) => {
                    if let Some(Item::Tool { state, output, .. }) = calls.get(tool_use_id).and_then(|&i| items.get_mut(i)) {
                        if *is_error {
                            *state = ToolState::Error;
                        }
                        *output = po_wire::clip(content, po_wire::MAX_OUTPUT);
                    }
                }
                (Role::Assistant, ContentBlock::Thinking { text, .. }) if !text.is_empty() => {
                    items.push(Item::Thinking { text: text.clone(), chars: text.len(), streaming: false })
                }
                (Role::Assistant, ContentBlock::Text { text }) if !text.trim().is_empty() => {
                    items.push(Item::Assistant { text: text.clone(), streaming: false })
                }
                (Role::Assistant, ContentBlock::ToolUse { name, input, .. }) if name == "task" => items.push(Item::Delegation {
                    agent: input["agent"].as_str().unwrap_or_default().to_owned(),
                    model: String::new(),
                    task: input["task"].as_str().unwrap_or_default().to_owned(),
                    background: input["background"].as_bool().unwrap_or(false),
                }),
                (Role::Assistant, ContentBlock::ToolUse { id, name, input }) => {
                    calls.insert(id.clone(), items.len());
                    items.push(Item::Tool {
                        name: name.clone(),
                        summary: po_agent::summarize_call(name, input),
                        state: ToolState::Done,
                        diff: None,
                        output: String::new(),
                    });
                }
                _ => {}
            }
        }
    }
    items
}

impl Mirror {
    pub fn push(&self, event: po_wire::Event) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let seq = state.journal.push(event.clone());
        // Sent under the lock: a follower subscribing meanwhile sees each event exactly once.
        let _ = self.live.send((seq, event));
    }

    /// A number for an approval or a question, so that an answer names what it answers.
    pub fn next_id(&self) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.next_id += 1;
        state.next_id
    }

    /// What the agent lets be seen, as the phone shows it.
    pub fn agent(&self, event: &po_agent::AgentEvent) {
        use po_agent::AgentEvent as A;
        use po_wire::Event as E;
        let event = match event {
            A::ModelSelected { model, .. } => E::Model { model: model.clone() },
            A::Thinking(text) => E::Thinking { text: text.clone() },
            A::ThinkingDelta(chars) => E::ThinkingDelta { chars: *chars },
            A::ThinkingText(text) => E::ThinkingText { text: text.clone() },
            A::Text(text) => E::Text { text: text.clone() },
            A::TextDelta(text) => E::TextDelta { text: text.clone() },
            A::ToolStarted { name, summary } => {
                let action = match name.as_str() {
                    "read" => if summary.is_empty() { "Lecture de fichier…".to_string() } else { format!("Lecture de {summary}…") },
                    "write" | "edit" => if summary.is_empty() { "Modification de fichier…".to_string() } else { format!("Modification de {summary}…") },
                    "bash" => if summary.is_empty() { "Exécution de commande…".to_string() } else { format!("Exécution : {summary}…") },
                    "glob" | "grep" => if summary.is_empty() { "Recherche dans le projet…".to_string() } else { format!("Recherche : {summary}…") },
                    "web_search" => if summary.is_empty() { "Recherche sur le web…".to_string() } else { format!("Recherche web : {summary}…") },
                    "web_fetch" => if summary.is_empty() { "Lecture d'une page web…".to_string() } else { format!("Lecture de {summary}…") },
                    "task" => if summary.is_empty() { "Sous-agent au travail…".to_string() } else { format!("Sous-agent : {summary}…") },
                    "po_labs" => "Po Labs au travail…".to_string(),
                    other => format!("{other}…"),
                };
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(s) = &mut state.status {
                        s.dynamic_action = Some(action);
                        Some(s.clone())
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                E::ToolStarted { name: name.clone(), summary: summary.clone() }
            },
            A::ToolFinished { name, summary, is_error } => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(s) = &mut state.status {
                        s.dynamic_action = None;
                        Some(s.clone())
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                E::ToolFinished { name: name.clone(), summary: summary.clone(), error: *is_error }
            },
            A::ToolDiff { diff } => E::ToolDiff { diff: po_wire::clip(diff, po_wire::MAX_DIFF) },
            A::ToolOutput { output } => E::ToolOutput { output: po_wire::clip(output, po_wire::MAX_OUTPUT) },
            A::Image { path } => E::Image { path: path.clone() },
            A::ToolDenied { name, summary } => E::ToolDenied { name: name.clone(), summary: summary.clone() },
            A::Asked { question, answer } => E::Asked { question: question.clone(), answer: answer.clone() },
            A::Delegated { model, agent, task, background } => E::Delegated { agent: agent.clone(), model: model.clone(), task: task.clone(), background: *background },
            A::ModelDisabled { model, reason } => E::Error { text: po_i18n::t!("tui.model.disabled", model = model, reason = reason) },
            A::Notice(text) => E::Notice { text: text.clone() },
            A::ContextWarning { message, .. } => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(mut s) = state.status.clone() {
                        s.context_warning = Some(message.clone());
                        state.status = Some(s.clone());
                        Some(s)
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::Context { used, window, breakdown } => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.status.clone().map(|mut s| {
                        s.context_used = *used;
                        s.context_window = *window;
                        s.context_tools = breakdown.tools;
                        s.context_thinking = breakdown.thinking;
                        s.context_assistant = breakdown.assistant;
                        s.context_user = breakdown.user;
                        s.context_system = breakdown.system;
                        state.status = Some(s.clone());
                        s
                    })
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::Goals(goals) => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.status.clone().map(|mut s| {
                        s.goals = goals
                            .iter()
                            .map(|(text, passed)| po_wire::GoalRec {
                                text: text.clone(),
                                state: match passed {
                                    Some(true) => "met",
                                    Some(false) => "failed",
                                    None => "pending",
                                }
                                .to_owned(),
                            })
                            .collect();
                        state.status = Some(s.clone());
                        s
                    })
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::ContextWarningCleared => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(mut s) = state.status.clone() {
                        s.context_warning = None;
                        state.status = Some(s.clone());
                        Some(s)
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::LoopWarning { message } => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(mut s) = state.status.clone() {
                        s.loop_warning = Some(message.clone());
                        state.status = Some(s.clone());
                        Some(s)
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::LoopWarningCleared => {
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(mut s) = state.status.clone() {
                        s.loop_warning = None;
                        state.status = Some(s.clone());
                        Some(s)
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::TurnCompleted { usage, cost, .. } => {
                let mut parts = vec![format!("{} tokens", usage.total())];
                if *cost > 0.0 {
                    parts.push(format!("${cost:.4}"));
                }
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.summary = Some(parts.join(" · "));
                    if let Some(s) = &mut state.status {
                        s.dynamic_action = None;
                        Some(s.clone())
                    } else {
                        None
                    }
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::CallStats(stats) => {
                // A running average, the recent calls weighing more: the speed the user sees now.
                let status = {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let blend = |old: f32, new: f64| if new <= 0.0 { old } else if old <= 0.0 { new as f32 } else { old * 0.7 + new as f32 * 0.3 };
                    state.speeds = (blend(state.speeds.0, stats.generation_tok_s), blend(state.speeds.1, stats.prefill_tok_s));
                    if let Some(s) = &mut state.status {
                        if stats.generation_tok_s > 0.0 {
                            s.instant_tok_s = stats.generation_tok_s as f32;
                        }
                        if stats.prefill_tok_s > 0.0 {
                            s.last_prefill_tok_s = stats.prefill_tok_s as f32;
                        }
                    }
                    state.status.clone()
                };
                if let Some(status) = status {
                    self.status(status);
                }
                return;
            }
            A::Queued | A::Retrying { .. } => return,
        };
        self.push(event);
    }

    /// What a subagent does, as the phone follows it.
    pub fn subagent(&self, update: &po_agent::SubagentUpdate) {
        use po_agent::SubagentUpdate as U;
        use po_wire::SubagentEvent as S;
        let mut out = Vec::new();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            fn flush(partial: &mut std::collections::HashMap<String, String>, id: &str, out: &mut Vec<po_wire::SubagentEvent>) {
                use po_wire::SubagentEvent as S;
                if let Some(rest) = partial.remove(id).filter(|r| !r.is_empty()) {
                    match po_agent::progress_line(&rest) {
                        Some(progress) => out.push(S::Progress { id: id.to_owned(), text: progress.to_owned() }),
                        None => out.push(S::Text { id: id.to_owned(), text: rest }),
                    }
                }
            }
            match update {
                U::Text { id, text } => {
                    let partial = state.partial.entry(id.clone()).or_default();
                    partial.push_str(text);
                    let mut lines = Vec::new();
                    while let Some(end) = partial.find('\n') {
                        lines.push(partial.drain(..=end).collect::<String>());
                    }
                    for line in lines {
                        match po_agent::progress_line(line.trim_end()) {
                            Some(progress) => out.push(S::Progress { id: id.clone(), text: progress.to_owned() }),
                            None => out.push(S::Text { id: id.clone(), text: line }),
                        }
                    }
                }
                U::Started { id, agent, model, task, background } => {
                    state.partial.remove(id);
                    out.push(S::Started { id: id.clone(), agent: agent.clone(), model: model.clone(), task: task.clone(), background: *background });
                }
                U::Thinking { id, text } => {
                    flush(&mut state.partial, id, &mut out);
                    out.push(S::Thinking { id: id.clone(), text: text.clone() });
                }
                U::Tool { id, name, summary } => {
                    flush(&mut state.partial, id, &mut out);
                    out.push(S::Tool { id: id.clone(), name: name.clone(), summary: summary.clone() });
                }
                U::ToolDone { id, name, ok } => out.push(S::ToolDone { id: id.clone(), name: name.clone(), ok: *ok }),
                U::Paused { id } => {
                    flush(&mut state.partial, id, &mut out);
                    out.push(S::Paused { id: id.clone() });
                }
                U::Finished { id, ok } => {
                    flush(&mut state.partial, id, &mut out);
                    out.push(S::Finished { id: id.clone(), ok: *ok });
                }
            }
        }
        for update in out {
            self.push(po_wire::Event::Subagent { update: Box::new(update) });
        }
    }

    /// A turn starts; `prompt` is the user's message, if the turn has one (a resumed turn has not).
    pub fn turn_started(&self, prompt: Option<&str>) {
        if let Some(text) = prompt {
            self.push(po_wire::Event::User { text: po_agent::session::typed_text(text) });
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).summary = None;
        self.push(po_wire::Event::TurnStarted);
    }

    pub fn turn_ended(&self, interrupted: bool, error: Option<String>, seconds: f64) {
        let summary = self.state.lock().unwrap_or_else(|e| e.into_inner()).summary.take();
        let summary = summary.map(|s| format!("{seconds:.1} s · {s}"));
        // Said in the conversation, as on the PC's screen: a stopped turn otherwise looked like
        // one that had simply ended without an answer.
        if interrupted && error.is_none() {
            self.push(po_wire::Event::Notice { text: po_i18n::t!("tui.note.interrupted").to_owned() });
        }
        self.push(po_wire::Event::TurnEnded { interrupted, error, summary });
    }

    /// What the status bar shows, sent only when it changed.
    pub fn status(&self, mut status: po_wire::Status) {
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (status.generation_tok_s, status.prefill_tok_s) = state.speeds;
            status.offer = state.offer.clone();
            // A loading the session is waiting for belongs to the mirror, not to whoever
            // publishes: a status going out in the middle of a load must carry it.
            status.loading = state.loading.clone();
            if state.status.as_ref() == Some(&status) {
                return;
            }
            state.status = Some(status.clone());
        }
        self.push(po_wire::Event::Status { status: Box::new(status) });
    }

    /// Says that a model is being loaded for this session (`None`: it is over). The status goes
    /// out again at once when it changed: a phone shows what the terminal's progress line says.
    pub fn current_loading(&self) -> Option<po_wire::Loading> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).loading.clone()
    }

    pub fn loading(&self, loading: Option<po_wire::Loading>) {
        let status = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.loading == loading {
                return;
            }
            state.loading = loading;
            state.status.clone()
        };
        if let Some(status) = status {
            self.status(status);
        }
    }

    /// The screen to offer the phones (`Some("/setup")`), or none any more; the status goes out
    /// again at once when it changed.
    pub fn offer(&self, line: Option<String>) {
        let status = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.offer == line {
                return;
            }
            state.offer = line;
            state.status.clone()
        };
        if let Some(status) = status {
            self.status(status);
        }
    }

    /// The session's history as it now is: if it was replaced (a new session, a resumed one, a
    /// compaction), the conversation is rebuilt from it and sent whole.
    pub fn sync(&self, session: Option<&str>, messages: &[po_core::Message], archive: &[po_core::Message]) {
        let replaced = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let replaced = state.session.as_deref() != session || messages.len() < state.messages;
            state.session = session.map(str::to_owned);
            state.messages = messages.len();
            replaced
        };
        if replaced {
            // The guard is let go before the event goes out: `push` takes the same lock.
            let status = {
                let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                // A phone coming back in the middle of a load must see it, even if no status went
                // out since it left.
                let mut status = state.status.clone().unwrap_or_default();
                status.loading = state.loading.clone();
                status
            };
            // The phone gets the whole conversation, archived part included, as the screen does.
            let all: Vec<po_core::Message> = archive.iter().chain(messages.iter()).cloned().collect();
            let mut conversation = po_wire::Conversation::new(items_from_messages(&all));
            conversation.status = status;
            self.push(po_wire::Event::Reset { conversation: Box::new(conversation) });
        }
    }

    /// What a follower needs to start: the run, what it missed (or everything), and the events
    /// to come.
    pub fn subscribe(&self, epoch: Option<&str>, since: u64) -> (String, po_wire::Catchup, tokio::sync::broadcast::Receiver<(u64, po_wire::Event)>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.journal.epoch.clone(), state.journal.since(epoch, since), self.live.subscribe())
    }

    pub fn conversation(&self) -> po_wire::Conversation {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).journal.conversation().clone()
    }

    /// The session in a few words, for the service's lists.
    pub fn info(&self) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let conversation = state.journal.conversation();
        // The loading the mirror knows about, even if no status went out since it began.
        let known = state.loading.clone().or_else(|| conversation.status.loading.clone());
        let status = po_wire::Status { loading: known, ..conversation.status.clone() };
        json!({
            "epoch": state.journal.epoch,
            "seq": state.journal.last(),
            "session": state.session,
            "busy": conversation.busy,
            "status": status,
            "approval": conversation.approval.is_some(),
            "question": conversation.question.is_some(),
            "exchanges": conversation.items.iter().filter(|i| matches!(i, po_wire::Item::User { .. })).count(),
            "endpoints": state.endpoints,
        })
    }

    /// The local servers this session's models are on (see `MirrorState::endpoints`).
    pub fn set_endpoints(&self, endpoints: Vec<String>) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).endpoints = endpoints;
    }
}

/// A menu of the interface, as the phone shows it.
pub fn wire_menu(menu: &crate::menu::Menu) -> po_wire::Menu {
    use crate::menu::Action as M;
    po_wire::Menu {
        title: menu.title.clone(),
        // A line under way is marked for the terminal's spinner, not for the phone.
        note: menu.note.as_ref().map(|n| n.lines().map(|l| l.strip_prefix("~ ").unwrap_or(l)).collect::<Vec<_>>().join("\n")),
        // "Close" is the terminal's: the phone closes a menu its own way.
        items: menu
            .items
            .iter()
            .filter(|item| !item.closes())
            .map(|item| {
                let (action, command) = match &item.action {
                    M::Run(c) => (po_wire::MenuAction::Run, c.clone()),
                    M::Show(c) => (po_wire::MenuAction::Show, c.clone()),
                    M::Fill(c) => (po_wire::MenuAction::Fill, c.clone()),
                };
                po_wire::MenuItem { label: item.label.clone(), detail: item.detail.clone(), action, command, current: item.current, then: item.then.clone() }
            })
            .collect(),
        back: menu.back.clone(),
        live: menu.live,
        busy: menu.busy,
    }
}

/// A command's report, as the phone shows it.
pub fn wire_lines(lines: &[crate::CardLine]) -> Vec<po_wire::OutLine> {
    use crate::CardLine as C;
    use po_wire::OutLine as O;
    lines
        .iter()
        .map(|line| match line.clone() {
            C::Plain(text) => O::Plain { text },
            C::Warn(text) => O::Warn { text },
            C::Dim(text) => O::Dim { text },
            C::Section(text) => O::Section { text },
            C::Item(name, detail) => O::Item { name, detail },
            C::Field(key, value) => O::Field { key, value },
        })
        .collect()
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

    /// One HTTP request, the answer as text (a stream is read for `wait` at most).
    async fn http(url: &str, method: &str, body: &str, wait: Duration) -> String {
        let rest = url.trim_start_matches("http://");
        let (host, path) = rest.split_once('/').unwrap();
        let mut stream = TcpStream::connect(host).await.unwrap();
        let request = format!("{method} /{path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(Ok(n)) = tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn feeds(response: &str) -> Vec<po_wire::Feed> {
        response.split("\r\n\r\n").nth(1).unwrap_or("").lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    #[tokio::test]
    async fn a_follower_gets_the_conversation_then_what_comes_and_resumes_where_it_left() {
        let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
        let server = start(None, tx).await.unwrap();
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                let _ = request.reply.send((format!("{:?}", request.action), false));
            }
        });
        mirror().push(po_wire::Event::Notice { text: "first".into() });
        let events = format!("{}/events", server.url);
        let pushing = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            mirror().push(po_wire::Event::Notice { text: "live".into() });
        });
        let got = feeds(&http(&events, "GET", "", Duration::from_millis(900)).await);
        pushing.await.unwrap();
        let Some(po_wire::Feed::Snapshot { epoch, seq, conversation }) = got.first().cloned() else { panic!("{got:?}") };
        assert!(conversation.items.contains(&po_wire::Item::Notice { text: "first".into(), warn: false }));
        let live = got.iter().find_map(|f| match f {
            po_wire::Feed::Event { seq: n, event: po_wire::Event::Notice { text } } if text == "live" => Some(*n),
            _ => None,
        });
        let live = live.expect("the live event");
        assert!(live > seq);
        // Coming back with the last number seen: only what came after.
        mirror().push(po_wire::Event::Notice { text: "missed".into() });
        let again = feeds(&http(&format!("{events}?epoch={epoch}&since={live}"), "GET", "", Duration::from_millis(400)).await);
        // Among them, not necessarily first: the tests running alongside share this mirror.
        assert!(again.iter().any(|f| matches!(f, po_wire::Feed::Event { event: po_wire::Event::Notice { text }, .. } if text == "missed")), "{again:?}");
        assert!(again.iter().all(|f| !matches!(f, po_wire::Feed::Event { seq: n, .. } if *n <= live)), "nothing from before: {again:?}");
        assert!(!again.iter().any(|f| matches!(f, po_wire::Feed::Snapshot { .. })));
        // Acting: an approval answer reaches the interface as such.
        let answer = http(&format!("{}/input", server.url), "POST", r#"{"kind":"approve","id":7,"granted":true}"#, Duration::from_secs(2)).await;
        assert!(answer.contains("Approve") && answer.contains("granted: true"), "{answer}");
        let empty = http(&format!("{}/input", server.url), "POST", r#"{"kind":"command","text":"rm"}"#, Duration::from_secs(2)).await;
        assert!(empty.starts_with("HTTP/1.1 400"), "a command without its slash is refused");
        let base = server.url.rsplit_once('/').unwrap().0.to_owned();
        assert!(http(&format!("{base}/wrong/events"), "GET", "", Duration::from_millis(300)).await.starts_with("HTTP/1.1 404"));
    }

    #[test]
    fn a_saved_history_becomes_the_same_rows() {
        use po_core::{ContentBlock, Message, Role};
        let messages = vec![
            Message::user("[mode: build]

fix it"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking { text: "hmm".into(), signature: None },
                    ContentBlock::Text { text: "On it.".into() },
                    ContentBlock::ToolUse { id: "1".into(), name: "read".into(), input: json!({"path": "a.rs"}) },
                ],
            },
            Message::tool_results(vec![ContentBlock::ToolResult { tool_use_id: "1".into(), content: "no".into(), is_error: true }]),
        ];
        let items = items_from_messages(&messages);
        assert_eq!(items[0], po_wire::Item::User { text: "fix it".into() });
        assert!(matches!(&items[1], po_wire::Item::Thinking { text, .. } if text == "hmm"));
        assert!(matches!(&items[3], po_wire::Item::Tool { name, state: po_wire::ToolState::Error, .. } if name == "read"));
        assert_eq!(items.len(), 4);
    }

    #[test]
    fn an_image_sent_with_a_message_comes_back_as_its_own_row() {
        let items = items_from_messages(&[po_core::Message::user("what is wrong here? @.po/uploads/20261106-101010-shot.png @notes.txt")]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1], po_wire::Item::Image { path: ".po/uploads/20261106-101010-shot.png".into() });
    }

    #[tokio::test]
    async fn an_approval_is_announced_and_its_end_too_answered_or_not() {
        use po_agent::Approver;
        let (approver, mut receiver) = crate::approval::channel();
        let (_, _, mut live) = mirror().subscribe(None, 0);
        let task = tokio::spawn(async move { approver.approve("bash", &json!({"command": "ls"})).await });
        let request = receiver.recv().await.unwrap();
        let id = request.id;
        drop(request);
        assert!(!task.await.unwrap(), "dropped unanswered: refused");
        let mut seen = Vec::new();
        while let Ok(Ok((_, event))) = tokio::time::timeout(Duration::from_millis(200), live.recv()).await {
            seen.push(event);
        }
        assert!(seen.contains(&po_wire::Event::Approval { id, tool: "bash".into(), summary: "ls".into() }));
        assert!(seen.contains(&po_wire::Event::ApprovalDone { id, granted: false }));
    }

    /// A mirror of its own: the tests must not read, or disturb, the events of the session other
    /// tests are following in the same process.
    fn local_mirror() -> Mirror {
        Mirror {
            state: std::sync::Mutex::new(MirrorState {
                journal: po_wire::Journal::new("test", po_wire::Journal::CAPACITY),
                next_id: 1,
                status: None,
                summary: None,
                session: None,
                messages: 0,
                partial: std::collections::HashMap::new(),
                speeds: (0.0, 0.0),
                offer: None,
                loading: None,
                endpoints: Vec::new(),
            }),
            live: tokio::sync::broadcast::channel(64).0,
        }
    }

    /// What a mirror has been told about a loading, among the statuses and snapshots it sent
    /// after `from`.
    async fn loadings(live: &mut tokio::sync::broadcast::Receiver<(u64, po_wire::Event)>, from: u64) -> Vec<Option<po_wire::Loading>> {
        let mut out = Vec::new();
        while let Ok(Ok((seq, event))) = tokio::time::timeout(Duration::from_millis(250), live.recv()).await {
            if seq > from {
                match event {
                    po_wire::Event::Status { status } => out.push(status.loading.clone()),
                    po_wire::Event::Reset { conversation } => out.push(conversation.status.loading.clone()),
                    _ => {}
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn a_loading_reaches_the_phones_once_per_stage_and_survives_a_snapshot() {
        let loading = |stage: &str| po_wire::Loading { model: "qwen".into(), stage: stage.into(), since_ms: 1000 };
        let mirror = local_mirror();
        let (epoch, catchup, mut live) = mirror.subscribe(None, 0);
        let seq = match &catchup {
            po_wire::Catchup::Events(events) => events.last().map(|(n, _)| *n).unwrap_or(0),
            po_wire::Catchup::Snapshot { seq, .. } => *seq,
        };
        // A turn starts: the status goes out, with nothing being loaded yet.
        mirror.status(po_wire::Status { model: "qwen".into(), busy: true, ..Default::default() });
        assert_eq!(loadings(&mut live, seq).await, vec![None], "a status published before the load says nothing");

        // The server starts loading the model: said once, then once per stage - a beat that only
        // ages the line is not an event, or a 30-second load would flood the journal.
        mirror.loading(Some(loading("starting the server")));
        mirror.loading(Some(loading("starting the server")));
        mirror.loading(Some(loading("loading the weights")));
        assert_eq!(
            loadings(&mut live, seq).await,
            vec![Some(loading("starting the server")), Some(loading("loading the weights"))],
            "said when it changed, {epoch}"
        );

        // A phone that opens in the middle of the load gets it in the snapshot it starts from, and
        // in the reset that follows.
        mirror.sync(Some("run"), &[], &[]);
        let (_, catchup, _) = mirror.subscribe(Some(&epoch), u64::MAX);
        match catchup {
            po_wire::Catchup::Snapshot { conversation, .. } => assert_eq!(conversation.status.loading, Some(loading("loading the weights"))),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            loadings(&mut live, seq).await.first(),
            Some(&Some(loading("loading the weights"))),
            "the reset sent to a follower coming back carries it too",
        );

        // A status published in the middle of the load carries it too, whoever published it.
        mirror.status(po_wire::Status { model: "qwen".into(), busy: true, context_used: 12, ..Default::default() });
        assert_eq!(loadings(&mut live, seq).await.last(), Some(&Some(loading("loading the weights"))));

        // The model answers: nothing is being waited for any more.
        mirror.loading(None);
        assert_eq!(loadings(&mut live, seq).await.last(), Some(&None));
        assert_eq!(mirror.conversation().status.loading, None);
        // The session list says it too, where a turn has not started yet.
        mirror.loading(Some(loading("loading the weights")));
        mirror.status(po_wire::Status { model: "qwen".into(), ..Default::default() });
        assert!(mirror.info()["status"]["loading"]["stage"].as_str() == Some("loading the weights"), "{:?}", mirror.info());
    }
}
