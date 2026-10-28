//! Canonical representation of exchanges.
//!
//! Every provider — Anthropic, OpenAI, Gemini, OpenAI-compatible local server — is translated
//! *to* these types and then *from* them. No provider format serves as a pivot: that is
//! precisely what breaks when a single-provider harness is diverted to another model (lost
//! reasoning blocks, reinvented tool identifiers, incompatible roles). The price is one adapter
//! per provider; the benefit is that a local subagent and a cloud orchestrator handle the same
//! history.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A piece of content. A message holds an ordered sequence of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Explicit reasoning of the model. `signature` carries the integrity token the provider
    /// may require to feed the block back on the next turn; losing it invalidates the turn.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// The model asks for a tool to be run.
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Result handed back to the model. `tool_use_id` must repeat the `id` of the matching `ToolUse`.
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
    Image {
        media_type: String,
        /// Base64-encoded data.
        data: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn tool_result(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn tool_error(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::ToolResult {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
        }
    }

    /// Displayable text of this block, empty if it has none.
    pub fn as_text(&self) -> &str {
        match self {
            Self::Text { text } | Self::Thinking { text, .. } => text,
            Self::ToolResult { content, .. } => content,
            _ => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![ContentBlock::text(text)] }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: vec![ContentBlock::text(text)] }
    }

    /// Message carrying tool results. At the protocol level these results are always
    /// attributed to the user: it is the environment that answers the model.
    pub fn tool_results(results: Vec<ContentBlock>) -> Self {
        Self { role: Role::User, content: results }
    }

    /// Tool requests carried by this message, in emission order.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &serde_json::Value)> {
        self.content.iter().filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    pub fn has_tool_use(&self) -> bool {
        self.tool_uses().next().is_some()
    }

    /// Concatenation of the visible text, without reasoning or tool calls.
    pub fn visible_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Why the model handed control back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its turn: it is the user's turn to speak.
    EndTurn,
    /// The model is waiting for tools to run: the loop must continue.
    ToolUse,
    /// Output limit reached — the response is truncated and this must be reported.
    MaxTokens,
    /// A stop sequence was hit.
    StopSequence,
    /// Refusal or filtering on the provider side.
    Refusal,
}

impl StopReason {
    /// Should the agent loop run one more turn?
    pub fn continues_loop(self) -> bool {
        self == Self::ToolUse
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_calls_are_listed_in_order() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("let me look"),
                ContentBlock::ToolUse { id: "a".into(), name: "read".into(), input: json!({}) },
                ContentBlock::ToolUse { id: "b".into(), name: "grep".into(), input: json!({}) },
            ],
        };
        let names: Vec<_> = message.tool_uses().map(|(_, name, _)| name).collect();
        assert_eq!(names, ["read", "grep"]);
        assert!(message.has_tool_use());
    }

    #[test]
    fn visible_text_excludes_reasoning_and_tools() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking { text: "hmm".into(), signature: None },
                ContentBlock::text("here is the answer"),
                ContentBlock::ToolUse { id: "a".into(), name: "read".into(), input: json!({}) },
            ],
        };
        assert_eq!(message.visible_text(), "here is the answer");
    }

    #[test]
    fn reasoning_signature_survives_a_round_trip() {
        let block = ContentBlock::Thinking {
            text: "reasoning".into(),
            signature: Some("sig-123".into()),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert_eq!(serde_json::from_str::<ContentBlock>(&json).unwrap(), block);
        assert!(json.contains("sig-123"), "the signature must never be omitted");
    }

    #[test]
    fn a_successful_result_does_not_clutter_serialization() {
        let ok = ContentBlock::tool_result("t1", "content");
        assert!(!serde_json::to_string(&ok).unwrap().contains("is_error"));
        let ko = ContentBlock::tool_error("t1", "failure");
        assert!(serde_json::to_string(&ko).unwrap().contains("is_error"));
    }

    #[test]
    fn only_tool_use_extends_the_loop() {
        assert!(StopReason::ToolUse.continues_loop());
        for reason in [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::Refusal,
        ] {
            assert!(!reason.continues_loop());
        }
    }
}
