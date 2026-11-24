//! Catalog of the models present on the machine.
//!
//! For "just selecting a model in the harness" to be enough, we still have to know which ones
//! are there. We sweep the locations where the ecosystem's tools drop their weights, read the
//! header of every GGUF found, and deduce what the machine can actually run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::gguf::{self, GgufInfo};
use crate::hardware::Hardware;
use crate::plan::{LaunchPlan, PlanRequest, plan};

/// `CREATE_NO_WINDOW`: the process starts without a console of its own - and above all without
/// taking over Po's.
///
/// Closing standard input was not enough: `wsl.exe` attaches to the inherited console and leaves
/// its output mode altered. The carriage return then stops accompanying the line feed, and
/// everything Po writes afterwards cascades down the screen like a staircase. Seen on screen
/// from Termius, on the welcome banner, after the model inventory.
#[cfg(windows)]
pub(crate) const NO_CONSOLE: u32 = 0x0800_0000;

/// Scan depth: Hugging Face repositories nest
/// `models--org--name/snapshots/<hash>/file.gguf`, i.e. four levels.
const MAX_DEPTH: usize = 5;

#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub path: PathBuf,
    pub info: GgufInfo,
    /// What this model would yield if launched now, on this machine.
    pub plan: LaunchPlan,
}

impl CatalogEntry {
    /// Short name displayable in a picker.
    pub fn label(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.info.name.clone())
    }

    pub fn size_mib(&self) -> u64 {
        self.info.file_size_bytes / (1024 * 1024)
    }

    /// Can it run entirely on the GPU, given the current state of memory?
    pub fn fits_on_gpu(&self) -> bool {
        self.plan.viable && self.plan.is_fully_offloaded() && self.plan.n_gpu_layers > 0
    }
}

/// Locations scanned, in order. The first ones are the most specific.
pub fn search_roots() -> Vec<PathBuf> {
    search_roots_with(&crate::prefs::LocalPrefs::load())
}

/// [`search_roots`] for given preferences (llama.cpp's model folders come from them).
pub fn search_roots_with(prefs: &crate::prefs::LocalPrefs) -> Vec<PathBuf> {
    let mut roots = Vec::new();

    // The llama.cpp cache, possibly moved by the user.
    if let Ok(custom) = std::env::var("LLAMA_CACHE") {
        roots.push(PathBuf::from(custom));
    }
    if let Ok(custom) = std::env::var("PO_MODELS_DIR") {
        roots.push(PathBuf::from(custom));
    }
    // Folders added during a session by `/local dossier` or `/moteurs llama modeles`.
    for dir in prefs.llama_models_dirs() {
        roots.push(PathBuf::from(dir));
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".cache").join("llama.cpp"));
        roots.push(home.join(".lmstudio").join("models"));
        roots.push(home.join(".ollama").join("models"));
        roots.push(home.join("models"));
        // Hugging Face cache: GGUFs downloaded by `huggingface-cli` land there.
        roots.push(home.join(".cache").join("huggingface").join("hub"));
    }
    roots.push(PathBuf::from("models"));

    // The same folder can be reached by two paths (variable and default): we do not want to
    // list the same model twice.
    let mut seen = std::collections::HashSet::new();
    roots
        .into_iter()
        .filter(|root| root.exists())
        .filter_map(|root| {
            let key = root.canonicalize().unwrap_or_else(|_| root.clone());
            seen.insert(key).then_some(root)
        })
        .collect()
}

/// Scans the known locations and builds the catalog, launch plan included.
pub fn scan(hardware: &Hardware, request: &PlanRequest) -> Vec<CatalogEntry> {
    scan_with(&crate::prefs::LocalPrefs::load(), hardware, request)
}

/// [`scan`] for given preferences.
pub fn scan_with(prefs: &crate::prefs::LocalPrefs, hardware: &Hardware, request: &PlanRequest) -> Vec<CatalogEntry> {
    let mut files = Vec::new();
    for root in search_roots_with(prefs) {
        collect_gguf(&root, 0, &mut files);
    }
    files.extend(wsl_gguf_files());
    build(files, hardware, request)
}

/// GGUF files under the given folders only (no usual place, no WSL): what a folder the user
/// named holds.
pub fn gguf_files_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in dirs {
        collect_gguf(dir, 0, &mut files);
    }
    files
}

/// Builds the catalog from a list of files. Kept apart from the scan so it is testable
/// without depending on the machine's directory tree.
pub fn build(files: Vec<PathBuf>, hardware: &Hardware, request: &PlanRequest) -> Vec<CatalogEntry> {
    // The same model can appear in two locations (link, copy, shared cache): we keep only one
    // per file name.
    let mut unique: BTreeMap<String, CatalogEntry> = BTreeMap::new();

    for path in files {
        // GGUFs split into several volumes only open through their first chunk; listing the
        // others would offer models that cannot be loaded.
        if is_split_continuation(&path) {
            continue;
        }
        let Ok(info) = read_header_anywhere(&path) else { continue };
        if !info.is_usable() {
            continue;
        }
        let plan = plan(&info, hardware, request);
        let key = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        unique.entry(key).or_insert(CatalogEntry { path, info, plan });
    }

    let mut entries: Vec<_> = unique.into_values().collect();
    // Models usable without compromise first, then the biggest: that is the order in which a
    // user wants to see them offered.
    entries.sort_by(|a, b| {
        b.fits_on_gpu()
            .cmp(&a.fits_on_gpu())
            .then(b.info.file_size_bytes.cmp(&a.info.file_size_bytes))
    });
    entries
}

/// Reads a model's header, wherever it lives.
///
/// A path shaped like the WSL share cannot be opened directly: depending on the Windows
/// session, the share is visible or not, and it does not follow the Unix symlinks of the
/// Hugging Face cache. So we ask the distribution for the file and parse the stream on the
/// fly - a GGUF header is read front to back, we never need the rest.
fn read_header_anywhere(path: &Path) -> Result<GgufInfo, gguf::GgufError> {
    #[cfg(windows)]
    if let Some((distro, linux)) = wsl_parts(path) {
        return read_header_via_wsl(&distro, &linux, path);
    }
    gguf::read_header(path)
}

#[cfg(windows)]
fn read_header_via_wsl(
    distro: &str,
    linux: &str,
    path: &Path,
) -> Result<GgufInfo, gguf::GgufError> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let unreadable = || {
        gguf::GgufError::Io(std::io::Error::other(po_i18n::t!(
            "local.catalog.unreadable_from_distro",
            path = linux,
            distro = distro
        )))
    };

    // A single pass: the size first, the content next, on the same stream.
    let mut child = Command::new("wsl.exe")
        .args(["-d", distro, "--exec", "sh", "-c", r#"stat -Lc %s "$0" && exec cat "$0""#, linux])
        .creation_flags(NO_CONSOLE)
        // Input closed: without this `wsl.exe` takes over the console and leaves its output mode
        // altered - everything Po writes afterwards shows up as a staircase. Seen on screen
        // from Termius, not deduced.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| unreadable())?;

    let stdout = child.stdout.take().ok_or_else(unreadable)?;
    let mut stream = BufReader::with_capacity(64 * 1024, stdout);
    let mut first_line = String::new();
    stream.read_line(&mut first_line).map_err(|_| unreadable())?;
    let size: u64 = first_line.trim().parse().map_err(|_| unreadable())?;

    let result = gguf::read_header_from(&mut stream, size, path);

    // `cat` is still pouring gigabytes into the pipe: we only read the header, so we cut it
    // off. Without this, every inventoried model would leave a process blocked on writing.
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Models visible from the running WSL distributions.
///
/// Enumeration happens **inside** the distribution: it follows the Hugging Face cache's
/// symlinks, which a Windows-side walk could not resolve, and it also works from an SSH
/// session, where the network share does not exist.
#[cfg(windows)]
fn wsl_gguf_files() -> Vec<PathBuf> {
    const SEARCH: &str = "for h in /root /home/*; do \
         for d in .cache/llama.cpp .lmstudio/models .ollama/models models .cache/huggingface/hub; do \
           [ -d \"$h/$d\" ] && find -L \"$h/$d\" -maxdepth 6 -name '*.gguf' -type f 2>/dev/null; \
         done; \
       done";

    let mut files = Vec::new();
    for distro in running_distros() {
        let Ok(output) = std::process::Command::new("wsl.exe")
            .args(["-d", &distro, "--exec", "sh", "-c", SEARCH])
            .creation_flags(NO_CONSOLE)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        files.extend(
            text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|linux| unc_path(&distro, linux)),
        );
    }
    files
}

#[cfg(not(windows))]
fn wsl_gguf_files() -> Vec<PathBuf> {
    Vec::new()
}

/// WSL distributions that are **already started**.
///
/// The restriction is deliberate: listing models does not justify waking a sleeping virtual
/// machine behind the user's back. A stopped distribution hosts no server to drive anyway; a
/// running one does.
#[cfg(windows)]
pub(crate) fn running_distros() -> Vec<String> {
    let Ok(output) = std::process::Command::new("wsl.exe")
        .args(["--list", "--running", "--quiet"])
        .creation_flags(NO_CONSOLE)
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    // `wsl.exe` answers in UTF-16LE, even when its output is redirected.
    let units: Vec<u16> = output
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
        .lines()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Outside Windows there is no guest distribution to inventory: the machine is already the one
/// where the models live.
#[cfg(not(windows))]
#[allow(dead_code)]
pub(crate) fn running_distros() -> Vec<String> {
    Vec::new()
}

/// Splits a WSL UNC path into (distribution, Linux path).
#[cfg(windows)]
pub(crate) fn wsl_parts(path: &Path) -> Option<(String, String)> {
    let text = path.to_string_lossy().replace('/', "\\");
    let rest = text
        .strip_prefix(r#"\\wsl.localhost\"#)
        .or_else(|| text.strip_prefix(r#"\\wsl$\"#))?;
    let (distro, after) = rest.split_once('\\')?;
    Some((distro.to_owned(), format!("/{}", after.replace('\\', "/"))))
}

/// UNC path corresponding to a Linux path inside a distribution.
#[cfg(windows)]
pub(crate) fn unc_path(distro: &str, linux: &str) -> PathBuf {
    PathBuf::from(format!(
        r"\\wsl.localhost\{distro}{}",
        linux.replace('/', "\\")
    ))
}

/// A `-00002-of-00003.gguf` file is not a model in its own right.
fn is_split_continuation(path: &Path) -> bool {
    let Some(name) = path.file_stem().map(|s| s.to_string_lossy()) else {
        return false;
    };
    // Pattern `-<n>-of-<m>` where n > 1.
    let Some(position) = name.rfind("-of-") else {
        return false;
    };
    let before = &name[..position];
    let Some(dash) = before.rfind('-') else {
        return false;
    };
    before[dash + 1..]
        .parse::<u32>()
        .map(|index| index > 1)
        .unwrap_or(false)
}

/// Entries looked at in one folder tree, at most. A user who names `C:\` as a models folder
/// must get an answer, not a frozen interface while every file of the disk is listed.
const MAX_ENTRIES: usize = 200_000;
/// GGUF files kept from one folder tree, at most.
const MAX_FILES: usize = 4096;

/// Folders that never hold models and can hold a great many files.
pub(crate) fn skipped_dir(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_lowercase();
    matches!(
        name.as_str(),
        ".git" | "node_modules" | "$recycle.bin" | "system volume information" | "windows" | "__pycache__" | ".venv" | "target"
    )
}

fn collect_gguf(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let mut limits = Limits { entries: MAX_ENTRIES, files: MAX_FILES, start: out.len() };
    collect_gguf_bounded(dir, depth, out, &mut limits);
}

/// What one scan may still look at.
struct Limits {
    entries: usize,
    files: usize,
    start: usize,
}

impl Limits {
    fn exhausted(&self, out: &[PathBuf]) -> bool {
        self.entries == 0 || out.len() - self.start >= self.files
    }
}

fn collect_gguf_bounded(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, limits: &mut Limits) {
    if depth > MAX_DEPTH || limits.exhausted(out) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if limits.exhausted(out) {
            return;
        }
        limits.entries -= 1;
        let path = entry.path();
        // `file_type` does not follow links: a link is then asked about its target, so that a
        // broken one is skipped instead of failing the folder.
        let Ok(kind) = entry.file_type() else { continue };
        let is_dir = if kind.is_symlink() { path.is_dir() } else { kind.is_dir() };
        if is_dir {
            if !skipped_dir(&entry.file_name()) {
                collect_gguf_bounded(&path, depth + 1, out, limits);
            }
        } else if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gguf")) && path.is_file() {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Gpu, GpuBackend};
    use std::io::Write;

    fn machine(vram_free: u64) -> Hardware {
        Hardware {
            gpus: vec![Gpu {
                index: None,
                name: "test".into(),
                backend: GpuBackend::Cuda,
                vram_total_mib: 12_282,
                vram_free_mib: vram_free,
            }],
            ram_total_mib: 32_768,
            ram_free_mib: 20_000,
            cpu_threads: 16,
        }
    }

    /// Writes a minimal but valid GGUF, of the requested size.
    fn write_gguf(path: &Path, layers: u32, padding_mib: usize) {
        let mut out = Vec::new();
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&10u64.to_le_bytes());
        out.extend_from_slice(&5u64.to_le_bytes());

        let push_key = |out: &mut Vec<u8>, key: &str| {
            out.extend_from_slice(&(key.len() as u64).to_le_bytes());
            out.extend_from_slice(key.as_bytes());
        };
        push_key(&mut out, "general.architecture");
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&5u64.to_le_bytes());
        out.extend_from_slice(b"llama");

        for (key, value) in [
            ("llama.block_count", layers),
            ("llama.embedding_length", 4096u32),
            ("llama.attention.head_count", 32),
            ("llama.context_length", 8192),
        ] {
            push_key(&mut out, key);
            out.extend_from_slice(&4u32.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        }

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(&out).unwrap();
        if padding_mib > 0 {
            file.write_all(&vec![0u8; padding_mib * 1024 * 1024]).unwrap();
        }
    }

    #[test]
    fn the_catalog_reads_models_and_computes_their_plan() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.gguf");
        write_gguf(&small, 24, 2);

        let entries = build(vec![small], &machine(11_000), &PlanRequest::default());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label(), "small");
        assert!(entries[0].plan.viable);
        assert!(entries[0].fits_on_gpu(), "2 MiB fit in 11 GB of free VRAM");
    }

    #[test]
    fn usable_models_are_offered_first() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small.gguf");
        let big = dir.path().join("big.gguf");
        write_gguf(&small, 24, 1);
        write_gguf(&big, 80, 40);

        // Little VRAM: the big one does not fit entirely, the small one does.
        let entries = build(vec![big, small], &machine(3_000), &PlanRequest::default());
        assert_eq!(entries[0].label(), "small");
        assert!(entries[0].fits_on_gpu());
        assert!(!entries[1].fits_on_gpu());
    }

    #[test]
    fn the_same_model_seen_twice_appears_once() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let first = a.path().join("model.gguf");
        let second = b.path().join("model.gguf");
        write_gguf(&first, 24, 1);
        write_gguf(&second, 24, 1);

        let entries = build(vec![first, second], &machine(11_000), &PlanRequest::default());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn an_unreadable_file_is_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake.gguf");
        std::fs::write(&fake, b"this is not a gguf").unwrap();

        assert!(build(vec![fake], &machine(11_000), &PlanRequest::default()).is_empty());
    }

    #[test]
    fn secondary_volumes_of_a_split_model_are_dropped() {
        // Only the first chunk opens; offering the others would list models that cannot be
        // loaded.
        assert!(is_split_continuation(Path::new("big-00002-of-00003.gguf")));
        assert!(is_split_continuation(Path::new("big-00003-of-00003.gguf")));
        assert!(!is_split_continuation(Path::new("big-00001-of-00003.gguf")));
        assert!(!is_split_continuation(Path::new("normal-model.gguf")));
    }

    #[test]
    fn the_scan_descends_into_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        // Typical Hugging Face cache tree.
        let deep = dir
            .path()
            .join("models--org--name")
            .join("snapshots")
            .join("abc123")
            .join("model.gguf");
        write_gguf(&deep, 24, 1);

        let mut found = Vec::new();
        collect_gguf(dir.path(), 0, &mut found);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn the_scan_does_not_descend_forever() {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        for i in 0..12 {
            path = path.join(format!("level{i}"));
        }
        write_gguf(&path.join("too-deep.gguf"), 24, 1);

        let mut found = Vec::new();
        collect_gguf(dir.path(), 0, &mut found);
        assert!(found.is_empty(), "a pathological tree must not freeze startup");
    }

    #[test]
    fn a_huge_flat_folder_is_scanned_quickly_and_completely() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20_000 {
            std::fs::write(dir.path().join(format!("file-{i}.txt")), b"").unwrap();
        }
        write_gguf(&dir.path().join("needle.gguf"), 24, 0);
        let started = std::time::Instant::now();
        let found = gguf_files_in(&[dir.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
    }

    #[test]
    fn a_whole_disk_named_as_a_folder_stops_at_the_budget() {
        // The same rules as the real limits, scaled down: creating 200,000 files would make the
        // test itself the slow thing.
        let dir = tempfile::tempdir().unwrap();
        for d in 0..20 {
            let sub = dir.path().join(format!("d{d}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..50 {
                std::fs::write(sub.join(format!("{f}.gguf")), b"").unwrap();
            }
        }
        let mut found = Vec::new();
        collect_gguf_bounded(dir.path(), 0, &mut found, &mut Limits { entries: 300, files: usize::MAX, start: 0 });
        assert!(found.len() < 300, "the entry budget stops the walk: {}", found.len());
        let mut found = Vec::new();
        collect_gguf_bounded(dir.path(), 0, &mut found, &mut Limits { entries: usize::MAX, files: 64, start: 0 });
        assert_eq!(found.len(), 64, "the file cap stops it too");
        assert_eq!(gguf_files_in(&[dir.path().to_path_buf()]).len(), 1000, "under the real limits, everything");
    }

    #[test]
    fn junk_named_gguf_is_ignored_without_failing_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("empty.gguf"), b"").unwrap();
        std::fs::write(dir.path().join("truncated.gguf"), b"GGUF\x03\x00").unwrap();
        std::fs::create_dir_all(dir.path().join("folder.gguf")).unwrap();
        // A header announcing a 4 GB string must not be believed.
        let mut lying = b"GGUF".to_vec();
        lying.extend_from_slice(&3u32.to_le_bytes());
        lying.extend_from_slice(&1u64.to_le_bytes());
        lying.extend_from_slice(&1u64.to_le_bytes());
        lying.extend_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(dir.path().join("lying.gguf"), lying).unwrap();
        write_gguf(&dir.path().join("good.gguf"), 24, 0);
        let files = gguf_files_in(&[dir.path().to_path_buf()]);
        assert!(!files.iter().any(|f| f.ends_with("folder.gguf")), "a folder is not a model file");
        let entries = build(files, &machine(11_000), &PlanRequest::default());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label(), "good");
    }

    #[cfg(windows)]
    #[test]
    fn a_name_that_is_not_unicode_still_shows() {
        use std::os::windows::ffi::OsStringExt;
        let dir = tempfile::tempdir().unwrap();
        // A lone surrogate: a valid Windows file name that is not valid Unicode.
        let mut wide: Vec<u16> = "model-".encode_utf16().collect();
        wide.push(0xD800);
        wide.extend(".gguf".encode_utf16());
        let path = dir.path().join(std::ffi::OsString::from_wide(&wide));
        write_gguf(&path, 24, 0);
        let entries = build(gguf_files_in(&[dir.path().to_path_buf()]), &machine(11_000), &PlanRequest::default());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].label().starts_with("model-"));
    }

    // Not on macOS: APFS refuses a file name that is not UTF-8, so the case cannot arise there.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_name_that_is_not_unicode_still_shows() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(std::ffi::OsStr::from_bytes(b"model-\xff.gguf"));
        write_gguf(&path, 24, 0);
        let entries = build(gguf_files_in(&[dir.path().to_path_buf()]), &machine(11_000), &PlanRequest::default());
        assert_eq!(entries.len(), 1);
        assert!(entries[0].label().starts_with("model-"));
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_loop_and_a_dangling_junction_do_not_trap_the_scan() {
        // Junctions need no privilege (symbolic links do): `mklink /J`.
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        write_gguf(&inner.join("model.gguf"), 24, 0);
        let junction = |link: &Path, target: &Path| {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if !junction(&inner.join("loop"), dir.path()) {
            return;
        }
        let gone = dir.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        assert!(junction(&dir.path().join("dangling"), &gone));
        std::fs::remove_dir(&gone).unwrap();
        let started = std::time::Instant::now();
        let found = gguf_files_in(&[dir.path().to_path_buf()]);
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
        assert!(!found.is_empty() && found.len() <= MAX_DEPTH + 2, "the loop is bounded: {} files", found.len());
        let entries = build(found, &machine(11_000), &PlanRequest::default());
        assert_eq!(entries.len(), 1, "the same model seen through the loop counts once");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_and_an_unreadable_folder_are_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("dangling.gguf")).unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        let locked = dir.path().join("locked");
        write_gguf(&locked.join("hidden.gguf"), 24, 0);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        write_gguf(&dir.path().join("visible.gguf"), 24, 0);
        let found = gguf_files_in(&[dir.path().to_path_buf()]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let entries = build(found, &machine(11_000), &PlanRequest::default());
        assert!(entries.iter().any(|e| e.label() == "visible"));
    }

    #[test]
    fn concurrent_scans_agree() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..40 {
            write_gguf(&dir.path().join(format!("m{}", i % 5)).join(format!("model-{i}.gguf")), 24, 0);
        }
        let root = dir.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || build(gguf_files_in(&[root]), &machine(11_000), &PlanRequest::default()).len())
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), 40);
        }
    }
}
