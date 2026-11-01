//! Adapter for Anthropic's Messages API.
//!
//! This format is closer to our canonical representation — native content blocks, explicit
//! `tool_use` and `tool_result` — but it has its own requirements: the system prompt is a
//! separate field, reasoning blocks must be fed back **with their signature** on the next turn
//! or be rejected, and the prompt cache distinguishes writes from reads, which changes the
//! bill.

use async_trait::async_trait;
use po_core::{
    Completion, CompletionResponse, ContentBlock, Message, Role, StopReason, ToolSpec, Usage,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Provider, ProviderError, classify_http};

const API_VERSION: &str = "2023-06-01";

pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url("anthropic", "https://api.anthropic.com/v1", api_key)
    }

    /// Allows targeting a proxy or a local server speaking the Messages format.
    pub fn with_base_url(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(&self, request: &Completion) -> Result<CompletionResponse, ProviderError> {
        let response = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&to_wire(request))
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
        from_wire(&self.name, &text)
    }
}

/// Builds the request body. A pure function, tested without a network.
pub fn to_wire(request: &Completion) -> Value {
    let messages: Vec<Value> = request
        .messages
        .iter()
        .filter_map(message_to_wire)
        .collect();

    let mut body = json!({
        "model": request.model.id,
        "messages": messages,
        "max_tokens": request.max_output_tokens,
    });

    // The system prompt is a dedicated field, never a message: slipping it in as a `system`
    // role message causes an immediate rejection by the API.
    if !request.system.is_empty() {
        body["system"] = json!(request.system);
    }
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(top_p) = request.top_p {
        body["top_p"] = json!(top_p);
    }
    if let Some(top_k) = request.top_k {
        body["top_k"] = json!(top_k);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(tool_to_wire).collect::<Vec<_>>());
    }
    if let Some(budget) = request.thinking_tokens {
        body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
    }
    body
}

fn tool_to_wire(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

fn message_to_wire(message: &Message) -> Option<Value> {
    let blocks: Vec<Value> = message.content.iter().filter_map(block_to_wire).collect();
    if blocks.is_empty() {
        // An empty message is rejected by the API; dropping it is better than a 400 error.
        return None;
    }
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    Some(json!({"role": role, "content": blocks}))
}

fn block_to_wire(block: &ContentBlock) -> Option<Value> {
    Some(match block {
        ContentBlock::Text { text } if text.is_empty() => return None,
        ContentBlock::Text { text } => json!({"type": "text", "text": text}),
        ContentBlock::Thinking { text, signature } => {
            // Without a signature, the block is rejected when fed back: better to omit it than to
            // fail the whole turn over reasoning that has already been consumed.
            let signature = signature.as_ref()?;
            json!({"type": "thinking", "thinking": text, "signature": signature})
        }
        ContentBlock::ToolUse { id, name, input } => {
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentBlock::ToolResult { tool_use_id, content, is_error } => json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": content,
            "is_error": is_error,
        }),
        ContentBlock::Image { media_type, data } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        }),
    })
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    #[serde(default)]
    model: String,
    content: Vec<WireBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireBlock {
    Text { text: String },
    Thinking {
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    ToolUse { id: String, name: String, input: Value },
    /// Block unknown to this version, from a later API version: ignored rather than fatal.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// Parses a complete response. A pure function, tested without a network.
pub fn from_wire(provider: &str, body: &str) -> Result<CompletionResponse, ProviderError> {
    let parsed: WireResponse = serde_json::from_str(body).map_err(|e| ProviderError::Malformed {
        provider: provider.to_owned(),
        detail: e.to_string(),
    })?;

    let content: Vec<ContentBlock> = parsed
        .content
        .into_iter()
        .filter_map(|block| match block {
            WireBlock::Text { text } => Some(ContentBlock::Text { text }),
            WireBlock::Thinking { thinking, signature } => {
                Some(ContentBlock::Thinking { text: thinking, signature })
            }
            WireBlock::ToolUse { id, name, input } => {
                Some(ContentBlock::ToolUse { id, name, input })
            }
            WireBlock::Unknown => None,
        })
        .collect();

    let has_tool_use = content.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    let stop_reason = match parsed.stop_reason.as_deref() {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("refusal") => StopReason::Refusal,
        _ if has_tool_use => StopReason::ToolUse,
        _ => StopReason::EndTurn,
    };

    let usage = parsed
        .usage
        .map(|u| Usage {
            // Unlike the OpenAI format, `input_tokens` already excludes the cache: there is
            // therefore nothing to subtract here.
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_write_tokens: u.cache_creation_input_tokens,
        })
        .unwrap_or_default();

    Ok(CompletionResponse {
        message: Message { role: Role::Assistant, content },
        stop_reason,
        usage,
        model_id: parsed.model,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_core::{Capabilities, ModelSpec, Origin, Pricing, Tier};

    fn model() -> ModelSpec {
        ModelSpec {
            id: "claude-opus-5".into(),
            display_name: "Opus 5".into(),
            origin: Origin::Cloud { provider: "anthropic".into() },
            tier: Tier::Frontier,
            capabilities: Capabilities::AGENTIC,
            context_window: 200_000,
            max_output_tokens: 8192,
            pricing: Pricing { input_per_mtok: 15.0, output_per_mtok: 75.0, ..Default::default() },
        }
    }

    fn request_for(messages: Vec<Message>) -> Completion {
        Completion::new(model(), "you are an agent", messages)
    }

    #[test]
    fn the_system_prompt_is_a_dedicated_field() {
        let wire = to_wire(&request_for(vec![Message::user("hi")]));
        assert_eq!(wire["system"], "you are an agent");
        assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
        assert_eq!(wire["messages"][0]["role"], "user");
    }

    #[test]
    fn tool_calls_keep_their_input_as_an_object() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "read".into(),
                input: json!({"path": "a.rs"}),
            }],
        };
        let wire = to_wire(&request_for(vec![message]));
        let block = &wire["messages"][0]["content"][0];
        assert_eq!(block["type"], "tool_use");
        assert!(block["input"].is_object(), "native object, unlike the OpenAI format");
    }

    #[test]
    fn results_stay_in_a_user_message() {
        let message = Message::tool_results(vec![ContentBlock::tool_result("toolu_1", "ok")]);
        let wire = to_wire(&request_for(vec![message]));
        assert_eq!(wire["messages"][0]["role"], "user");
        assert_eq!(wire["messages"][0]["content"][0]["type"], "tool_result");
        assert_eq!(wire["messages"][0]["content"][0]["tool_use_id"], "toolu_1");
    }

    #[test]
    fn reasoning_without_a_signature_is_omitted() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking { text: "hmm".into(), signature: None },
                ContentBlock::text("answer"),
            ],
        };
        let wire = to_wire(&request_for(vec![message]));
        let blocks = wire["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1, "the unsigned block would be rejected by the API");
        assert_eq!(blocks[0]["type"], "text");
    }

    #[test]
    fn signed_reasoning_is_fed_back_as_is() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                text: "hmm".into(),
                signature: Some("sig".into()),
            }],
        };
        let wire = to_wire(&request_for(vec![message]));
        let block = &wire["messages"][0]["content"][0];
        assert_eq!(block["type"], "thinking");
        assert_eq!(block["signature"], "sig");
    }

    #[test]
    fn a_message_that_became_empty_is_dropped() {
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking { text: "hmm".into(), signature: None }],
        };
        let wire = to_wire(&request_for(vec![Message::user("a"), message]));
        assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_reasoning_budget_is_forwarded() {
        let mut request = request_for(vec![Message::user("x")]);
        request.thinking_tokens = Some(4000);
        let wire = to_wire(&request);
        assert_eq!(wire["thinking"]["budget_tokens"], 4000);
        assert_eq!(wire["thinking"]["type"], "enabled");
    }

    #[test]
    fn parses_a_response_with_a_tool_and_reasoning() {
        let body = r#"{
            "model": "claude-opus-5",
            "content": [
                {"type":"thinking","thinking":"I am thinking","signature":"sig-1"},
                {"type":"text","text":"I read the file"},
                {"type":"tool_use","id":"toolu_9","name":"read","input":{"path":"a.rs"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 500, "output_tokens": 40,
                      "cache_read_input_tokens": 2000, "cache_creation_input_tokens": 100}
        }"#;
        let response = from_wire("anthropic", body).unwrap();

        assert!(response.wants_tools());
        assert_eq!(response.message.visible_text(), "I read the file");
        match &response.message.content[0] {
            ContentBlock::Thinking { signature, .. } => {
                assert_eq!(signature.as_deref(), Some("sig-1"), "the signature must survive")
            }
            other => panic!("unexpected block: {other:?}"),
        }
        assert_eq!(response.usage.input_tokens, 500);
        assert_eq!(response.usage.cache_read_tokens, 2000);
        assert_eq!(response.usage.cache_write_tokens, 100);
    }

    #[test]
    fn an_unknown_block_is_not_fatal() {
        let body = r#"{"content":[
            {"type":"text","text":"ok"},
            {"type":"a_future_block","data":{}}
        ],"stop_reason":"end_turn"}"#;
        let response = from_wire("anthropic", body).unwrap();
        assert_eq!(response.message.content.len(), 1);
        assert_eq!(response.stop_reason, StopReason::EndTurn);
    }

    #[test]
    fn the_full_cycle_preserves_identifiers() {
        let body = r#"{"content":[{"type":"tool_use","id":"toolu_42","name":"grep","input":{}}],
                       "stop_reason":"tool_use"}"#;
        let response = from_wire("anthropic", body).unwrap();
        let follow_up = vec![
            Message::user("search"),
            response.message.clone(),
            Message::tool_results(vec![ContentBlock::tool_result("toolu_42", "found")]),
        ];
        let wire = to_wire(&request_for(follow_up));
        assert_eq!(wire["messages"][1]["content"][0]["id"], "toolu_42");
        assert_eq!(wire["messages"][2]["content"][0]["tool_use_id"], "toolu_42");
    }
}
