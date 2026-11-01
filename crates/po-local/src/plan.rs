//! Derivation of a local server's launch parameters.
//!
//! This is where the difference between "Po can talk to a local server" and "Po can run a local
//! model" is made. From the model's geometry and the memory that is really free, we decide how
//! many layers go to the GPU, which context window is sustainable and whether the cache must be
//! quantized.
//!
//! Three principles:
//! - we size on **free** memory, never on nominal capacity;
//! - we **say** what we had to sacrifice, instead of doing it silently;
//! - we never propose a plan we know will fail at load time.

use std::fmt;

use crate::gguf::GgufInfo;
use crate::hardware::{GpuBackend, Hardware};
use crate::placement::{Draft, LlamaPlacement, Placement};

/// Margin left to the display system and the driver on the GPU.
const DISPLAY_RESERVE_MIB: u64 = 768;
/// llama.cpp compute buffers beyond weights and KV cache: graph, activations, batches.
const COMPUTE_OVERHEAD_MIB: u64 = 512;
/// Below this, a context window no longer allows useful agentic work.
const MIN_USABLE_CONTEXT: u32 = 4096;
/// Least room for a card to take a share of a model: below, its compute buffers and a layer or
/// two are all it holds, and the traffic between cards costs more than it brings.
const MIN_SHARE_MIB: u64 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheType {
    /// Native cache precision: no loss, two bytes per element.
    F16,
    /// 8-bit quantized cache: half the memory, negligible degradation in practice.
    Q8,
}

impl CacheType {
    pub fn bytes_per_element(self) -> u64 {
        match self {
            Self::F16 => 2,
            Self::Q8 => 1,
        }
    }

    pub fn flag_value(self) -> &'static str {
        match self {
            Self::F16 => "f16",
            Self::Q8 => "q8_0",
        }
    }
}

/// What the user asks for before arbitration.
#[derive(Debug, Clone)]
pub struct PlanRequest {
    /// Desired context; will be trimmed if it does not fit.
    pub desired_context: u32,
    /// Simultaneous processing slots — one per concurrent local sub-agent.
    pub parallel: u32,
    /// Allow cache quantization if it is the only way to fit.
    pub allow_cache_quant: bool,
}

impl Default for PlanRequest {
    fn default() -> Self {
        Self { desired_context: 32_768, parallel: 1, allow_cache_quant: true }
    }
}

/// What had to be sacrificed for the plan to fit. Shown to the user: a silently degraded plan
/// is paid for later in unexplained slowness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compromise {
    /// The requested context did not fit.
    ContextReduced { asked: u32, granted: u32 },
    /// The cache was quantized to free up room.
    CacheQuantized,
    /// Some layers stay on the CPU: throughput drops sharply.
    PartialOffload { on_gpu: u32, total: u32 },
    /// No layer on the GPU: fully CPU inference.
    CpuOnly,
    /// The request exceeds even the trained window: we align to it.
    ContextCappedByModel { trained: u32 },
    /// No memory measurement available: the plan could not be verified.
    ///
    /// We do not refuse for all that — the absence of a measurement is not proof of
    /// insufficiency — but the user must know that nothing guarantees the load.
    MemoryUnmeasured,
}

impl fmt::Display for Compromise {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use po_i18n::t;
        match self {
            Self::ContextReduced { asked, granted } => {
                write!(f, "{}", t!("local.plan.context_reduced", asked = asked, granted = granted))
            }
            Self::CacheQuantized => write!(f, "{}", t!("local.plan.cache_quantized")),
            Self::PartialOffload { on_gpu, total } => {
                write!(f, "{}", t!("local.plan.partial_offload", on_gpu = on_gpu, total = total))
            }
            Self::CpuOnly => write!(f, "{}", t!("local.plan.cpu_only")),
            Self::ContextCappedByModel { trained } => {
                write!(f, "{}", t!("local.plan.context_capped", trained = trained))
            }
            Self::MemoryUnmeasured => write!(f, "{}", t!("local.plan.memory_unmeasured")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LaunchPlan {
    pub n_gpu_layers: u32,
    pub context: u32,
    pub parallel: u32,
    pub threads: u32,
    pub flash_attn: bool,
    pub cache_type: CacheType,
    /// Expected memory footprint, in mebibytes.
    pub estimated_vram_mib: u64,
    pub estimated_ram_mib: u64,
    pub compromises: Vec<Compromise>,
    /// False when no sustainable configuration was found: the server must then not be launched.
    pub viable: bool,
    /// The cards the layers are spread over, by index with the MiB each can take; empty: one
    /// card, the freest.
    pub split: Vec<(u32, u64)>,
    /// The cards the user chose for it (`placement::Placement`): what the server sees and its
    /// device arguments. None: Po's own choice.
    pub placed: Option<LlamaPlacement>,
    /// The model's layers, and how many of the last ones are its draft head (MTP).
    pub block_count: u32,
    pub draft_layers: u32,
    /// The draft head guesses the next tokens (`--spec-type draft-mtp`): on by default for a
    /// model that has one — measured +26 % words per second on a 9B (70 → 88 tok/s).
    pub mtp: bool,
}

impl LaunchPlan {
    /// Arguments to pass to `llama-server`, in order.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "-ngl".into(),
            self.n_gpu_layers.to_string(),
            "-c".into(),
            self.context.to_string(),
            "-np".into(),
            self.parallel.to_string(),
            "-t".into(),
            self.threads.to_string(),
        ];
        if self.flash_attn {
            // Recent llama.cpp versions expect a value (`on`/`off`/`auto`): passing the bare
            // flag makes the server fail at startup with a usage message, with no other
            // explanation.
            args.push("--flash-attn".into());
            args.push("on".into());
        }
        let draft_only = self.placed.as_ref().is_some_and(|p| p.draft_only);
        if self.split.len() >= 2 || draft_only {
            // Shares in the cards' order once restricted to them (see `device_env`); a card
            // that holds only the draft head takes none of the layers.
            let mut shares: Vec<String> = if self.split.is_empty() {
                self.placed.iter().flat_map(|p| p.cards.iter()).map(|(_, mib)| (*mib).max(1).to_string()).collect()
            } else {
                self.split.iter().map(|(_, mib)| mib.to_string()).collect()
            };
            if draft_only {
                shares.push("0".into());
            }
            args.push("--split-mode".into());
            args.push("layer".into());
            args.push("--tensor-split".into());
            args.push(shares.join(","));
        }
        if self.cache_type != CacheType::F16 {
            args.push("--cache-type-k".into());
            args.push(self.cache_type.flag_value().into());
            args.push("--cache-type-v".into());
            args.push(self.cache_type.flag_value().into());
        }
        if let Some(placed) = &self.placed {
            args.push("--device".into());
            args.push(placed.devices.join(","));
        }
        if self.mtp && self.draft_layers > 0 {
            args.push("--spec-type".into());
            args.push("draft-mtp".into());
            if let Some(buffer) = self.placed.as_ref().and_then(|p| p.draft_buffer.as_ref()) {
                let first = self.block_count.saturating_sub(self.draft_layers);
                let layers: Vec<String> = (first..self.block_count).map(|l| l.to_string()).collect();
                args.push("--override-tensor".into());
                args.push(format!("blk\\.({})\\.={buffer}", layers.join("|")));
            }
        }
        args
    }

    /// The same plan for a server that does not know MTP: no draft head, nor its own card.
    pub fn without_mtp(&self) -> Self {
        let mut out = self.clone();
        out.mtp = false;
        out.placed = out.placed.map(|p| p.without_draft());
        out
    }

    /// Environment that shows the server the cards of this plan, and only those: the freest
    /// one alone, or every card the layers are spread over, in their bus order.
    pub fn device_env(&self, hardware: &Hardware) -> Vec<(String, String)> {
        if let Some(placed) = &self.placed {
            return placed.env.clone();
        }
        if self.split.len() < 2 {
            return hardware.device_pin();
        }
        let list = self.split.iter().map(|(index, _)| index.to_string()).collect::<Vec<_>>().join(",");
        let variable = match hardware.best_gpu().map(|g| g.backend) {
            Some(GpuBackend::Cuda) => "CUDA_VISIBLE_DEVICES",
            Some(GpuBackend::Rocm) => "HIP_VISIBLE_DEVICES",
            Some(GpuBackend::Vulkan) => "GGML_VK_VISIBLE_DEVICES",
            _ => return Vec::new(),
        };
        let mut env = vec![(variable.to_owned(), list)];
        if variable == "CUDA_VISIBLE_DEVICES" {
            env.insert(0, ("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()));
        }
        env
    }

    pub fn is_fully_offloaded(&self) -> bool {
        !self.compromises.iter().any(|c| {
            matches!(c, Compromise::PartialOffload { .. } | Compromise::CpuOnly)
        })
    }

    /// One-line summary for the status bar.
    pub fn summary(&self) -> String {
        if !self.viable {
            return po_i18n::t!("local.plan.no_viable_config").to_owned();
        }
        let placement = if self.n_gpu_layers == 0 {
            "CPU".to_owned()
        } else if self.is_fully_offloaded() && self.split.len() >= 2 {
            format!("GPU ×{}", self.split.len())
        } else if self.is_fully_offloaded() {
            "GPU".to_owned()
        } else {
            format!("GPU {}/{}", self.n_gpu_layers, self.n_gpu_layers + self.cpu_layers())
        };
        // Deliberately compact: this line appears in a list, next to the model's name and size.
        // Too verbose, it overflows the width of an ordinary terminal and the line wrap breaks
        // the alignment of the whole list.
        let vram = if self.estimated_vram_mib > 0 {
            po_i18n::t!("local.plan.summary_vram", mib = self.estimated_vram_mib)
        } else {
            String::new()
        };
        format!("{placement} · {} tok{vram}", self.context)
    }

    fn cpu_layers(&self) -> u32 {
        self.compromises
            .iter()
            .find_map(|c| match c {
                Compromise::PartialOffload { on_gpu, total } => Some(total - on_gpu),
                _ => None,
            })
            .unwrap_or(0)
    }
}

const MIB: u64 = 1024 * 1024;

/// Computes the best sustainable plan, or a plan marked non-viable.
///
/// On the freest card first. When a model does not fit there whole but several cards of the
/// same kind would hold more of it together, its layers are spread over them: a 27B in Q4
/// (16 GB) runs entirely on a 12 GB and an 8 GB card, where one alone left a third of it on the
/// processor. Splitting costs some speed between the cards, so it is only chosen when it gains.
pub fn plan(info: &GgufInfo, hardware: &Hardware, request: &PlanRequest) -> LaunchPlan {
    let single = plan_on(info, hardware, request, hardware.usable_vram_mib(DISPLAY_RESERVE_MIB), COMPUTE_OVERHEAD_MIB);
    let cards = shareable_cards(hardware);
    let wanted = if info.context_length > 0 { request.desired_context.min(info.context_length) } else { request.desired_context };
    if cards.len() < 2 || (single.is_fully_offloaded() && single.context >= wanted) {
        return single;
    }
    let pooled: u64 = cards.iter().map(|(_, mib)| mib).sum();
    let mut spread = plan_on(info, hardware, request, pooled, COMPUTE_OVERHEAD_MIB * cards.len() as u64);
    let better = spread.viable
        && (spread.n_gpu_layers > single.n_gpu_layers || (spread.n_gpu_layers == single.n_gpu_layers && spread.context > single.context));
    if !better {
        return single;
    }
    spread.split = cards;
    spread
}

/// A plan on the cards the user chose ([`Placement`]): their free memory together, the layers
/// split between them in the chosen order when there are several (the leading card last), the
/// draft head where it was put. An automatic placement is [`plan`]'s own choice.
pub fn plan_placed(info: &GgufInfo, hardware: &Hardware, request: &PlanRequest, placement: &Placement) -> LaunchPlan {
    let Some(placed) = placement.llama(hardware) else { return plan(info, hardware, request) };
    let cards: Vec<(u32, u64)> = placed.cards.iter().map(|(index, free)| (*index, free.saturating_sub(DISPLAY_RESERVE_MIB))).collect();
    let mut budget: u64 = cards.iter().map(|(_, mib)| mib).sum();
    // The draft head on one of the model's own cards takes its layers' room there.
    if let Some(Draft::Card(card)) = placement.draft
        && placement.gpus.contains(&card)
    {
        budget = budget.saturating_sub((info.bytes_per_layer() / MIB).max(1) * u64::from(info.draft_layers.max(1)));
    }
    let mut plan = plan_on(info, hardware, request, budget, COMPUTE_OVERHEAD_MIB * cards.len().max(1) as u64);
    if cards.len() >= 2 {
        plan.split = cards;
    }
    plan.placed = Some(placed);
    plan
}

/// Cards that can take a share of a model: the same kind as the freest one (CUDA, ROCm or
/// Vulkan — Metal is a single unified memory), with an index to name them, and enough room left
/// once the display's reserve is set aside. A card another server already fills is left out by
/// itself: its free memory is what is measured.
fn shareable_cards(hardware: &Hardware) -> Vec<(u32, u64)> {
    let Some(best) = hardware.best_gpu() else { return Vec::new() };
    if best.backend == GpuBackend::Metal {
        return Vec::new();
    }
    let mut cards: Vec<(u32, u64)> = hardware
        .gpus
        .iter()
        .filter(|g| g.backend == best.backend)
        .filter_map(|g| Some((g.index?, g.vram_free_mib.saturating_sub(DISPLAY_RESERVE_MIB))))
        .filter(|(_, usable)| *usable >= MIN_SHARE_MIB)
        .collect();
    cards.sort_by_key(|(index, _)| *index);
    cards
}

/// A plan for `vram_budget` MiB of card memory, `overhead_mib` of which go to the compute
/// buffers (one set per card used).
fn plan_on(info: &GgufInfo, hardware: &Hardware, request: &PlanRequest, vram_budget: u64, overhead_mib: u64) -> LaunchPlan {
    let mut compromises = Vec::new();
    let parallel = request.parallel.max(1);
    let threads = hardware.inference_threads();

    // Without usable geometry no computation is honest: we say so rather than invent one.
    if !info.is_usable() {
        return LaunchPlan {
            n_gpu_layers: 0,
            context: MIN_USABLE_CONTEXT,
            parallel,
            threads,
            flash_attn: false,
            cache_type: CacheType::F16,
            estimated_vram_mib: 0,
            estimated_ram_mib: 0,
            compromises: vec![Compromise::CpuOnly],
            viable: false,
            split: Vec::new(),
            placed: None,
            block_count: info.block_count,
            draft_layers: info.draft_layers,
            mtp: info.draft_layers > 0,
        };
    }

    // 1. The context cannot exceed the model's training window.
    let mut context = request.desired_context;
    if info.context_length > 0 && context > info.context_length {
        compromises.push(Compromise::ContextCappedByModel { trained: info.context_length });
        context = info.context_length;
    }

    let weights_mib = info.file_size_bytes / MIB;
    let flash_attn = matches!(
        hardware.best_gpu().map(|gpu| gpu.backend),
        Some(GpuBackend::Cuda | GpuBackend::Metal)
    );

    // 2. We look for the best context/cache combination, from the most generous to the most
    //    frugal. The order expresses a priority: a quantized cache beats a truncated context,
    //    because the former's quality loss is marginal while the latter's is structural.
    let bytes_per_layer_mib = (info.bytes_per_layer() / MIB).max(1);
    let mut cache_type = CacheType::F16;
    let mut kv_mib = kv_cache_mib(info, context, parallel, cache_type);

    if vram_budget > 0 && weights_mib + kv_mib + overhead_mib > vram_budget && request.allow_cache_quant {
        cache_type = CacheType::Q8;
        kv_mib = kv_cache_mib(info, context, parallel, cache_type);
        compromises.push(Compromise::CacheQuantized);
    }

    // 3. If the context is so large that it prevents *any* layer from fitting, we reduce it in
    //    steps. The condition is on a single layer and its cache: the full cache never has to
    //    fit in VRAM when the model is shared with the CPU.
    while vram_budget > 0
        && context > MIN_USABLE_CONTEXT
        && kv_layer_mib(info, context, parallel, cache_type) + bytes_per_layer_mib
            + overhead_mib
            >= vram_budget
    {
        context /= 2;
        kv_mib = kv_cache_mib(info, context, parallel, cache_type);
    }
    if context < request.desired_context
        && !compromises.iter().any(|c| matches!(c, Compromise::ContextCappedByModel { .. }))
    {
        compromises.push(Compromise::ContextReduced { asked: request.desired_context, granted: context });
    }

    // 4. Layers on the GPU. Each offloaded layer costs its weight *and* its cache: that is the
    //    true unit price, and it lets us solve for the layer count directly instead of first
    //    subtracting a full cache that will never fit entirely on the card.
    let kv_per_layer_mib = kv_layer_mib(info, context, parallel, cache_type);
    let cost_per_layer_mib = bytes_per_layer_mib + kv_per_layer_mib;
    let layer_budget = vram_budget.saturating_sub(overhead_mib);
    let affordable_layers = (layer_budget / cost_per_layer_mib) as u32;
    let n_gpu_layers = affordable_layers.min(info.block_count);

    if n_gpu_layers == 0 {
        compromises.push(Compromise::CpuOnly);

        // Nothing goes to the GPU: it is now RAM that bounds the context, not VRAM. Keeping the
        // reduction computed above would cut the window for a constraint that no longer applies
        // — a 32 GB machine would end up with 4,096 tokens because the GPU is busy with
        // something else.
        compromises.retain(|c| {
            !matches!(c, Compromise::ContextReduced { .. } | Compromise::CacheQuantized)
        });
        cache_type = CacheType::F16;
        context = request.desired_context;
        if info.context_length > 0 {
            context = context.min(info.context_length);
        }
        kv_mib = kv_cache_mib(info, context, parallel, cache_type);

        let ram_budget = hardware.ram_free_mib;
        if ram_budget > 0 {
            if weights_mib + kv_mib + COMPUTE_OVERHEAD_MIB > ram_budget && request.allow_cache_quant {
                cache_type = CacheType::Q8;
                kv_mib = kv_cache_mib(info, context, parallel, cache_type);
                compromises.push(Compromise::CacheQuantized);
            }
            while context > MIN_USABLE_CONTEXT
                && weights_mib + kv_mib + COMPUTE_OVERHEAD_MIB > ram_budget
            {
                context /= 2;
                kv_mib = kv_cache_mib(info, context, parallel, cache_type);
            }
            if context < request.desired_context
                && !compromises
                    .iter()
                    .any(|c| matches!(c, Compromise::ContextCappedByModel { .. }))
            {
                compromises.push(Compromise::ContextReduced {
                    asked: request.desired_context,
                    granted: context,
                });
            }
        }
    } else if n_gpu_layers < info.block_count {
        compromises.push(Compromise::PartialOffload {
            on_gpu: n_gpu_layers,
            total: info.block_count,
        });
    }

    let gpu_weights_mib = n_gpu_layers as u64 * bytes_per_layer_mib;
    let cpu_weights_mib = weights_mib.saturating_sub(gpu_weights_mib);
    // The cache follows its layers: what stays on the CPU weighs on RAM, not on the card.
    let kv_gpu_mib = n_gpu_layers as u64 * kv_layer_mib(info, context, parallel, cache_type);
    let kv_ram_mib = kv_mib.saturating_sub(kv_gpu_mib);
    let estimated_vram_mib = if n_gpu_layers == 0 {
        0
    } else {
        gpu_weights_mib + kv_gpu_mib + overhead_mib
    };
    let estimated_ram_mib = cpu_weights_mib + kv_ram_mib;

    // 5. Viability: what stays on the CPU must fit in the available RAM.
    //    An unknown RAM (0) must not make the plan fail: we only block on a measured overflow.
    let ram_ok = hardware.ram_free_mib == 0 || estimated_ram_mib <= hardware.ram_free_mib;
    let viable = ram_ok && context >= MIN_USABLE_CONTEXT;

    // Without a measurement, the plan is still offered but stops presenting itself as validated:
    // that is the difference between "it will fit" and "I could not check".
    if hardware.ram_free_mib == 0 && estimated_ram_mib > 0 {
        compromises.push(Compromise::MemoryUnmeasured);
    }

    LaunchPlan {
        n_gpu_layers,
        context,
        parallel,
        threads,
        // Flash attention is a GPU kernel optimization: announcing it while everything runs on
        // the CPU produces a flag that is at best inert, at worst rejected by the server.
        flash_attn: flash_attn && n_gpu_layers > 0,
        cache_type,
        estimated_vram_mib,
        estimated_ram_mib,
        compromises,
        viable,
        split: Vec::new(),
        placed: None,
        block_count: info.block_count,
        draft_layers: info.draft_layers,
        mtp: info.draft_layers > 0,
    }
}

/// KV cache of a single layer, in MiB, never zero: a layer always costs something.
fn kv_layer_mib(info: &GgufInfo, context: u32, parallel: u32, cache: CacheType) -> u64 {
    (info.kv_cache_bytes_per_layer(context, parallel, cache.bytes_per_element()) / MIB).max(1)
}

fn kv_cache_mib(info: &GgufInfo, context: u32, parallel: u32, cache: CacheType) -> u64 {
    info.kv_cache_bytes(context, parallel, cache.bytes_per_element()) / MIB
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Gpu, GpuBackend};

    /// Geometry of a 27B model with GQA, quantized to Q4 (~16 GiB on disk).
    fn qwen27b() -> GgufInfo {
        GgufInfo {
            architecture: "qwen3".into(),
            name: "Qwen3 27B".into(),
            block_count: 48,
            embedding_length: 5120,
            head_count: 40,
            head_count_kv: 8,
            context_length: 32_768,
            file_size_bytes: 16 * 1024 * MIB,
            tensor_count: 500,
            draft_layers: 0,
        }
    }

    /// Small 3B model, ~2 GiB.
    fn small3b() -> GgufInfo {
        GgufInfo {
            architecture: "qwen3".into(),
            name: "Qwen3 3B".into(),
            block_count: 36,
            embedding_length: 2048,
            head_count: 16,
            head_count_kv: 2,
            context_length: 32_768,
            file_size_bytes: 2 * 1024 * MIB,
            tensor_count: 300,
            draft_layers: 0,
        }
    }

    fn machine(vram_free: u64, ram_free: u64) -> Hardware {
        Hardware {
            gpus: vec![Gpu {
                index: None,
                name: "RTX 4070 SUPER".into(),
                backend: GpuBackend::Cuda,
                vram_total_mib: 12_282,
                vram_free_mib: vram_free,
            }],
            ram_total_mib: 32_768,
            ram_free_mib: ram_free,
            cpu_threads: 16,
        }
    }

    /// Real geometry of a Qwen3.8-27B quantized to IQ2_XXS, read from an existing file:
    /// 65 layers, GQA 24/4, 262,144 training tokens, 8,592 MiB on disk.
    ///
    /// It is the model named in the specification, and the case where sizing really matters:
    /// too optimistic, the load fails; too cautious, half the card is left idle.
    fn qwen38_27b() -> GgufInfo {
        GgufInfo {
            architecture: "qwen35".into(),
            name: "Qwen3.8-27B-UD-IQ2_XXS".into(),
            block_count: 65,
            embedding_length: 3072,
            head_count: 24,
            head_count_kv: 4,
            context_length: 262_144,
            file_size_bytes: 8592 * MIB,
            tensor_count: 800,
            draft_layers: 0,
        }
    }

    fn card(index: u32, name: &str, backend: GpuBackend, total: u64, free: u64) -> Gpu {
        Gpu { index: Some(index), name: name.into(), backend, vram_total_mib: total, vram_free_mib: free }
    }

    fn two_cards(free_a: u64, free_b: u64) -> Hardware {
        Hardware {
            gpus: vec![
                card(0, "RTX 3070 Ti", GpuBackend::Cuda, 8192, free_a),
                card(1, "RTX 4070 SUPER", GpuBackend::Cuda, 12_282, free_b),
            ],
            ram_total_mib: 65_536,
            ram_free_mib: 40_000,
            cpu_threads: 12,
        }
    }

    #[test]
    fn a_model_too_big_for_one_card_is_spread_over_two() {
        let hardware = two_cards(7800, 11_900);
        let plan = plan(&qwen27b(), &hardware, &PlanRequest { desired_context: 8192, ..Default::default() });
        assert!(plan.viable);
        assert!(plan.is_fully_offloaded(), "{:?}", plan.compromises);
        assert_eq!(plan.split.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1], "in bus order");
        let args = plan.to_args().join(" ");
        assert!(args.contains("--split-mode layer --tensor-split "), "{args}");
        let env = plan.device_env(&hardware);
        assert!(env.contains(&("CUDA_VISIBLE_DEVICES".to_owned(), "0,1".to_owned())), "{env:?}");
        assert!(plan.summary().starts_with("GPU ×2"), "{}", plan.summary());
    }

    #[test]
    fn the_cards_chosen_by_hand_are_the_ones_used_in_their_order() {
        let hardware = two_cards(7800, 11_900);
        let request = PlanRequest { desired_context: 8192, ..Default::default() };
        // A small model the planner would put on the freest card (the 4070 SUPER), asked on the
        // 3070 Ti: it goes there, and only there.
        let on_0 = plan_placed(&small3b(), &hardware, &request, &Placement::parse("0").unwrap());
        assert!(on_0.viable && on_0.split.is_empty());
        assert_eq!(on_0.device_env(&hardware), vec![("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()), ("CUDA_VISIBLE_DEVICES".into(), "0".into())]);
        assert!(on_0.to_args().join(" ").ends_with("--device CUDA0"), "{:?}", on_0.to_args());
        // Both, the 3070 Ti leading: the 4070 SUPER first, the shares in that order.
        let split = plan_placed(&qwen27b(), &hardware, &request, &Placement::parse("1,0").unwrap());
        assert!(split.viable && split.is_fully_offloaded(), "{:?}", split.compromises);
        assert_eq!(split.split.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [1, 0]);
        let args = split.to_args().join(" ");
        assert!(args.contains("--tensor-split 11132,7032") && args.contains("--device CUDA0,CUDA1"), "{args}");
        assert_eq!(split.device_env(&hardware)[1].1, "1,0");
        // Too big for the card chosen alone: it says so in its compromises, it does not move.
        let squeezed = plan_placed(&qwen27b(), &hardware, &request, &Placement::parse("0").unwrap());
        assert!(!squeezed.is_fully_offloaded() && squeezed.split.is_empty());
        // The draft head on the other card: that card is seen, the model stays off it.
        let mut mtp = small3b();
        mtp.draft_layers = 1;
        let drafted = plan_placed(&mtp, &hardware, &request, &Placement::parse("1/mtp=0").unwrap());
        let args = drafted.to_args().join(" ");
        let last = mtp.block_count - 1;
        assert!(args.contains("--tensor-split 11900,0"), "the draft's card takes no layer: {args}");
        assert!(args.ends_with(&format!("--device CUDA0,CUDA1 --spec-type draft-mtp --override-tensor blk\\.({last})\\.=CUDA1")), "{args}");
        assert_eq!(drafted.device_env(&hardware)[1].1, "1,0");
        // A server too old for MTP: neither the head nor its card.
        let old = drafted.without_mtp().to_args().join(" ");
        assert!(!old.contains("draft-mtp") && !old.contains("tensor-split") && old.ends_with("--device CUDA0"), "{old}");
        assert_eq!(drafted.without_mtp().device_env(&hardware)[1].1, "1");
        // The head on the processor; and by default, with the model, as llama.cpp places it.
        let on_cpu = plan_placed(&mtp, &hardware, &request, &Placement::parse("1/mtp=cpu").unwrap()).to_args().join(" ");
        assert!(on_cpu.ends_with(&format!("--spec-type draft-mtp --override-tensor blk\\.({last})\\.=CPU")) && !on_cpu.contains("tensor-split"), "{on_cpu}");
        let plain = plan(&mtp, &hardware, &request).to_args().join(" ");
        assert!(plain.ends_with("--spec-type draft-mtp") && !plain.contains("override-tensor"), "{plain}");
        assert!(!plan(&small3b(), &hardware, &request).to_args().join(" ").contains("draft-mtp"), "no head, no MTP");
        // Automatic: the planner's own choice, unchanged.
        let auto = plan_placed(&small3b(), &hardware, &request, &Placement::default());
        assert!(auto.placed.is_none());
        assert_eq!(auto.device_env(&hardware), hardware.device_pin());
    }

    #[test]
    fn a_model_that_fits_one_card_stays_on_it() {
        let hardware = two_cards(7800, 11_900);
        let plan = plan(&small3b(), &hardware, &PlanRequest::default());
        assert!(plan.split.is_empty(), "no traffic between cards for nothing");
        assert!(!plan.to_args().join(" ").contains("--tensor-split"));
        assert_eq!(plan.device_env(&hardware), hardware.device_pin());
    }

    #[test]
    fn a_card_another_server_fills_takes_no_share() {
        // The 4070 SUPER holds another model: 471 MiB free. Only the 3070 Ti is usable.
        let hardware = two_cards(7800, 471);
        let plan = plan(&qwen27b(), &hardware, &PlanRequest { desired_context: 8192, ..Default::default() });
        assert!(plan.split.is_empty());
        assert!(!plan.is_fully_offloaded());
    }

    #[test]
    fn cards_of_different_kinds_are_not_mixed() {
        let hardware = Hardware {
            gpus: vec![card(0, "RTX 4070 SUPER", GpuBackend::Cuda, 12_282, 11_900), card(1, "Radeon RX 7600", GpuBackend::Vulkan, 8192, 7800)],
            ram_total_mib: 65_536,
            ram_free_mib: 40_000,
            cpu_threads: 12,
        };
        let plan = plan(&qwen27b(), &hardware, &PlanRequest { desired_context: 8192, ..Default::default() });
        assert!(plan.split.is_empty(), "CUDA and Vulkan do not share one model here");
    }

    #[test]
    fn three_cards_share_in_proportion_to_their_room() {
        let hardware = Hardware {
            gpus: vec![
                card(0, "A", GpuBackend::Cuda, 8192, 8000),
                card(1, "B", GpuBackend::Cuda, 8192, 8000),
                card(2, "C", GpuBackend::Cuda, 8192, 4000),
            ],
            ram_total_mib: 65_536,
            ram_free_mib: 40_000,
            cpu_threads: 12,
        };
        let plan = plan(&qwen27b(), &hardware, &PlanRequest { desired_context: 8192, ..Default::default() });
        assert_eq!(plan.split.len(), 3);
        assert_eq!(plan.split[2].1, 4000 - DISPLAY_RESERVE_MIB);
        assert_eq!(plan.device_env(&hardware).last().unwrap().1, "0,1,2");
    }

    /// 32 GiB Apple Silicon Mac: memory is unified, and the Metal budget reserves a quarter of
    /// it for the system. It is the machine I cannot run — hence this test, which at least
    /// checks that the plan it would receive is consistent.
    fn mac_apple_silicon(unified_mib: u64) -> Hardware {
        let budget = unified_mib * 3 / 4;
        Hardware {
            gpus: vec![Gpu {
                index: None,
                name: "Apple Silicon (unified memory)".into(),
                backend: GpuBackend::Metal,
                vram_total_mib: budget,
                vram_free_mib: budget,
            }],
            ram_total_mib: unified_mib,
            ram_free_mib: unified_mib * 3 / 4,
            cpu_threads: 10,
        }
    }

    #[test]
    fn an_apple_silicon_mac_offloads_the_27b_to_metal() {
        // 32 GiB unified, so 24 GiB promised to the GPU: the 27B must fit entirely.
        let plan = plan(&qwen38_27b(), &mac_apple_silicon(32_768), &PlanRequest::default());
        assert!(plan.viable);
        assert_eq!(plan.n_gpu_layers, 65, "everything must go to Metal");
        assert_eq!(plan.context, 32_768);
    }

    #[test]
    fn a_sixteen_gigabyte_mac_does_not_promise_the_impossible() {
        // 16 GiB unified -> 12 GiB budget: the model does not fit whole, and the plan must say
        // so rather than announce an offload the driver will refuse.
        let plan = plan(&qwen38_27b(), &mac_apple_silicon(16_384), &PlanRequest::default());
        assert!(plan.viable);
        // 12 GiB of budget is enough for this quantization: everything goes to Metal, and the
        // estimate must stay under the driver cap, never above it.
        assert_eq!(plan.n_gpu_layers, 65);
        assert!(
            plan.estimated_vram_mib <= 12_288,
            "{} MiB promised for a budget of 12,288",
            plan.estimated_vram_mib
        );
        assert_eq!(plan.context, 32_768, "the context stays intact");
    }

    #[test]
    fn a_machine_without_memory_measurement_says_so() {
        // This is exactly what Po returned on macOS before `detect_memory` could read
        // `sysctl`: zero everywhere.
        let blind = Hardware {
            gpus: Vec::new(),
            ram_total_mib: 0,
            ram_free_mib: 0,
            cpu_threads: 8,
        };
        let plan = plan(&qwen38_27b(), &blind, &PlanRequest::default());
        // We do not refuse: the absence of a measurement is not proof of insufficiency, and
        // refusing would condemn every uninstrumented platform. But we say so.
        assert!(plan.viable);
        assert!(
            plan.compromises.contains(&Compromise::MemoryUnmeasured),
            "an unverifiable plan must announce itself as such: {:?}",
            plan.compromises
        );
    }

    #[test]
    fn sizing_grows_with_available_vram() {
        // More memory must always give at least as many layers on the GPU: a regression that
        // reversed this monotonicity would otherwise go unnoticed.
        let mut previous = 0;
        for vram in [267u64, 4096, 8192, 11_000, 16_384, 24_576] {
            let plan = plan(&qwen38_27b(), &machine(vram, 40_000), &PlanRequest::default());
            assert!(plan.viable, "at {vram} MiB, a plan must exist");
            assert!(
                plan.n_gpu_layers >= previous,
                "at {vram} MiB: {} layers, fewer than with less memory ({previous})",
                plan.n_gpu_layers
            );
            assert!(plan.n_gpu_layers <= 65);
            previous = plan.n_gpu_layers;
        }
    }

    #[test]
    fn a_27b_fits_entirely_on_a_big_card() {
        let plan = plan(&qwen38_27b(), &machine(24_576, 40_000), &PlanRequest::default());
        assert_eq!(plan.n_gpu_layers, 65);
        assert!(plan.is_fully_offloaded());
        assert_eq!(plan.cache_type, CacheType::F16, "no need to trim the cache here");
    }

    #[test]
    fn a_27b_on_twelve_gigabytes_is_split_without_trimming_the_context() {
        // The development machine's case: an RTX 4070 SUPER whose VRAM is partly free. We want
        // the maximum number of layers, an intact context, and the split to be announced.
        let plan = plan(&qwen38_27b(), &machine(11_000, 40_000), &PlanRequest::default());
        assert!(plan.n_gpu_layers > 40 && plan.n_gpu_layers < 65);
        assert_eq!(plan.context, 32_768, "the requested context must be preserved");
        assert_eq!(plan.cache_type, CacheType::Q8, "quantizing the cache frees up layers");
        assert!(
            plan.compromises.iter().any(|c| matches!(c, Compromise::PartialOffload { .. })),
            "the GPU/CPU split must be announced"
        );
    }

    #[test]
    fn a_27b_on_a_saturated_card_falls_back_to_cpu_without_lying() {
        // 267 MiB free, measured on the development machine during the trials.
        let plan = plan(&qwen38_27b(), &machine(267, 40_000), &PlanRequest::default());
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(plan.compromises.contains(&Compromise::CpuOnly));
        assert!(plan.viable, "RAM is enough: the model remains usable, just slower");
        assert!(!plan.flash_attn, "no GPU optimization without a layer on the GPU");
    }

    #[test]
    fn a_very_large_trained_context_does_not_blow_up_the_cache() {
        // This model announces 262,144 training tokens. Claiming them all would fill the card
        // with cache alone; Po must stick to what is requested, not to what is possible.
        let plan = plan(&qwen38_27b(), &machine(24_576, 40_000), &PlanRequest::default());
        assert_eq!(plan.context, 32_768);
        assert!(plan.estimated_vram_mib < 24_576);
    }

    #[test]
    fn an_almost_full_card_still_offloads_what_it_can() {
        // Real measurement on the development machine: 3,046 MiB free on the 4070 SUPER, the
        // rest taken by the graphical session. The old computation subtracted from the GPU
        // budget the KV cache of all 65 layers — including the fifty-odd left on the CPU — and
        // concluded zero layers offloaded. The cache follows its layers: about ten fit fine.
        let plan = plan(&qwen38_27b(), &machine(3_046, 22_810), &PlanRequest::default());
        assert!(
            plan.n_gpu_layers > 0,
            "a card with 2 GiB usable must host a few layers"
        );
        assert!(plan.n_gpu_layers < 65);
        assert!(!plan.compromises.contains(&Compromise::CpuOnly));
        assert!(
            plan.estimated_vram_mib <= 3_046,
            "the plan must not promise more than the free VRAM: {} MiB",
            plan.estimated_vram_mib
        );
        assert!(plan.viable);
    }

    #[test]
    fn the_cache_of_layers_left_on_the_cpu_weighs_on_ram() {
        // Corollary of the previous point: what the GPU no longer bills, RAM must bear.
        // Without this counterpart, the plan would become optimistic on both sides at once.
        let plan = plan(&qwen38_27b(), &machine(3_046, 22_810), &PlanRequest::default());
        let cpu_weights = 8592 - plan.n_gpu_layers as u64 * (8592 / 65);
        assert!(
            plan.estimated_ram_mib > cpu_weights,
            "RAM must carry the remaining weights *and* their cache ({} MiB for {cpu_weights} MiB of weights)",
            plan.estimated_ram_mib
        );
    }

    #[test]
    fn a_small_model_fits_entirely_on_the_gpu() {
        let plan = plan(&small3b(), &machine(11_000, 20_000), &PlanRequest::default());
        assert!(plan.viable);
        assert_eq!(plan.n_gpu_layers, 36, "all the layers");
        assert!(plan.is_fully_offloaded());
        assert_eq!(plan.context, 32_768);
        assert!(plan.flash_attn, "CUDA must enable flash attention");
    }

    #[test]
    fn a_big_model_on_a_small_gpu_is_partially_offloaded() {
        let plan = plan(&qwen27b(), &machine(11_000, 20_000), &PlanRequest::default());
        assert!(plan.viable);
        assert!(plan.n_gpu_layers > 0 && plan.n_gpu_layers < 48);
        assert!(!plan.is_fully_offloaded());
        assert!(
            plan.compromises.iter().any(|c| matches!(c, Compromise::PartialOffload { .. })),
            "the compromise must be announced, not endured"
        );
    }

    #[test]
    fn already_saturated_vram_falls_back_to_cpu() {
        // The case measured on the development machine: 509 MiB free out of 12 GB.
        let plan = plan(&qwen27b(), &machine(509, 20_000), &PlanRequest::default());
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(plan.compromises.contains(&Compromise::CpuOnly));
        assert_eq!(plan.estimated_vram_mib, 0);
        assert!(!plan.flash_attn, "a GPU optimization makes no sense without a layer on the GPU");
        assert!(!plan.to_args().join(" ").contains("--flash-attn"));
    }

    #[test]
    fn in_cpu_mode_the_context_depends_on_ram_not_vram() {
        // Measured case: GPU saturated by another application, but 20 GB of free RAM.
        // Trimming the window because of VRAM we do not use would be absurd.
        let plan = plan(&small3b(), &machine(800, 20_000), &PlanRequest::default());
        assert_eq!(plan.n_gpu_layers, 0);
        assert_eq!(plan.context, 32_768, "RAM is amply sufficient");
        assert_eq!(plan.cache_type, CacheType::F16);
        assert!(!plan.compromises.iter().any(|c| matches!(c, Compromise::ContextReduced { .. })));
        assert!(plan.viable);
    }

    #[test]
    fn in_cpu_mode_tight_ram_does_trim_the_context() {
        // 27B in Q4 (16 GiB) with 18 GB of RAM: the weights fit, but not a full cache.
        let plan = plan(&qwen27b(), &machine(0, 18_000), &PlanRequest::default());
        assert_eq!(plan.n_gpu_layers, 0);
        assert!(plan.context < 32_768, "the context must yield to RAM");
        assert!(plan.context >= 4096);
        assert!(plan.viable);
    }

    #[test]
    fn the_context_does_not_exceed_the_trained_window() {
        let request = PlanRequest { desired_context: 200_000, ..Default::default() };
        let plan = plan(&small3b(), &machine(11_000, 20_000), &request);
        assert_eq!(plan.context, 32_768);
        assert!(plan.compromises.contains(&Compromise::ContextCappedByModel { trained: 32_768 }));
    }

    #[test]
    fn the_cache_is_quantized_before_trimming_the_context() {
        // 27B in Q4 on 11 GB: the weights alone overflow, so quantization is attempted.
        let plan = plan(&qwen27b(), &machine(11_000, 20_000), &PlanRequest::default());
        assert_eq!(plan.cache_type, CacheType::Q8);
        assert!(plan.compromises.contains(&Compromise::CacheQuantized));
        assert_eq!(plan.context, 32_768, "the context must be preserved first");
    }

    #[test]
    fn forbidding_quantization_preserves_the_native_cache() {
        let request = PlanRequest { allow_cache_quant: false, ..Default::default() };
        let plan = plan(&qwen27b(), &machine(11_000, 20_000), &request);
        assert_eq!(plan.cache_type, CacheType::F16);
        assert!(!plan.compromises.contains(&Compromise::CacheQuantized));
    }

    #[test]
    fn parallelism_multiplies_the_cache() {
        let single = PlanRequest { parallel: 1, ..Default::default() };
        let quad = PlanRequest { parallel: 4, ..Default::default() };
        let a = plan(&small3b(), &machine(11_000, 20_000), &single);
        let b = plan(&small3b(), &machine(11_000, 20_000), &quad);
        assert!(
            b.estimated_vram_mib > a.estimated_vram_mib,
            "four simultaneous sub-agents cost four caches"
        );
        assert_eq!(b.parallel, 4);
    }

    #[test]
    fn insufficient_ram_makes_the_plan_non_viable() {
        // No GPU and 1 GiB of free RAM: a 16 GiB model will not fit.
        let cpu_only = Hardware { gpus: vec![], ram_free_mib: 1024, cpu_threads: 8, ram_total_mib: 8192 };
        let plan = plan(&qwen27b(), &cpu_only, &PlanRequest::default());
        assert!(!plan.viable, "a server doomed to fail must not be launched");
    }

    #[test]
    fn unknown_ram_does_not_block() {
        let unknown = Hardware { gpus: vec![], ram_free_mib: 0, cpu_threads: 8, ram_total_mib: 0 };
        let plan = plan(&small3b(), &unknown, &PlanRequest::default());
        assert!(plan.viable, "the absence of a measurement is not proof of insufficiency");
    }

    #[test]
    fn a_model_without_geometry_is_never_launched() {
        let unusable = GgufInfo {
            architecture: "?".into(),
            name: "?".into(),
            block_count: 0,
            embedding_length: 0,
            head_count: 0,
            head_count_kv: 0,
            context_length: 0,
            file_size_bytes: 1024 * MIB,
            tensor_count: 0,
            draft_layers: 0,
        };
        let plan = plan(&unusable, &machine(11_000, 20_000), &PlanRequest::default());
        assert!(!plan.viable);
    }

    #[test]
    fn arguments_reflect_the_plan() {
        let plan = plan(&qwen27b(), &machine(11_000, 20_000), &PlanRequest::default());
        let args = plan.to_args().join(" ");
        assert!(args.contains(&format!("-ngl {}", plan.n_gpu_layers)));
        assert!(args.contains(&format!("-c {}", plan.context)));
        assert!(args.contains("--flash-attn on"), "the flag requires an explicit value");
        assert!(args.contains("--cache-type-k q8_0"));
    }

    #[test]
    fn an_f16_cache_adds_no_cache_flag() {
        let plan = plan(&small3b(), &machine(11_000, 20_000), &PlanRequest::default());
        assert_eq!(plan.cache_type, CacheType::F16);
        assert!(!plan.to_args().join(" ").contains("--cache-type"));
    }

    #[test]
    fn the_summary_is_readable() {
        let plan = plan(&small3b(), &machine(11_000, 20_000), &PlanRequest::default());
        let summary = plan.summary();
        assert!(summary.contains("GPU"));
        assert!(summary.contains("32768"));
        // The line must stay short: it is shown in a list, next to the model's name and size.
        assert!(summary.len() <= 40, "summary too long ({}): {summary}", summary.len());
    }
}
