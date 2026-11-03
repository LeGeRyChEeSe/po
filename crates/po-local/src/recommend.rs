//! Which model, in which quantization, for this machine.
//!
//! A beginner does not know that a 27B dense model must fit in the card's memory to be usable,
//! while a mixture of experts (35B-A3B) runs well with its experts in RAM, nor that a 2-bit file
//! of a big model can beat a 4-bit file of a small one. The rules below encode that, on the
//! machine's **capacity** (the card's total memory, the RAM installed): a recommendation is about
//! what the machine can do, not about what another program happens to occupy right now. The live
//! state is said separately, by the launch plan.
//!
//! The file sizes are those published on Hugging Face (read with the Hub's API on 2026-10-31);
//! the rules for Strata are those of its own documentation (`docs/MODELS.md`: RAM + VRAM − 5 GB,
//! and the experts in RAM plus 10 GB).

use crate::hardware::{GpuBackend, Hardware};

const MIB: u64 = 1024 * 1024;
/// Left to the display and the driver on the card.
const DISPLAY_RESERVE_MIB: u64 = 768;
/// Left to the system and the user's other programs in RAM.
const SYSTEM_RAM_MIB: u64 = 6 * 1024;

/// The engine a model card runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    LlamaCpp,
    Strata,
}

/// One downloadable file of a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quant {
    pub name: &'static str,
    /// The file in the repository (the first shard for a split model).
    pub file: &'static str,
    pub bytes: u64,
    /// Good enough to accept a slower placement for it (4 bits or less: beyond, a bigger file
    /// is not worth running from RAM).
    pub everyday: bool,
    /// Strata: what its experts take in RAM, in bytes.
    pub experts_bytes: u64,
    /// Strata: the "RAM + VRAM − 5 GB" tier, in GiB, from which its setup pre-selects it.
    pub tier_gib: u64,
}

const fn q(name: &'static str, file: &'static str, gb_hundredths: u64, everyday: bool) -> Quant {
    Quant { name, file, bytes: gb_hundredths * 10_000_000, everyday, experts_bytes: 0, tier_gib: 0 }
}

const fn sq(name: &'static str, file: &'static str, gb_hundredths: u64, experts_tenths: u64, tier_gib: u64) -> Quant {
    Quant { name, file, bytes: gb_hundredths * 10_000_000, everyday: true, experts_bytes: experts_tenths * 100_000_000, tier_gib }
}

/// A model Po can recommend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Card {
    pub id: &'static str,
    pub name: &'static str,
    pub engine: Engine,
    pub repo: &'static str,
    /// Mixture of experts: its experts can live in RAM at little cost.
    pub moe: bool,
    /// Run with Po and checked by hand on a real machine.
    pub tested: bool,
    /// Billions of parameters (total): a dense model above ~20B is too slow from the CPU.
    pub billions: u32,
    /// KV cache and compute buffers for a 32K window, on top of the weights.
    pub overhead_mib: u64,
    /// Every quantization offered, best quality first.
    pub quants: &'static [Quant],
    /// Key of the one-line description.
    pub about_key: &'static str,
}

impl Card {
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}", self.repo)
    }

    /// The `llama-server` line that downloads and serves it (`-hf repo:quant`).
    pub fn hf_command(&self, quant: &Quant) -> String {
        format!("llama-server -hf {}:{}", self.repo, quant.name)
    }
}

pub const QWEN38_27B: Card = Card {
    id: "qwen3.8-27b",
    name: "Qwen3.8-27B",
    engine: Engine::LlamaCpp,
    repo: "unsloth/Qwen3.8-27B-GGUF",
    moe: false,
    tested: true,
    billions: 27,
    overhead_mib: 2048,
    quants: &[
        q("UD-Q6_K_XL", "Qwen3.8-27B-UD-Q6_K_XL.gguf", 2530, false),
        q("UD-Q5_K_XL", "Qwen3.8-27B-UD-Q5_K_XL.gguf", 2088, false),
        q("UD-Q4_K_XL", "Qwen3.8-27B-UD-Q4_K_XL.gguf", 1756, true),
        q("UD-IQ4_XS", "Qwen3.8-27B-UD-IQ4_XS.gguf", 1425, true),
        q("UD-Q3_K_XL", "Qwen3.8-27B-UD-Q3_K_XL.gguf", 1315, true),
        q("UD-IQ3_XXS", "Qwen3.8-27B-UD-IQ3_XXS.gguf", 1093, true),
        q("UD-Q2_K_XL", "Qwen3.8-27B-UD-Q2_K_XL.gguf", 983, true),
        q("UD-IQ2_XXS", "Qwen3.8-27B-UD-IQ2_XXS.gguf", 727, true),
    ],
    about_key: "local.reco.about.qwen38_27b",
};

pub const QWEN36_35B_A3B: Card = Card {
    id: "qwen3.6-35b-a3b",
    name: "Qwen3.6-35B-A3B",
    engine: Engine::LlamaCpp,
    repo: "unsloth/Qwen3.6-35B-A3B-GGUF",
    moe: true,
    tested: true,
    billions: 35,
    overhead_mib: 1536,
    quants: &[
        q("Q8_0", "Qwen3.6-35B-A3B-Q8_0.gguf", 3690, false),
        q("UD-Q6_K_XL", "Qwen3.6-35B-A3B-UD-Q6_K_XL.gguf", 3184, false),
        q("UD-Q5_K_XL", "Qwen3.6-35B-A3B-UD-Q5_K_XL.gguf", 2659, false),
        q("UD-Q4_K_XL", "Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf", 2236, true),
        q("UD-IQ4_XS", "Qwen3.6-35B-A3B-UD-IQ4_XS.gguf", 1773, true),
        q("UD-Q3_K_XL", "Qwen3.6-35B-A3B-UD-Q3_K_XL.gguf", 1685, true),
        q("UD-IQ3_XXS", "Qwen3.6-35B-A3B-UD-IQ3_XXS.gguf", 1321, true),
        q("UD-Q2_K_XL", "Qwen3.6-35B-A3B-UD-Q2_K_XL.gguf", 1229, true),
    ],
    about_key: "local.reco.about.qwen36_35b",
};

pub const GEMMA4_12B: Card = Card {
    id: "gemma-4-12b",
    name: "Gemma-4-12B",
    engine: Engine::LlamaCpp,
    repo: "unsloth/gemma-4-12b-it-GGUF",
    moe: false,
    tested: false,
    billions: 12,
    overhead_mib: 1536,
    quants: &[
        q("Q8_0", "gemma-4-12b-it-Q8_0.gguf", 1267, false),
        q("UD-Q6_K_XL", "gemma-4-12b-it-UD-Q6_K_XL.gguf", 1069, false),
        q("Q5_K_M", "gemma-4-12b-it-Q5_K_M.gguf", 841, false),
        q("UD-Q4_K_XL", "gemma-4-12b-it-UD-Q4_K_XL.gguf", 737, true),
        q("Q4_K_M", "gemma-4-12b-it-Q4_K_M.gguf", 712, true),
        q("IQ4_XS", "gemma-4-12b-it-IQ4_XS.gguf", 638, true),
        q("UD-Q3_K_XL", "gemma-4-12b-it-UD-Q3_K_XL.gguf", 602, true),
        q("UD-IQ3_XXS", "gemma-4-12b-it-UD-IQ3_XXS.gguf", 464, true),
    ],
    about_key: "local.reco.about.gemma4_12b",
};

/// Qwen3.8-Flash-Next for Strata: ISTA-DASLab's GSQ-RCO files. Sizes of shard 1 (the memory
/// Strata's tiers count); shard 2, a 28.8 GB table, stays on the disk.
pub const FLASH_NEXT: Card = Card {
    id: "qwen3.8-flash-next",
    name: "Qwen3.8-Flash-Next",
    engine: Engine::Strata,
    repo: "ISTA-DASLab/Qwen3.8-Flash-Next-GSQ-RCO-GGUF",
    moe: true,
    tested: true,
    billions: 0,
    overhead_mib: 0,
    quants: &[
        sq("IQ3_S", "Qwen3.8-Flash-Next-GSQ-RCO-IQ3_S-00001-of-00002.gguf", 5482, 500, 63),
        sq("IQ3_XXS", "Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf", 4704, 430, 47),
        sq("IQ2_XS", "Qwen3.8-Flash-Next-GSQ-RCO-IQ2_XS-00001-of-00002.gguf", 3923, 355, 39),
        sq("Q2_0", "Qwen3.8-Flash-Next-GSQ-RCO-Q2_0-00001-of-00002.gguf", 3762, 340, 37),
    ],
    about_key: "local.reco.about.flash_next",
};

/// The Coder: half the experts, kept for code. For the PCs the full model does not fit.
pub const FLASH_NEXT_CODER: Card = Card {
    id: "qwen3.8-flash-next-coder",
    name: "Qwen3.8-Flash-Next Coder",
    engine: Engine::Strata,
    repo: "ISTA-DASLab/Qwen3.8-Flash-Next-GSQ-RCO-Coder-GGUF",
    moe: true,
    tested: true,
    billions: 0,
    overhead_mib: 0,
    quants: &[sq("IQ1_M", "Qwen3.8-Flash-Next-GSQ-RCO-IQ1_M-00001-of-00002.gguf", 2961, 230, 31)],
    about_key: "local.reco.about.flash_next_coder",
};

pub const LLAMA_CARDS: [&Card; 3] = [&QWEN38_27B, &QWEN36_35B_A3B, &GEMMA4_12B];
pub const STRATA_CARDS: [&Card; 2] = [&FLASH_NEXT, &FLASH_NEXT_CODER];

/// Least memory for a card to take a share of a model (as the launch plan counts it).
const MIN_SHARE_MIB: u64 = 2048;

/// What the machine has, for recommendations: capacity, not the current occupation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capacity {
    /// The largest card's memory.
    pub vram_mib: u64,
    /// Memory of every card of the largest one's kind, when there are several: a llama.cpp
    /// model can be spread over them.
    pub pooled_vram_mib: u64,
    /// How many cards make up `pooled_vram_mib` (0 or 1: no spreading).
    pub pooled_count: usize,
    pub gpu_name: Option<String>,
    pub backend: Option<GpuBackend>,
    pub gpu_count: usize,
    pub ram_mib: u64,
    /// Apple Silicon: the card's memory *is* the RAM.
    pub unified: bool,
}

impl Capacity {
    pub fn of(hardware: &Hardware) -> Self {
        let largest = hardware.gpus.iter().max_by_key(|g| g.vram_total_mib);
        let same_kind: Vec<u64> = match largest {
            Some(l) if l.backend != GpuBackend::Metal => {
                hardware.gpus.iter().filter(|g| g.backend == l.backend && g.vram_total_mib >= MIN_SHARE_MIB).map(|g| g.vram_total_mib).collect()
            }
            _ => Vec::new(),
        };
        Self {
            vram_mib: largest.map(|g| g.vram_total_mib).unwrap_or(0),
            pooled_vram_mib: if same_kind.len() >= 2 { same_kind.iter().sum() } else { 0 },
            pooled_count: if same_kind.len() >= 2 { same_kind.len() } else { 0 },
            gpu_name: largest.map(|g| g.name.clone()),
            backend: largest.map(|g| g.backend),
            gpu_count: hardware.gpus.len(),
            ram_mib: hardware.ram_total_mib,
            unified: largest.is_some_and(|g| g.backend == GpuBackend::Metal),
        }
    }

    fn vram_budget(&self) -> u64 {
        self.vram_mib.saturating_sub(DISPLAY_RESERVE_MIB)
    }

    /// Room on all the cards together, each keeping its display reserve.
    fn pooled_budget(&self) -> u64 {
        self.pooled_vram_mib.saturating_sub(DISPLAY_RESERVE_MIB * self.pooled_count as u64)
    }

    /// RAM a model can take beside the card: none more on unified memory, already counted.
    fn ram_budget(&self) -> u64 {
        if self.unified { 0 } else { self.ram_mib.saturating_sub(SYSTEM_RAM_MIB) }
    }
}

/// Where a model would run, and how well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Entirely in the card's memory: the fastest.
    Gpu,
    /// Entirely in the memory of several cards together: nearly as fast, a better size than one
    /// card holds.
    Gpus,
    /// A mixture of experts with its experts in RAM, the rest on the card: fast enough.
    ExpertsInRam,
    /// Part of the layers on the CPU: works, clearly slower.
    Partial,
    /// No usable card: on the processor alone, slow.
    Cpu,
    /// Fits in memory but too slow to be useful (a big dense model on the CPU).
    TooSlow,
    /// Does not fit in this machine's memory.
    TooBig,
    /// This engine needs a graphics card.
    NeedsGpu,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gpu => po_i18n::t!("local.reco.fit.gpu"),
            Self::Gpus => po_i18n::t!("local.reco.fit.gpus"),
            Self::ExpertsInRam => po_i18n::t!("local.reco.fit.experts"),
            Self::Partial => po_i18n::t!("local.reco.fit.partial"),
            Self::Cpu => po_i18n::t!("local.reco.fit.cpu"),
            Self::TooSlow => po_i18n::t!("local.reco.fit.too_slow"),
            Self::TooBig => po_i18n::t!("local.reco.fit.too_big"),
            Self::NeedsGpu => po_i18n::t!("local.reco.fit.needs_gpu"),
        }
    }

    /// Worth offering to a beginner.
    pub fn usable(self) -> bool {
        matches!(self, Self::Gpu | Self::Gpus | Self::ExpertsInRam | Self::Partial | Self::Cpu)
    }
}

/// A recommendation: the card, the quantization chosen for this machine, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Advice {
    pub card: &'static Card,
    pub quant: &'static Quant,
    pub fit: Fit,
    /// Names of the files of this model already on the machine.
    pub installed: Vec<String>,
    /// The chosen quantization itself is on the machine.
    pub chosen_installed: bool,
}

impl Advice {
    /// One sentence that says why this quantization, in the user's words.
    pub fn reason(&self, capacity: &Capacity) -> String {
        let gb = |bytes: u64| format!("{:.1}", bytes as f64 / 1e9);
        let card = capacity.gpu_name.clone().unwrap_or_default();
        let vram = format!("{:.0}", capacity.vram_mib as f64 / 1024.0);
        let ram = format!("{:.0}", capacity.ram_mib as f64 / 1024.0);
        let size = gb(self.quant.bytes);
        match self.fit {
            Fit::Gpu => po_i18n::t!("local.reco.why.gpu", size = size, card = card, vram = vram),
            Fit::Gpus => po_i18n::t!(
                "local.reco.why.gpus",
                size = size,
                n = capacity.pooled_count,
                vram = format!("{:.0}", capacity.pooled_vram_mib as f64 / 1024.0)
            ),
            Fit::ExpertsInRam if self.card.engine == Engine::Strata => po_i18n::t!("local.reco.why.strata", size = size, ram = ram, vram = vram),
            Fit::ExpertsInRam => po_i18n::t!("local.reco.why.experts", size = size, vram = vram, ram = ram),
            Fit::Partial if self.card.engine == Engine::Strata => po_i18n::t!("local.reco.why.strata_low_ram", size = size, ram = ram),
            Fit::Partial => po_i18n::t!("local.reco.why.partial", size = size, vram = vram),
            Fit::Cpu => po_i18n::t!("local.reco.why.cpu", size = size, ram = ram),
            Fit::TooSlow => po_i18n::t!("local.reco.why.too_slow", ram = ram),
            Fit::TooBig => po_i18n::t!("local.reco.why.too_big", size = size, ram = ram, vram = vram),
            Fit::NeedsGpu => po_i18n::t!("local.reco.why.needs_gpu").to_owned(),
        }
    }
}

fn mib(bytes: u64) -> u64 {
    bytes.div_ceil(MIB)
}

/// A size that visibly loses quality: 1 to 3 bits per weight.
fn below_four_bits(quant: &Quant) -> bool {
    let name = quant.name.to_ascii_uppercase();
    ["Q1", "Q2", "Q3"].iter().any(|bits| name.contains(bits))
}

/// The best quantization of a llama.cpp model for this capacity.
pub fn advise_llama(card: &'static Card, capacity: &Capacity) -> (&'static Quant, Fit) {
    let vram = capacity.vram_budget();
    let ram = capacity.ram_budget();
    let need = |q: &Quant| mib(q.bytes) + card.overhead_mib;
    // Entirely on the card, best quality first - unless one card only holds a size under 4 bits
    // and several together hold a better one: spread, a 27B goes from a size that loses quality
    // to one that keeps it. A model already well kept on one card stays there: spreading is a
    // little slower, for a gain nobody would notice.
    let alone = if vram > 0 { card.quants.iter().position(|q| need(q) <= vram) } else { None };
    let pooled = capacity.pooled_budget();
    let together = if pooled > vram {
        card.quants.iter().position(|q| need(q) + card.overhead_mib * capacity.pooled_count.saturating_sub(1) as u64 <= pooled)
    } else {
        None
    };
    match (alone, together) {
        (Some(a), Some(t)) if t < a && below_four_bits(&card.quants[a]) => return (&card.quants[t], Fit::Gpus),
        (Some(a), _) => return (&card.quants[a], Fit::Gpu),
        // A mixture of experts runs well with its experts in RAM: spread over the cards only if
        // that keeps 4 bits, rather than dropping to 3 to stay on them.
        (None, Some(t)) if !card.moe || !below_four_bits(&card.quants[t]) => return (&card.quants[t], Fit::Gpus),
        _ => {}
    }
    // Shared with the RAM: only the everyday sizes are worth the slowdown.
    let shared = card.quants.iter().filter(|q| q.everyday).find(|q| need(q) <= vram + ram);
    let smallest = card.quants.last().expect("a card has quantizations");
    match shared {
        Some(quant) if vram > 0 && card.moe => (quant, Fit::ExpertsInRam),
        Some(quant) if vram > 0 => (quant, Fit::Partial),
        // On the processor alone, a dense model of more than 20 billion parameters writes a word
        // every few seconds: it fits, but nobody can work with it.
        Some(quant) if !card.moe && card.billions > 20 => (quant, Fit::TooSlow),
        Some(quant) => (quant, Fit::Cpu),
        None => (smallest, Fit::TooBig),
    }
}

/// The best quantization of a Strata model, by its own rules: the tier (RAM + VRAM − 5 GB) and
/// the experts held in RAM with 10 GB to spare. Under the RAM rule its low-RAM mode starts by
/// itself: it works, much slower.
pub fn advise_strata(card: &'static Card, capacity: &Capacity) -> (&'static Quant, Fit) {
    let smallest = card.quants.last().expect("a card has quantizations");
    if capacity.vram_mib == 0 || capacity.backend == Some(GpuBackend::Vulkan) {
        return (smallest, Fit::NeedsGpu);
    }
    // Rounded: a "12 GB" card reports 12,227 MiB, which Strata counts as 12.
    let gib = |m: u64| (m + 512) / 1024;
    let memory_gib = if capacity.unified { gib(capacity.ram_mib) } else { gib(capacity.ram_mib) + gib(capacity.vram_mib) }.saturating_sub(5);
    let ram_bytes = capacity.ram_mib * MIB;
    let tiered = card.quants.iter().find(|q| memory_gib >= q.tier_gib);
    match tiered {
        Some(quant) if capacity.unified || quant.experts_bytes + 10_000_000_000 <= ram_bytes => (quant, Fit::ExpertsInRam),
        Some(quant) => (quant, Fit::Partial),
        None => (smallest, Fit::TooBig),
    }
}

/// Every recommendation for this machine, given the model files already on it (file names).
pub fn advise(capacity: &Capacity, installed_files: &[String], with_strata: bool) -> Vec<Advice> {
    let mut cards: Vec<&'static Card> = LLAMA_CARDS.to_vec();
    if with_strata {
        cards.extend(STRATA_CARDS);
    }
    cards
        .into_iter()
        .map(|card| {
            let (quant, fit) = match card.engine {
                Engine::LlamaCpp => advise_llama(card, capacity),
                Engine::Strata => advise_strata(card, capacity),
            };
            let installed: Vec<String> = installed_files.iter().filter(|f| belongs_to(card, f)).cloned().collect();
            let chosen_installed = installed.iter().any(|f| f.eq_ignore_ascii_case(quant.file));
            Advice { card, quant, fit, installed, chosen_installed }
        })
        .collect()
}

/// Is this file one of the card's model (any quantization)?
fn belongs_to(card: &Card, file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    if card.quants.iter().any(|q| q.file.eq_ignore_ascii_case(file)) {
        return true;
    }
    // Another publisher's file of the same model: same name, a quantization we do not list.
    let stem = card.quants[0].file.to_ascii_lowercase();
    let family = match card.engine {
        Engine::Strata => stem.split("-gsq-rco-").next().unwrap_or(&stem).to_owned() + "-gsq-rco-",
        Engine::LlamaCpp => card.name.to_ascii_lowercase() + "-",
    };
    lower.starts_with(&family) && !lower.starts_with("mmproj") && !(card.id == FLASH_NEXT.id && lower.contains("-iq1_m-"))
}

/// How an installed Strata model fits, from its shard 1 and its quantization.
pub fn strata_config_fit(capacity: &Capacity, shard1_bytes: u64, quant: Option<&str>) -> Fit {
    if capacity.vram_mib == 0 {
        return Fit::NeedsGpu;
    }
    let known = STRATA_CARDS.iter().flat_map(|c| c.quants.iter()).find(|q| quant.is_some_and(|n| n.eq_ignore_ascii_case(q.name)));
    // Unknown size: shard 1 minus the dense part the card takes (~4.5 GB in every size).
    let experts = known.map(|q| q.experts_bytes).unwrap_or(shard1_bytes.saturating_sub(4_500_000_000));
    let memory = if capacity.unified { capacity.ram_mib } else { capacity.ram_mib + capacity.vram_mib } * MIB;
    if shard1_bytes > memory.saturating_sub(5 * 1024 * MIB) {
        return Fit::TooBig;
    }
    if capacity.unified || experts + 10_000_000_000 <= capacity.ram_mib * MIB { Fit::ExpertsInRam } else { Fit::Partial }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::Gpu;

    fn gpu(name: &str, total: u64, free: u64, index: u32) -> Gpu {
        Gpu { name: name.into(), backend: GpuBackend::Cuda, vram_total_mib: total, vram_free_mib: free, index: Some(index) }
    }

    /// The development machine: an RTX 3070 Ti and an RTX 4070 SUPER, 64 GB of RAM, with Strata
    /// holding the 4070 SUPER (real `nvidia-smi` figures).
    fn dev_machine() -> Hardware {
        Hardware {
            gpus: vec![gpu("NVIDIA GeForce RTX 3070 Ti", 8192, 5777, 0), gpu("NVIDIA GeForce RTX 4070 SUPER", 12282, 471, 1)],
            ram_total_mib: 65_430,
            ram_free_mib: 5_000,
            cpu_threads: 24,
        }
    }

    #[test]
    fn two_cards_together_earn_a_better_size_of_a_dense_model() {
        let one = Capacity::of(&Hardware { gpus: vec![gpu("RTX 4070 SUPER", 12_282, 12_000, 0)], ..dev_machine() });
        let two = Capacity::of(&dev_machine());
        assert_eq!(two.pooled_count, 2);
        let (alone, fit_alone) = advise_llama(&QWEN38_27B, &one);
        let (together, fit_together) = advise_llama(&QWEN38_27B, &two);
        assert_eq!(fit_alone, Fit::Gpu);
        assert_eq!(fit_together, Fit::Gpus, "{} vs {}", alone.name, together.name);
        assert!(together.bytes > alone.bytes, "a larger, truer size: {} vs {}", together.name, alone.name);
        assert!(!Advice { card: &QWEN38_27B, quant: together, fit: fit_together, installed: vec![], chosen_installed: false }.reason(&two).is_empty());
    }

    #[test]
    fn one_card_or_cards_of_different_kinds_pool_nothing() {
        assert_eq!(Capacity::of(&one_8gb_card()).pooled_count, 0);
        let mixed = Hardware {
            gpus: vec![gpu("RTX 4070 SUPER", 12_282, 12_000, 0), Gpu { backend: GpuBackend::Vulkan, ..gpu("Radeon", 8192, 8000, 1) }],
            ..dev_machine()
        };
        assert_eq!(Capacity::of(&mixed).pooled_count, 0);
        assert_eq!(Capacity::of(&mac(32_768)).pooled_count, 0);
    }

    fn one_8gb_card() -> Hardware {
        Hardware { gpus: vec![gpu("RTX 4060", 8188, 7500, 0)], ram_total_mib: 32_768, ram_free_mib: 20_000, cpu_threads: 16 }
    }

    fn cpu_only_16gb() -> Hardware {
        Hardware { gpus: vec![], ram_total_mib: 16_384, ram_free_mib: 10_000, cpu_threads: 8 }
    }

    fn mac(unified_mib: u64) -> Hardware {
        let budget = unified_mib * 3 / 4;
        Hardware {
            gpus: vec![Gpu { name: "Apple Silicon".into(), backend: GpuBackend::Metal, vram_total_mib: budget, vram_free_mib: budget, index: None }],
            ram_total_mib: unified_mib,
            ram_free_mib: unified_mib / 2,
            cpu_threads: 10,
        }
    }

    fn pick(hardware: &Hardware, card: &'static Card) -> (&'static str, Fit) {
        let capacity = Capacity::of(hardware);
        let (quant, fit) = match card.engine {
            Engine::LlamaCpp => advise_llama(card, &capacity),
            Engine::Strata => advise_strata(card, &capacity),
        };
        (quant.name, fit)
    }

    #[test]
    fn the_largest_card_counts_not_the_freest() {
        // Strata occupies the 4070 SUPER: the plan of the moment uses the 3070 Ti, but the machine
        // *can* host a 12 GB model, and that is what a recommendation is about.
        let capacity = Capacity::of(&dev_machine());
        assert_eq!(capacity.vram_mib, 12_282);
        assert_eq!(capacity.gpu_count, 2);
        assert!(capacity.gpu_name.unwrap().contains("4070 SUPER"));
    }

    #[test]
    fn the_development_machine() {
        let machine = dev_machine();
        // 12 GB card: the 35B-A3B in 4-bit with its experts in RAM,
        // Gemma 12B in 5-bit on the card, and Strata's best size.
        // 12 + 8 GB together: the 27B spread over both in 4 bits, rather than 2 bits on one card.
        assert_eq!(pick(&machine, &QWEN38_27B), ("UD-IQ4_XS", Fit::Gpus));
        assert_eq!(pick(&machine, &QWEN36_35B_A3B), ("UD-Q4_K_XL", Fit::ExpertsInRam));
        assert_eq!(pick(&machine, &GEMMA4_12B), ("Q5_K_M", Fit::Gpu));
        assert_eq!(pick(&machine, &FLASH_NEXT), ("IQ3_S", Fit::ExpertsInRam));
        assert_eq!(pick(&machine, &FLASH_NEXT_CODER), ("IQ1_M", Fit::ExpertsInRam));
    }

    #[test]
    fn one_card_of_eight_gigabytes() {
        let machine = one_8gb_card();
        assert_eq!(pick(&machine, &QWEN38_27B), ("UD-Q4_K_XL", Fit::Partial), "too big for the card: shared, and said slower");
        assert_eq!(pick(&machine, &QWEN36_35B_A3B), ("UD-Q4_K_XL", Fit::ExpertsInRam));
        assert_eq!(pick(&machine, &GEMMA4_12B), ("UD-Q3_K_XL", Fit::Gpu));
        // 32 + 8 − 5 = 35 GiB: the full model does not reach its first tier, the Coder does, and
        // its 23 GB of experts fit in 32 GB of RAM with 10 to spare.
        assert_eq!(pick(&machine, &FLASH_NEXT).1, Fit::TooBig);
        assert_eq!(pick(&machine, &FLASH_NEXT_CODER), ("IQ1_M", Fit::ExpertsInRam));
    }

    #[test]
    fn a_twelve_gigabyte_card_and_thirty_two_of_ram_is_strata_s_forty_tier() {
        // Strata's own example (MODELS.md): 32 + 12 − 5 = 39, the 40 GB tier, IQ2_XS - but its
        // 35.5 GB of experts do not fit 32 GB of RAM with room to spare: the low-RAM mode.
        let machine = Hardware { gpus: vec![gpu("RTX 5070", 12_227, 11_000, 0)], ram_total_mib: 32_768, ram_free_mib: 20_000, cpu_threads: 12 };
        assert_eq!(pick(&machine, &FLASH_NEXT), ("IQ2_XS", Fit::Partial));
    }

    #[test]
    fn a_machine_without_a_card() {
        let machine = cpu_only_16gb();
        assert_eq!(pick(&machine, &QWEN38_27B).1, Fit::TooSlow, "a 27B dense on the CPU is not offered as usable");
        assert_eq!(pick(&machine, &QWEN36_35B_A3B).1, Fit::TooBig, "12 GB at the smallest, 10 GB of RAM to give");
        assert_eq!(pick(&machine, &GEMMA4_12B), ("UD-Q4_K_XL", Fit::Cpu));
        assert_eq!(pick(&machine, &FLASH_NEXT).1, Fit::NeedsGpu);
        assert!(!Fit::TooSlow.usable() && Fit::Cpu.usable());
    }

    #[test]
    fn an_apple_silicon_mac_counts_its_memory_once() {
        let machine = mac(32_768);
        assert_eq!(pick(&machine, &QWEN38_27B), ("UD-Q5_K_XL", Fit::Gpu));
        assert_eq!(pick(&machine, &QWEN36_35B_A3B), ("UD-Q4_K_XL", Fit::Gpu));
        assert_eq!(pick(&machine, &GEMMA4_12B), ("Q8_0", Fit::Gpu));
        // 32 GB − 5: under the Coder's tier.
        assert_eq!(pick(&machine, &FLASH_NEXT_CODER).1, Fit::TooBig);
        let big = mac(65_536);
        assert_eq!(pick(&big, &FLASH_NEXT).1, Fit::ExpertsInRam, "unified memory: no RAM rule apart");
    }

    #[test]
    fn more_memory_never_gives_a_worse_quantization() {
        for card in LLAMA_CARDS {
            let mut previous = usize::MAX;
            for vram in [0u64, 4096, 6144, 8192, 12_288, 16_384, 24_576, 49_152] {
                let machine = Hardware { gpus: if vram > 0 { vec![gpu("x", vram, vram, 0)] } else { vec![] }, ram_total_mib: 32_768, ram_free_mib: 16_000, cpu_threads: 8 };
                let (quant, fit) = advise_llama(card, &Capacity::of(&machine));
                let rank = card.quants.iter().position(|q| q == quant).unwrap();
                if fit == Fit::Gpu {
                    assert!(rank <= previous, "{}: {vram} MiB gave {}, worse than with less memory", card.name, quant.name);
                    previous = rank;
                }
            }
        }
    }

    #[test]
    fn installed_files_are_recognised() {
        let capacity = Capacity::of(&dev_machine());
        let files = vec![
            "Qwen3.8-27B-UD-IQ2_XXS.gguf".to_owned(),
            "Qwen3.8-Flash-Next-GSQ-RCO-IQ3_XXS-00001-of-00002.gguf".to_owned(),
            "Qwen3.8-Flash-Next-GSQ-RCO-IQ1_M-00001-of-00002.gguf".to_owned(),
            "Qwen3.5-9B-Q4_K_M.gguf".to_owned(),
            "mmproj-BF16.gguf".to_owned(),
        ];
        let advice = advise(&capacity, &files, true);
        let of = |id: &str| advice.iter().find(|a| a.card.id == id).unwrap();
        assert_eq!(of("qwen3.8-27b").installed, vec!["Qwen3.8-27B-UD-IQ2_XXS.gguf".to_owned()]);
        assert!(!of("qwen3.8-27b").chosen_installed, "another quantization is installed, not the one advised");
        assert_eq!(of("qwen3.8-flash-next").installed.len(), 1, "the Coder's file is not the full model's");
        assert!(of("qwen3.8-flash-next-coder").chosen_installed);
        assert!(of("gemma-4-12b").installed.is_empty());
        assert_eq!(advise(&capacity, &files, false).len(), 3, "without Strata, no Strata model is recommended");
    }

    #[test]
    fn an_installed_strata_model_is_judged_by_its_own_rules() {
        let capacity = Capacity::of(&dev_machine());
        assert_eq!(strata_config_fit(&capacity, 47_040_000_000, Some("IQ3_XXS")), Fit::ExpertsInRam);
        let small = Capacity::of(&one_8gb_card());
        assert_eq!(strata_config_fit(&small, 47_040_000_000, Some("IQ3_XXS")), Fit::TooBig);
        assert_eq!(strata_config_fit(&Capacity::of(&cpu_only_16gb()), 1, None), Fit::NeedsGpu);
        assert_eq!(strata_config_fit(&small, 29_610_000_000, Some("IQ1_M")), Fit::ExpertsInRam, "the Coder on 32 GB of RAM");
        // An unknown size is judged from its shard, cautiously.
        assert_eq!(strata_config_fit(&small, 29_610_000_000, None), Fit::Partial);
    }

    #[test]
    fn every_card_is_consistent() {
        for card in LLAMA_CARDS.iter().chain(STRATA_CARDS.iter()) {
            assert!(!card.quants.is_empty());
            for pair in card.quants.windows(2) {
                assert!(pair[0].bytes > pair[1].bytes, "{}: best quality, so the biggest, first", card.name);
            }
            for quant in card.quants {
                assert_eq!(crate::quant::from_name(quant.file).as_deref(), Some(quant.name), "{}", quant.file);
            }
            assert!(card.url().starts_with("https://huggingface.co/"));
        }
        assert_eq!(QWEN36_35B_A3B.hf_command(&QWEN36_35B_A3B.quants[3]), "llama-server -hf unsloth/Qwen3.6-35B-A3B-GGUF:UD-Q4_K_XL");
    }

    #[test]
    fn every_reason_reads() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        for hardware in [dev_machine(), one_8gb_card(), cpu_only_16gb(), mac(32_768)] {
            let capacity = Capacity::of(&hardware);
            for advice in advise(&capacity, &[], true) {
                let reason = advice.reason(&capacity);
                assert!(!reason.is_empty() && !reason.contains('{'), "{reason}");
                assert!(!advice.fit.label().is_empty());
            }
        }
    }
}
