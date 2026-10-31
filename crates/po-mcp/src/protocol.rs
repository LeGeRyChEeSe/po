//! Encoding of the MCP protocol.
//!
//! MCP is JSON-RPC 2.0 carried line by line over the server's standard input and output. This
//! module only translates: building requests, reading replies, telling a result from an error.
//! No process is started here, which makes the whole thing testable.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Protocol version announced at initialization.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("{}", po_i18n::t!("mcp.error.rpc", server = .server, code = .code, message = .message))]
    Rpc { server: String, code: i64, message: String },
    #[error("{}", po_i18n::t!("mcp.error.malformed", server = .server, detail = .detail))]
    Malformed { server: String, detail: String },
    #[error("{}", po_i18n::t!("mcp.error.timeout", server = .server))]
    Timeout { server: String },
    #[error("{}", po_i18n::t!("mcp.error.closed", server = .server))]
    Closed { server: String },
    #[error("{}", po_i18n::t!("mcp.error.spawn", server = .server, detail = .detail))]
    Spawn { server: String, detail: String },
}

/// Builds a JSON-RPC request.
pub fn request(id: u64, method: &str, params: Value) -> String {
    let mut message = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    });
    // A strict server rejects `params: null`; we only include the field when it has content.
    if !params.is_null() {
        message["params"] = params;
    }
    message.to_string()
}

/// Notification: same shape as a request, without an identifier, and with no reply expected.
pub fn notification(method: &str, params: Value) -> String {
    let mut message = json!({"jsonrpc": "2.0", "method": method});
    if !params.is_null() {
        message["params"] = params;
    }
    message.to_string()
}

/// Initialization parameters.
pub fn initialize_params(client_name: &str, version: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        // We only announce what we really can do: claiming to handle sampling or prompts would
        // make the server fail on first use.
        "capabilities": {},
        "clientInfo": {"name": client_name, "version": version},
    })
}

/// A parsed message.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Reply to a request, successful.
    Result { id: u64, value: Value },
    /// Reply to a request, in error.
    Error { id: u64, code: i64, message: String },
    /// Message sent by the server on its own initiative (logging, progress).
    Notification { method: String, params: Value },
}

impl Incoming {
    pub fn id(&self) -> Option<u64> {
        match self {
            Self::Result { id, .. } | Self::Error { id, .. } => Some(*id),
            Self::Notification { .. } => None,
        }
    }
}

/// Parses a line received from the server.
///
/// Returns `None` for an empty or non-conforming line: servers sometimes write diagnostic
/// messages on their standard output, and treating them as protocol errors would make the
/// integration unusable.
/// The answer owed to a request the server sends us (a line with both a method and an
/// identifier): `ping` gets its empty result, anything else "method not found". Left
/// unanswered, a server waiting on it hung until our own call timed out.
pub fn answer_to_server_request(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    let method = value.get("method")?.as_str()?;
    let id = value.get("id")?.clone();
    let reply = if method == "ping" {
        json!({"jsonrpc": "2.0", "id": id, "result": {}})
    } else {
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not supported by the client: {method}")}})
    };
    Some(reply.to_string())
}

pub fn parse_line(line: &str) -> Option<Incoming> {
    let line = line.trim();
    if line.is_empty() || !line.starts_with('{') {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;

    if let Some(method) = value.get("method").and_then(Value::as_str) {
        // A message carrying both an identifier *and* a method is a request from the server to
        // us; we announce no capability, so there is none to handle.
        if value.get("id").is_none() {
            return Some(Incoming::Notification {
                method: method.to_owned(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        return None;
    }

    let id = value.get("id")?.as_u64()?;
    if let Some(error) = value.get("error") {
        return Some(Incoming::Error {
            id,
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error without a message")
                .to_owned(),
        });
    }
    Some(Incoming::Result {
        id,
        value: value.get("result").cloned().unwrap_or(Value::Null),
    })
}

/// Description of a tool exposed by a server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default = "empty_schema")]
    pub input_schema: Value,
}

fn empty_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

/// Extracts the tool list from a reply to `tools/list`.
pub fn parse_tools(result: &Value) -> Vec<McpTool> {
    result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| serde_json::from_value(tool.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Flattens the result of a `tools/call` into text the model can use.
///
/// The protocol allows several blocks of various types; a model only expects text. Non-text
/// blocks are described rather than omitted, so that the model knows there was something
/// else.
pub fn flatten_content(result: &Value) -> (String, bool) {
    let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);

    let Some(items) = result.get("content").and_then(Value::as_array) else {
        // Some servers directly return a structured value.
        if let Some(structured) = result.get("structuredContent") {
            return (structured.to_string(), is_error);
        }
        return (result.to_string(), is_error);
    };

    let mut parts = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("image") => {
                let media = item
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("image");
                parts.push(format!("[image {media} returned by the tool]"));
            }
            Some("resource") => {
                let uri = item
                    .pointer("/resource/uri")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                parts.push(format!("[resource {uri}]"));
            }
            other => {
                parts.push(format!("[content of type {} not supported]", other.unwrap_or("?")));
            }
        }
    }
    (parts.join("\n"), is_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_well_formed() {
        let raw = request(1, "tools/list", Value::Null);
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 1);
        assert_eq!(value["method"], "tools/list");
        assert!(value.get("params").is_none(), "a null params must not be emitted");
    }

    #[test]
    fn a_request_from_the_server_gets_an_answer() {
        let pong: Value = serde_json::from_str(&answer_to_server_request(r#"{"jsonrpc":"2.0","id":"k1","method":"ping"}"#).unwrap()).unwrap();
        assert_eq!(pong["id"], "k1");
        assert_eq!(pong["result"], json!({}));
        let refused: Value =
            serde_json::from_str(&answer_to_server_request(r#"{"jsonrpc":"2.0","id":7,"method":"roots/list"}"#).unwrap()).unwrap();
        assert_eq!(refused["error"]["code"], -32601);
        assert!(answer_to_server_request(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#).is_none());
        assert!(answer_to_server_request(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_none());
    }

    #[test]
    fn parameters_are_forwarded_when_present() {
        let raw = request(2, "tools/call", json!({"name": "read", "arguments": {"a": 1}}));
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["params"]["name"], "read");
    }

    #[test]
    fn a_notification_has_no_identifier() {
        let raw = notification("notifications/initialized", Value::Null);
        let value: Value = serde_json::from_str(&raw).unwrap();
        assert!(value.get("id").is_none());
    }

    #[test]
    fn a_result_is_recognized() {
        let incoming = parse_line(r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#).unwrap();
        assert_eq!(incoming.id(), Some(7));
        assert!(matches!(incoming, Incoming::Result { .. }));
    }

    #[test]
    fn an_error_is_recognized_with_its_code() {
        let incoming =
            parse_line(r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"unknown method"}}"#)
                .unwrap();
        match incoming {
            Incoming::Error { id, code, message } => {
                assert_eq!(id, 3);
                assert_eq!(code, -32601);
                assert_eq!(message, "unknown method");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[test]
    fn a_server_notification_is_recognized() {
        let incoming =
            parse_line(r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}}"#)
                .unwrap();
        assert!(matches!(incoming, Incoming::Notification { .. }));
        assert_eq!(incoming.id(), None);
    }

    #[test]
    fn noise_on_standard_output_is_ignored() {
        // Many servers write startup messages: treating them as protocol errors would make the
        // integration unusable in practice.
        assert!(parse_line("").is_none());
        assert!(parse_line("Server listening on stdio").is_none());
        assert!(parse_line("{ this is not json").is_none());
    }

    #[test]
    fn tools_are_extracted_with_their_schema() {
        let result = json!({"tools": [
            {"name": "read_file", "description": "Reads", "inputSchema": {"type": "object"}},
            {"name": "no_description"}
        ]});
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[1].description, "", "a missing description must not fail");
        assert_eq!(tools[1].input_schema["type"], "object", "a default schema is provided");
    }

    #[test]
    fn a_missing_list_gives_zero_tools() {
        assert!(parse_tools(&json!({})).is_empty());
        assert!(parse_tools(&Value::Null).is_empty());
    }

    #[test]
    fn text_content_is_flattened() {
        let result = json!({"content": [
            {"type": "text", "text": "first part"},
            {"type": "text", "text": "second part"}
        ]});
        let (text, is_error) = flatten_content(&result);
        assert_eq!(text, "first part\nsecond part");
        assert!(!is_error);
    }

    #[test]
    fn a_tool_error_is_flagged() {
        let result = json!({"content": [{"type": "text", "text": "file missing"}], "isError": true});
        let (text, is_error) = flatten_content(&result);
        assert!(is_error);
        assert_eq!(text, "file missing");
    }

    #[test]
    fn non_text_content_is_described_not_lost() {
        let result = json!({"content": [
            {"type": "text", "text": "here"},
            {"type": "image", "mimeType": "image/png", "data": "..."},
            {"type": "resource", "resource": {"uri": "file:///a.txt"}}
        ]});
        let (text, _) = flatten_content(&result);
        assert!(text.contains("here"));
        assert!(text.contains("image/png"), "the model must know an image exists");
        assert!(text.contains("file:///a.txt"));
    }

    #[test]
    fn a_structured_result_is_kept() {
        let result = json!({"structuredContent": {"total": 42}});
        let (text, _) = flatten_content(&result);
        assert!(text.contains("42"));
    }

    #[test]
    fn initialization_only_announces_what_is_supported() {
        let params = initialize_params("po", "0.1.0");
        assert_eq!(params["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(params["clientInfo"]["name"], "po");
        assert!(params["capabilities"].as_object().unwrap().is_empty());
    }
}

/// Looks for the reply `id` in the body of an HTTP response: a JSON object, or an SSE stream
/// whose every `data:` event carries a JSON-RPC message (notifications included, ignored).
pub fn find_response(body: &str, id: u64) -> Option<Incoming> {
    let trimmed = body.trim();
    if trimmed.starts_with('{') {
        return parse_line(trimmed).filter(|m| m.id() == Some(id));
    }
    let mut data = String::new();
    let mut found = None;
    for line in body.lines().chain(std::iter::once("")) {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim_start());
        } else if line.trim().is_empty() && !data.is_empty() {
            if let Some(message) = parse_line(&data)
                && message.id() == Some(id)
            {
                found = Some(message);
            }
            data.clear();
        }
    }
    found
}

#[cfg(test)]
mod http_tests {
    use super::*;

    #[test]
    fn an_sse_reply_is_recognized_among_notifications() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"ok\":true}}\n\n";
        assert!(matches!(find_response(body, 3), Some(Incoming::Result { id: 3, .. })));
        assert!(find_response(body, 4).is_none());
    }

    #[test]
    fn a_direct_json_reply_is_recognized() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        assert!(matches!(find_response(body, 1), Some(Incoming::Result { id: 1, .. })));
    }
}
