//! Work mode: plan, builder, bypass (Shift+Tab).
//!
//! It decides what happens when a tool asks for approval. The mode lives in a shared counter:
//! the interface can change it at any moment, even mid-turn (switch to bypass while a
//! permission prompt is pending, go back to plan while the model is writing), and the agent
//! rereads it on every tool call.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkMode {
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
            Self::Plan => "plan",
            Self::Build => "builder",
            Self::Bypass => "bypass",
        }
    }

    /// Shift+Tab order.
    pub fn next(self) -> Self {
        match self {
            Self::Plan => Self::Build,
            Self::Build => Self::Bypass,
            Self::Bypass => Self::Plan,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "plan" => Some(Self::Plan),
            "build" | "builder" | "normal" => Some(Self::Build),
            "bypass" | "auto" => Some(Self::Bypass),
            _ => None,
        }
    }

    /// What the model must know when the mode changes. Attached to the next request rather
    /// than written into the system prompt, which must stay identical for the server's cache.
    pub fn note(self) -> &'static str {
        match self {
            Self::Plan => {
                "[Plan mode enabled by the user] Modify nothing: no file writes, no command that changes the \
                 machine's state. Read, search, analyze, then propose a numbered action plan. The user will \
                 switch to builder mode to execute it."
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
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Plan,
            2 => Self::Bypass,
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
            Self::Build => Gate::Ask,
            Self::Bypass => Gate::Allow,
        }
    }
}

/// Message handed to the model for a call refused in plan mode.
pub const PLAN_REFUSAL: &str =
    "plan mode: no modification or state-changing command — finish your analysis and propose the plan";

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
    fn shift_tab_cycles_through_the_three_modes() {
        let handle = WorkModeHandle::default();
        assert_eq!(handle.get(), WorkMode::Build);
        assert_eq!(handle.cycle(), WorkMode::Bypass);
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
    }
}
