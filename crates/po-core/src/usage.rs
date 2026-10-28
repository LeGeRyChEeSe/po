//! Token and cost accounting.
//!
//! The "economic routing" pillar is only worth something if the savings are measured: a mode
//! that promises to save tokens without showing how many is merely a preference. So every turn
//! is counted, the cache is distinguished (its price differs), and cloud usage — billed per
//! token — is summed separately from local usage — billed in compute time, so free in money
//! but costly in seconds and VRAM.

use std::fmt;
use std::ops::{Add, AddAssign};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Tokens read from the prompt cache: billed far cheaper than regular input.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Tokens written to the cache: billed *more* than input, once.
    #[serde(default)]
    pub cache_write_tokens: u64,
}

impl Usage {
    /// Raw total of processed tokens, cache included.
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

impl Add for Usage {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cache_read_tokens: self.cache_read_tokens + other.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens + other.cache_write_tokens,
        }
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        *self = *self + other;
    }
}

/// A model's prices, in dollars per million tokens.
/// A local model has an all-zero grid: its expense is measured elsewhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: f64,
    #[serde(default)]
    pub cache_write_per_mtok: f64,
}

impl Pricing {
    /// The zero grid: that of any model running on the user's machine.
    pub const FREE: Self = Self {
        input_per_mtok: 0.0,
        output_per_mtok: 0.0,
        cache_read_per_mtok: 0.0,
        cache_write_per_mtok: 0.0,
    };

    pub fn cost_of(&self, usage: &Usage) -> f64 {
        let per_mtok = |tokens: u64, rate: f64| (tokens as f64) * rate / 1_000_000.0;
        per_mtok(usage.input_tokens, self.input_per_mtok)
            + per_mtok(usage.output_tokens, self.output_per_mtok)
            + per_mtok(usage.cache_read_tokens, self.cache_read_per_mtok)
            + per_mtok(usage.cache_write_tokens, self.cache_write_per_mtok)
    }

    pub fn is_free(&self) -> bool {
        *self == Self::FREE
    }
}

/// Running totals of a session: what the user sees in the status bar, and what the router
/// relies on to decide whether to keep delegating to the cloud.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    /// Billed usage, by model identifier.
    pub cloud: Vec<(String, Usage, f64)>,
    /// Local usage, by model identifier: tokens and compute time.
    pub local: Vec<(String, Usage, f64)>,
}

impl Ledger {
    /// Records a turn. `seconds` only makes sense for a local model.
    pub fn record(&mut self, model: &str, usage: Usage, pricing: &Pricing, seconds: f64) {
        let entries = if pricing.is_free() { &mut self.local } else { &mut self.cloud };
        let amount = if pricing.is_free() { seconds } else { pricing.cost_of(&usage) };

        if let Some(entry) = entries.iter_mut().find(|(name, _, _)| name == model) {
            entry.1 += usage;
            entry.2 += amount;
        } else {
            entries.push((model.to_owned(), usage, amount));
        }
    }

    /// Total monetary spend, in dollars.
    pub fn total_cost(&self) -> f64 {
        // `+ 0.0` normalizes negative zero: in Rust, the sum of an empty `f64` iterator is
        // `-0.0`, which prints as "-0.0000" and looks like a refund.
        let total: f64 = self.cloud.iter().map(|(_, _, cost)| cost).sum();
        total + 0.0
    }

    /// Accumulated local compute seconds.
    pub fn total_local_seconds(&self) -> f64 {
        let total: f64 = self.local.iter().map(|(_, _, seconds)| seconds).sum();
        total + 0.0
    }

    pub fn total_tokens(&self) -> u64 {
        self.cloud.iter().chain(&self.local).map(|(_, usage, _)| usage.total()).sum()
    }

    /// Share of tokens processed outside billing, between 0 and 1.
    /// It directly measures the efficiency of the chosen saving mode.
    pub fn local_share(&self) -> f64 {
        let total = self.total_tokens();
        if total == 0 {
            return 0.0;
        }
        let local: u64 = self.local.iter().map(|(_, usage, _)| usage.total()).sum();
        local as f64 / total as f64
    }

    /// What the session would have cost had everything gone through this price: used to
    /// quantify the savings actually obtained rather than promise them.
    pub fn counterfactual_cost(&self, reference: &Pricing) -> f64 {
        self.cloud
            .iter()
            .chain(&self.local)
            .map(|(_, usage, _)| reference.cost_of(usage))
            .sum()
    }
}

impl fmt::Display for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cost = format!("{:.4}", self.total_cost());
        let share = format!("{:.0}", self.local_share() * 100.0);
        write!(f, "{}", po_i18n::t!("core.ledger.summary", tokens = self.total_tokens(), cost = cost, share = share))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPUS: Pricing = Pricing {
        input_per_mtok: 15.0,
        output_per_mtok: 75.0,
        cache_read_per_mtok: 1.5,
        cache_write_per_mtok: 18.75,
    };

    fn usage(input: u64, output: u64) -> Usage {
        Usage { input_tokens: input, output_tokens: output, ..Default::default() }
    }

    #[test]
    fn cost_follows_the_price_grid() {
        let cost = OPUS.cost_of(&usage(1_000_000, 1_000_000));
        assert!((cost - 90.0).abs() < 1e-9, "$15 input + $75 output");
    }

    #[test]
    fn cache_is_billed_at_its_own_rate() {
        let u = Usage { cache_read_tokens: 1_000_000, ..Default::default() };
        assert!((OPUS.cost_of(&u) - 1.5).abs() < 1e-9);
    }

    #[test]
    fn a_local_model_costs_nothing_but_counts_time() {
        let mut ledger = Ledger::default();
        ledger.record("qwen3-27b", usage(10_000, 2_000), &Pricing::FREE, 12.5);
        assert_eq!(ledger.total_cost(), 0.0);
        assert_eq!(ledger.total_local_seconds(), 12.5);
        assert_eq!(ledger.total_tokens(), 12_000);
    }

    #[test]
    fn turns_of_the_same_model_aggregate() {
        let mut ledger = Ledger::default();
        ledger.record("opus", usage(1000, 100), &OPUS, 0.0);
        ledger.record("opus", usage(2000, 200), &OPUS, 0.0);
        assert_eq!(ledger.cloud.len(), 1, "a single line per model");
        assert_eq!(ledger.cloud[0].1, usage(3000, 300));
    }

    #[test]
    fn local_share_measures_the_savings() {
        let mut ledger = Ledger::default();
        ledger.record("opus", usage(1_000, 0), &OPUS, 0.0);
        ledger.record("qwen", usage(3_000, 0), &Pricing::FREE, 5.0);
        assert!((ledger.local_share() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn counterfactual_quantifies_what_was_avoided() {
        let mut ledger = Ledger::default();
        ledger.record("opus", usage(1_000_000, 0), &OPUS, 0.0);
        ledger.record("qwen", usage(1_000_000, 0), &Pricing::FREE, 30.0);

        let actual = ledger.total_cost();
        let all_cloud = ledger.counterfactual_cost(&OPUS);
        assert!((actual - 15.0).abs() < 1e-9);
        assert!((all_cloud - 30.0).abs() < 1e-9);
        assert!(actual < all_cloud, "the savings must be demonstrable, not promised");
    }

    #[test]
    fn an_empty_ledger_does_not_divide_by_zero() {
        let ledger = Ledger::default();
        assert_eq!(ledger.local_share(), 0.0);
        assert_eq!(ledger.total_tokens(), 0);
    }

    #[test]
    fn a_ledger_without_spend_prints_a_positive_zero() {
        // The sum of an empty f64 iterator is -0.0 in Rust: without normalization,
        // a fully local session would print "$-0.0000".
        let mut ledger = Ledger::default();
        ledger.record("local", Usage { input_tokens: 10, ..Default::default() }, &Pricing::FREE, 1.0);
        assert!(!format!("{:.4}", ledger.total_cost()).starts_with('-'));
        assert!(!ledger.to_string().contains("$-"));

        let empty = Ledger::default();
        assert!(!format!("{:.4}", empty.total_cost()).starts_with('-'));
        assert!(!format!("{:.4}", empty.total_local_seconds()).starts_with('-'));
    }
}
