//! Where a model runs: on which graphics cards, which one leads, where its draft head goes.
//!
//! By default Po chooses (the freest card, or several of the same kind when one is not enough).
//! A placement chosen by the user is kept in the model's profile and applied at every start:
//!
//! - **one card**: the whole model on it;
//! - **several cards**: the layers split between them, the **last one leading** — it runs the
//!   last layers, the output head and, by default, the draft head (the main card);
//! - **the draft head elsewhere**: a model that carries its own guessing layers (MTP) runs them
//!   on another card, or on the processor (llama.cpp's `--device-draft`); Strata can put them on
//!   the first card of a split;
//! - **a card as an expert cache** (Strata only): the second card keeps experts, not layers
//!   (`--peer-device`).
//!
//! Cards are numbered as `nvidia-smi` numbers them (the PCI bus order), the same numbers Strata
//! uses; the engines are shown them in the order chosen, through the visible-devices variable.

use std::fmt;

use po_i18n::t;
use serde::{Deserialize, Serialize};

use crate::hardware::{Gpu, GpuBackend, Hardware};

/// Where the draft head (MTP) runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Draft {
    /// On the processor.
    Cpu,
    /// On this card.
    Card(u32),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// The cards that hold the layers, the leading one last. Empty: Po chooses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<Draft>,
    /// Strata: a card that holds a second expert cache, not layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experts: Option<u32>,
}

impl Placement {
    pub fn is_auto(&self) -> bool {
        self.gpus.is_empty()
    }

    /// The card that leads: the last of the list.
    pub fn main(&self) -> Option<u32> {
        self.gpus.last().copied()
    }

    /// Reads the written form: `auto`, `1` (one card), `0,1` (split, 1 leads), `1/mtp=0`,
    /// `1/mtp=cpu`, `1/experts=0`, combined as `0,1/mtp=0`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        if matches!(text.as_str(), "auto" | "automatique" | "defaut" | "défaut" | "default" | "") {
            return Some(Self::default());
        }
        let mut parts = text.split('/');
        let mut placement = Self::default();
        for card in parts.next()?.split(',') {
            let card: u32 = card.trim().parse().ok()?;
            if placement.gpus.contains(&card) {
                return None;
            }
            placement.gpus.push(card);
        }
        if placement.gpus.is_empty() || placement.gpus.len() > 16 {
            return None;
        }
        for part in parts {
            let (key, value) = part.split_once('=')?;
            match key.trim() {
                "mtp" | "draft" | "brouillon" => {
                    placement.draft = Some(match value.trim() {
                        "cpu" | "processeur" => Draft::Cpu,
                        n => Draft::Card(n.parse().ok()?),
                    })
                }
                "experts" | "cache" => {
                    let card: u32 = value.trim().parse().ok()?;
                    if placement.gpus.contains(&card) || placement.gpus.len() != 1 {
                        return None;
                    }
                    placement.experts = Some(card);
                }
                _ => return None,
            }
        }
        Some(placement)
    }

    /// Every card it names.
    fn cards(&self) -> Vec<u32> {
        let mut cards = self.gpus.clone();
        if let Some(Draft::Card(card)) = self.draft
            && !cards.contains(&card)
        {
            cards.push(card);
        }
        if let Some(card) = self.experts
            && !cards.contains(&card)
        {
            cards.push(card);
        }
        cards
    }

    /// Can it run on this machine as it is? Every card it names must be there, of one kind.
    pub fn check(&self, hardware: &Hardware) -> Result<(), String> {
        if self.is_auto() {
            return Ok(());
        }
        let mut kind: Option<GpuBackend> = None;
        for card in self.cards() {
            let Some(gpu) = gpu(hardware, card) else {
                return Err(t!("local.placement.missing", card = card).to_owned());
            };
            if gpu.backend == GpuBackend::Metal {
                return Err(t!("local.placement.unified").to_owned());
            }
            if kind.is_some_and(|k| k != gpu.backend) {
                return Err(t!("local.placement.mixed").to_owned());
            }
            kind = Some(gpu.backend);
        }
        Ok(())
    }

    /// In words, with the cards' names: `RTX 3070 Ti + RTX 4070 SUPER · main: RTX 4070 SUPER ·
    /// MTP head on RTX 3070 Ti`.
    pub fn describe(&self, hardware: &Hardware) -> String {
        if self.is_auto() {
            return t!("local.placement.auto").to_owned();
        }
        let name = |card: u32| gpu(hardware, card).map(|g| short_name(&g.name)).unwrap_or_else(|| t!("local.placement.card", card = card).to_owned());
        let mut text = if self.gpus.len() == 1 {
            name(self.gpus[0])
        } else {
            let names: Vec<String> = self.gpus.iter().map(|c| name(*c)).collect();
            format!("{} · {}", names.join(" + "), t!("local.placement.main", name = name(self.gpus[self.gpus.len() - 1])))
        };
        match self.draft {
            Some(Draft::Cpu) => text.push_str(&format!(" · {}", t!("local.placement.draft_on", place = t!("local.placement.cpu")))),
            Some(Draft::Card(card)) => text.push_str(&format!(" · {}", t!("local.placement.draft_on", place = name(card)))),
            None => {}
        }
        if let Some(card) = self.experts {
            text.push_str(&format!(" · {}", t!("local.placement.experts_on", place = name(card))));
        }
        text
    }

    /// What llama-server is given to run this way: the cards it sees (the variable, in the
    /// chosen order) and its arguments (the devices of the model and of its draft head).
    pub fn llama(&self, hardware: &Hardware) -> Option<LlamaPlacement> {
        if self.is_auto() {
            return None;
        }
        let backend = gpu(hardware, self.gpus[0])?.backend;
        let (variable, prefix) = match backend {
            GpuBackend::Cuda => ("CUDA_VISIBLE_DEVICES", "CUDA"),
            GpuBackend::Rocm => ("HIP_VISIBLE_DEVICES", "ROCm"),
            GpuBackend::Vulkan => ("GGML_VK_VISIBLE_DEVICES", "Vulkan"),
            GpuBackend::Metal => return None,
        };
        // The layer cards first, in their order (the leading one last), then the draft's card.
        let mut visible = self.gpus.clone();
        if let Some(Draft::Card(card)) = self.draft
            && !visible.contains(&card)
        {
            visible.push(card);
        }
        let device = |card: u32| visible.iter().position(|c| *c == card).map(|i| format!("{prefix}{i}"));
        let mut env = vec![(variable.to_owned(), visible.iter().map(u32::to_string).collect::<Vec<_>>().join(","))];
        if backend == GpuBackend::Cuda {
            env.insert(0, ("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()));
        }
        // The draft head is part of the model's own weights (its last layers): llama.cpp's
        // `--device-draft` does not move it, an override of those tensors' buffer does. A card
        // that holds only the draft head is given to the server with no share of the layers.
        let draft_buffer = match self.draft {
            None => None,
            Some(Draft::Cpu) => Some("CPU".to_owned()),
            Some(Draft::Card(card)) => Some(device(card)?),
        };
        let draft_only = matches!(self.draft, Some(Draft::Card(card)) if !self.gpus.contains(&card));
        Some(LlamaPlacement {
            env,
            devices: visible.iter().filter_map(|c| device(*c)).collect(),
            draft_only,
            draft_buffer,
            cards: self.gpus.iter().filter_map(|c| gpu(hardware, *c).map(|g| (*c, g.vram_free_mib))).collect(),
        })
    }
}

impl fmt::Display for Placement {
    /// The written form [`Placement::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_auto() {
            return write!(f, "auto");
        }
        write!(f, "{}", self.gpus.iter().map(u32::to_string).collect::<Vec<_>>().join(","))?;
        match self.draft {
            Some(Draft::Cpu) => write!(f, "/mtp=cpu")?,
            Some(Draft::Card(card)) => write!(f, "/mtp={card}")?,
            None => {}
        }
        if let Some(card) = self.experts {
            write!(f, "/experts={card}")?;
        }
        Ok(())
    }
}

/// llama-server's share of a placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaPlacement {
    /// The cards the server sees, in the chosen order.
    pub env: Vec<(String, String)>,
    /// Its devices (`--device`): the layer cards, then the draft head's own card if any.
    pub devices: Vec<String>,
    /// The last device holds only the draft head (no share of the layers).
    pub draft_only: bool,
    /// Where the draft head's weights go (`CUDA1`, `CPU`); none: with the last layers.
    pub draft_buffer: Option<String>,
    /// The layer cards with their free memory (MiB), in order.
    pub cards: Vec<(u32, u64)>,
}

impl LlamaPlacement {
    /// The same without the draft head's own place (a server too old for MTP).
    pub fn without_draft(&self) -> Self {
        let mut out = self.clone();
        out.draft_buffer = None;
        if out.draft_only {
            out.draft_only = false;
            out.devices.pop();
            if let Some((_, list)) = out.env.iter_mut().find(|(k, _)| k.ends_with("VISIBLE_DEVICES")) {
                *list = list.rsplit_once(',').map(|(head, _)| head.to_owned()).unwrap_or_default();
            }
        }
        out
    }
}

fn gpu(hardware: &Hardware, card: u32) -> Option<&Gpu> {
    hardware.gpus.iter().find(|g| g.index == Some(card))
}

/// `RTX 4070 SUPER` for `NVIDIA GeForce RTX 4070 SUPER`: the maker's words add nothing in a list.
pub fn short_name(name: &str) -> String {
    let mut name = name.trim();
    for prefix in ["NVIDIA ", "GeForce ", "AMD ", "Radeon(TM) ", "Intel(R) "] {
        name = name.strip_prefix(prefix).unwrap_or(name);
    }
    name.to_owned()
}

/// The placements worth offering on this machine, the useful ones first: each card alone, then
/// every card together with each one leading, then (when the model has a draft head) the draft
/// head on another card or the processor, then (Strata) a card as an expert cache. Cards of the
/// kind that has the most free memory, at most four (beyond, the list would be endless).
pub fn choices(hardware: &Hardware, engine: Engine, has_draft: bool) -> Vec<Placement> {
    let Some(best) = hardware.best_gpu() else { return Vec::new() };
    if best.backend == GpuBackend::Metal {
        return Vec::new();
    }
    let mut cards: Vec<u32> = hardware.gpus.iter().filter(|g| g.backend == best.backend).filter_map(|g| g.index).collect();
    cards.sort_unstable();
    cards.truncate(4);
    if cards.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Placement> = cards.iter().map(|c| Placement { gpus: vec![*c], ..Default::default() }).collect();
    if cards.len() >= 2 {
        for main in &cards {
            let mut gpus: Vec<u32> = cards.iter().filter(|c| *c != main).copied().collect();
            gpus.push(*main);
            out.push(Placement { gpus: gpus.clone(), ..Default::default() });
            // Strata: the draft head on the first card of the split rather than the main one.
            if engine == Engine::Strata && has_draft {
                out.push(Placement { draft: Some(Draft::Card(gpus[0])), gpus, ..Default::default() });
            }
        }
    }
    for main in &cards {
        for other in cards.iter().filter(|c| *c != main) {
            match engine {
                Engine::Llama if has_draft => out.push(Placement { gpus: vec![*main], draft: Some(Draft::Card(*other)), experts: None }),
                Engine::Strata => out.push(Placement { gpus: vec![*main], draft: None, experts: Some(*other) }),
                Engine::Llama => {}
            }
        }
        if engine == Engine::Llama && has_draft {
            out.push(Placement { gpus: vec![*main], draft: Some(Draft::Cpu), experts: None });
        }
    }
    out
}

/// The engine a placement is for: they do not offer the same things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Llama,
    Strata,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_cards() -> Hardware {
        let card = |index, name: &str, total, free| Gpu { name: name.into(), backend: GpuBackend::Cuda, vram_total_mib: total, vram_free_mib: free, index: Some(index) };
        Hardware {
            gpus: vec![card(0, "NVIDIA GeForce RTX 3070 Ti", 8192, 7100), card(1, "NVIDIA GeForce RTX 4070 SUPER", 12282, 1000)],
            ram_total_mib: 65536,
            ram_free_mib: 20000,
            cpu_threads: 16,
        }
    }

    #[test]
    fn the_written_form_reads_back_and_wrong_ones_are_refused() {
        for text in ["auto", "1", "0,1", "1,0", "1/mtp=0", "1/mtp=cpu", "1/experts=0", "0,1/mtp=0"] {
            let placement = Placement::parse(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(placement.to_string(), text);
            assert_eq!(Placement::parse(&placement.to_string()), Some(placement));
        }
        assert_eq!(Placement::parse("défaut"), Some(Placement::default()));
        for bad in ["x", "1,1", "1/mtp=x", "1/experts=1", "0,1/experts=2", "1/what=0", ","] {
            assert_eq!(Placement::parse(bad), None, "{bad}");
        }
        let split = Placement::parse("0,1").unwrap();
        assert_eq!(split.main(), Some(1), "the last card leads");
    }

    #[test]
    fn it_is_checked_against_the_cards_there_are() {
        let hardware = two_cards();
        po_i18n::set_lang(po_i18n::Lang::Fr);
        assert!(Placement::parse("0,1/mtp=0").unwrap().check(&hardware).is_ok());
        let missing = Placement::parse("2").unwrap().check(&hardware).unwrap_err();
        assert!(missing.contains('2'), "{missing}");
        assert!(Placement::parse("1/mtp=3").unwrap().check(&hardware).is_err());
        let mut mixed = hardware.clone();
        mixed.gpus[0].backend = GpuBackend::Vulkan;
        assert!(Placement::parse("0,1").unwrap().check(&mixed).is_err(), "one kind of card per model");
        assert!(Placement::default().check(&Hardware::default()).is_ok());
    }

    #[test]
    fn llama_sees_the_chosen_cards_in_order_and_its_draft_on_its_own() {
        let hardware = two_cards();
        let single = Placement::parse("1").unwrap().llama(&hardware).unwrap();
        assert_eq!(single.env, vec![("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()), ("CUDA_VISIBLE_DEVICES".into(), "1".into())]);
        assert_eq!(single.devices, vec!["CUDA0"]);
        assert!(!single.draft_only && single.draft_buffer.is_none());
        let split = Placement::parse("1,0").unwrap().llama(&hardware).unwrap();
        assert_eq!(split.env[1].1, "1,0", "the leading card last");
        assert_eq!(split.devices, vec!["CUDA0", "CUDA1"]);
        assert_eq!(split.cards, vec![(1, 1000), (0, 7100)]);
        let draft = Placement::parse("1/mtp=0").unwrap().llama(&hardware).unwrap();
        assert_eq!(draft.env[1].1, "1,0", "the draft's card is seen too");
        assert_eq!(draft.devices, vec!["CUDA0", "CUDA1"]);
        assert!(draft.draft_only);
        assert_eq!(draft.draft_buffer.as_deref(), Some("CUDA1"));
        let plain = draft.without_draft();
        assert_eq!((plain.env[1].1.as_str(), plain.devices.len(), plain.draft_only, plain.draft_buffer.clone()), ("1", 1, false, None));
        let cpu = Placement::parse("0/mtp=cpu").unwrap().llama(&hardware).unwrap();
        assert_eq!(cpu.env[1].1, "0");
        assert_eq!(cpu.draft_buffer.as_deref(), Some("CPU"));
        assert!(!cpu.draft_only);
        let in_split = Placement::parse("0,1/mtp=0").unwrap().llama(&hardware).unwrap();
        assert_eq!(in_split.draft_buffer.as_deref(), Some("CUDA0"), "on the split's first card");
        assert!(!in_split.draft_only);
        assert_eq!(Placement::default().llama(&hardware), None, "auto: Po's own choice");
    }

    #[test]
    fn the_choices_offered_follow_the_engine_and_the_model() {
        let hardware = two_cards();
        let llama = choices(&hardware, Engine::Llama, false);
        let texts: Vec<String> = llama.iter().map(Placement::to_string).collect();
        assert_eq!(texts, vec!["0", "1", "1,0", "0,1"]);
        let mtp: Vec<String> = choices(&hardware, Engine::Llama, true).iter().map(Placement::to_string).collect();
        assert!(mtp.contains(&"1/mtp=0".to_owned()) && mtp.contains(&"0/mtp=1".to_owned()) && mtp.contains(&"1/mtp=cpu".to_owned()), "{mtp:?}");
        let strata: Vec<String> = choices(&hardware, Engine::Strata, true).iter().map(Placement::to_string).collect();
        assert!(strata.contains(&"0,1/mtp=0".to_owned()) && strata.contains(&"1/experts=0".to_owned()), "{strata:?}");
        assert!(!strata.iter().any(|t| t.contains("mtp=cpu")), "Strata keeps its draft head on a card");
        let one = Hardware { gpus: vec![hardware.gpus[0].clone()], ..hardware.clone() };
        assert_eq!(choices(&one, Engine::Llama, true).iter().map(Placement::to_string).collect::<Vec<_>>(), vec!["0", "0/mtp=cpu"]);
        assert!(choices(&Hardware::default(), Engine::Llama, true).is_empty());
    }

    #[test]
    fn it_is_said_with_the_cards_names() {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let hardware = two_cards();
        assert_eq!(Placement::parse("1").unwrap().describe(&hardware), "RTX 4070 SUPER");
        let split = Placement::parse("0,1/mtp=0").unwrap().describe(&hardware);
        assert!(split.starts_with("RTX 3070 Ti + RTX 4070 SUPER") && split.contains("MTP") && split.ends_with("RTX 3070 Ti"), "{split}");
        assert!(Placement::parse("1/experts=0").unwrap().describe(&hardware).contains("experts"));
    }
}
