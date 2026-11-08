//! Work mode: auto, auto with bypass, plan, builder, bypass (Shift+Tab).
//!
//! It decides what happens when a tool asks for approval. The mode lives in a shared counter:
//! the interface can change it at any moment, even mid-turn (switch to bypass while a
//! permission prompt is pending, go back to plan while the model is writing), and the agent
//! rereads it on every tool call.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkMode {
    /// The default: the model explores, commits to verifiable goals (see `goals`), builds, and Po
    /// checks the goals before it hands back. Sensitive actions ask, as in builder.
    Auto,
    /// Auto, without any approval for the session.
    AutoBypass,
    /// Read-only: anything that modifies or executes is refused, the model proposes a plan.
    Plan,
    /// The model acts; anything sensitive asks for the user's approval.
    Build,
    /// Everything is allowed without approval, except what the harness always forbids.
    Bypass,
}

impl WorkMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::AutoBypass => "auto · bypass",
            Self::Plan => "plan",
            Self::Build => "builder",
            Self::Bypass => "bypass",
        }
    }

    /// Whether Po drives the work by goals (auto, with or without approvals).
    pub fn is_auto(self) -> bool {
        matches!(self, Self::Auto | Self::AutoBypass)
    }

    /// Shift+Tab order.
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::AutoBypass,
            Self::AutoBypass => Self::Plan,
            Self::Plan => Self::Build,
            Self::Build => Self::Bypass,
            Self::Bypass => Self::Auto,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "auto-bypass" | "auto bypass" | "auto · bypass" | "autobypass" | "auto+bypass" => Some(Self::AutoBypass),
            "plan" => Some(Self::Plan),
            "build" | "builder" | "normal" => Some(Self::Build),
            "bypass" => Some(Self::Bypass),
            _ => None,
        }
    }

    /// What the model must know when the mode changes. Attached to the next request rather
    /// than written into the system prompt, which must stay identical for the server's cache.
    pub fn note(self) -> &'static str {
        match self {
            Self::Auto | Self::AutoBypass => {
                "[Auto mode] You lead the work: explore first (reading is free), then, before any change, \
                 call `goals` with a few concrete goals, each with a shell check that exits 0 only when the \
                 goal is really met (writing a new test file first is allowed, and makes the best check: \
                 one assertion for every rule, example and rejected input the spec names). \
                 Then build. When you believe the goals are met, simply finish: Po runs the checks itself and \
                 tells you what fails. Never weaken a test or a check to make it pass."
            }
            Self::Plan => {
                "[Plan mode enabled by the user] Modify nothing: no file writes, no command that changes the \
                 machine's state. Read, search, analyze (delegate broad exploration to subagents to keep your \
                 context small), then save the plan with the `plan` tool: Po asks the user to launch it, and \
                 its implementation starts in a fresh context from that plan alone."
            }
            Self::Build => {
                "[Builder mode enabled by the user] You may modify files and run commands again; sensitive \
                 actions require their approval."
            }
            Self::Bypass => {
                "[Bypass mode enabled by the user] Your actions are allowed without confirmation. Stay \
                 careful: nothing irreversible unless necessary."
            }
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Plan => 0,
            Self::Build => 1,
            Self::Bypass => 2,
            Self::Auto => 3,
            Self::AutoBypass => 4,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Plan,
            1 => Self::Build,
            2 => Self::Bypass,
            4 => Self::AutoBypass,
            3 => Self::Auto,
            _ => Self::Build,
        }
    }
}

/// What happens to a call that would require approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Ask,
    Allow,
    Refuse,
}

impl WorkMode {
    pub fn gate(self) -> Gate {
        match self {
            Self::Plan => Gate::Refuse,
            Self::Build | Self::Auto => Gate::Ask,
            Self::Bypass | Self::AutoBypass => Gate::Allow,
        }
    }
}

/// Message handed to the model for a call refused in plan mode.
pub const PLAN_REFUSAL: &str =
    "plan mode: no modification or state-changing command (only read-only commands without file writes are permitted) - finish your analysis and propose the plan";

/// Mode shared between the agent and the interface.
#[derive(Debug, Clone)]
pub struct WorkModeHandle(Arc<AtomicU8>);

impl Default for WorkModeHandle {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(WorkMode::Build.as_u8())))
    }
}

impl WorkModeHandle {
    pub fn get(&self) -> WorkMode {
        WorkMode::from_u8(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, mode: WorkMode) {
        self.0.store(mode.as_u8(), Ordering::Relaxed);
    }

    /// Moves to the next mode and returns it.
    pub fn cycle(&self) -> WorkMode {
        let next = self.get().next();
        self.set(next);
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_is_the_default_and_shift_tab_cycles_through_the_five_modes() {
        let handle = WorkModeHandle::default();
        assert_eq!(handle.get(), WorkMode::Build);
        assert_eq!(handle.cycle(), WorkMode::Bypass);
        assert_eq!(handle.cycle(), WorkMode::Auto);
        assert_eq!(handle.cycle(), WorkMode::AutoBypass);
        assert_eq!(handle.cycle(), WorkMode::Plan);
        assert_eq!(handle.cycle(), WorkMode::Build);
        let copy = handle.clone();
        copy.set(WorkMode::Plan);
        assert_eq!(handle.get(), WorkMode::Plan, "the interface and the agent share the same mode");
    }

    #[test]
    fn each_mode_has_its_rule() {
        assert_eq!(WorkMode::Plan.gate(), Gate::Refuse);
        assert_eq!(WorkMode::Build.gate(), Gate::Ask);
        assert_eq!(WorkMode::Bypass.gate(), Gate::Allow);
        assert_eq!(WorkMode::parse("Builder"), Some(WorkMode::Build));
        assert_eq!(WorkMode::Auto.gate(), Gate::Ask, "auto asks for what is sensitive");
        assert_eq!(WorkMode::AutoBypass.gate(), Gate::Allow);
        assert_eq!(WorkMode::parse("auto"), Some(WorkMode::Auto), "no longer an alias of bypass");
        for mode in [WorkMode::Auto, WorkMode::AutoBypass, WorkMode::Plan, WorkMode::Build, WorkMode::Bypass] {
            assert_eq!(WorkMode::parse(mode.label()), Some(mode), "{}", mode.label());
            assert_eq!(WorkMode::from_u8(mode.as_u8()), mode);
        }
    }
}
