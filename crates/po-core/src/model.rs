//! Description of a model, whatever its origin.
//!
//! The router needs to compare a cloud model and a local model on the same criteria: what it
//! can do, how large its window is, how much it costs. A local model is therefore not a special
//! case bolted on the side, it is the same structure with a zero price grid and a different
//! origin.

use serde::{Deserialize, Serialize};

use crate::usage::Pricing;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    /// Served by a remote, billed API.
    Cloud { provider: String },
    /// Served by a process Po can see, and often drive, on this machine.
    Local {
        /// `llama.cpp`, `ollama`, `lmstudio`, `vllm`…
        backend: String,
        /// Base URL of the server exposing it.
        endpoint: String,
    },
}

impl Origin {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local { .. })
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Cloud { provider } => provider,
            Self::Local { backend, .. } => backend,
        }
    }
}

/// Power class, as the router handles it. Deliberately coarse: a fine hierarchy would be
/// invalidated by every model release, whereas these three tiers describe stable uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Extraction, classification, short summaries. A few billion parameters are enough.
    Small,
    /// Code editing, search, targeted review. A good local model lives here.
    Strong,
    /// Orchestration, architecture, long reasoning. Today the cloud's domain.
    Frontier,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    /// Can call tools. Without this, the model cannot play an agent role.
    pub tools: bool,
    /// Can emit several tool calls in a single turn.
    pub parallel_tools: bool,
    pub vision: bool,
    /// Reads videos and sounds attached to the conversation (announced by the server).
    pub video: bool,
    pub audio: bool,
    /// Exposes a reasoning channel separate from the response text.
    pub thinking: bool,
    /// Can constrain its output to a given JSON schema.
    pub structured_output: bool,
}

impl Capabilities {
    /// The minimum to play an agent role in the loop.
    pub const AGENTIC: Self = Self {
        tools: true,
        parallel_tools: false,
        vision: false,
        video: false,
        audio: false,
        thinking: false,
        structured_output: false,
    };

    /// Does `self` cover everything `required` demands?
    pub fn covers(&self, required: &Self) -> bool {
        (!required.tools || self.tools)
            && (!required.parallel_tools || self.parallel_tools)
            && (!required.vision || self.vision)
            && (!required.video || self.video)
            && (!required.audio || self.audio)
            && (!required.thinking || self.thinking)
            && (!required.structured_output || self.structured_output)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Identifier as sent over the network (`claude-opus-5`, `Qwen3-27B-Q4_K_M.gguf`).
    pub id: String,
    /// Human-readable name shown in the picker.
    pub display_name: String,
    pub origin: Origin,
    pub tier: Tier,
    pub capabilities: Capabilities,
    /// Total context window, in tokens.
    pub context_window: u32,
    /// Output cap per turn.
    pub max_output_tokens: u32,
    #[serde(default)]
    pub pricing: Pricing,
}

impl ModelSpec {
    /// Can this model handle a request of `prompt_tokens` while leaving `reserve` of margin
    /// for the response? Checked *before* the call: an overflow discovered afterwards costs the
    /// price of the call and the user's trust.
    pub fn fits(&self, prompt_tokens: u32, reserve: u32) -> bool {
        prompt_tokens.saturating_add(reserve) <= self.context_window
    }

    /// Remaining margin after a given prompt.
    pub fn headroom(&self, prompt_tokens: u32) -> u32 {
        self.context_window.saturating_sub(prompt_tokens)
    }

    pub fn is_local(&self) -> bool {
        self.origin.is_local()
    }

    /// Can play an agent role: without tool calls, the loop has nothing to run.
    pub fn is_agentic(&self) -> bool {
        self.capabilities.tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(context: u32, tier: Tier, local: bool) -> ModelSpec {
        ModelSpec {
            id: "test".into(),
            display_name: "Test".into(),
            origin: if local {
                Origin::Local { backend: "llama.cpp".into(), endpoint: "http://127.0.0.1:8080".into() }
            } else {
                Origin::Cloud { provider: "anthropic".into() }
            },
            tier,
            capabilities: Capabilities::AGENTIC,
            context_window: context,
            max_output_tokens: 4096,
            pricing: if local { Pricing::FREE } else { Pricing { input_per_mtok: 3.0, ..Default::default() } },
        }
    }

    #[test]
    fn context_check_reserves_the_response() {
        let m = model(8192, Tier::Strong, true);
        assert!(m.fits(4000, 4000));
        assert!(!m.fits(4000, 4193), "the overflow must be seen before the call");
        assert_eq!(m.headroom(6000), 2192);
    }

    #[test]
    fn overflow_does_not_panic() {
        let m = model(8192, Tier::Strong, true);
        assert!(!m.fits(u32::MAX, 1000));
        assert_eq!(m.headroom(u32::MAX), 0);
    }

    #[test]
    fn tiers_are_ordered() {
        assert!(Tier::Frontier > Tier::Strong);
        assert!(Tier::Strong > Tier::Small);
    }

    #[test]
    fn capability_coverage_is_directional() {
        let full = Capabilities {
            tools: true,
            parallel_tools: true,
            vision: true,
            video: true,
            audio: true,
            thinking: true,
            structured_output: true,
        };
        assert!(full.covers(&Capabilities::AGENTIC));
        assert!(!Capabilities::AGENTIC.covers(&full));
        assert!(Capabilities::AGENTIC.covers(&Capabilities::default()));
    }

    #[test]
    fn a_local_model_is_free_and_recognized_as_such() {
        let local = model(32768, Tier::Strong, true);
        assert!(local.is_local());
        assert!(local.pricing.is_free());
        assert_eq!(local.origin.label(), "llama.cpp");

        let cloud = model(200_000, Tier::Frontier, false);
        assert!(!cloud.is_local());
        assert!(!cloud.pricing.is_free());
    }
}
