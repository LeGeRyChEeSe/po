//! Discovery of the hardware available for inference.
//!
//! The sizing logic depends on only three numbers: free VRAM, free RAM and core count. They come
//! from system commands whose *parsing* is kept apart from their execution — that separation is
//! what makes detection testable without a GPU at hand.
//!
//! One point of caution drives the whole module: it is the **free** VRAM that matters, not the
//! total. A development machine already runs a browser, a compositor, sometimes another model;
//! sizing on nominal capacity guarantees a load failure.

use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuBackend {
    Cuda,
    Rocm,
    Metal,
    Vulkan,
}

impl GpuBackend {
    /// Name of the llama.cpp build usually compiled for this backend.
    pub fn label(self) -> &'static str {
        match self {
            Self::Cuda => "CUDA",
            Self::Rocm => "ROCm",
            Self::Metal => "Metal",
            Self::Vulkan => "Vulkan",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Gpu {
    pub name: String,
    pub backend: GpuBackend,
    pub vram_total_mib: u64,
    pub vram_free_mib: u64,
    /// Card index in PCI bus order (the one `nvidia-smi` uses), when known.
    pub index: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct Hardware {
    pub gpus: Vec<Gpu>,
    pub ram_total_mib: u64,
    pub ram_free_mib: u64,
    pub cpu_threads: u32,
}

impl Hardware {
    pub fn detect() -> Self {
        Self {
            gpus: detect_gpus(),
            ram_total_mib: 0,
            ram_free_mib: 0,
            cpu_threads: std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(4),
        }
        .with_memory()
    }

    fn with_memory(mut self) -> Self {
        let (total, free) = detect_memory();
        self.ram_total_mib = total;
        self.ram_free_mib = free;
        self
    }

    /// The GPU with the most free memory, if there is one.
    pub fn best_gpu(&self) -> Option<&Gpu> {
        self.gpus.iter().max_by_key(|gpu| gpu.vram_free_mib)
    }

    /// VRAM that can really be used, leaving a margin for the display system.
    /// Without this reserve the load succeeds, then the driver evicts pages under desktop
    /// pressure and throughput collapses with no visible error.
    pub fn usable_vram_mib(&self, reserve_mib: u64) -> u64 {
        self.best_gpu().map(|gpu| gpu.vram_free_mib.saturating_sub(reserve_mib)).unwrap_or(0)
    }

    /// Environment variables that restrict a server to the card chosen by the plan.
    ///
    /// The plan sizes the model for *one* card, the one with the most free memory. Without a
    /// restriction, llama.cpp spreads layers over every visible card: it then also allocated on
    /// the card another server already occupies (Strata on the 4070 SUPER, observed), at the risk
    /// of exhausting it. `PCI_BUS_ID` aligns CUDA's ordering with that of `nvidia-smi`.
    pub fn device_pin(&self) -> Vec<(String, String)> {
        let cuda: Vec<&Gpu> = self.gpus.iter().filter(|g| g.backend == GpuBackend::Cuda).collect();
        if cuda.len() < 2 {
            return Vec::new();
        }
        match self.best_gpu().and_then(|gpu| gpu.index) {
            Some(index) => vec![
                ("CUDA_DEVICE_ORDER".into(), "PCI_BUS_ID".into()),
                ("CUDA_VISIBLE_DEVICES".into(), index.to_string()),
            ],
            None => Vec::new(),
        }
    }

    pub fn has_gpu(&self) -> bool {
        !self.gpus.is_empty()
    }

    /// Recommended thread count for the CPU share: one core is left to the rest of the system.
    pub fn inference_threads(&self) -> u32 {
        self.cpu_threads.saturating_sub(1).max(1)
    }
}

fn detect_gpus() -> Vec<Gpu> {
    // NVIDIA cards from their driver's tool (exact free memory), the others — AMD, Intel Arc —
    // from the system: a machine with only a Radeon was seen as having no card at all.
    let mut gpus = nvidia_smi().unwrap_or_default();
    let nvidia_found = !gpus.is_empty();
    gpus.extend(other_gpus().into_iter().filter(|g| !(nvidia_found && is_nvidia(&g.name))));
    if !gpus.is_empty() {
        return gpus;
    }
    if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        // On Apple Silicon memory is unified: "VRAM" is RAM, and llama.cpp offloads everything
        // to Metal by default. An Intel Mac lacks this property — without the architecture
        // guard we would promise it a GPU it does not have.
        let (total, free) = detect_memory();
        let (total, free) = metal_budget(total, free);
        if total > 0 {
            return vec![Gpu {
                index: None,
                name: po_i18n::t!("local.hardware.apple_unified").to_owned(),
                backend: GpuBackend::Metal,
                vram_total_mib: total,
                vram_free_mib: free,
            }];
        }
    }
    Vec::new()
}

/// Share of unified memory we agree to promise to the GPU.
///
/// macOS refuses to pin all of the RAM to the GPU: the default cap is around three quarters.
/// Announcing the whole amount would plan an offload the driver will refuse, or one that
/// pushes the machine into swap.
fn metal_budget(total: u64, free: u64) -> (u64, u64) {
    let cap = total * 3 / 4;
    (cap, free.min(cap))
}

fn nvidia_smi() -> Option<Vec<Gpu>> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total,memory.free,index",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let gpus = parse_nvidia_smi(&text);
    if gpus.is_empty() { None } else { Some(gpus) }
}

fn is_nvidia(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("nvidia") || lower.contains("geforce") || lower.contains("quadro")
}

/// Least memory for a card to count: below are integrated chips and virtual displays, which
/// would only mislead the sizing.
const MIN_CARD_MIB: u64 = 2048;

/// The cards the system lists (Windows: the display adapters' registry entries, with their
/// exact memory; Linux: the AMD driver's figures). Their free memory is not known: the total
/// stands for it, and the plan's reserve keeps a margin.
#[cfg(target_os = "windows")]
fn other_gpus() -> Vec<Gpu> {
    const CLASS: &str = r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let query = |value: &str| {
        use std::os::windows::process::CommandExt;
        Command::new("reg")
            .args(["query", CLASS, "/s", "/v", value])
            .creation_flags(crate::catalog::NO_CONSOLE)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    parse_display_adapters(&query("DriverDesc"), &query("HardwareInformation.qwMemorySize"))
}

#[cfg(target_os = "linux")]
fn other_gpus() -> Vec<Gpu> {
    let Ok(cards) = std::fs::read_dir("/sys/class/drm") else { return Vec::new() };
    let rocm = std::path::Path::new("/opt/rocm").exists();
    let mut out = Vec::new();
    for card in cards.flatten() {
        let name = card.file_name().to_string_lossy().into_owned();
        // `card0`, not its connectors (`card0-DP-1`).
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = card.path().join("device");
        let read = |file: &str| std::fs::read_to_string(device.join(file)).ok().map(|t| t.trim().to_owned());
        if read("vendor").as_deref() != Some("0x1002") {
            continue;
        }
        let bytes = |file: &str| read(file).and_then(|t| t.parse::<u64>().ok()).unwrap_or(0);
        let total = bytes("mem_info_vram_total") / (1024 * 1024);
        let used = bytes("mem_info_vram_used") / (1024 * 1024);
        if total < MIN_CARD_MIB {
            continue;
        }
        out.push(Gpu {
            index: None,
            name: read("product_name").filter(|n| !n.is_empty()).unwrap_or_else(|| "AMD Radeon".to_owned()),
            backend: if rocm { GpuBackend::Rocm } else { GpuBackend::Vulkan },
            vram_total_mib: total,
            vram_free_mib: total.saturating_sub(used),
        });
    }
    out
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn other_gpus() -> Vec<Gpu> {
    Vec::new()
}

/// Reads the display adapters out of two `reg query … /s /v <value>` listings: one with their
/// names, one with their memory. Virtual displays (remote desktop, streaming) have no memory
/// entry, integrated chips a small one: neither counts.
pub fn parse_display_adapters(names: &str, memory: &str) -> Vec<Gpu> {
    fn values(text: &str, wanted: &str) -> Vec<(String, String)> {
        let mut key = String::new();
        let mut out = Vec::new();
        for line in text.lines() {
            if line.starts_with("HKEY_") {
                key = line.trim().to_owned();
            } else if let Some(rest) = line.trim_start().strip_prefix(wanted) {
                let mut parts = rest.split_whitespace();
                let _type = parts.next();
                let value = parts.collect::<Vec<_>>().join(" ");
                if !key.is_empty() && !value.is_empty() {
                    out.push((key.clone(), value));
                }
            }
        }
        out
    }
    let sizes = values(memory, "HardwareInformation.qwMemorySize");
    values(names, "DriverDesc")
        .into_iter()
        .filter_map(|(key, name)| {
            let raw = &sizes.iter().find(|(k, _)| *k == key)?.1;
            let bytes = u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok()?;
            let mib = bytes / (1024 * 1024);
            if mib < MIN_CARD_MIB {
                return None;
            }
            let backend = if is_nvidia(&name) { GpuBackend::Cuda } else { GpuBackend::Vulkan };
            Some(Gpu { index: None, name, backend, vram_total_mib: mib, vram_free_mib: mib })
        })
        .collect()
}

/// Parses the CSV output of `nvidia-smi`. Kept separate so it is testable without a GPU.
pub fn parse_nvidia_smi(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(',').map(str::trim);
            let name = fields.next()?;
            let total: u64 = fields.next()?.parse().ok()?;
            let free: u64 = fields.next()?.parse().ok()?;
            let index = fields.next().and_then(|i| i.parse().ok());
            if name.is_empty() {
                return None;
            }
            Some(Gpu {
                name: name.to_owned(),
                backend: GpuBackend::Cuda,
                vram_total_mib: total,
                // A free value above the total would signal an inconsistent driver: we prefer
                // to clamp rather than propagate an absurd figure into the flag computation.
                vram_free_mib: free.min(total),
                index,
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn detect_memory() -> (u64, u64) {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return (0, 0);
    };
    parse_meminfo(&text)
}

/// Parses `/proc/meminfo`, whose values are in kibibytes.
#[cfg(any(target_os = "linux", test))]
pub fn parse_meminfo(text: &str) -> (u64, u64) {
    let field = |name: &str| -> u64 {
        text.lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u64>().ok())
            .map(|kib| kib / 1024)
            .unwrap_or(0)
    };
    // `MemAvailable` is the only honest estimate of what is reusable: `MemFree` ignores the
    // page cache, reclaimable on demand, and therefore underestimates massively.
    (field("MemTotal:"), field("MemAvailable:"))
}

#[cfg(target_os = "windows")]
fn detect_memory() -> (u64, u64) {
    // The system API first: `wmic` is gone from Windows 11 (observed on the development
    // machine), and without a RAM measurement the launch plan cannot check anything.
    if let Some(measure) = global_memory_status() {
        return measure;
    }
    let output = Command::new("wmic")
        .args(["OS", "get", "TotalVisibleMemorySize,FreePhysicalMemory", "/value"])
        .output();

    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let parsed = parse_wmic_memory(&text);
        if parsed.0 > 0 {
            return parsed;
        }
    }
    (0, 0)
}

/// Total and available memory, in MiB, according to `GlobalMemoryStatusEx`.
#[cfg(target_os = "windows")]
fn global_memory_status() -> Option<(u64, u64)> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: the structure is initialised and its size filled in, as the API requires.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 || status.ullTotalPhys == 0 {
        return None;
    }
    const MIB: u64 = 1024 * 1024;
    Some((status.ullTotalPhys / MIB, status.ullAvailPhys / MIB))
}

/// Parses wmic's `/value` output, in kibibytes.
#[cfg(any(target_os = "windows", test))]
pub fn parse_wmic_memory(text: &str) -> (u64, u64) {
    let field = |name: &str| -> u64 {
        text.lines()
            .filter_map(|line| line.trim().split_once('='))
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| value.trim().parse::<u64>().ok())
            .map(|kib| kib / 1024)
            .unwrap_or(0)
    };
    (field("TotalVisibleMemorySize"), field("FreePhysicalMemory"))
}

/// macOS, and any Unix that is neither Linux nor Windows: we try `sysctl`/`vm_stat` rather
/// than give up outright. On a system without them the result is `(0, 0)` — exactly what the
/// old fallback returned, but without ruling out the systems that have them.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn detect_memory() -> (u64, u64) {
    darwin_memory()
}

/// Memory as seen by `sysctl` and `vm_stat`.
///
/// Deliberately compiled on every platform rather than hidden behind a
/// `cfg(target_os = "macos")`: without a Mac at hand, code that is never compiled is code that
/// is never checked. Here, every `cargo check` and clippy run verifies it. Outside macOS the
/// two commands are simply absent and the result is `(0, 0)` — which the `detect_memory` of the
/// platform concerned never uses.
#[allow(dead_code, reason = "only called outside Linux/Windows, compiled everywhere on purpose")]
fn darwin_memory() -> (u64, u64) {
    let total = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok())
        .map(|bytes| bytes / (1024 * 1024))
        .unwrap_or(0);

    let free = Command::new("vm_stat")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| parse_vm_stat(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or(0);

    (total, free.min(total))
}

/// Parses `vm_stat` output, counted in pages whose size is announced in the header.
///
/// The equivalent of `MemAvailable`: free pages alone underestimate massively, because macOS
/// keeps as inactive and speculative everything it can hand back on demand.
pub fn parse_vm_stat(text: &str) -> u64 {
    // "Mach Virtual Memory Statistics: (page size of 16384 bytes)"
    let page_size = text
        .lines()
        .next()
        .and_then(|line| line.split("page size of ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(4096);

    let pages = |name: &str| -> u64 {
        text.lines()
            .find_map(|line| line.trim_start().strip_prefix(name)?.strip_prefix(':'))
            // Counters are followed by a period: "Pages free:   123456."
            .and_then(|value| value.trim().trim_end_matches('.').parse::<u64>().ok())
            .unwrap_or(0)
    };

    let total_pages = pages("Pages free")
        + pages("Pages inactive")
        + pages("Pages speculative")
        + pages("Pages purgeable");
    total_pages * page_size / (1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darwin_measurement_does_not_lie_outside_macos() {
        // This test runs on every CI platform. Where `sysctl` and `vm_stat` do not exist, the
        // function must return zero without panicking; where they do (macOS, and a BSD `sysctl`
        // elsewhere), free memory never exceeds the total.
        let (total, free) = darwin_memory();
        assert!(free <= total, "free ({free}) > total ({total})");
    }

    #[test]
    fn parses_vm_stat_output() {
        // Real output from an Apple Silicon Mac, 16 KiB pages.
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)
                    Pages free:                              65536.
                    Pages active:                           524288.
                    Pages inactive:                         131072.
                    Pages speculative:                       32768.
                    Pages throttled:                             0.
                    Pages wired down:                       262144.
                    Pages purgeable:                         16384.
                    Pages purged:                             4096.
";
        // (65536 + 131072 + 32768 + 16384) pages x 16 KiB = 3,840 MiB
        assert_eq!(parse_vm_stat(text), 3840);
    }

    #[test]
    fn unreadable_vm_stat_promises_nothing() {
        // Truncated output or a missing command must not invent memory.
        assert_eq!(parse_vm_stat(""), 0);
        assert_eq!(parse_vm_stat("Mach Virtual Memory Statistics:
Pages free: n/a
"), 0);
    }

    #[test]
    fn metal_budget_does_not_promise_all_memory() {
        // macOS refuses to pin all of the RAM to the GPU. Promising 16 GiB on a 16 GiB machine
        // would plan an offload the driver will refuse.
        let (total, free) = metal_budget(16_384, 12_000);
        assert_eq!(total, 12_288);
        assert_eq!(free, 12_000);

        // Free memory is bounded by the budget, never the other way round.
        let (total, free) = metal_budget(16_384, 15_000);
        assert_eq!(total, 12_288);
        assert_eq!(free, 12_288, "we never promise more than the driver cap");

        // No measurement, no promise: this is what prevents announcing a phantom GPU.
        assert_eq!(metal_budget(0, 0), (0, 0));
    }

    #[test]
    fn parses_nvidia_smi_output() {
        let text = "NVIDIA GeForce RTX 4070 SUPER, 12282, 509\n";
        let gpus = parse_nvidia_smi(text);
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 4070 SUPER");
        assert_eq!(gpus[0].vram_total_mib, 12282);
        assert_eq!(gpus[0].vram_free_mib, 509);
        assert_eq!(gpus[0].backend, GpuBackend::Cuda);
    }

    #[test]
    fn several_gpus_are_ranked_by_free_memory() {
        let text = "GPU A, 24576, 2048\nGPU B, 12282, 11000\n";
        let hardware = Hardware {
            gpus: parse_nvidia_smi(text),
            cpu_threads: 8,
            ..Default::default()
        };
        assert_eq!(hardware.best_gpu().unwrap().name, "GPU B");
    }

    #[test]
    fn unreadable_output_yields_no_gpu() {
        assert!(parse_nvidia_smi("").is_empty());
        assert!(parse_nvidia_smi("nvidia-smi: command not found").is_empty());
        assert!(parse_nvidia_smi("GPU without numbers, abc, def").is_empty());
    }

    #[test]
    fn inconsistent_free_memory_is_clamped() {
        let gpus = parse_nvidia_smi("GPU, 8192, 99999\n");
        assert_eq!(gpus[0].vram_free_mib, 8192, "free cannot exceed total");
    }

    #[test]
    fn display_reserve_is_subtracted() {
        let hardware = Hardware {
            gpus: parse_nvidia_smi("GPU, 12282, 11489\n"),
            cpu_threads: 16,
            ..Default::default()
        };
        assert_eq!(hardware.usable_vram_mib(1024), 10465);
        // A reserve larger than what is available must not underflow.
        assert_eq!(hardware.usable_vram_mib(99_999), 0);
    }

    #[test]
    fn without_gpu_usable_vram_is_zero() {
        let hardware = Hardware { cpu_threads: 8, ..Default::default() };
        assert!(!hardware.has_gpu());
        assert_eq!(hardware.usable_vram_mib(0), 0);
    }

    #[test]
    fn parses_meminfo() {
        let text = "MemTotal:       32764620 kB\nMemFree:         1234567 kB\nMemAvailable:   20971520 kB\n";
        let (total, available) = parse_meminfo(text);
        assert_eq!(total, 31996);
        assert_eq!(available, 20480, "MemAvailable, not MemFree");
    }

    #[test]
    fn display_adapters_with_memory_are_cards_virtual_displays_are_not() {
        let k = r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
        let names = format!(
            "{k}\0000\n    DriverDesc    REG_SZ    NVIDIA GeForce RTX 4070 SUPER\n\n\
             {k}\0001\n    DriverDesc    REG_SZ    Sunshine Virtual Display Driver\n\n\
             {k}\0002\n    DriverDesc    REG_SZ    AMD Radeon RX 7800 XT\n\n\
             {k}\0003\n    DriverDesc    REG_SZ    Intel(R) UHD Graphics 770\n\n"
        );
        let memory = format!(
            "{k}\0000\n    HardwareInformation.qwMemorySize    REG_QWORD    0x2ffa00000\n\n\
             {k}\0002\n    HardwareInformation.qwMemorySize    REG_QWORD    0x400000000\n\n\
             {k}\0003\n    HardwareInformation.qwMemorySize    REG_QWORD    0x8000000\n\n\
             Fin de la recherche : 3 correspondance(s) trouvée(s).\n"
        );
        let gpus = parse_display_adapters(&names, &memory);
        let names: Vec<&str> = gpus.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["NVIDIA GeForce RTX 4070 SUPER", "AMD Radeon RX 7800 XT"], "no virtual display, no 128 MiB chip");
        assert_eq!(gpus[0].backend, GpuBackend::Cuda);
        assert_eq!(gpus[0].vram_total_mib, 12_282);
        assert_eq!(gpus[1].backend, GpuBackend::Vulkan);
        assert_eq!(gpus[1].vram_total_mib, 16_384);
        assert!(parse_display_adapters("", "").is_empty());
    }

    #[test]
    fn parses_wmic() {
        let text = "\r\nFreePhysicalMemory=10485760\r\nTotalVisibleMemorySize=33554432\r\n";
        let (total, free) = parse_wmic_memory(text);
        assert_eq!(total, 32768);
        assert_eq!(free, 10240);
    }

    #[test]
    fn one_thread_is_left_to_the_system() {
        let hardware = Hardware { cpu_threads: 16, ..Default::default() };
        assert_eq!(hardware.inference_threads(), 15);
        // A single-core machine must not end up with zero threads.
        let single = Hardware { cpu_threads: 1, ..Default::default() };
        assert_eq!(single.inference_threads(), 1);
    }

    #[test]
    fn with_two_cards_the_server_is_restricted_to_the_freest() {
        // Real output from the development machine: Strata occupies the 4070 SUPER.
        let gpus = parse_nvidia_smi(
            "NVIDIA GeForce RTX 3070 Ti, 8192, 6447, 0\nNVIDIA GeForce RTX 4070 SUPER, 12282, 609, 1\n",
        );
        let machine = Hardware { gpus, ram_total_mib: 65536, ram_free_mib: 8000, cpu_threads: 12 };
        assert_eq!(
            machine.device_pin(),
            vec![
                ("CUDA_DEVICE_ORDER".to_owned(), "PCI_BUS_ID".to_owned()),
                ("CUDA_VISIBLE_DEVICES".to_owned(), "0".to_owned()),
            ]
        );
        let single = Hardware { gpus: machine.gpus[..1].to_vec(), ..machine.clone() };
        assert!(single.device_pin().is_empty(), "a single card: nothing to restrict");
    }
}
