//! Declaration of the tools exposed to the model.
//!
//! `po-core` describes what a tool is; it cannot run them — that is the job of `po-tools`.
//! This separation lets a subagent receive a subset of tools without embedding their
//! implementation, and lets the router reject a model unable to call them.

use serde::{Deserialize, Serialize};

/// What the model sees of a tool: a name, a description, an input schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON schema of the input object. This text weighs on the context every turn, hence the
    /// value of exposing only the tools useful to the agent concerned.
    pub input_schema: serde_json::Value,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self { name: name.into(), description: description.into(), input_schema }
    }

    /// Approximate cost of the declaration in tokens, to decide what to expose.
    /// Deliberately coarse — four characters per token — since it is used to compare tools
    /// with each other, not to bill.
    pub fn approx_tokens(&self) -> usize {
        let schema = self.input_schema.to_string();
        (self.name.len() + self.description.len() + schema.len()).div_ceil(4)
    }
}

/// Key of the input given to a call whose arguments did not read as JSON: the agent answers it
/// with the reason rather than running the tool on nothing.
pub const INVALID_ARGUMENTS: &str = "_invalid_arguments";

/// A call's arguments, as they arrive serialized. A local model sometimes sends an empty string
/// for a tool without parameters: that is an empty object. Arguments that do not read — most
/// often cut by the output limit in the middle of a file to write — keep their text under
/// [`INVALID_ARGUMENTS`], so the model learns why, instead of "missing `path`".
pub fn parse_tool_arguments(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| {
        let head: String = trimmed.chars().take(200).collect();
        serde_json::json!({ INVALID_ARGUMENTS: head })
    })
}

/// Result of a tool run, as it goes back to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// Identifier of the request this result answers.
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
    /// What the user sees in the transcript, when it differs from the content handed to the
    /// model (a 5,000-line file is summarized on screen but goes whole to the model).
    pub display: Option<String>,
    /// Image, video or sound attached to the result (`read` on a media file): they go to the
    /// model with the tool results, as media blocks.
    pub attachments: Vec<crate::ContentBlock>,
    /// Line diff of the file a tool changed (`edit`, `write`), for display only.
    pub diff: Option<String>,
}

impl ToolOutcome {
    pub fn ok(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
            display: None,
            attachments: Vec::new(),
            diff: None,
        }
    }

    pub fn with_attachment(mut self, block: crate::ContentBlock) -> Self {
        self.attachments.push(block);
        self
    }

    /// Tool failure. It goes back to the model as a result, not as a transport error: it is
    /// actionable information the model must be able to recover from on its own.
    pub fn error(tool_use_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: message.into(),
            is_error: true,
            display: None,
            attachments: Vec::new(),
            diff: None,
        }
    }

    pub fn with_display(mut self, display: impl Into<String>) -> Self {
        self.display = Some(display.into());
        self
    }

    pub fn to_block(&self) -> crate::message::ContentBlock {
        crate::message::ContentBlock::ToolResult {
            tool_use_id: self.tool_use_id.clone(),
            content: self.content.clone(),
            is_error: self.is_error,
        }
    }
}

/// Permission decision for a given call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Run without asking.
    Allow,
    /// The user must decide.
    Ask,
    /// Denied without asking; the model is informed and may choose another path.
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec() -> ToolSpec {
        ToolSpec::new(
            "read",
            "Reads a file",
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )
    }

    #[test]
    fn declaration_cost_is_nonzero_and_increasing() {
        let short = spec();
        let long = ToolSpec::new("read", "Reads a file ".repeat(20), short.input_schema.clone());
        assert!(short.approx_tokens() > 0);
        assert!(long.approx_tokens() > short.approx_tokens());
    }

    #[test]
    fn a_failure_remains_an_actionable_result() {
        let outcome = ToolOutcome::error("t1", "file not found");
        let block = outcome.to_block();
        match block {
            crate::message::ContentBlock::ToolResult { tool_use_id, is_error, content } => {
                assert_eq!(tool_use_id, "t1");
                assert!(is_error);
                assert_eq!(content, "file not found");
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn display_may_differ_from_what_goes_to_the_model() {
        let outcome = ToolOutcome::ok("t1", "x".repeat(10_000)).with_display("10,000 characters read");
        assert_eq!(outcome.display.as_deref(), Some("10,000 characters read"));
        assert_eq!(outcome.content.len(), 10_000, "the model receives the whole content");
    }

    #[test]
    fn the_spec_survives_a_round_trip() {
        let spec = spec();
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<ToolSpec>(&json).unwrap(), spec);
    }
}
