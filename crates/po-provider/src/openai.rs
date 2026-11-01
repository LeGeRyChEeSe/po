//! Adapter for the `chat/completions` format.
//!
//! This is the most profitable adapter of the project: it covers local servers (llama.cpp,
//! Ollama, LM Studio, vLLM) and most cloud providers (OpenAI, Groq, OpenRouter, DeepSeek,
//! Mistral, Qwen...) in one go, all of which have converged on this format.
//!
//! The pitfalls handled here, all met in practice:
//! - tool arguments travel as a **string** containing JSON, not as an object;
//! - a local model sometimes returns `arguments: ""` for a tool without parameters;
//! - reasoning arrives in a non-standardized field (`reasoning_content`, `reasoning`);
//! - tool results take the `tool` role, not `user`, and losing the `tool_call_id` breaks the
//!   next turn without any error message.

use async_trait::async_trait;
use po_core::{
    Completion, CompletionResponse, ContentBlock, Message, Role, StopReason, ToolSpec, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use po_core::tool::parse_tool_arguments as parse_arguments;

use crate::sse::{Delta, SseDecoder, StreamAccumulator, Utf8Joiner};
use crate::{Provider, ProviderError, classify_http};

pub struct OpenAiProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
    /// Reasoning level requested from the server (`none`, `low`, `medium`, `high`). Absent by
    /// default: a provider that does not know this field may reject the request, so it is only
    /// sent if the user asked for it.
    reasoning_effort: Option<String>,
    /// llama.cpp only fills the variables of its Jinja template from `chat_template_kwargs`: a
    /// top-level `reasoning_effort` is silently ignored there. The level is then also sent in
    /// that field. Other servers understand it at the top level (checked on Strata: `none`
    /// removes all reasoning).
    effort_via_template_kwargs: bool,
    /// Local server: it accepts session settings (reasoning level). A remote provider that does
    /// not know these fields could reject the request.
    local: bool,
}

impl OpenAiProvider {
    /// `base_url` is the root of the API, without `/chat/completions`
    /// (for example `https://api.openai.com/v1` or `http://127.0.0.1:8080/v1`).
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key,
            client: reqwest::Client::new(),
            reasoning_effort: None,
            effort_via_template_kwargs: false,
            local: false,
        }
    }

    /// Are all the server's slots busy? `None` if the server does not publish `/slots`
    /// (llama.cpp does, and so does Strata). Very short timeout: the question must never delay
    /// the request itself.
    async fn server_busy(&self) -> Option<bool> {
        let root = self.base_url.trim_end_matches("/v1");
        let response = self
            .client
            .get(format!("{root}/slots"))
            .timeout(std::time::Duration::from_millis(250))
            .send()
            .await
            .ok()?;
        let slots: Value = response.json().await.ok()?;
        let slots = slots.as_array()?;
        if slots.is_empty() {
            return None;
        }
        Some(slots.iter().all(|slot| slot["is_processing"].as_bool().unwrap_or(false)))
    }

    pub fn for_local_server(mut self) -> Self {
        self.local = true;
        self
    }

    pub fn with_effort_in_template_kwargs(mut self, enabled: bool) -> Self {
        self.effort_via_template_kwargs = enabled;
        self
    }

    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    fn body_for(&self, request: &Completion) -> Value {
        let mut body = to_wire(request);
        // The session setting wins over the provider's, but is only sent to a local server.
        // With a thinking budget, the budget carries low/medium/high: the level itself would change
        // the prompt template, and the server would reread the whole conversation (measured on
        // Strata: 3,643 tokens out of 3,643). Only `none`, which switches thinking off, still goes.
        let session_effort = if self.local { request.reasoning_effort.as_ref() } else { None }
            .filter(|effort| *effort == "none" || request.thinking_tokens.is_none());
        if let Some(effort) = session_effort.or(self.reasoning_effort.as_ref()) {
            body["reasoning_effort"] = json!(effort);
            if self.effort_via_template_kwargs {
                body["chat_template_kwargs"] =
                    json!({"reasoning_effort": effort, "enable_thinking": effort != "none"});
            }
        }
        // The thinking cap, under the name llama.cpp reads (Strata reads it too). A remote API
        // never gets it: OpenAI rejects unknown fields.
        if let Some(budget) = request.thinking_tokens.filter(|_| self.local) {
            body["thinking_budget_tokens"] = json!(budget);
        }
        body
    }

    /// Provider pointing at a local server. No key is required by default, but llama.cpp
    /// accepts `--api-key`, hence the option being kept.
    pub fn local(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::new(name, base_url, None)
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let builder = self.client.post(format!("{}{path}", self.base_url));
        match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, request: &Completion) -> Result<CompletionResponse, ProviderError> {
        let body = self.body_for(request);
        let response = self
            .request("/chat/completions")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        if !(200..300).contains(&status) {
            return Err(classify_http(&self.name, status, &text, retry_after));
        }
        let known: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();
        from_wire_with_tools(&self.name, &text, &known)
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn stream(
        &self,
        request: &Completion,
        on_delta: &mut (dyn FnMut(Delta) + Send),
    ) -> Result<CompletionResponse, ProviderError> {
        use futures_util::StreamExt;

        // A single-slot local server serves one request at a time: if another client occupies
        // it, ours waits without receiving anything. Saying so avoids the impression of a hang.
        if self.local && self.server_busy().await == Some(true) {
            on_delta(Delta::Queued);
        }
        let mut body = self.body_for(request);
        body["stream"] = json!(true);
        // Without this option, most servers omit usage in streaming mode, and the session
        // would no longer know what it spends.
        body["stream_options"] = json!({"include_usage": true});

        let response = self
            .request("/chat/completions")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = None;
            let text = response.text().await.unwrap_or_default();
            return Err(classify_http(&self.name, status, &text, retry_after));
        }

        let mut decoder = SseDecoder::default();
        let mut accumulator = StreamAccumulator::default();
        let mut stream = response.bytes_stream();
        // The raw body is kept: some servers accept `stream: true` and then answer in one
        // block anyway, as plain JSON. Without this safety net, the response would be silently
        // empty — the agent would think the model said nothing.
        let mut raw = String::new();
        let mut saw_event = false;
        let mut utf8 = Utf8Joiner::default();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Transport(e.to_string()))?;
            // A packet can end in the middle of a multibyte character: its first bytes wait for
            // the next packet, else an "é" written by the model became "��" — in a file too.
            let text = utf8.push(&chunk);
            raw.push_str(&text);
            for payload in decoder.push(&text) {
                saw_event = true;
                for delta in accumulator.absorb_all(&payload) {
                    on_delta(delta);
                }
            }
        }

        let known: Vec<String> = request.tools.iter().map(|t| t.name.clone()).collect();

        // No SSE event: the server did not stream. We reread the body as a complete response
        // rather than returning an empty message.
        if !saw_event {
            let response = from_wire_with_tools(&self.name, raw.trim(), &known)?;
            let text = response.message.visible_text();
            if !text.is_empty() {
                on_delta(Delta::Text(text));
            }
            on_delta(Delta::Done);
            return Ok(response);
        }

        let (message, stop_reason, usage, model_id) = accumulator.finish(&known);
        Ok(CompletionResponse {
            message,
            stop_reason,
            usage,
            model_id: if model_id.is_empty() { request.model.id.clone() } else { model_id },
        })
    }

    async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let builder = self.client.get(format!("{}/models", self.base_url));
        let builder = match &self.api_key {
            Some(key) => builder.bearer_auth(key),
            None => builder,
        };
        let response = builder
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let value: Value = serde_json::from_str(&text).map_err(|e| ProviderError::Malformed {
            provider: self.name.clone(),
            detail: e.to_string(),
        })?;
        Ok(value["data"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Conversion: canonical -> wire format
// ---------------------------------------------------------------------------

/// Builds the request body. A pure function: this is what we test, not the network.
pub fn to_wire(request: &Completion) -> Value {
    let mut messages = Vec::new();

    if !request.system.is_empty() {
        messages.push(json!({"role": "system", "content": request.system}));
    }
    for message in &request.messages {
        push_wire_messages(&mut messages, message);
    }

    let mut body = json!({
        "model": request.model.id,
        "messages": messages,
        "max_tokens": request.max_output_tokens,
    });

    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }
    // llama.cpp's and Strata's own: an OpenAI API would refuse them.
    if request.model.origin.is_local() {
        if let Some(top_k) = request.top_k {
            body["top_k"] = json!(top_k);
        }
        if let Some(min_p) = request.min_p {
            body["min_p"] = json!(min_p);
        }
        if let Some(penalty) = request.repeat_penalty {
            body["repeat_penalty"] = json!(penalty);
        }
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(tool_to_wire).collect::<Vec<_>>());
    }
    body
}

fn tool_to_wire(tool: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    })
}

fn push_wire_messages(out: &mut Vec<Value>, message: &Message) {
    // Tool results are not user messages in this format: each one becomes a `tool` role
    // message. Grouping them would lose the correspondence with the calls, and the model
    // would answer off target.
    let tool_results: Vec<_> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                Some(json!({
                    "role": "tool",
                    "tool_call_id": tool_use_id,
                    "content": if *is_error { format!("Error: {content}") } else { content.clone() },
                }))
            }
            _ => None,
        })
        .collect();

    if !tool_results.is_empty() {
        out.extend(tool_results);
        // A message may contain only results; in that case there is nothing else.
        if message.content.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })) {
            return;
        }
    }

    let text: String = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let tool_calls: Vec<Value> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some(json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    // Arguments travel serialized as a string, never as an object.
                    "arguments": input.to_string(),
                }
            })),
            _ => None,
        })
        .collect();

    if text.is_empty() && tool_calls.is_empty() {
        return;
    }

    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    // With images, the content becomes a list of parts: the text, then each image as a data
    // URL, the form understood by llama.cpp, vLLM and Strata.
    let images: Vec<Value> = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image { media_type, data } => Some(media_part(media_type, data)),
            _ => None,
        })
        .collect();
    if !images.is_empty() && matches!(message.role, Role::User) {
        let mut parts = vec![json!({"type": "text", "text": text})];
        parts.extend(images);
        out.push(json!({"role": role, "content": parts}));
        return;
    }
    let mut wire = json!({"role": role});
    // An assistant message that only calls tools must carry `content: null`: some servers
    // reject an empty string.
    wire["content"] = if text.is_empty() { Value::Null } else { json!(text) };
    if !tool_calls.is_empty() {
        wire["tool_calls"] = json!(tool_calls);
    }
    out.push(wire);
}

/// Message part for a media file: `image_url` (llama.cpp, vLLM, Strata), `video_url`
/// (vLLM, Qwen-VL), `input_audio` (OpenAI, llama.cpp).
fn media_part(media_type: &str, data: &str) -> Value {
    if media_type.starts_with("video/") {
        json!({"type": "video_url", "video_url": {"url": format!("data:{media_type};base64,{data}")}})
    } else if let Some(format) = media_type.strip_prefix("audio/") {
        let format = match format {
            "mpeg" => "mp3",
            other => other,
        };
        json!({"type": "input_audio", "input_audio": {"data": data, "format": format}})
    } else {
        json!({"type": "image_url", "image_url": {"url": format!("data:{media_type};base64,{data}")}})
    }
}

// ---------------------------------------------------------------------------
// Conversion: wire format -> canonical
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: String,
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
struct WireChoice {
    message: WireMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireMessage {
    #[serde(default)]
    content: Option<String>,
    /// Reasoning field, whose name varies between implementations.
    #[serde(default, alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCall>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireToolCall {
    #[serde(default)]
    id: String,
    function: WireFunction,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireFunction {
    name: String,
    #[serde(default)]
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    prompt_tokens_details: Option<WireCacheDetails>,
}

#[derive(Debug, Deserialize)]
struct WireCacheDetails {
    #[serde(default)]
    cached_tokens: u64,
}

/// Parses a complete response, without recovering textual calls.
pub fn from_wire(provider: &str, body: &str) -> Result<CompletionResponse, ProviderError> {
    from_wire_with_tools(provider, body, &[])
}

/// Parses a response, recovering if needed the tool calls written as text.
///
/// `known_tools` is the list of tools actually declared in the request: it serves as a
/// safeguard, since a model can only trigger what it was offered.
pub fn from_wire_with_tools(
    provider: &str,
    body: &str,
    known_tools: &[String],
) -> Result<CompletionResponse, ProviderError> {
    let parsed: WireResponse = serde_json::from_str(body).map_err(|e| ProviderError::Malformed {
        provider: provider.to_owned(),
        detail: e.to_string(),
    })?;

    let choice = parsed.choices.into_iter().next().ok_or_else(|| ProviderError::Malformed {
        provider: provider.to_owned(),
        detail: po_i18n::t!("provider.error.no_choice").into(),
    })?;

    let mut content = Vec::new();
    if let Some(reasoning) = choice.message.reasoning_content.filter(|r| !r.is_empty()) {
        content.push(ContentBlock::Thinking { text: reasoning, signature: None });
    }

    // A server that structures the calls is authoritative: we only reread the text when it
    // produced none, so as never to run a call twice.
    let mut recovered = Vec::new();
    let text = choice.message.content.unwrap_or_default();
    let text = if choice.message.tool_calls.is_empty() && !known_tools.is_empty() {
        let (cleaned, calls) = crate::recovery::recover(&text, known_tools);
        recovered = calls;
        cleaned
    } else {
        text
    };
    if !text.is_empty() {
        content.push(ContentBlock::text(text));
    }
    content.extend(crate::recovery::to_blocks(recovered, 0));

    for (index, call) in choice.message.tool_calls.into_iter().enumerate() {
        content.push(ContentBlock::ToolUse {
            // A local server sometimes omits the identifier; making up a stable one is better than
            // losing the call, since it is what will link the result on the next turn.
            id: if call.id.is_empty() { format!("call_{index}") } else { call.id },
            name: call.function.name,
            input: parse_arguments(&call.function.arguments),
        });
    }

    let has_tool_use = content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let stop_reason = parse_finish_reason(choice.finish_reason.as_deref(), has_tool_use);

    let usage = parsed.usage.map(|u| {
        let cached = u.prompt_tokens_details.map(|d| d.cached_tokens).unwrap_or(0);
        Usage {
            // Cached tokens are counted *within* `prompt_tokens`: adding them up would inflate the
            // total and skew the session's accounting.
            input_tokens: u.prompt_tokens.saturating_sub(cached),
            output_tokens: u.completion_tokens,
            cache_read_tokens: cached,
            cache_write_tokens: 0,
        }
    });

    Ok(CompletionResponse {
        message: Message { role: Role::Assistant, content },
        stop_reason,
        usage: usage.unwrap_or_default(),
        model_id: parsed.model,
    })
}

fn parse_finish_reason(raw: Option<&str>, has_tool_use: bool) -> StopReason {
    match raw {
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some("length") | Some("max_tokens") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::Refusal,
        Some("stop") | Some("end_turn") => {
            // Many local servers announce `stop` while emitting tool calls. Trusting the field
            // alone would stop the loop at the first tool.
            if has_tool_use { StopReason::ToolUse } else { StopReason::EndTurn }
        }
        _ if has_tool_use => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_core::{Capabilities, ModelSpec, Origin, Pricing, Tier};

    fn model() -> ModelSpec {
        ModelSpec {
            id: "qwen3-27b".into(),
            display_name: "Qwen3".into(),
            origin: Origin::Local {
                backend: "llama.cpp".into(),
                endpoint: "http://127.0.0.1:8080".into(),
            },
            tier: Tier::Strong,
            capabilities: Capabilities::AGENTIC,
            context_window: 32_768,
            max_output_tokens: 2048,
            pricing: Pricing::FREE,
        }
    }

    fn request_for(messages: Vec<Message>) -> Completion {
        Completion::new(model(), "you are an agent", messages)
    }

    #[test]
    fn the_system_prompt_becomes_the_first_message() {
        let wire = to_wire(&request_for(vec![Message::user("salut")]));
        assert_eq!(wire["messages"][0]["role"], "system");
        assert_eq!(wire["messages"][0]["content"], "you are an agent");
        assert_eq!(wire["messages"][1]["role"], "user");
        assert_eq!(wire["model"], "qwen3-27b");
        assert_eq!(wire["max_tokens"], 2048);
    }

    #[test]
    fn an_empty_system_prompt_adds_no_message() {
        let request = Completion::new(model(), "", vec![Message::user("salut")]);
        let wire = to_wire(&request);
        assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn tool_arguments_are_sent_serialized_as_a_string() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "read".into(),
                input: json!({"path": "src/main.rs"}),
            }],
        };
        let wire = to_wire(&request_for(vec![message]));
        let call = &wire["messages"][1]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "read");
        let arguments = call["function"]["arguments"].as_str().expect("a string, not an object");
        assert_eq!(serde_json::from_str::<Value>(arguments).unwrap()["path"], "src/main.rs");
        assert!(wire["messages"][1]["content"].is_null(), "content null, not an empty string");
    }

    #[test]
    fn tool_results_become_tool_role_messages() {
        let message = Message::tool_results(vec![
            ContentBlock::tool_result("call_1", "file content"),
            ContentBlock::tool_error("call_2", "file missing"),
        ]);
        let wire = to_wire(&request_for(vec![message]));
        let messages = wire["messages"].as_array().unwrap();
        // system + two results, and no stray `user` message.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_1");
        assert_eq!(messages[2]["tool_call_id"], "call_2");
        assert!(messages[2]["content"].as_str().unwrap().contains("Error"));
    }

    #[test]
    fn tools_are_declared_in_function_format() {
        let tools = vec![ToolSpec::new("read", "Reads", json!({"type": "object"}))];
        let request = request_for(vec![Message::user("x")]).with_tools(tools);
        let wire = to_wire(&request);
        assert_eq!(wire["tools"][0]["type"], "function");
        assert_eq!(wire["tools"][0]["function"]["name"], "read");
        assert_eq!(wire["tools"][0]["function"]["parameters"]["type"], "object");
    }

    #[test]
    fn without_tools_the_field_is_absent() {
        let wire = to_wire(&request_for(vec![Message::user("x")]));
        assert!(wire.get("tools").is_none(), "an empty array would make some servers fail");
    }

    #[test]
    fn parses_a_text_response() {
        let body = r#"{
            "model": "qwen3-27b",
            "choices": [{"message": {"content": "hello"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20}
        }"#;
        let response = from_wire("local", body).unwrap();
        assert_eq!(response.message.visible_text(), "hello");
        assert_eq!(response.stop_reason, StopReason::EndTurn);
        assert_eq!(response.usage.input_tokens, 100);
        assert_eq!(response.usage.output_tokens, 20);
        assert!(!response.wants_tools());
    }

    #[test]
    fn parses_a_tool_call() {
        let body = r#"{
            "choices": [{"message": {"content": null, "tool_calls": [
                {"id": "call_abc", "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}}
            ]}, "finish_reason": "tool_calls"}]
        }"#;
        let response = from_wire("local", body).unwrap();
        assert!(response.wants_tools());
        let (id, name, input) = response.message.tool_uses().next().unwrap();
        assert_eq!(id, "call_abc");
        assert_eq!(name, "read");
        assert_eq!(input["path"], "a.rs");
    }

    #[test]
    fn a_local_server_announcing_stop_with_tools_extends_the_loop() {
        // Frequent llama.cpp behaviour: `finish_reason` is `stop` despite the call.
        let body = r#"{
            "choices": [{"message": {"tool_calls": [
                {"id": "c1", "function": {"name": "ls", "arguments": ""}}
            ]}, "finish_reason": "stop"}]
        }"#;
        let response = from_wire("local", body).unwrap();
        assert_eq!(response.stop_reason, StopReason::ToolUse);
        assert!(response.wants_tools(), "otherwise the agent stops at the first tool");
    }

    #[test]
    fn empty_arguments_give_an_empty_object_and_invalid_ones_keep_their_text() {
        assert_eq!(parse_arguments(""), json!({}));
        assert_eq!(parse_arguments("   "), json!({}));
        assert_eq!(parse_arguments("{not json"), json!({po_core::tool::INVALID_ARGUMENTS: "{not json"}));
        assert_eq!(parse_arguments(r#"{"a":1}"#), json!({"a": 1}));
    }

    #[test]
    fn a_call_without_an_identifier_gets_one() {
        let body = r#"{"choices":[{"message":{"tool_calls":[
            {"function":{"name":"ls","arguments":"{}"}}
        ]}}]}"#;
        let response = from_wire("local", body).unwrap();
        let (id, _, _) = response.message.tool_uses().next().unwrap();
        assert_eq!(id, "call_0", "without an identifier, the result could not be attached");
    }

    #[test]
    fn reasoning_is_captured_whatever_the_field() {
        for field in ["reasoning_content", "reasoning"] {
            let body = format!(
                r#"{{"choices":[{{"message":{{"{field}":"I am thinking","content":"answer"}}}}]}}"#
            );
            let response = from_wire("local", &body).unwrap();
            assert!(matches!(response.message.content[0], ContentBlock::Thinking { .. }));
            assert_eq!(response.message.visible_text(), "answer");
        }
    }

    #[test]
    fn cached_tokens_are_not_counted_twice() {
        let body = r#"{"choices":[{"message":{"content":"x"}}],
            "usage":{"prompt_tokens":1000,"completion_tokens":50,
                     "prompt_tokens_details":{"cached_tokens":800}}}"#;
        let response = from_wire("local", body).unwrap();
        assert_eq!(response.usage.cache_read_tokens, 800);
        assert_eq!(response.usage.input_tokens, 200, "1000 already includes the 800 cached");
        assert_eq!(response.usage.total(), 1050);
    }

    #[test]
    fn truncation_is_signaled() {
        let body = r#"{"choices":[{"message":{"content":"cut"},"finish_reason":"length"}]}"#;
        assert_eq!(from_wire("local", body).unwrap().stop_reason, StopReason::MaxTokens);
    }

    #[test]
    fn a_non_streamed_response_stays_usable() {
        // Some servers accept `stream: true` and answer in one block anyway. The body is then
        // plain JSON, without a single `data:` line: without a safety net, the accumulator
        // would return an empty message and the session would seem mute.
        let mut decoder = SseDecoder::default();
        let body = r#"{"choices":[{"message":{"content":"whole answer"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":120,"completion_tokens":30}}"#;
        assert!(
            decoder.push(body).is_empty(),
            "a JSON body contains no SSE event"
        );

        // This is exactly the case that `from_wire` must catch.
        let response = from_wire("test", body).unwrap();
        assert_eq!(response.message.visible_text(), "whole answer");
        assert_eq!(response.usage.output_tokens, 30);
    }

    #[test]
    fn a_response_without_choices_is_an_error() {
        assert!(from_wire("local", r#"{"choices":[]}"#).is_err());
        assert!(from_wire("local", "not json").is_err());
    }

    #[test]
    fn a_full_round_trip_preserves_tool_calls() {
        // The assistant turn that calls a tool must be sendable as is on the next turn: this
        // is the cycle that breaks when an identifier is lost.
        let body = r#"{"choices":[{"message":{"content":null,"tool_calls":[
            {"id":"call_xyz","function":{"name":"grep","arguments":"{\"q\":\"fn main\"}"}}
        ]},"finish_reason":"tool_calls"}]}"#;
        let response = from_wire("local", body).unwrap();

        let follow_up = vec![
            Message::user("search"),
            response.message.clone(),
            Message::tool_results(vec![ContentBlock::tool_result("call_xyz", "3 results")]),
        ];
        let wire = to_wire(&request_for(follow_up));
        let messages = wire["messages"].as_array().unwrap();

        assert_eq!(messages[2]["tool_calls"][0]["id"], "call_xyz");
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_xyz");
    }

    #[test]
    fn the_reasoning_level_is_only_sent_when_requested() {
        let request = Completion::new(model(), "sys", vec![Message::user("hello")]);

        let without = OpenAiProvider::local("srv", "http://127.0.0.1:8080/v1");
        assert!(without.body_for(&request).get("reasoning_effort").is_none());

        let with = OpenAiProvider::local("srv", "http://127.0.0.1:8080/v1")
            .with_reasoning_effort(Some("low".into()));
        assert_eq!(with.body_for(&request)["reasoning_effort"], "low");
    }

    #[test]
    fn llama_cpp_gets_the_level_in_the_template_variables_too() {
        let request = Completion::new(model(), "sys", vec![Message::user("hello")]);
        let provider = OpenAiProvider::local("llama.cpp", "http://127.0.0.1:8080/v1")
            .with_reasoning_effort(Some("none".into()))
            .with_effort_in_template_kwargs(true);
        let body = provider.body_for(&request);
        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);

        let provider = OpenAiProvider::local("llama.cpp", "http://127.0.0.1:8080/v1")
            .with_reasoning_effort(Some("high".into()))
            .with_effort_in_template_kwargs(true);
        assert_eq!(provider.body_for(&request)["chat_template_kwargs"]["enable_thinking"], true);
    }

    #[test]
    fn the_session_setting_only_goes_to_a_local_server() {
        let mut request = Completion::new(model(), "sys", vec![Message::user("hello")]);
        request.reasoning_effort = Some("none".into());
        let remote = OpenAiProvider::new("cloud", "https://api.example.com/v1", None);
        assert!(remote.body_for(&request).get("reasoning_effort").is_none());
        let local = OpenAiProvider::local("strata", "http://127.0.0.1:8080/v1").for_local_server();
        assert_eq!(local.body_for(&request)["reasoning_effort"], "none");
    }

    #[test]
    fn with_a_budget_the_level_leaves_the_template_alone() {
        let local = OpenAiProvider::local("strata", "http://127.0.0.1:8080/v1").for_local_server();
        let mut request = Completion::new(model(), "sys", vec![Message::user("hello")]);
        request.reasoning_effort = Some("low".into());
        request.thinking_tokens = Some(2048);
        let body = local.body_for(&request);
        assert!(body.get("reasoning_effort").is_none() && body.get("chat_template_kwargs").is_none());
        assert_eq!(body["thinking_budget_tokens"], 2048);
        request.reasoning_effort = Some("none".into());
        request.thinking_tokens = None;
        assert_eq!(local.body_for(&request)["reasoning_effort"], "none", "switching thinking off still goes");
    }

    #[test]
    fn the_thinking_budget_only_goes_to_a_local_server() {
        let mut request = Completion::new(model(), "sys", vec![Message::user("hello")]);
        request.thinking_tokens = Some(8192);
        let remote = OpenAiProvider::new("cloud", "https://api.example.com/v1", None);
        assert!(remote.body_for(&request).get("thinking_budget_tokens").is_none());
        let local = OpenAiProvider::local("strata", "http://127.0.0.1:8080/v1").for_local_server();
        assert_eq!(local.body_for(&request)["thinking_budget_tokens"], 8192);
    }

    #[test]
    fn an_image_is_sent_as_a_data_url() {
        let message = Message {
            role: Role::User,
            content: vec![
                ContentBlock::text("what do you see?"),
                ContentBlock::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
        };
        let request = Completion::new(model(), "", vec![message]);
        let body = to_wire(&request);
        let parts = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "what do you see?");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,QUJD");
    }

    #[test]
    fn video_and_sound_have_their_own_part() {
        assert_eq!(media_part("video/mp4", "QQ==")["video_url"]["url"], "data:video/mp4;base64,QQ==");
        let sound = media_part("audio/mpeg", "QQ==");
        assert_eq!(sound["input_audio"]["format"], "mp3");
        assert_eq!(sound["input_audio"]["data"], "QQ==");
    }

    #[test]
    fn an_image_read_by_a_tool_follows_the_results() {
        // The results message also carries the image read by `read`: it goes right after, in a
        // multi-part user message.
        let message = Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult { tool_use_id: "r1".into(), content: "image attached".into(), is_error: false },
                ContentBlock::text("[Attached files]"),
                ContentBlock::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
        };
        let body = to_wire(&Completion::new(model(), "", vec![message]));
        assert_eq!(body["messages"][0]["role"], "tool");
        assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
    }
}
