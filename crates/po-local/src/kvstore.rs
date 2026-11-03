//! Conversations parked on disk: the server's cache of a conversation, saved to a file before
//! another conversation takes its only slot, and loaded back when it resumes.
//!
//! A single-slot server (Strata, llama.cpp with one slot) keeps the cache of one conversation.
//! When a subagent, Po Labs or another session uses the slot, the conversation it held is lost
//! and its next request is read again in full. Measured on Strata: 27,106 tokens read again in
//! 13.9 s, against 0.33 s to load them back from an NVMe disk (650 MB, ~24 KB per token).
//!
//! The server writes the files (llama.cpp's `POST /slots/0?action=save|restore`, which Strata
//! implements too) into its own folder; Po keeps a register of the ones it created, all named
//! `po-…`, and cleans them up: one file per conversation, overwritten; deleted when the
//! conversation ends, is cleared or compacted; a size limit, the least recently used going
//! first; a maximum age; and a sweep at startup for what a crash left behind. Files that are not
//! Po's are never touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Below this size, reading the conversation again takes a few seconds: not worth a file.
pub const MIN_TOKENS: u32 = 4000;
pub const DEFAULT_QUOTA_GB: u64 = 20;
pub const DEFAULT_MAX_DAYS: u64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    file: String,
    bytes: u64,
    tokens: u64,
    /// Seconds since the Unix epoch.
    used: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Register {
    /// The server's folder, so that a session deleted while Po does not run its server
    /// (`po sessions rm`) still loses its file.
    #[serde(default)]
    dir: Option<PathBuf>,
    entries: HashMap<String, Entry>,
}

/// Deletes the file of `key` in every register under `folder`, without the server.
pub fn forget_everywhere(folder: &Path, key: &str) {
    let Ok(listing) = std::fs::read_dir(folder) else { return };
    for item in listing.flatten().filter(|i| i.path().extension().is_some_and(|e| e == "json")) {
        let path = item.path();
        let Some(mut register) = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Register>(&t).ok())
        else {
            continue;
        };
        if let (Some(entry), Some(dir)) = (register.entries.remove(key), register.dir.clone()) {
            let _ = std::fs::remove_file(dir.join(entry.file));
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&register).unwrap_or_default());
        }
    }
}

/// The parking of one local server.
#[derive(Debug, Clone)]
pub struct KvStore {
    /// Server root, without `/v1`.
    root: String,
    /// Where the server writes its files.
    dir: PathBuf,
    register: PathBuf,
    pub quota_bytes: u64,
    pub max_age: Duration,
}

/// What the parking holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    pub files: usize,
    pub quota_bytes: u64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// File name for a conversation key: only letters, digits and dashes, behind the `po-` prefix.
fn file_name(key: &str) -> String {
    let clean: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    format!("po-{clean}.bin")
}

impl KvStore {
    pub fn new(base_url: &str, dir: PathBuf, register: PathBuf) -> Self {
        Self {
            root: base_url.trim_end_matches('/').trim_end_matches("/v1").to_owned(),
            dir,
            register,
            quota_bytes: DEFAULT_QUOTA_GB * 1_000_000_000,
            max_age: Duration::from_secs(DEFAULT_MAX_DAYS * 86_400),
        }
    }

    /// Limits from the preferences (`kv_quota_gb`, `kv_max_days`); 0 keeps the default.
    pub fn with_limits(mut self, quota_gb: u64, max_days: u64) -> Self {
        if quota_gb > 0 {
            self.quota_bytes = quota_gb * 1_000_000_000;
        }
        if max_days > 0 {
            self.max_age = Duration::from_secs(max_days * 86_400);
        }
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn load(&self) -> Register {
        std::fs::read_to_string(&self.register).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn store(&self, register: &Register) {
        if let Some(parent) = self.register.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let register = Register { dir: Some(self.dir.clone()), entries: register.entries.clone() };
        let _ = std::fs::write(&self.register, serde_json::to_string_pretty(&register).unwrap_or_default());
    }

    async fn call(&self, action: &str, file: &str) -> Result<serde_json::Value, String> {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(120)).build().map_err(|e| e.to_string())?;
        let response = client
            .post(format!("{}/slots/0?action={action}", self.root))
            .json(&serde_json::json!({"filename": file}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        if !status.is_success() {
            return Err(body["error"]["message"].as_str().or(body["error"].as_str()).unwrap_or("refused").to_owned());
        }
        Ok(body)
    }

    /// Saves what the slot holds as the conversation `key`. Returns the size written; a file
    /// that alone would exceed the limit is deleted at once and the save reported as refused.
    pub async fn park(&self, key: &str) -> Result<u64, String> {
        let file = file_name(key);
        let body = self.call("save", &file).await?;
        let bytes = std::fs::metadata(self.dir.join(&file)).map(|m| m.len()).unwrap_or(0);
        let tokens = body["n_saved"].as_u64().unwrap_or(0);
        let mut register = self.load();
        register.entries.insert(key.to_owned(), Entry { file: file.clone(), bytes, tokens, used: now() });
        self.enforce(&mut register, Some(key));
        let kept = register.entries.contains_key(key);
        self.store(&register);
        if kept { Ok(bytes) } else { Err("over the disk limit".into()) }
    }

    /// Loads the conversation `key` back into the slot, if it was parked. Returns the tokens
    /// restored; `None` when there is nothing to load (the next request then reads it again).
    pub async fn unpark(&self, key: &str) -> Option<u64> {
        let mut register = self.load();
        let entry = register.entries.get_mut(key)?;
        if !self.dir.join(&entry.file).is_file() {
            register.entries.remove(key);
            self.store(&register);
            return None;
        }
        let body = self.call("restore", &entry.file.clone()).await.ok()?;
        entry.used = now();
        let tokens = body["n_restored"].as_u64().unwrap_or(entry.tokens);
        self.store(&register);
        Some(tokens)
    }

    /// Whether `key` has a file, for the session list.
    pub fn holds(&self, key: &str) -> bool {
        self.load().entries.get(key).is_some_and(|e| self.dir.join(&e.file).is_file())
    }

    /// Deletes the file of `key`: the conversation ended, was cleared or compacted.
    pub fn forget(&self, key: &str) {
        let mut register = self.load();
        if let Some(entry) = register.entries.remove(key) {
            let _ = std::fs::remove_file(self.dir.join(entry.file));
            self.store(&register);
        }
    }

    /// Deletes every file of Po's.
    pub fn clear(&self) -> Usage {
        let before = self.usage();
        let mut register = self.load();
        for (_, entry) in register.entries.drain() {
            let _ = std::fs::remove_file(self.dir.join(entry.file));
        }
        self.store(&register);
        before
    }

    /// Startup cleanup: files too old, Po files missing from the register (a crash between the
    /// save and the register), entries whose file is gone, and the size limit.
    pub fn sweep(&self) {
        let mut register = self.load();
        let known: Vec<String> = register.entries.values().map(|e| e.file.clone()).collect();
        if let Ok(listing) = std::fs::read_dir(&self.dir) {
            for item in listing.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                if name.starts_with("po-") && name.ends_with(".bin") && !known.contains(&name) {
                    let _ = std::fs::remove_file(item.path());
                }
            }
        }
        register.entries.retain(|_, e| self.dir.join(&e.file).is_file());
        self.enforce(&mut register, None);
        self.store(&register);
    }

    /// Age limit, then size limit - least recently used first; `keep` is never chosen unless
    /// it alone is over the limit.
    fn enforce(&self, register: &mut Register, keep: Option<&str>) {
        let oldest_allowed = now().saturating_sub(self.max_age.as_secs());
        let expired: Vec<String> =
            register.entries.iter().filter(|(_, e)| e.used < oldest_allowed).map(|(k, _)| k.clone()).collect();
        for key in expired {
            if let Some(entry) = register.entries.remove(&key) {
                let _ = std::fs::remove_file(self.dir.join(entry.file));
            }
        }
        loop {
            let total: u64 = register.entries.values().map(|e| e.bytes).sum();
            if total <= self.quota_bytes {
                return;
            }
            let victim = register
                .entries
                .iter()
                .filter(|(k, _)| Some(k.as_str()) != keep || register.entries.len() == 1)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { return };
            if let Some(entry) = register.entries.remove(&victim) {
                let _ = std::fs::remove_file(self.dir.join(entry.file));
            }
        }
    }

    pub fn usage(&self) -> Usage {
        let register = self.load();
        Usage {
            bytes: register.entries.values().map(|e| e.bytes).sum(),
            files: register.entries.len(),
            quota_bytes: self.quota_bytes,
        }
    }
}

/// Where a local server saves its slots, read from its command line: `--slot-save-path DIR`
/// (llama.cpp, Strata), or `slot_save_path` in the configuration file Strata was given with
/// `--config`. `PO_KV_DIR` overrides it. `None` when the server cannot save (no such option).
pub fn detect_save_dir(port: u16) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("PO_KV_DIR").map(PathBuf::from) {
        return Some(dir);
    }
    command_lines()
        .iter()
        .filter(|line| serves_port(line, port))
        .find_map(|line| save_dir_of(line))
}

/// The server listens on `port`: an explicit `--port`, or none for the usual 8080.
fn serves_port(line: &str, port: u16) -> bool {
    let words = split_args(line);
    match words.iter().position(|w| w == "--port") {
        Some(i) => words.get(i + 1).and_then(|p| p.parse::<u16>().ok()) == Some(port),
        None => port == 8080 && (line.contains("llama-server") || line.contains("serve/server.py") || line.contains("serve\\server.py")),
    }
}

pub fn save_dir_of(line: &str) -> Option<PathBuf> {
    let words = split_args(line);
    let value = |flag: &str| words.iter().position(|w| w == flag).and_then(|i| words.get(i + 1)).cloned();
    if let Some(dir) = value("--slot-save-path") {
        return Some(PathBuf::from(dir)).filter(|d| d.is_absolute() && d.is_dir());
    }
    let config = value("--config")?;
    let cfg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&config).ok()?).ok()?;
    let dir = PathBuf::from(cfg["slot_save_path"].as_str()?);
    let dir = if dir.is_absolute() {
        dir
    } else {
        PathBuf::from(cfg["cwd"].as_str().map(str::to_owned).or_else(|| Path::new(&config).parent().map(|p| p.display().to_string()))?)
            .join(dir)
    };
    dir.is_dir().then_some(dir)
}

/// Splits a command line on spaces, honoring double quotes.
fn split_args(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Command lines of the running processes.
fn command_lines() -> Vec<String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -match 'slot-save-path|--config|llama-server' } | ForEach-Object { $_.CommandLine }",
            ])
            .creation_flags(0x0800_0000)
            .output();
        output.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_owned).collect()).unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
        entries
            .flatten()
            .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
            .map(|raw| {
                raw.split(|b| *b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        let arg = String::from_utf8_lossy(p).into_owned();
                        if arg.contains(' ') { format!("\"{arg}\"") } else { arg }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_in(dir: &Path) -> KvStore {
        KvStore::new("http://127.0.0.1:1/v1", dir.to_path_buf(), dir.join("register.json"))
    }

    fn fake_file(store: &KvStore, key: &str, bytes: usize, used: u64) {
        let file = file_name(key);
        std::fs::write(store.dir.join(&file), vec![0u8; bytes]).unwrap();
        let mut register = store.load();
        register.entries.insert(key.into(), Entry { file, bytes: bytes as u64, tokens: 1, used });
        store.store(&register);
    }

    #[test]
    fn the_size_limit_drops_the_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.quota_bytes = 2500;
        fake_file(&store, "old", 1000, now() - 300);
        fake_file(&store, "mid", 1000, now() - 200);
        fake_file(&store, "new", 1000, now() - 100);
        let mut register = store.load();
        store.enforce(&mut register, Some("new"));
        store.store(&register);
        assert!(!store.holds("old"), "the oldest goes");
        assert!(store.holds("mid") && store.holds("new"));
        assert!(!dir.path().join(file_name("old")).exists(), "and its file with it");
    }

    #[test]
    fn a_file_alone_over_the_limit_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = store_in(dir.path());
        store.quota_bytes = 500;
        fake_file(&store, "huge", 1000, now());
        let mut register = store.load();
        store.enforce(&mut register, Some("huge"));
        assert!(register.entries.is_empty());
    }

    #[test]
    fn old_files_orphans_and_foreign_files_are_handled_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        fake_file(&store, "stale", 10, now() - 30 * 86_400);
        fake_file(&store, "fresh", 10, now());
        std::fs::write(dir.path().join("po-crashed.bin"), b"x").unwrap();
        std::fs::write(dir.path().join("user-session.bin"), b"x").unwrap();
        store.sweep();
        assert!(!store.holds("stale"), "older than the maximum age");
        assert!(store.holds("fresh"));
        assert!(!dir.path().join("po-crashed.bin").exists(), "a Po file missing from the register");
        assert!(dir.path().join("user-session.bin").exists(), "never a file that is not Po's");
        forget_everywhere(dir.path(), "fresh");
        assert_eq!(store.usage().files, 0, "found through the register alone");
        assert!(!dir.path().join(file_name("fresh")).exists());
    }

    #[test]
    fn the_save_folder_is_read_from_the_server_command_line() {
        let dir = tempfile::tempdir().unwrap();
        let saves = dir.path().join("sessions");
        std::fs::create_dir(&saves).unwrap();
        let config = dir.path().join("strata.json");
        std::fs::write(&config, serde_json::json!({"slot_save_path": saves.display().to_string()}).to_string()).unwrap();
        let strata = format!("\"python.exe\" E:/LLM/Strata\\serve/server.py --engine strata --config {} --port 8080", config.display());
        assert!(serves_port(&strata, 8080) && !serves_port(&strata, 8081));
        assert_eq!(save_dir_of(&strata), Some(saves.clone()));
        let llama = format!("llama-server -m model.gguf --slot-save-path \"{}\" --port 8081", saves.display());
        assert!(serves_port(&llama, 8081));
        assert_eq!(save_dir_of(&llama), Some(saves));
        assert_eq!(save_dir_of("llama-server -m model.gguf --port 8081"), None, "no option, no parking");
    }

    #[test]
    fn file_names_stay_inside_the_folder() {
        assert_eq!(file_name("18e2cb/../x"), "po-18e2cb----x.bin");
    }
}
