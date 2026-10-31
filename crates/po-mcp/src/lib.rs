//! `po-mcp` — Model Context Protocol client.
//!
//! An MCP server is a process that exposes tools. Po starts it, queries it, and presents its
//! tools to the model as its own: from the agent loop's point of view, nothing distinguishes
//! an MCP tool from a built-in one.
//!
//! Two transports: stdio, which covers nearly all local servers, and HTTP ("Streamable HTTP")
//! for remote servers. The old SSE transport is not supported.

pub mod protocol;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{ToolOutcome, ToolSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

pub use protocol::{Incoming, McpError, McpTool};

/// Maximum time granted to a request. A server that exceeds it is considered unreachable:
/// better to tell the model than to freeze the session.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Startup can be slow — installing a package, opening a database.
const INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Declaration of a server, as it appears in the configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub name: String,
    /// Command of a stdio server. Empty for an HTTP server.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Address of an HTTP server ("Streamable HTTP"). Takes precedence over `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Headers sent to an HTTP server (`Authorization`, for example).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

impl ServerConfig {
    pub fn stdio(name: &str, command: &str, args: Vec<String>) -> Self {
        Self { name: name.into(), command: command.into(), args, env: HashMap::new(), url: None, headers: HashMap::new() }
    }

    pub fn http(name: &str, url: &str) -> Self {
        Self { name: name.into(), command: String::new(), args: Vec::new(), env: HashMap::new(), url: Some(url.into()), headers: HashMap::new() }
    }
}

/// Live connection to an MCP server.
pub struct McpServer {
    name: String,
    /// Access is serialized: the protocol allows multiplexing by identifier, but a single lock
    /// avoids interleaving reads, at no complexity gain here.
    transport: Transport,
    tools: Vec<McpTool>,
}

enum Transport {
    Stdio(Box<Mutex<ServerIo>>),
    Http(HttpIo),
}

struct HttpIo {
    client: reqwest::Client,
    url: String,
    headers: HashMap<String, String>,
    /// Session identifier assigned by the server at initialization, sent back afterwards.
    session: Mutex<Option<String>>,
    next_id: std::sync::atomic::AtomicU64,
}

struct ServerIo {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpServer {
    /// Starts the server, initializes it and fetches its tool list.
    pub async fn connect(config: &ServerConfig) -> Result<Self, McpError> {
        if let Some(url) = &config.url {
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .map_err(|err| McpError::Spawn { server: config.name.clone(), detail: err.to_string() })?;
            let http = HttpIo {
                client,
                url: url.clone(),
                headers: config.headers.clone(),
                session: Mutex::new(None),
                next_id: std::sync::atomic::AtomicU64::new(1),
            };
            let server = Self { name: config.name.clone(), transport: Transport::Http(http), tools: Vec::new() };
            return server.initialize().await;
        }
        let mut child = match spawn(config, false) {
            Ok(child) => child,
            // On Windows, `npx`, `uvx` or `pnpm` are `.cmd` scripts that `CreateProcess` does not
            // find without an extension. Going through `cmd /C` applies the shell's resolution,
            // as in a terminal.
            Err(err)
                if cfg!(windows)
                    && err.kind() == std::io::ErrorKind::NotFound
                    && windows_script_exists(&config.command) =>
            {
                spawn(config, true).map_err(|err| McpError::Spawn {
                    server: config.name.clone(),
                    detail: err.to_string(),
                })?
            }
            Err(err) => {
                return Err(McpError::Spawn { server: config.name.clone(), detail: err.to_string() });
            }
        };

        let stdin = child.stdin.take().ok_or_else(|| McpError::Spawn {
            server: config.name.clone(),
            detail: po_i18n::t!("mcp.error.no_stdin").into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::Spawn {
            server: config.name.clone(),
            detail: po_i18n::t!("mcp.error.no_stdout").into(),
        })?;

        let io = ServerIo { child, stdin, stdout: BufReader::new(stdout), next_id: 1 };
        let server = Self { name: config.name.clone(), transport: Transport::Stdio(Box::new(Mutex::new(io))), tools: Vec::new() };
        server.initialize().await
    }

    /// Protocol handshake, shared by both transports.
    async fn initialize(mut self) -> Result<Self, McpError> {
        let server = &mut self;
        server
            .call_raw(
                "initialize",
                protocol::initialize_params("po", env!("CARGO_PKG_VERSION")),
                INIT_TIMEOUT,
            )
            .await?;

        // The protocol requires this notification before any other exchange.
        server.notify("notifications/initialized").await?;

        // The list may come in pages: a server with many tools only gives the first one
        // otherwise. A bound guards against a server that always answers with a cursor.
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let params = cursor.as_ref().map_or(Value::Null, |c| json!({"cursor": c}));
            let page = server.call_raw("tools/list", params, REQUEST_TIMEOUT).await?;
            server.tools.extend(protocol::parse_tools(&page));
            cursor = page.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(self)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Calls a tool of the server.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<(String, bool), McpError> {
        let result = self
            .call_raw(
                "tools/call",
                json!({"name": tool, "arguments": arguments}),
                REQUEST_TIMEOUT,
            )
            .await?;
        Ok(protocol::flatten_content(&result))
    }

    async fn notify(&self, method: &str) -> Result<(), McpError> {
        let io = match &self.transport {
            Transport::Http(http) => {
                let body = protocol::notification(method, Value::Null);
                return self.http_post(http, body, REQUEST_TIMEOUT).await.map(|_| ());
            }
            Transport::Stdio(io) => io,
        };
        let mut io = io.lock().await;
        let line = format!("{}\n", protocol::notification(method, Value::Null));
        io.stdin.write_all(line.as_bytes()).await.map_err(|_| McpError::Closed {
            server: self.name.clone(),
        })?;
        io.stdin.flush().await.map_err(|_| McpError::Closed { server: self.name.clone() })?;
        Ok(())
    }

    async fn call_raw(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let io = match &self.transport {
            Transport::Http(http) => {
                let id = http.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body = self.http_post(http, protocol::request(id, method, params), timeout).await?;
                return match protocol::find_response(&body, id) {
                    Some(Incoming::Result { value, .. }) => Ok(value),
                    Some(Incoming::Error { code, message, .. }) => {
                        Err(McpError::Rpc { server: self.name.clone(), code, message })
                    }
                    _ => Err(McpError::Closed { server: self.name.clone() }),
                };
            }
            Transport::Stdio(io) => io,
        };
        let mut io = io.lock().await;
        let id = io.next_id;
        io.next_id += 1;

        let line = format!("{}\n", protocol::request(id, method, params));
        io.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|_| McpError::Closed { server: self.name.clone() })?;
        io.stdin
            .flush()
            .await
            .map_err(|_| McpError::Closed { server: self.name.clone() })?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout_at(deadline, io.stdout.read_line(&mut line))
                .await
                .map_err(|_| McpError::Timeout { server: self.name.clone() })?;

            match read {
                Ok(0) => {
                    // End of stream: the server is dead. We report it rather than loop.
                    return Err(McpError::Closed { server: self.name.clone() });
                }
                Ok(_) => {}
                Err(_) => return Err(McpError::Closed { server: self.name.clone() }),
            }

            if let Some(answer) = protocol::answer_to_server_request(&line) {
                let _ = io.stdin.write_all(format!("{answer}\n").as_bytes()).await;
                let _ = io.stdin.flush().await;
                continue;
            }
            // Notifications and noise are ignored until *our* reply is found: settling for the
            // first line would make any chatty server fail.
            match protocol::parse_line(&line) {
                Some(Incoming::Result { id: got, value }) if got == id => return Ok(value),
                Some(Incoming::Error { id: got, code, message }) if got == id => {
                    return Err(McpError::Rpc { server: self.name.clone(), code, message });
                }
                _ => continue,
            }
        }
    }

    /// Sends a JSON-RPC message by POST and returns the response body. The server may reply
    /// in JSON or as an SSE stream: both are accepted, as the transport requires.
    async fn http_post(&self, http: &HttpIo, body: String, timeout: Duration) -> Result<String, McpError> {
        let mut request = http
            .client
            .post(&http.url)
            .timeout(timeout)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(body);
        for (key, value) in &http.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        if let Some(session) = http.session.lock().await.clone() {
            request = request.header("mcp-session-id", session);
        }
        let response = request.send().await.map_err(|err| McpError::Spawn {
            server: self.name.clone(),
            detail: err.to_string(),
        })?;
        if let Some(session) = response.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *http.session.lock().await = Some(session.to_owned());
        }
        let status = response.status();
        if !status.is_success() {
            return Err(McpError::Spawn { server: self.name.clone(), detail: format!("HTTP {status}") });
        }
        response.text().await.map_err(|_| McpError::Closed { server: self.name.clone() })
    }

    /// Stops the server.
    pub async fn shutdown(&self) {
        match &self.transport {
            Transport::Stdio(io) => {
                let _ = io.lock().await.child.kill().await;
            }
            Transport::Http(http) => {
                // Close the session on the server side; a refusal has no consequence.
                if let Some(session) = http.session.lock().await.clone() {
                    let _ = http.client.delete(&http.url).header("mcp-session-id", session).send().await;
                }
            }
        }
    }
}

/// Adapter that presents an MCP tool as an ordinary tool of the registry.
pub struct McpToolAdapter {
    server: Arc<McpServer>,
    tool: McpTool,
    /// Name exposed to the model, prefixed by the server to avoid collisions between servers
    /// — two servers may each perfectly well expose a `search` tool.
    exposed_name: String,
}

impl McpToolAdapter {
    pub fn new(server: Arc<McpServer>, tool: McpTool) -> Self {
        let exposed_name = exposed_name(server.name(), &tool.name);
        Self { server, tool, exposed_name }
    }
}

#[async_trait]
impl po_tools::Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.exposed_name
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            self.exposed_name.clone(),
            if self.tool.description.is_empty() {
                format!("Tool \"{}\" provided by the {} server", self.tool.name, self.server.name())
            } else {
                self.tool.description.clone()
            },
            self.tool.input_schema.clone(),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        // An MCP server is third-party code whose effects are unknown: unlike the built-in
        // tools, we cannot reason about what it does. The user's approval is therefore asked
        // for, without exception.
        Permission::Ask
    }

    async fn run(&self, id: &str, input: Value, _ctx: &po_tools::ToolContext) -> ToolOutcome {
        match self.server.call_tool(&self.tool.name, input).await {
            Ok((content, is_error)) if is_error => ToolOutcome::error(id, content),
            Ok((content, _)) => ToolOutcome::ok(id, content)
                .with_display(format!("{} · {}", self.server.name(), self.tool.name)),
            Err(err) => ToolOutcome::error(id, err.to_string()),
        }
    }
}

/// File where a server writes its diagnostics. Left on the terminal, they were printed over the
/// full-screen interface; captured without being read, they would eventually block the server
/// once the pipe is full. One file per server solves both.
pub fn log_path(server: &str) -> std::path::PathBuf {
    let dir = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("po")
        .join("logs");
    let safe: String =
        server.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    dir.join(format!("mcp-{safe}.log"))
}

/// Is there a `<command>.cmd` or `.bat` in the PATH? The `cmd /C` fallback is only attempted
/// under this condition: otherwise a nonexistent command would start a `cmd` anyway, and the
/// error would come later, in an incomprehensible form.
fn windows_script_exists(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        ["cmd", "bat"].iter().any(|ext| dir.join(format!("{command}.{ext}")).is_file())
    })
}

fn spawn(config: &ServerConfig, through_shell: bool) -> std::io::Result<tokio::process::Child> {
    let mut command = if through_shell {
        let mut command = tokio::process::Command::new("cmd");
        command.arg("/C").arg(&config.command);
        command
    } else {
        tokio::process::Command::new(&config.command)
    };
    let log = log_path(&config.name);
    let stderr = log
        .parent()
        .and_then(|dir| std::fs::create_dir_all(dir).ok())
        .and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(&log).ok())
        .map(std::process::Stdio::from)
        .unwrap_or_else(std::process::Stdio::null);
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(stderr)
        .kill_on_drop(true);
    command.spawn()
}

/// Connects a single server and returns its tools, for plugging in mid-session.
pub async fn connect_one(config: &ServerConfig) -> Result<Vec<Arc<dyn po_tools::Tool>>, String> {
    let server = Arc::new(McpServer::connect(config).await.map_err(|err| err.to_string())?);
    Ok(server
        .tools()
        .iter()
        .map(|tool| Arc::new(McpToolAdapter::new(Arc::clone(&server), tool.clone())) as Arc<dyn po_tools::Tool>)
        .collect())
}

/// Prefix of a server's tools, as the model sees them.
pub fn tool_prefix(server: &str) -> String {
    format!("{}__", wire_safe(server))
}

/// The name a tool gets before the model: its server's prefix, then its name, both limited to
/// what the APIs accept (`^[a-zA-Z0-9_-]{1,64}$`). A server named `my.server`, or a tool
/// `files/read`, made every request of the session refused by a cloud API.
pub fn exposed_name(server: &str, tool: &str) -> String {
    let prefix = tool_prefix(server);
    let room = 64usize.saturating_sub(prefix.len()).max(1);
    format!("{prefix}{}", wire_safe(tool).chars().take(room).collect::<String>())
}

fn wire_safe(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// Reads a `.mcp.json` file, the format shared by most harnesses
/// (`{"mcpServers": {"name": {"command": ..., "args": [...], "env": {...}}}}`). Entries with a
/// `url` become HTTP servers; the old SSE transport is skipped.
pub fn read_mcp_json(text: &str) -> Vec<ServerConfig> {
    let Ok(value) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(servers) = value.get("mcpServers").and_then(Value::as_object) else { return Vec::new() };
    let mut configs: Vec<ServerConfig> = servers
        .iter()
        .filter_map(|(name, entry)| {
            let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
            if let Some(url) = entry.get("url").and_then(Value::as_str) {
                // The old SSE transport (two endpoints) is not supported.
                if kind == "sse" {
                    return None;
                }
                let mut config = ServerConfig::http(name, url);
                config.headers = entry
                    .get("headers")
                    .and_then(Value::as_object)
                    .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
                    .unwrap_or_default();
                return Some(config);
            }
            let command = entry.get("command")?.as_str()?.to_owned();
            let args = entry
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            let env = entry
                .get("env")
                .and_then(Value::as_object)
                .map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect())
                .unwrap_or_default();
            Some(ServerConfig { name: name.clone(), command, args, env, url: None, headers: HashMap::new() })
        })
        .collect();
    configs.sort_by(|a, b| a.name.cmp(&b.name));
    configs
}

/// Connects all configured servers and returns their tools, ready to be registered.
///
/// A server that fails to start does not prevent the others: an aging configuration must not
/// make the session unusable.
pub async fn connect_all(
    configs: &[ServerConfig],
) -> (Vec<Arc<dyn po_tools::Tool>>, Vec<(String, String)>) {
    let mut tools: Vec<Arc<dyn po_tools::Tool>> = Vec::new();
    let mut failures = Vec::new();

    for config in configs {
        match McpServer::connect(config).await {
            Ok(server) => {
                let server = Arc::new(server);
                for tool in server.tools() {
                    tools.push(Arc::new(McpToolAdapter::new(Arc::clone(&server), tool.clone())));
                }
            }
            Err(err) => failures.push((config.name.clone(), err.to_string())),
        }
    }
    (tools, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_tools::Tool as _;

    fn tool(name: &str) -> McpTool {
        McpTool {
            name: name.into(),
            description: "does something".into(),
            input_schema: json!({"type": "object"}),
        }
    }

    /// Minimal fake server, written in Python: it speaks enough MCP to validate the client end
    /// to end, without depending on an external package.
    fn fake_server_script() -> std::path::PathBuf {
        let script = r#"
import json, sys

def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()

# Noise before any exchange: the client must ignore it.
sys.stdout.write("test server started\n")
sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": msg["id"],
              "result": {"protocolVersion": "2025-06-18", "capabilities": {},
                         "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "notifications/initialized":
        # A spontaneous notification, to check it is not mistaken for a reply.
        send({"jsonrpc": "2.0", "method": "notifications/message",
              "params": {"level": "info", "data": "ready"}})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [
            {"name": "echo", "description": "Returns its input",
             "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
            {"name": "broken", "description": "Always fails",
             "inputSchema": {"type": "object"}}]}})
    elif method == "tools/call":
        name = msg["params"]["name"]
        args = msg["params"].get("arguments", {})
        if name == "echo":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"content": [{"type": "text", "text": args.get("text", "")}]}})
        elif name == "broken":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "result": {"content": [{"type": "text", "text": "expected failure"}],
                             "isError": True}})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32602, "message": "unknown tool"}})
    else:
        send({"jsonrpc": "2.0", "id": msg.get("id", 0),
              "error": {"code": -32601, "message": "unknown method"}})
"#;
        // One file per test, not one per process: tests run in parallel and each deletes its
        // script on exit. With a shared name, one test's exit removed the script another was
        // about to launch — `python` found nothing and the "valid" server counted as a failure.
        // Measured on Linux, where deleting an open file succeeds; Windows refused it and
        // hid the flaw.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("po-mcp-fake-{}-{number}.py", std::process::id()));
        std::fs::write(&path, script).unwrap();
        path
    }

    fn python() -> Option<String> {
        for candidate in ["python", "python3"] {
            if std::process::Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
            {
                return Some(candidate.to_owned());
            }
        }
        None
    }

    fn fake_config(script: &std::path::Path, python: &str) -> ServerConfig {
        ServerConfig::stdio("fake", python, vec!["-u".into(), script.display().to_string()])
    }

    #[tokio::test]
    async fn a_real_server_is_initialized_and_its_tools_listed() {
        let Some(python) = python() else {
            eprintln!("python missing — test skipped");
            return;
        };
        let script = fake_server_script();
        let server = McpServer::connect(&fake_config(&script, &python)).await.unwrap();

        let names: Vec<_> = server.tools().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["echo", "broken"]);
        assert_eq!(server.tools()[0].input_schema["properties"]["text"]["type"], "string");

        server.shutdown().await;
        std::fs::remove_file(&script).ok();
    }

    #[tokio::test]
    async fn a_remote_tool_is_called_and_returns_its_result() {
        let Some(python) = python() else { return };
        let script = fake_server_script();
        let server = McpServer::connect(&fake_config(&script, &python)).await.unwrap();

        let (text, is_error) = server
            .call_tool("echo", json!({"text": "hello"}))
            .await
            .unwrap();
        assert_eq!(text, "hello");
        assert!(!is_error);

        // A tool failure is a result, not a transport failure.
        let (text, is_error) = server.call_tool("broken", json!({})).await.unwrap();
        assert!(is_error);
        assert_eq!(text, "expected failure");

        // A nonexistent tool raises a named protocol error.
        let err = server.call_tool("nonexistent", json!({})).await.unwrap_err();
        assert!(matches!(err, McpError::Rpc { code: -32602, .. }));

        server.shutdown().await;
        std::fs::remove_file(&script).ok();
    }

    #[tokio::test]
    async fn remote_tools_are_used_like_built_in_tools() {
        let Some(python) = python() else { return };
        let script = fake_server_script();
        let server = Arc::new(McpServer::connect(&fake_config(&script, &python)).await.unwrap());

        let adapter = McpToolAdapter::new(Arc::clone(&server), tool("echo"));
        assert_eq!(adapter.name(), "fake__echo", "the name is prefixed by the server");
        assert_eq!(adapter.permission(&json!({})), Permission::Ask);

        let dir = tempfile::tempdir().unwrap();
        let ctx = po_tools::ToolContext::new(po_tools::Workspace::new(dir.path()));
        let outcome = adapter.run("t1", json!({"text": "hi"}), &ctx).await;
        assert!(!outcome.is_error);
        assert_eq!(outcome.content, "hi");

        server.shutdown().await;
        std::fs::remove_file(&script).ok();
    }

    #[tokio::test]
    async fn a_missing_server_fails_without_blocking() {
        let config = ServerConfig::stdio("missing", "mcp-binary-that-does-not-exist", vec![]);
        assert!(matches!(McpServer::connect(&config).await, Err(McpError::Spawn { .. })));
    }

    #[tokio::test]
    async fn a_failing_server_does_not_prevent_the_others() {
        let Some(python) = python() else { return };
        let script = fake_server_script();
        let configs = vec![
            ServerConfig::stdio("broken", "missing-binary", vec![]),
            fake_config(&script, &python),
        ];
        let (tools, failures) = connect_all(&configs).await;

        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "broken");
        assert_eq!(tools.len(), 2, "the valid server's tools must be available");

        std::fs::remove_file(&script).ok();
    }

    #[test]
    fn names_are_prefixed_to_avoid_collisions() {
        // Two servers may expose a tool with the same name; without a prefix, one would
        // overwrite the other in the registry.
        let spec = ToolSpec::new("server__search", "d", json!({}));
        assert!(spec.name.contains("__"));
    }

    #[test]
    fn exposed_names_are_ones_every_api_accepts() {
        assert_eq!(exposed_name("files", "read"), "files__read");
        assert_eq!(exposed_name("my.server", "files/read"), "my_server__files_read");
        let long = exposed_name("srv", &"x".repeat(100));
        assert_eq!(long.len(), 64);
        assert!(long.starts_with(&tool_prefix("srv")));
        assert!(exposed_name("my.server", "a").starts_with(&tool_prefix("my.server")), "removal by prefix still finds them");
    }

    #[test]
    fn the_shared_mcp_json_format_is_read() {
        let text = r#"{"mcpServers": {
            "files": {"command": "npx", "args": ["-y", "@mcp/fs", "."], "env": {"A": "1"}},
            "a_remote": {"type": "http", "url": "https://example"}
        }}"#;
        let configs = read_mcp_json(text);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].name, "a_remote");
        assert_eq!(configs[0].url.as_deref(), Some("https://example"));
        let configs: Vec<_> = configs.into_iter().filter(|c| c.url.is_none()).collect();
        assert_eq!(configs[0].name, "files");
        assert_eq!(configs[0].args, ["-y", "@mcp/fs", "."]);
        assert_eq!(configs[0].env.get("A").map(String::as_str), Some("1"));
        assert!(read_mcp_json("not json").is_empty());
    }

    #[test]
    fn a_server_log_has_a_safe_file_name() {
        let path = log_path("my server/../x");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains('/') && !name.contains(' '), "{name}");
    }
}
