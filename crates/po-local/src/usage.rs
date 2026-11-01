//! What the machine is using right now — processor, memory, graphics cards, disks — for the
//! phone's view of a session's model: how loaded the PC is while the model works.
//!
//! Measured on demand (a quarter of a second for the processor's share), nothing kept running.

use std::process::Command;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    /// Share of all processor cores in use, 0–100.
    pub cpu_percent: f32,
    pub ram_used_mib: u64,
    pub ram_total_mib: u64,
    pub gpus: Vec<GpuUsage>,
    pub disks: Vec<DiskUsage>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuUsage {
    pub name: String,
    pub percent: f32,
    pub vram_used_mib: u64,
    pub vram_total_mib: u64,
    pub temperature_c: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskUsage {
    /// `C:` on Windows, the mount point elsewhere.
    pub name: String,
    pub free_gib: f32,
    pub total_gib: f32,
}

/// Measures everything; blocking (a quarter of a second): call it off an async thread.
pub fn measure() -> Usage {
    let (ram_total_mib, ram_available_mib) = crate::hardware::memory();
    Usage {
        cpu_percent: cpu_percent(Duration::from_millis(250)),
        ram_used_mib: ram_total_mib.saturating_sub(ram_available_mib),
        ram_total_mib,
        gpus: gpus(),
        disks: disks(),
    }
}

fn gpus() -> Vec<GpuUsage> {
    let Ok(output) = Command::new("nvidia-smi")
        .args(["--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu", "--format=csv,noheader,nounits"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_gpus(&String::from_utf8_lossy(&output.stdout))
}

/// The CSV `nvidia-smi` gives for the query above.
pub fn parse_gpus(text: &str) -> Vec<GpuUsage> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(str::trim).collect();
            let [name, percent, used, total, temperature] = fields[..] else { return None };
            Some(GpuUsage {
                name: name.trim_start_matches("NVIDIA ").trim_start_matches("GeForce ").to_owned(),
                percent: percent.parse().unwrap_or(0.0),
                vram_used_mib: used.parse().ok()?,
                vram_total_mib: total.parse().ok()?,
                temperature_c: temperature.parse().ok(),
            })
        })
        .collect()
}

#[cfg(windows)]
fn cpu_percent(over: Duration) -> f32 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    fn read() -> Option<(u64, u64)> {
        let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut idle, mut kernel, mut user) = (zero, zero, zero);
        // SAFETY: three valid out-pointers to FILETIME values that outlive the call.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        let value = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        // The kernel time includes the idle time.
        Some((value(idle), value(kernel) + value(user)))
    }
    let Some((idle_a, total_a)) = read() else { return 0.0 };
    std::thread::sleep(over);
    let Some((idle_b, total_b)) = read() else { return 0.0 };
    busy_share(idle_b.saturating_sub(idle_a), total_b.saturating_sub(total_a))
}

#[cfg(target_os = "linux")]
fn cpu_percent(over: Duration) -> f32 {
    fn read() -> Option<(u64, u64)> {
        let text = std::fs::read_to_string("/proc/stat").ok()?;
        parse_proc_stat(&text)
    }
    let Some((idle_a, total_a)) = read() else { return 0.0 };
    std::thread::sleep(over);
    let Some((idle_b, total_b)) = read() else { return 0.0 };
    busy_share(idle_b.saturating_sub(idle_a), total_b.saturating_sub(total_a))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn cpu_percent(_over: Duration) -> f32 {
    0.0
}

fn busy_share(idle: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (100.0 * (total - idle.min(total)) as f32 / total as f32).clamp(0.0, 100.0) }
}

/// Idle and total time from the first line of `/proc/stat`.
pub fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().next()?.strip_prefix("cpu ")?;
    let values: Vec<u64> = line.split_whitespace().filter_map(|v| v.parse().ok()).collect();
    // user nice system idle iowait irq softirq steal
    let idle = values.get(3)? + values.get(4).copied().unwrap_or(0);
    Some((idle, values.iter().take(8).sum()))
}

#[cfg(windows)]
fn disks() -> Vec<DiskUsage> {
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
    const DRIVE_FIXED: u32 = 3;
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `root` is a NUL-terminated wide string that outlives the calls.
        if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let (mut free, mut total) = (0u64, 0u64);
        // SAFETY: valid out-pointers; the third one (total free) may be null.
        if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) } == 0 || total == 0 {
            continue;
        }
        out.push(DiskUsage { name: format!("{letter}:"), free_gib: free as f32 / 1_073_741_824.0, total_gib: total as f32 / 1_073_741_824.0 });
    }
    out
}

#[cfg(not(windows))]
fn disks() -> Vec<DiskUsage> {
    // `df` is everywhere and says the same as statvfs, mount by mount.
    let Ok(output) = Command::new("df").args(["-kP", "/", "/home"]).output() else { return Vec::new() };
    let mut out: Vec<DiskUsage> = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 || out.iter().any(|d| d.name == fields[5]) {
            continue;
        }
        let total: f32 = fields[1].parse().unwrap_or(0.0);
        let free: f32 = fields[3].parse().unwrap_or(0.0);
        out.push(DiskUsage { name: fields[5].to_owned(), free_gib: free / 1_048_576.0, total_gib: total / 1_048_576.0 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cards_and_the_processor_read_from_their_tools() {
        let gpus = parse_gpus("NVIDIA GeForce RTX 3090, 87, 20480, 24576, 71\nNVIDIA GeForce RTX 3070 Ti, 0, 512, 8192, [N/A]\n");
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "RTX 3090");
        assert_eq!(gpus[0].vram_used_mib, 20480);
        assert_eq!(gpus[0].temperature_c, Some(71));
        assert_eq!(gpus[1].temperature_c, None);
        assert_eq!(parse_proc_stat("cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3"), Some((850, 1000)));
        assert!((busy_share(850, 1000) - 15.0).abs() < 0.01);
        assert_eq!(busy_share(0, 0), 0.0);
    }

    #[test]
    fn a_measure_says_something_sensible_here() {
        let usage = measure();
        assert!(usage.ram_total_mib > 0 && usage.ram_used_mib <= usage.ram_total_mib);
        assert!((0.0..=100.0).contains(&usage.cpu_percent));
    }
}
