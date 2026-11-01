//! `po-core` — the types shared by the rest of Po.
//!
//! This crate performs no I/O: it defines the conversation history, models, tools and
//! accounting. It is the common vocabulary of a cloud orchestrator and a local subagent.

pub mod message;
pub mod model;
pub mod tool;
pub mod usage;

pub use message::{ContentBlock, Message, Role, StopReason};
pub use model::{Capabilities, ModelSpec, Origin, Tier};
pub use tool::{ToolOutcome, ToolSpec};
pub use usage::{Ledger, Pricing, Usage};

/// A request sent to a provider, independent of its wire format.
#[derive(Debug, Clone)]
pub struct Completion {
    pub model: ModelSpec,
    /// System instructions. Carried separately because providers place them differently:
    /// a dedicated field for some, the first message for others.
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
    /// Reasoning budget, for models that expose one.
    pub thinking_tokens: Option<u32>,
    /// Reasoning level (`none`, `low`, `medium`, `high`), for servers that understand it.
    /// Adjustable during a session; `None` lets the server decide.
    pub reasoning_effort: Option<String>,
    /// Sampling the model's profile asks for; `None` lets the server decide.
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f32>,
    pub repeat_penalty: Option<f32>,
}

impl Completion {
    pub fn new(model: ModelSpec, system: impl Into<String>, messages: Vec<Message>) -> Self {
        let max_output_tokens = model.max_output_tokens;
        Self {
            model,
            system: system.into(),
            messages,
            tools: Vec::new(),
            max_output_tokens,
            temperature: None,
            thinking_tokens: None,
            reasoning_effort: None,
            top_p: None,
            top_k: None,
            min_p: None,
            repeat_penalty: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }
}

/// A provider response, normalized.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResponse {
    pub message: Message,
    pub stop_reason: StopReason,
    pub usage: Usage,
    /// Identifier of the model that actually answered — a router or a local server may
    /// serve something other than what was requested; the user must know.
    pub model_id: String,
}

impl CompletionResponse {
    /// Should the agent loop continue?
    pub fn wants_tools(&self) -> bool {
        self.stop_reason.continues_loop() && self.message.has_tool_use()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model() -> ModelSpec {
        ModelSpec {
            id: "m".into(),
            display_name: "M".into(),
            origin: Origin::Cloud { provider: "test".into() },
            tier: Tier::Strong,
            capabilities: Capabilities::AGENTIC,
            context_window: 8192,
            max_output_tokens: 2048,
            pricing: Pricing::FREE,
        }
    }

    #[test]
    fn request_inherits_the_model_output_cap() {
        let completion = Completion::new(model(), "sys", vec![Message::user("hello")]);
        assert_eq!(completion.max_output_tokens, 2048);
        assert!(completion.tools.is_empty());
    }

    #[test]
    fn response_without_tool_call_does_not_extend_the_loop() {
        let response = CompletionResponse {
            message: Message::assistant("done"),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model_id: "m".into(),
        };
        assert!(!response.wants_tools());
    }

    #[test]
    fn inconsistent_stop_reason_does_not_loop() {
        // A provider may announce `tool_use` without attaching a call: we must not loop on
        // nothing, otherwise the agent spins forever on a sterile turn.
        let response = CompletionResponse {
            message: Message::assistant("text only"),
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            model_id: "m".into(),
        };
        assert!(!response.wants_tools());
    }

    #[test]
    fn response_with_tool_call_extends_the_loop() {
        let response = CompletionResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "1".into(),
                    name: "read".into(),
                    input: json!({"path": "a.rs"}),
                }],
            },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            model_id: "m".into(),
        };
        assert!(response.wants_tools());
    }
}
