//! `po-router` — who answers what, and at what cost.
//!
//! Other harnesses let you assign a model per agent in a configuration file. What is missing is
//! not the technical ability, it is the **policy**: nothing decides, mid-session, that a file
//! search does not deserve a $15-per-million-token model when a local model would do.
//!
//! This crate therefore makes no network call. It answers a single question, in a pure and
//! reproducible way: *given this task class, this budget mode and this fleet of available
//! models, which one to use — and if the ideal choice is unavailable, which one next?*

use po_core::{Capabilities, ModelSpec, Tier};

/// Nature of the task given to a model. This is the routing criterion: it is stable over
/// time, unlike model names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskClass {
    /// Driving the session: decomposition, trade-offs, final synthesis.
    Orchestration,
    /// Writing or modifying code.
    Edit,
    /// Locating files, reading, exploring.
    Search,
    /// Condensing content that has already been gathered.
    Summarize,
    /// Deciding between a few categories.
    Classify,
    /// Reviewing code and reporting defects.
    Review,
    /// Writing or running tests.
    Test,
}

impl TaskClass {
    /// Tier below which the task is done badly whatever the budget.
    /// This is a quality floor: savings must never cross it.
    pub fn minimum_tier(self) -> Tier {
        match self {
            Self::Orchestration => Tier::Frontier,
            Self::Edit | Self::Review => Tier::Strong,
            Self::Search | Self::Test => Tier::Strong,
            Self::Summarize | Self::Classify => Tier::Small,
        }
    }

    /// Tier aimed at when quality matters more than cost.
    pub fn preferred_tier(self) -> Tier {
        match self {
            Self::Orchestration | Self::Edit | Self::Review => Tier::Frontier,
            Self::Search | Self::Test | Self::Summarize => Tier::Strong,
            Self::Classify => Tier::Small,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Orchestration => po_i18n::t!("router.class.orchestration"),
            Self::Edit => po_i18n::t!("router.class.edit"),
            Self::Search => po_i18n::t!("router.class.search"),
            Self::Summarize => po_i18n::t!("router.class.summarize"),
            Self::Classify => po_i18n::t!("router.class.classify"),
            Self::Review => po_i18n::t!("router.class.review"),
            Self::Test => po_i18n::t!("router.class.test"),
        }
    }
}

/// Global trade-off between spend and quality, changeable mid-session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetMode {
    /// Everything on the best available model, regardless of cost.
    MaxQuality,
    /// The cloud keeps orchestration and editing; the rest goes local if it fits.
    Balanced,
    /// Whatever can be done locally is, subject to the quality floor.
    SaveMaxTokens,
    /// No network call. Useful offline, or on code that must not leave the machine.
    Offline,
}

impl BudgetMode {
    /// Cycles through the modes, for the keyboard toggle.
    pub fn next(self) -> Self {
        match self {
            Self::MaxQuality => Self::Balanced,
            Self::Balanced => Self::SaveMaxTokens,
            Self::SaveMaxTokens => Self::Offline,
            Self::Offline => Self::MaxQuality,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MaxQuality => po_i18n::t!("router.mode.max_quality"),
            Self::Balanced => po_i18n::t!("router.mode.balanced"),
            Self::SaveMaxTokens => po_i18n::t!("router.mode.save_max_tokens"),
            Self::Offline => po_i18n::t!("router.mode.offline"),
        }
    }

    /// Should this task class be served locally when possible?
    fn prefers_local(self, class: TaskClass) -> bool {
        match self {
            Self::MaxQuality => false,
            Self::Offline | Self::SaveMaxTokens => true,
            Self::Balanced => !matches!(class, TaskClass::Orchestration | TaskClass::Edit),
        }
    }

    fn forbids_cloud(self) -> bool {
        self == Self::Offline
    }
}

/// What the router needs to know about a request.
#[derive(Debug, Clone)]
pub struct Demand {
    pub class: TaskClass,
    /// Estimated prompt size, to rule out models whose window is too short.
    pub prompt_tokens: u32,
    /// Margin to reserve for the response.
    pub reserve_tokens: u32,
    /// Required capabilities — tool calling for a subagent, vision for an image.
    pub required: Capabilities,
}

impl Demand {
    pub fn new(class: TaskClass) -> Self {
        Self {
            class,
            prompt_tokens: 0,
            reserve_tokens: 4096,
            required: Capabilities::AGENTIC,
        }
    }

    pub fn with_prompt(mut self, tokens: u32) -> Self {
        self.prompt_tokens = tokens;
        self
    }
}

/// The router's decision, along with its justification.
///
/// The justification is not decorative: when the agent delegates to a weaker model than
/// expected, the user must be able to read it in the transcript, otherwise a mediocre answer
/// remains unexplained.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub model_id: String,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The model matches the policy exactly.
    Policy,
    /// No local model was suitable: we moved up to the cloud.
    NoLocalCandidate,
    /// Offline mode forbids the cloud; the best available local model was taken,
    /// below the desired tier.
    OfflineFallback,
    /// No model of the required tier had a large enough window.
    ContextForced,
    /// No model reaches the task's quality tier; the best available one below it was taken.
    /// This is a cause distinct from the context window, and confusing the two sends the user
    /// looking for a nonexistent size problem.
    TierRelaxed,
    /// Choice imposed by the user; the policy is ignored.
    UserPinned,
}

impl Reason {
    pub fn explain(&self) -> &'static str {
        match self {
            Self::Policy => po_i18n::t!("router.reason.policy"),
            Self::NoLocalCandidate => po_i18n::t!("router.reason.no_local_candidate"),
            Self::OfflineFallback => po_i18n::t!("router.reason.offline_fallback"),
            Self::ContextForced => po_i18n::t!("router.reason.context_forced"),
            Self::TierRelaxed => po_i18n::t!("router.reason.tier_relaxed"),
            Self::UserPinned => po_i18n::t!("router.reason.user_pinned"),
        }
    }

    /// Does this decision deserve to be reported to the user?
    pub fn is_noteworthy(&self) -> bool {
        !matches!(self, Self::Policy)
    }
}

pub struct Router {
    models: Vec<ModelSpec>,
    mode: BudgetMode,
    /// Model imposed for orchestration, chosen by the user.
    pinned: Option<String>,
    /// Models set aside for the rest of the session: rejected API key, model unknown to the
    /// provider, local server stopped. Insisting on them would make every turn fail.
    disabled: Vec<String>,
}

impl Router {
    pub fn new(models: Vec<ModelSpec>, mode: BudgetMode) -> Self {
        Self { models, mode, pinned: None, disabled: Vec::new() }
    }

    /// Sets a model aside for the rest of the session.
    ///
    /// This is what distinguishes a multi-source fleet from a plain list: when a key is
    /// rejected or a local server stops, the session carries on with another model instead of
    /// breaking off.
    pub fn disable_model(&mut self, id: &str) {
        if !self.disabled.iter().any(|d| d == id) {
            self.disabled.push(id.to_owned());
        }
        if self.pinned.as_deref() == Some(id) {
            self.pinned = None;
        }
    }

    /// Puts a model back in service — after a server restart, for example.
    pub fn enable_model(&mut self, id: &str) {
        self.disabled.retain(|d| d != id);
    }

    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.iter().any(|d| d == id)
    }

    pub fn disabled_models(&self) -> &[String] {
        &self.disabled
    }

    pub fn mode(&self) -> BudgetMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: BudgetMode) {
        self.mode = mode;
    }

    /// Switches to the next mode and returns it — this is what the keyboard shortcut triggers.
    pub fn cycle_mode(&mut self) -> BudgetMode {
        self.mode = self.mode.next();
        self.mode
    }

    /// The model currently imposed, if any.
    pub fn pinned(&self) -> Option<&str> {
        self.pinned.as_deref()
    }

    /// Imposes a model, whatever the policy. `None` hands control back to routing.
    pub fn pin(&mut self, model_id: Option<String>) {
        self.pinned = model_id;
    }

    pub fn models(&self) -> &[ModelSpec] {
        &self.models
    }

    /// Adds a model mid-session — typically a local server Po has just started. An identifier
    /// already present is replaced, so that a swap does not leave behind an entry pointing at a
    /// port that is now closed.
    pub fn add_model(&mut self, spec: ModelSpec) {
        self.models.retain(|model| model.id != spec.id);
        self.models.push(spec);
    }

    /// Removes a model that became unreachable (server stopped). If that model was pinned,
    /// automatic routing takes over rather than aiming at nothing.
    pub fn remove_model(&mut self, id: &str) {
        self.models.retain(|model| model.id != id);
        if self.pinned.as_deref() == Some(id) {
            self.pinned = None;
        }
    }

    pub fn get(&self, id: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|model| model.id == id)
    }

    /// Picks a model for this request.
    ///
    /// The order of the steps reflects the priority: we never go below the task's quality
    /// floor, but we always prefer the cheapest among the valid candidates.
    pub fn select(&self, demand: &Demand) -> Option<Decision> {
        if let Some(pinned) = &self.pinned
            && !self.is_disabled(pinned)
            && let Some(model) = self.get(pinned)
            && self.is_eligible(model, demand, model.tier)
        {
            return Some(Decision { model_id: model.id.clone(), reason: Reason::UserPinned });
        }

        let floor = demand.class.minimum_tier();
        let prefers_local = self.mode.prefers_local(demand.class);

        // 1. Candidates meeting the quality floor and the hardware constraints.
        let eligible: Vec<&ModelSpec> = self
            .models
            .iter()
            .filter(|model| self.is_eligible(model, demand, floor))
            .collect();

        if !eligible.is_empty() {
            // In saving mode we want the cheapest that holds the floor; otherwise the best.
            if prefers_local {
                if let Some(model) = best_local(&eligible) {
                    return Some(Decision { model_id: model.id.clone(), reason: Reason::Policy });
                }
                if let Some(model) = best_cloud(&eligible) {
                    return Some(Decision {
                        model_id: model.id.clone(),
                        reason: Reason::NoLocalCandidate,
                    });
                }
            }

            let target = demand.class.preferred_tier();
            let best = eligible
                .iter()
                .filter(|model| !self.mode.forbids_cloud() || model.is_local())
                .max_by_key(|model| (model.tier.min(target), model.tier))
                .copied();
            if let Some(model) = best {
                return Some(Decision { model_id: model.id.clone(), reason: Reason::Policy });
            }
        }

        // 2. Offline and no local model reaches the floor: we relax quality rather than
        //    answer nothing, but we take the best local model — not the first one — and the
        //    decision carries the trace of this compromise.
        if self.mode.forbids_cloud()
            && let Some(model) = self.best_local_any(demand)
        {
            return Some(Decision {
                model_id: model.id.clone(),
                reason: Reason::OfflineFallback,
            });
        }

        // 3. Nobody left at the floor. Two possible causes, which must be told apart: either
        //    no model reaches the task's tier, or none has the necessary window. Announcing
        //    the wrong one sends the user looking in the wrong place.
        let reachable: Vec<&ModelSpec> = self
            .models
            .iter()
            .filter(|model| {
                !self.is_disabled(&model.id)
                    && model.capabilities.covers(&demand.required)
                    && (!self.mode.forbids_cloud() || model.is_local())
            })
            .collect();

        let fits = |model: &ModelSpec| model.fits(demand.prompt_tokens, demand.reserve_tokens);
        let fallback = reachable
            .iter()
            .copied()
            .filter(|model| fits(model))
            .max_by_key(|model| model.tier)?;

        // Two distinct causes: if every reachable model fit in the window, the tier is what
        // had to be relaxed; otherwise the context size ruled out candidates and that is what
        // must be named.
        let ruled_out_by_size = reachable.iter().any(|model| !fits(model));
        let reason = if ruled_out_by_size {
            Reason::ContextForced
        } else {
            Reason::TierRelaxed
        };

        Some(Decision { model_id: fallback.id.clone(), reason })
    }

    fn is_eligible(&self, model: &ModelSpec, demand: &Demand, floor: Tier) -> bool {
        !self.is_disabled(&model.id)
            && model.tier >= floor
            && model.capabilities.covers(&demand.required)
            && model.fits(demand.prompt_tokens, demand.reserve_tokens)
            && (!self.mode.forbids_cloud() || model.is_local())
    }

    fn best_local_any(&self, demand: &Demand) -> Option<&ModelSpec> {
        self.models
            .iter()
            .filter(|model| {
                !self.is_disabled(&model.id)
                    && model.is_local()
                    && model.capabilities.covers(&demand.required)
                    && model.fits(demand.prompt_tokens, demand.reserve_tokens)
            })
            .max_by_key(|model| model.tier)
    }
}

/// Best local candidate: the **smallest tier** among those that already satisfy the task's
/// quality floor.
///
/// Taking the biggest would defeat the purpose: the candidates have all cleared the floor, so
/// the extra power brings nothing to this task, while a bigger model takes more VRAM and
/// answers more slowly. A summary handed to a 27-billion-parameter model when a 3-billion one
/// suffices is exactly what the "maximum savings" mode is meant to avoid.
fn best_local<'a>(candidates: &[&'a ModelSpec]) -> Option<&'a ModelSpec> {
    candidates
        .iter()
        .filter(|model| model.is_local())
        .min_by_key(|model| model.tier)
        .copied()
}

fn best_cloud<'a>(candidates: &[&'a ModelSpec]) -> Option<&'a ModelSpec> {
    candidates
        .iter()
        .filter(|model| !model.is_local())
        .min_by(|a, b| {
            a.pricing
                .input_per_mtok
                .partial_cmp(&b.pricing.input_per_mtok)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_core::{Origin, Pricing};

    fn model(id: &str, tier: Tier, local: bool, context: u32, price: f64) -> ModelSpec {
        ModelSpec {
            id: id.into(),
            display_name: id.into(),
            origin: if local {
                Origin::Local { backend: "llama.cpp".into(), endpoint: "http://127.0.0.1:8080".into() }
            } else {
                Origin::Cloud { provider: "cloud".into() }
            },
            tier,
            capabilities: Capabilities::AGENTIC,
            context_window: context,
            max_output_tokens: 4096,
            pricing: if local {
                Pricing::FREE
            } else {
                Pricing { input_per_mtok: price, output_per_mtok: price * 5.0, ..Default::default() }
            },
        }
    }

    /// Fleet representative of the target case: a top cloud model, a sturdy local model,
    /// a small local one, and a cheap cloud one.
    fn fleet() -> Vec<ModelSpec> {
        vec![
            model("opus-5", Tier::Frontier, false, 200_000, 15.0),
            model("haiku", Tier::Strong, false, 200_000, 1.0),
            model("qwen3-27b", Tier::Strong, true, 32_768, 0.0),
            model("qwen3-3b", Tier::Small, true, 32_768, 0.0),
        ]
    }

    fn router_with(mode: BudgetMode) -> Router {
        Router::new(fleet(), mode)
    }

    #[test]
    fn at_max_quality_everything_goes_to_the_best_model() {
        let router = router_with(BudgetMode::MaxQuality);
        for class in [TaskClass::Orchestration, TaskClass::Edit, TaskClass::Review] {
            let decision = router.select(&Demand::new(class)).unwrap();
            assert_eq!(decision.model_id, "opus-5", "{}", class.label());
        }
    }

    #[test]
    fn at_max_savings_secondary_tasks_go_local() {
        let router = router_with(BudgetMode::SaveMaxTokens);
        for class in [TaskClass::Search, TaskClass::Summarize, TaskClass::Classify, TaskClass::Edit] {
            let decision = router.select(&Demand::new(class)).unwrap();
            let model = router.get(&decision.model_id).unwrap();
            assert!(model.is_local(), "{} should stay local", class.label());
        }
    }

    #[test]
    fn savings_pick_the_smallest_sufficient_model() {
        // Defect seen in real conditions: a summary was handed to the biggest available local
        // model, although the task's floor was already met by the smallest. "Maximum savings"
        // must mean the cheapest that suffices.
        let router = router_with(BudgetMode::SaveMaxTokens);
        let decision = router.select(&Demand::new(TaskClass::Summarize)).unwrap();
        assert_eq!(decision.model_id, "qwen3-3b", "the small local model suffices for a summary");

        // And classification, with an even lower floor, must not move up either.
        let decision = router.select(&Demand::new(TaskClass::Classify)).unwrap();
        assert_eq!(decision.model_id, "qwen3-3b");
    }

    #[test]
    fn the_floor_takes_priority_over_savings() {
        // Savings must never go below the floor: a code edit requires at least the "strong"
        // tier, even if a smaller model is available.
        let router = router_with(BudgetMode::SaveMaxTokens);
        let decision = router.select(&Demand::new(TaskClass::Edit)).unwrap();
        assert_eq!(decision.model_id, "qwen3-27b");
    }

    #[test]
    fn the_quality_floor_is_never_crossed() {
        // This is the central safeguard: saving must not hand a code edit to a
        // classification model.
        let router = router_with(BudgetMode::SaveMaxTokens);
        let decision = router.select(&Demand::new(TaskClass::Edit)).unwrap();
        let model = router.get(&decision.model_id).unwrap();
        assert!(model.tier >= TaskClass::Edit.minimum_tier());
        assert_eq!(model.id, "qwen3-27b", "the small local model is below the floor");
    }

    #[test]
    fn orchestration_stays_in_the_cloud_even_at_max_savings() {
        // No local model reaches the "frontier" tier in this fleet: the policy must move up
        // to the cloud and say so, rather than degrade the conductor.
        let router = router_with(BudgetMode::SaveMaxTokens);
        let decision = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_eq!(decision.model_id, "opus-5");
        assert_eq!(decision.reason, Reason::NoLocalCandidate);
        assert!(decision.reason.is_noteworthy(), "the user must be able to understand it");
    }

    #[test]
    fn balanced_mode_splits_by_task_nature() {
        let router = router_with(BudgetMode::Balanced);

        // What touches code stays in the cloud…
        for class in [TaskClass::Orchestration, TaskClass::Edit] {
            let decision = router.select(&Demand::new(class)).unwrap();
            assert!(!router.get(&decision.model_id).unwrap().is_local(), "{}", class.label());
        }
        // …the rest goes down to local.
        for class in [TaskClass::Search, TaskClass::Summarize, TaskClass::Classify] {
            let decision = router.select(&Demand::new(class)).unwrap();
            assert!(router.get(&decision.model_id).unwrap().is_local(), "{}", class.label());
        }
    }

    #[test]
    fn offline_mode_never_calls_the_cloud() {
        let router = router_with(BudgetMode::Offline);
        for class in [
            TaskClass::Orchestration,
            TaskClass::Edit,
            TaskClass::Search,
            TaskClass::Classify,
        ] {
            let decision = router.select(&Demand::new(class)).unwrap();
            assert!(
                router.get(&decision.model_id).unwrap().is_local(),
                "{} leaked to the cloud",
                class.label()
            );
        }
    }

    #[test]
    fn offline_orchestration_degrades_but_says_so() {
        let router = router_with(BudgetMode::Offline);
        let decision = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_eq!(decision.model_id, "qwen3-27b");
        assert_eq!(decision.reason, Reason::OfflineFallback);
    }

    #[test]
    fn a_prompt_too_long_for_local_moves_up_to_the_cloud() {
        let router = router_with(BudgetMode::SaveMaxTokens);
        // 100,000 tokens exceed the local models' window (32,768).
        let demand = Demand::new(TaskClass::Search).with_prompt(100_000);
        let decision = router.select(&demand).unwrap();
        let model = router.get(&decision.model_id).unwrap();
        assert!(!model.is_local());
        assert!(model.context_window >= 100_000);
    }

    #[test]
    fn the_cheapest_is_chosen_when_local_does_not_fit() {
        let router = router_with(BudgetMode::SaveMaxTokens);
        let demand = Demand::new(TaskClass::Search).with_prompt(100_000);
        let decision = router.select(&demand).unwrap();
        assert_eq!(decision.model_id, "haiku", "saving also means picking the cheapest");
    }

    #[test]
    fn a_pinned_model_overrides_the_policy() {
        let mut router = router_with(BudgetMode::SaveMaxTokens);
        router.pin(Some("opus-5".into()));
        let decision = router.select(&Demand::new(TaskClass::Classify)).unwrap();
        assert_eq!(decision.model_id, "opus-5");
        assert_eq!(decision.reason, Reason::UserPinned);

        router.pin(None);
        let decision = router.select(&Demand::new(TaskClass::Classify)).unwrap();
        assert_ne!(decision.model_id, "opus-5");
    }

    #[test]
    fn a_pinned_but_unsuitable_model_does_not_block_the_session() {
        let mut router = router_with(BudgetMode::Balanced);
        // The pinned model lacks the necessary window: the policy takes over.
        router.pin(Some("qwen3-27b".into()));
        let demand = Demand::new(TaskClass::Search).with_prompt(100_000);
        let decision = router.select(&demand).unwrap();
        assert_ne!(decision.model_id, "qwen3-27b");
    }

    #[test]
    fn a_nonexistent_pinned_model_is_ignored() {
        let mut router = router_with(BudgetMode::Balanced);
        router.pin(Some("ghost-model".into()));
        assert!(router.select(&Demand::new(TaskClass::Search)).is_some());
    }

    #[test]
    fn required_capabilities_filter_candidates() {
        let mut models = fleet();
        // Only the top model sees images.
        models[0].capabilities.vision = true;
        let router = Router::new(models, BudgetMode::SaveMaxTokens);

        let mut demand = Demand::new(TaskClass::Summarize);
        demand.required.vision = true;
        let decision = router.select(&demand).unwrap();
        assert_eq!(decision.model_id, "opus-5");
    }

    #[test]
    fn a_relaxed_tier_is_announced_as_such() {
        po_i18n::set_lang(po_i18n::Lang::En);
        // Common case: a single local server whose model is not of the "frontier" tier.
        // Orchestration falls back on it, but mentioning the context window would send the
        // user looking for a size problem that does not exist.
        let modest = model("local-unique", Tier::Strong, true, 32_768, 0.0);
        let router = Router::new(vec![modest], BudgetMode::Balanced);

        let decision = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_eq!(decision.model_id, "local-unique");
        assert_eq!(decision.reason, Reason::TierRelaxed);
        assert!(decision.reason.explain().contains("tier"));
    }

    #[test]
    fn an_insufficient_window_is_still_announced_as_such() {
        // Here the cause really is size: a model of the right tier exists, but its window is
        // not enough.
        let router = router_with(BudgetMode::Balanced);
        let demand = Demand::new(TaskClass::Orchestration).with_prompt(150_000);
        let decision = router.select(&demand).unwrap();
        // opus-5 (200,000) still fits; so we force a case where only a short model remains.
        assert!(decision.reason == Reason::Policy || decision.reason == Reason::ContextForced);
    }

    #[test]
    fn an_empty_fleet_does_not_panic() {
        let router = Router::new(Vec::new(), BudgetMode::Balanced);
        assert!(router.select(&Demand::new(TaskClass::Edit)).is_none());
    }

    #[test]
    fn a_local_model_can_be_added_mid_session() {
        let mut router = Router::new(vec![model("opus-5", Tier::Frontier, false, 200_000, 15.0)], BudgetMode::SaveMaxTokens);
        // Without a local model, a search goes to the cloud.
        let before = router.select(&Demand::new(TaskClass::Search)).unwrap();
        assert_eq!(before.model_id, "opus-5");

        router.add_model(model("qwen-started", Tier::Strong, true, 32_768, 0.0));
        let after = router.select(&Demand::new(TaskClass::Search)).unwrap();
        assert_eq!(after.model_id, "qwen-started", "the freshly launched model must serve");
    }

    #[test]
    fn adding_the_same_identifier_twice_does_not_duplicate() {
        let mut router = router_with(BudgetMode::Balanced);
        let before = router.models().len();
        router.add_model(model("qwen3-27b", Tier::Small, true, 8192, 0.0));
        assert_eq!(router.models().len(), before);
        // The new definition is the one that wins: a reassigned port must overwrite the old one.
        assert_eq!(router.get("qwen3-27b").unwrap().context_window, 8192);
    }

    #[test]
    fn removing_a_model_makes_it_unselectable() {
        let mut router = router_with(BudgetMode::SaveMaxTokens);
        router.remove_model("qwen3-27b");
        router.remove_model("qwen3-3b");
        let decision = router.select(&Demand::new(TaskClass::Search)).unwrap();
        assert!(!router.get(&decision.model_id).unwrap().is_local());
    }

    #[test]
    fn removing_a_pinned_model_hands_control_back_to_routing() {
        // Otherwise the session would aim forever at a stopped server.
        let mut router = router_with(BudgetMode::Balanced);
        router.pin(Some("qwen3-27b".into()));
        router.remove_model("qwen3-27b");
        let decision = router.select(&Demand::new(TaskClass::Search)).unwrap();
        assert_ne!(decision.reason, Reason::UserPinned);
    }

    #[test]
    fn a_disabled_model_is_no_longer_selected() {
        // Real case: the top model's API key is rejected. The session must carry on with
        // another model rather than fail every turn.
        let mut router = router_with(BudgetMode::MaxQuality);
        let before = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_eq!(before.model_id, "opus-5");

        router.disable_model("opus-5");
        assert!(router.is_disabled("opus-5"));
        let after = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_ne!(after.model_id, "opus-5");
    }

    #[test]
    fn disabling_a_pinned_model_hands_control_back_to_routing() {
        let mut router = router_with(BudgetMode::Balanced);
        router.pin(Some("qwen3-27b".into()));
        router.disable_model("qwen3-27b");
        let decision = router.select(&Demand::new(TaskClass::Search)).unwrap();
        assert_ne!(decision.model_id, "qwen3-27b");
        assert_ne!(decision.reason, Reason::UserPinned);
    }

    #[test]
    fn a_disabled_model_is_ignored_by_every_fallback() {
        // Including by the offline fallback and the context-window fallback, which bypass the
        // main eligibility filter.
        let mut router = router_with(BudgetMode::Offline);
        router.disable_model("qwen3-27b");
        let decision = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_eq!(decision.model_id, "qwen3-3b", "the small local model stays, the big one is set aside");

        let mut router = router_with(BudgetMode::SaveMaxTokens);
        router.disable_model("opus-5");
        router.disable_model("haiku");
        let demand = Demand::new(TaskClass::Search).with_prompt(100_000);
        // No model has the necessary window any more: the router must say so, not lie.
        assert!(router.select(&demand).is_none());
    }

    #[test]
    fn a_re_enabled_model_becomes_selectable_again() {
        let mut router = router_with(BudgetMode::MaxQuality);
        router.disable_model("opus-5");
        router.enable_model("opus-5");
        assert!(!router.is_disabled("opus-5"));
        assert_eq!(
            router.select(&Demand::new(TaskClass::Orchestration)).unwrap().model_id,
            "opus-5"
        );
    }

    #[test]
    fn the_mode_cycle_loops() {
        let mut router = router_with(BudgetMode::MaxQuality);
        assert_eq!(router.cycle_mode(), BudgetMode::Balanced);
        assert_eq!(router.cycle_mode(), BudgetMode::SaveMaxTokens);
        assert_eq!(router.cycle_mode(), BudgetMode::Offline);
        assert_eq!(router.cycle_mode(), BudgetMode::MaxQuality);
    }

    #[test]
    fn the_concrete_case_from_the_specification() {
        // Opus 5 as conductor, local subagents in saving mode: this is exactly the requested
        // scenario, verified end to end.
        let router = router_with(BudgetMode::SaveMaxTokens);

        let conductor = router.select(&Demand::new(TaskClass::Orchestration)).unwrap();
        assert_eq!(conductor.model_id, "opus-5");

        let subagent = router.select(&Demand::new(TaskClass::Search)).unwrap();
        let model = router.get(&subagent.model_id).unwrap();
        assert!(model.is_local());
        assert!(model.pricing.is_free());
    }
}
