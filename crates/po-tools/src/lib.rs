//! `po-tools` — the tools the agent can run, and the guardrails around them.
//!
//! Three responsibilities: describing the tools to the model, running them in a confined
//! workspace, and classifying each call as allowed, denied, or subject to user approval.
//! The classification is deliberately made **before** execution and from the real arguments:
//! "this tool is dangerous" is a useless approximation, what matters is "this call writes
//! outside the repository" or "this command recursively deletes".

pub mod diff;
pub mod fs;
pub mod media;
pub mod search;
pub mod shell;
pub mod skills;
pub mod workspace;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use po_core::{ToolOutcome, ToolSpec};
use po_core::tool::Permission;
use serde_json::Value;

pub use workspace::{PathError, Workspace};

/// Execution context shared by all the tools of a session.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace: Workspace,
    /// Cap on the output returned to the model. Beyond it the content is truncated: a chatty
    /// command must not consume the whole context window.
    pub max_output_bytes: usize,
    /// What the model that will read the result can see or hear: `read` attaches an image
    /// or a video only if it can read it.
    pub media: Media,
}

/// Media a model accepts, according to what its server announces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Media {
    pub image: bool,
    pub video: bool,
    pub audio: bool,
}

impl ToolContext {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace, max_output_bytes: 30_000, media: Media::default() }
    }

    /// Truncates an output while announcing what was cut, rather than letting the model
    /// believe it saw everything.
    pub fn clamp(&self, text: &str) -> String {
        if text.len() <= self.max_output_bytes {
            return text.to_owned();
        }
        let mut end = self.max_output_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let omitted = text.len() - end;
        format!("{}\n\n[… {omitted} bytes truncated …]", &text[..end])
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;

    /// Declaration sent to the model.
    fn spec(&self) -> ToolSpec;

    /// Permission decision for *this* call, arguments included.
    fn permission(&self, input: &Value) -> Permission;

    /// Runs the call. A business error must be returned as an error `ToolOutcome`, not as an
    /// `Err`: the model must be able to read it and correct itself.
    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome;
}

/// Set of tools exposed to a given agent. A sub-agent may receive a subset: fewer declared
/// tools means fewer tokens on every turn.
#[derive(Clone, Default)]
pub struct Registry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All the built-in tools.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(fs::ReadTool));
        registry.register(Arc::new(fs::WriteTool));
        registry.register(Arc::new(fs::EditTool));
        registry.register(Arc::new(fs::ListTool));
        registry.register(Arc::new(search::GlobTool));
        registry.register(Arc::new(search::GrepTool));
        registry.register(Arc::new(shell::BashTool));
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_owned(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Removes the tools whose name starts with `prefix` and returns how many there were.
    /// Used to unplug an MCP server in the middle of a session.
    pub fn remove_prefixed(&mut self, prefix: &str) -> usize {
        let before = self.tools.len();
        self.tools.retain(|name, _| !name.starts_with(prefix));
        before - self.tools.len()
    }

    /// Tool names, in the order the model sees them.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|tool| tool.spec()).collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Restricts the registry to a list of names. Unknown names are silently ignored:
    /// an aging configuration must not prevent startup.
    pub fn subset(&self, names: &[&str]) -> Self {
        Self {
            tools: names
                .iter()
                .filter_map(|name| self.tools.get_key_value(*name))
                .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
                .collect(),
        }
    }

    /// Runs a call. An unknown tool produces an error result the model can act on, never a
    /// panic: models regularly invent tool names.
    pub async fn dispatch(&self, id: &str, name: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        match self.tools.get(name) {
            Some(tool) => tool.run(id, input, ctx).await,
            None => {
                let available: Vec<_> = self.tools.keys().map(String::as_str).collect();
                ToolOutcome::error(
                    id,
                    format!(
                        "unknown tool: \"{name}\". Available tools: {}",
                        available.join(", ")
                    ),
                )
            }
        }
    }
}

/// Extracts a required text field from the arguments.
pub(crate) fn required_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("parameter \"{field}\" missing or not a string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        (dir, ctx)
    }

    #[test]
    fn builtin_registry_exposes_the_expected_tools() {
        let registry = Registry::with_builtins();
        for expected in ["read", "write", "edit", "list", "glob", "grep", "bash"] {
            assert!(registry.get(expected).is_some(), "{expected} missing");
        }
        assert_eq!(registry.specs().len(), registry.len());
    }

    #[test]
    fn a_subset_reduces_the_declaration_cost() {
        let full = Registry::with_builtins();
        let restricted = full.subset(&["read", "grep"]);
        assert_eq!(restricted.len(), 2);

        let full_cost: usize = full.specs().iter().map(|s| s.approx_tokens()).sum();
        let restricted_cost: usize = restricted.specs().iter().map(|s| s.approx_tokens()).sum();
        assert!(restricted_cost < full_cost);
    }

    #[test]
    fn an_unknown_name_in_a_subset_is_ignored() {
        let registry = Registry::with_builtins().subset(&["read", "vanished-tool"]);
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_tool_returns_a_readable_error() {
        let (_dir, ctx) = context();
        let outcome = Registry::with_builtins()
            .dispatch("t1", "invented", Value::Null, &ctx)
            .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("invented"));
        assert!(outcome.content.contains("read"), "the real tools must be listed");
    }

    #[test]
    fn truncation_announces_what_is_missing() {
        let (_dir, mut ctx) = context();
        ctx.max_output_bytes = 100;
        let output = ctx.clamp(&"x".repeat(500));
        assert!(output.contains("truncated"));
        assert!(output.len() < 200);

        // A short output is left untouched.
        assert_eq!(ctx.clamp("short"), "short");
    }

    #[test]
    fn truncation_respects_utf8() {
        let (_dir, mut ctx) = context();
        ctx.max_output_bytes = 51;
        let output = ctx.clamp(&"é".repeat(100));
        assert!(output.contains("truncated"), "must not panic in the middle of a character");
    }

    #[test]
    fn required_fields_are_checked() {
        let input = serde_json::json!({"path": "a.rs", "count": 3});
        assert_eq!(required_str(&input, "path").unwrap(), "a.rs");
        assert!(required_str(&input, "missing").is_err());
        assert!(required_str(&input, "count").is_err(), "a number is not a string");
    }
}
