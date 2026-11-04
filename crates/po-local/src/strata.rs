//! Strata: finding an install, reading the models it was set up for, and running one.
//!
//! Strata is not a single binary like `llama-server`: it is a folder (a source checkout that ran
//! its setup, or the desktop app's install) holding a Python server (`serve/server.py`), its
//! engine (`engine/strata.exe`) and one **run config** per model set up (`strata-<tag>.json`):
//! the engine's arguments, the model files, the port, the card. Launching a model is
//! `python serve/server.py --engine strata --config strata-<tag>.json --port <port>` from that
//! folder - exactly what the `run-<tag>.bat` files written by its setup do.
//!
//! Everything here only **reads** an install: nothing is created, copied or moved in it. And a
//! Strata that Po did not start is never stopped by Po: it may be the user's production server,
//! started by hand or by the app, and serving someone else.
//!
//! Where an install is found, in order (the same places Strata's own `discovery.py` looks at):
//! the folder the user gave Po, `STRATA_INSTALL_DIR`, the installs remembered in Strata's
//! settings (`%APPDATA%\Strata\settings.json`, `installs`), the desktop app's install folder
//! (`%LOCALAPPDATA%\Strata\install`) and the installs its settings name (`app-settings.json`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::placement::{Draft, Placement};

/// The default port of Strata's server, and the one its setup writes in every run config.
pub const DEFAULT_PORT: u16 = 8080;
/// How many run configs are read from one folder, at most (Strata itself lists 128).
const MAX_CONFIGS: usize = 128;
/// A run config is a few kilobytes; a bigger file named like one is not read.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// How an install was found: shown to the user, who can then tell a guess from their own choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The exact path the user set.
    UserPath,
    /// Found in the folder the user gave.
    UserFolder,
    /// `STRATA_INSTALL_DIR`.
    EnvVar,
    /// Remembered in Strata's own settings file.
    StrataSettings,
    /// The desktop app's install folder.
    App,
}

/// A model Strata was set up for: one run config.
#[derive(Debug, Clone, PartialEq)]
pub struct RunConfig {
    pub path: PathBuf,
    /// `iq3_xxs` for `strata-iq3_xxs.json`.
    pub tag: String,
    /// The name the server gives the model (`qwen3.8-flash-next-iq3_xxs`).
    pub model_name: String,
    pub port: u16,
    /// The model's files named by the engine's arguments (both shards, in order).
    pub model_files: Vec<PathBuf>,
    /// The files among them that are not on disk: the config cannot start.
    pub missing: Vec<PathBuf>,
    /// The first shard's size: the memory Strata's own tiers count (RAM + VRAM).
    pub shard1_bytes: u64,
    /// The size on disk of every model file found.
    pub size_bytes: u64,
    /// The quantization read from the file names (`IQ3_XXS`).
    pub quant: Option<String>,
    /// Cards it runs on, as `nvidia-smi` numbers them.
    pub gpus: Vec<u32>,
    pub engine_exe: Option<PathBuf>,
    /// It runs a draft head (`--mtp`), which a split can move to its first card.
    pub mtp: bool,
}

impl RunConfig {
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty() && !self.model_files.is_empty()
    }
}

/// A Strata install found on the machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    pub dir: PathBuf,
    pub origin: Origin,
    /// The Python that runs its server; `None` when neither its `.venv` nor a bundled one is there.
    pub python: Option<PathBuf>,
    /// `serve/server.py`, when the folder has it (the desktop app's install may not).
    pub server: Option<PathBuf>,
    /// The engine's version (`engine/BUILD.json`).
    pub version: Option<String>,
    pub configs: Vec<RunConfig>,
    /// Strata's data folder (models, packs), from its settings.
    pub data_dir: Option<PathBuf>,
}

impl Install {
    /// Can Po start a model from it?
    pub fn launchable(&self) -> bool {
        self.python.is_some() && self.server.is_some()
    }
}

/// What the environment says, gathered once: tests give their own.
#[derive(Debug, Clone, Default)]
pub struct Places {
    pub install_dir_var: Option<PathBuf>,
    pub app_data: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl Places {
    pub fn system() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            install_dir_var: var("STRATA_INSTALL_DIR"),
            app_data: var("APPDATA"),
            local_app_data: var("LOCALAPPDATA").or_else(|| var("XDG_DATA_HOME")),
            home: dirs::home_dir(),
        }
    }

    /// Strata's settings file of the command line (`settings.json`).
    fn settings_file(&self) -> Option<PathBuf> {
        if cfg!(windows) {
            self.app_data.clone().or_else(|| self.home.as_ref().map(|h| h.join("AppData").join("Roaming"))).map(|d| d.join("Strata").join("settings.json"))
        } else {
            self.home.as_ref().map(|h| h.join(".config").join("strata").join("settings.json"))
        }
    }

    /// The desktop app's folder (`%LOCALAPPDATA%\Strata`, `~/.local/share/strata`).
    fn app_root(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "Strata" } else { "strata" };
        self.local_app_data
            .clone()
            .or_else(|| self.home.as_ref().map(|h| if cfg!(windows) { h.join("AppData").join("Local") } else { h.join(".local").join("share") }))
            .map(|d| d.join(name))
    }
}

/// Reads a JSON file of at most `MAX_CONFIG_BYTES`.
fn read_json(path: &Path) -> Option<Value> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// `installs` of a Strata settings file, at most twenty, never one in a recycle bin.
fn installs_in(settings: &Value) -> Vec<PathBuf> {
    settings["installs"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .take(20)
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| !in_trash(p))
                .collect()
        })
        .unwrap_or_default()
}

/// A Strata folder deleted with the file manager still exists in the recycle bin: it must not
/// come back as an install (Strata #3410).
fn in_trash(path: &Path) -> bool {
    path.components().any(|c| {
        let part = c.as_os_str().to_string_lossy().to_ascii_lowercase();
        part == "$recycle.bin" || part == ".trash" || part.starts_with(".trash-")
    })
}

fn is_run_config(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("strata-") && lower.ends_with(".json") && !lower.ends_with(".shared-settings.json") && lower.len() > "strata-.json".len()
}

/// Does this folder look like a Strata install (run configs, or its server)?
pub fn looks_like_install(dir: &Path) -> bool {
    dir.join("serve").join("server.py").is_file() || has_run_configs(dir)
}

fn has_run_configs(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().take(10_000).any(|e| is_run_config(&e.file_name().to_string_lossy())))
        .unwrap_or(false)
}

/// The value after `flag` in a list of arguments.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

/// Reads one run config. `None` when the file is not one (not JSON, no model).
pub fn parse_run_config(path: &Path, value: &Value) -> Option<RunConfig> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let tag = name.get("strata-".len()..name.len().saturating_sub(".json".len()))?.to_owned();
    let args: Vec<String> = value["args"].as_array()?.iter().filter_map(|a| a.as_str().map(str::to_owned)).collect();
    // The engine reads its model through `--native` (shard 1) and `--ple-gguf` (shard 2); some
    // families name a single GGUF with `-m` or `--model`.
    let mut model_files: Vec<PathBuf> = ["--native", "--ple-gguf", "-m", "--model"]
        .iter()
        .filter_map(|flag| flag_value(&args, flag))
        .map(PathBuf::from)
        .collect();
    model_files.dedup();
    if model_files.is_empty() {
        model_files.push(PathBuf::from(value["vision"]["model"].as_str()?));
    }
    let mut size = 0u64;
    let mut shard1 = 0u64;
    let mut missing = Vec::new();
    for (i, file) in model_files.iter().enumerate() {
        match std::fs::metadata(file) {
            Ok(meta) if meta.is_file() => {
                size += meta.len();
                if i == 0 {
                    shard1 = meta.len();
                }
            }
            _ => missing.push(file.clone()),
        }
    }
    let model_name = value["model_name"].as_str().filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("strata-{tag}"));
    let quant = model_files.first().and_then(|f| f.file_name()).and_then(|n| crate::quant::from_name(&n.to_string_lossy())).or_else(|| crate::quant::from_name(&tag));
    Some(RunConfig {
        path: path.to_path_buf(),
        tag,
        model_name,
        port: value["port"].as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p > 0).unwrap_or(DEFAULT_PORT),
        model_files,
        missing,
        shard1_bytes: shard1,
        size_bytes: size,
        quant,
        gpus: value["gpu"].as_array().map(|g| g.iter().filter_map(|v| v.as_u64()).filter_map(|v| u32::try_from(v).ok()).collect()).unwrap_or_default(),
        engine_exe: value["exe"].as_str().map(PathBuf::from),
        mtp: args.iter().any(|a| a == "--mtp") && value["mtp"].as_bool() != Some(false),
    })
}

/// Every run config of a folder, in name order.
pub fn run_configs_in(dir: &Path) -> Vec<RunConfig> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .take(10_000)
        .filter(|e| is_run_config(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
        .into_iter()
        .take(MAX_CONFIGS)
        .filter_map(|p| {
            let value = read_json(&p)?;
            parse_run_config(&p, &value)
        })
        .collect()
}

/// The Python that runs an install's server: the bundled one first (the Terminal's archive),
/// then the folder's own environment - what `START-HERE.bat` picks.
pub fn python_of(dir: &Path) -> Option<PathBuf> {
    let candidates: &[&[&str]] = if cfg!(windows) {
        &[&["python", "python.exe"], &[".venv", "Scripts", "python.exe"]]
    } else {
        &[&["python", "bin", "python3"], &[".venv", "bin", "python3"], &[".venv", "bin", "python"]]
    };
    candidates.iter().map(|parts| parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part))).find(|p| p.is_file())
}

/// The engine's version, from `engine/BUILD.json` (`0.1.47`).
pub fn version_of(dir: &Path) -> Option<String> {
    let build = read_json(&dir.join("engine").join("BUILD.json"))?;
    let version = build["version"].as_str()?.trim();
    (!version.is_empty() && version.len() < 40).then(|| version.to_owned())
}

/// The install a user-given path designates: the folder itself, or the folder of a Python of
/// its `.venv`, of `serve/server.py`, of `engine/strata.exe`, of a run config. A beginner pastes
/// whatever file they found; Po finds the install from it.
pub fn install_from_path(path: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    if path.is_dir() {
        return looks_like_install(path).then(|| (path.to_path_buf(), None));
    }
    if !path.is_file() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let ancestors: Vec<&Path> = path.ancestors().skip(1).take(4).collect();
    let python = name.starts_with("python");
    for dir in ancestors {
        if looks_like_install(dir) {
            return Some((dir.to_path_buf(), python.then(|| path.to_path_buf())));
        }
    }
    None
}

/// Folders, at most two levels under `root`, that are installs. A user who points at
/// `E:\LLM` rather than `E:\LLM\Strata` still gets their install.
pub fn installs_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if looks_like_install(root) {
        found.push(root.to_path_buf());
    }
    let mut level = vec![root.to_path_buf()];
    for _ in 0..2 {
        let mut next = Vec::new();
        for dir in &level {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            for entry in entries.flatten().take(2_000) {
                let Ok(kind) = entry.file_type() else { continue };
                if !kind.is_dir() || crate::catalog::skipped_dir(&entry.file_name()) {
                    continue;
                }
                let path = entry.path();
                // A backup copy of an install (`Strata-backup-…`) is an install too, but not the
                // one to run: it is listed after the others.
                if looks_like_install(&path) {
                    found.push(path.clone());
                }
                next.push(path);
            }
        }
        level = next;
    }
    found.sort_by_key(|p| p.strip_prefix(root).unwrap_or(p).to_string_lossy().to_ascii_lowercase().contains("backup"));
    found
}

/// What the user set for Strata in Po's preferences.
#[derive(Debug, Clone, Default)]
pub struct Choice {
    pub executable: Option<PathBuf>,
    pub search_dir: Option<PathBuf>,
    /// More folders, searched after `search_dir`.
    pub more_dirs: Vec<PathBuf>,
    pub extra_dirs: Vec<PathBuf>,
}

/// The result of a search: the install, and what went wrong with the user's own choices.
#[derive(Debug, Clone, Default)]
pub struct Detection {
    pub install: Option<Install>,
    /// Other installs found (shown, not used).
    pub others: Vec<PathBuf>,
    /// The path the user set does not designate an install.
    pub bad_executable: Option<PathBuf>,
    /// The folder the user gave holds no install.
    pub bad_search_dir: Option<PathBuf>,
}

/// Finds Strata on the machine.
pub fn detect(choice: &Choice, places: &Places) -> Detection {
    let mut detection = Detection::default();
    let mut candidates: Vec<(PathBuf, Origin, Option<PathBuf>)> = Vec::new();

    if let Some(exe) = &choice.executable {
        match install_from_path(exe) {
            Some((dir, python)) => candidates.push((dir, Origin::UserPath, python)),
            None => detection.bad_executable = Some(exe.clone()),
        }
    }
    let mut searched_any = false;
    for dir in choice.search_dir.iter().chain(&choice.more_dirs) {
        let under = installs_under(dir);
        if under.is_empty() {
            detection.bad_search_dir.get_or_insert_with(|| dir.clone());
        }
        searched_any |= !under.is_empty();
        candidates.extend(under.into_iter().map(|d| (d, Origin::UserFolder, None)));
    }
    // One of the folders holds it: the others are simply more places, not a mistake.
    if searched_any {
        detection.bad_search_dir = None;
    }
    if let Some(dir) = &places.install_dir_var {
        candidates.push((dir.clone(), Origin::EnvVar, None));
    }
    let settings = places.settings_file().and_then(|f| read_json(&f));
    if let Some(settings) = &settings {
        candidates.extend(installs_in(settings).into_iter().map(|d| (d, Origin::StrataSettings, None)));
    }
    let app_settings = places.app_root().and_then(|root| read_json(&root.join("app-settings.json")));
    if let Some(root) = places.app_root() {
        candidates.push((root.join("install"), Origin::App, None));
    }
    if let Some(app) = &app_settings {
        candidates.extend(installs_in(app).into_iter().map(|d| (d, Origin::App, None)));
    }

    let mut seen: Vec<PathBuf> = Vec::new();
    for (dir, origin, python) in candidates {
        let key = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if seen.contains(&key) || !looks_like_install(&dir) {
            continue;
        }
        seen.push(key);
        if detection.install.is_some() {
            detection.others.push(dir);
            continue;
        }
        let mut configs = run_configs_in(&dir);
        for extra in &choice.extra_dirs {
            for config in run_configs_in(extra) {
                if !configs.iter().any(|c| c.path == config.path) {
                    configs.push(config);
                }
            }
        }
        let server = Some(dir.join("serve").join("server.py")).filter(|p| p.is_file());
        let data_dir = settings
            .as_ref()
            .or(app_settings.as_ref())
            .and_then(|s| s["data_dir"].as_str())
            .map(PathBuf::from)
            .filter(|p| p.is_dir());
        detection.install = Some(Install {
            python: python.or_else(|| python_of(&dir)),
            server,
            version: version_of(&dir),
            configs,
            data_dir,
            dir,
            origin,
        });
    }
    detection
}

/// What a running Strata server says about itself (`GET /v1/status`, read-only).
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub port: u16,
    pub model: String,
    pub loaded: bool,
    pub engine: Option<String>,
    pub in_flight: u64,
}

/// Reads `/v1/status`'s answer; `None` when it is not Strata's.
pub fn parse_status(port: u16, body: &str) -> Option<Live> {
    let value: Value = serde_json::from_str(body).ok()?;
    if value["service"].as_str()? != "strata" {
        return None;
    }
    Some(Live {
        port,
        model: value["model"].as_str().unwrap_or("?").to_owned(),
        loaded: value["loaded"].as_bool().unwrap_or(false),
        engine: value["engine"].as_str().map(str::to_owned),
        in_flight: value["activity"]["in_flight"].as_u64().unwrap_or(0),
    })
}

/// Asks the given ports whether a Strata answers there. Only `GET /v1/status`, which changes
/// nothing on the server: no completion, no load, no unload.
pub async fn probe(ports: &[u16]) -> Vec<Live> {
    let Ok(client) = reqwest::Client::builder().connect_timeout(Duration::from_millis(300)).timeout(Duration::from_secs(2)).build() else {
        return Vec::new();
    };
    let mut ports = ports.to_vec();
    ports.sort_unstable();
    ports.dedup();
    let probes = ports.into_iter().map(|port| {
        let client = client.clone();
        async move {
            let body = client.get(format!("http://127.0.0.1:{port}/v1/status")).send().await.ok()?.text().await.ok()?;
            parse_status(port, &body)
        }
    });
    // In parallel: a closed port costs up to the connection timeout, and there may be several.
    futures_util::future::join_all(probes).await.into_iter().flatten().collect()
}

/// The arguments that start a run config's server, after the Python interpreter.
pub fn launch_args(install: &Path, config: &Path, port: u16) -> Vec<String> {
    vec![
        install.join("serve").join("server.py").display().to_string(),
        "--engine".into(),
        "strata".into(),
        "--config".into(),
        config.display().to_string(),
        "--port".into(),
        port.to_string(),
    ]
}

/// What changes for one start of a run config: a model's profile, game mode. Strata reads its
/// settings from the run config only, so a change goes into a **copy** of it, written outside
/// Strata's folder: the user's config is never edited.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Overrides {
    /// `--max-context`.
    pub context: Option<u32>,
    /// `--kv`: `int8`, `q4_0`, `k8v4`.
    pub kv: Option<String>,
    /// The config's `reasoning_effort`: `none`, `low`, `medium`, `high`.
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub min_p: Option<f32>,
    /// `--pool-workers`: fewer processor workers for the experts (game mode).
    pub pool_workers: Option<u32>,
    /// Below normal priority (game mode), inherited by the engine the server starts.
    pub low_priority: bool,
    /// The cards chosen for it ([`Placement`]): one, a split, the draft head on the split's
    /// first card, a second card as an expert cache.
    pub placement: Option<Placement>,
    /// A preset of the user's ([`Preset`]): settings of their own added to the config, which
    /// otherwise runs as Strata's developers wrote it.
    pub preset: Option<Preset>,
    /// The machine's cards (nvidia-smi numbers): a preset may put the vision encoder on one the
    /// engine does not use.
    pub cards: Vec<u32>,
    /// Another chat template (over the preset's): `depot` for the one in Strata's folder
    /// (`serve/chat_template.jinja`), or a `.jinja` file. None: the model's own (its tokenizer's).
    pub template: Option<String>,
}

/// A set of settings kept apart from Strata's config, chosen per model (`/profil` → preset):
/// a file `strata/<name>.json` of an extension (`/catalog`), installed beside Po's `local.json`:
///
/// ```json
/// { "description": "…", "args": {"--spec-min-p": "0.7", "--adapt-async": "1"},
///   "vision_args": {"--vram-reserve-mib": "450"}, "env": {"STRATA_RING_BYTES": "1"},
///   "learn_profile": true, "vision_on_spare_card": true, "template": "depot" }
/// ```
///
/// `args` are set (a value replaced if the flag is there; `""`: a flag alone), `vision_args`
/// only when the config runs vision, `drop` removed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vision_args: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drop: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The expert profile saved back as it runs (`--expert-profile-save`, same file).
    #[serde(default)]
    pub learn_profile: bool,
    /// The vision encoder on a card the engine does not use.
    #[serde(default)]
    pub vision_on_spare_card: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// The folder where extensions put their Strata presets.
pub fn presets_dir() -> Option<PathBuf> {
    Some(crate::prefs::path()?.parent()?.join("strata"))
}

/// The presets there, by name, sorted.
pub fn presets() -> Vec<(String, Preset)> {
    presets_dir().map(|d| presets_in(&d)).unwrap_or_default()
}

pub fn presets_in(dir: &Path) -> Vec<(String, Preset)> {
    let mut out: Vec<(String, Preset)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().into_owned();
            let preset = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
            Some((name, preset))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Puts a preset into a run config.
fn apply_preset(object: &mut serde_json::Map<String, Value>, args: &mut Vec<Value>, preset: &Preset, cards: &[u32]) {
    let has = |args: &Vec<Value>, flag: &str| args.iter().any(|a| a.as_str() == Some(flag));
    for flag in &preset.drop {
        let with_value = args.iter().position(|a| a.as_str() == Some(flag.as_str())).and_then(|i| args.get(i + 1)).and_then(Value::as_str).is_some_and(|v| !v.starts_with("--"));
        drop_flag(args, flag, with_value);
    }
    let vision = has(args, "--vision");
    for (flag, value) in preset.args.iter().chain(preset.vision_args.iter().filter(|_| vision)) {
        if value.is_empty() {
            if !has(args, flag) {
                args.push(Value::String(flag.clone()));
            }
        } else {
            set_flag(args, flag, value.clone());
        }
    }
    if preset.learn_profile
        && !has(args, "--expert-profile-save")
        && let Some(i) = args.iter().position(|a| a.as_str() == Some("--expert-profile"))
        && let Some(profile) = args.get(i + 1).and_then(Value::as_str).map(str::to_owned)
    {
        set_flag(args, "--expert-profile-save", profile);
    }
    if !preset.env.is_empty() {
        let mut env = object.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
        for (k, v) in &preset.env {
            env.insert(k.clone(), Value::String(v.clone()));
        }
        object.insert("env".into(), Value::Object(env));
    }
    if !preset.vision_on_spare_card {
        return;
    }
    let engine: Vec<u32> = match object.get("gpu") {
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_u64).map(|c| c as u32).collect(),
        Some(v) => v.as_u64().map(|c| vec![c as u32]).unwrap_or_default(),
        None => Vec::new(),
    };
    let spare = cards.iter().copied().find(|c| !engine.is_empty() && !engine.contains(c));
    if let (Some(spare), Some(vision)) = (spare, object.get_mut("vision").and_then(Value::as_object_mut)) {
        let now = vision.get("cuda_device").and_then(Value::as_u64).map(|c| c as u32);
        if now.is_none_or(|c| engine.contains(&c) || !cards.contains(&c)) {
            vision.insert("cuda_device".into(), Value::from(spare));
        }
    }
}

/// A tokenizer folder of Po's own beside the copy: the model's files, with `template` as its
/// chat template (Strata takes the tokenizer's `chat_template.jinja` first).
fn with_template(object: &mut serde_json::Map<String, Value>, template: &Path, out_dir: &Path, stem: &str) -> std::io::Result<()> {
    let source = object.get("tokenizer").and_then(Value::as_str).map(PathBuf::from).ok_or_else(|| std::io::Error::other("no tokenizer in the config"))?;
    let dir = out_dir.join(format!("{stem}.tokenizer"));
    std::fs::create_dir_all(&dir)?;
    for entry in std::fs::read_dir(&source)?.flatten() {
        if entry.path().is_file() && entry.file_name() != "chat_template.jinja" {
            let target = dir.join(entry.file_name());
            let same = std::fs::metadata(&target).ok().map(|m| m.len()) == entry.metadata().ok().map(|m| m.len());
            if !same {
                std::fs::copy(entry.path(), target)?;
            }
        }
    }
    std::fs::copy(template, dir.join("chat_template.jinja"))?;
    object.insert("tokenizer".into(), Value::String(dir.display().to_string()));
    Ok(())
}

/// The file a template choice names: `depot` / `repo`, Strata's own beside `config_dir`.
pub fn template_file(choice: &str, config_dir: &Path) -> Option<PathBuf> {
    let path = match choice {
        "depot" | "dépôt" | "repo" | "froggeric" => config_dir.join("serve").join("chat_template.jinja"),
        other => PathBuf::from(other),
    };
    path.is_file().then_some(path)
}

/// The template a tokenizer folder holds, by its version line when it says one.
pub fn template_name(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().find(|l| l.contains("template_version"))?;
    Some(line.split('"').nth(1)?.to_owned())
}

impl Overrides {
    /// Does the config itself have to change?
    pub fn touches_config(&self) -> bool {
        Self { low_priority: false, cards: Vec::new(), ..self.clone() } != Self::default()
    }
}

/// Sets `flag value` in an argument list, replacing the value if the flag is there.
fn set_flag(args: &mut Vec<Value>, flag: &str, value: String) {
    match args.iter().position(|a| a.as_str() == Some(flag)) {
        Some(i) if i + 1 < args.len() => args[i + 1] = Value::String(value),
        Some(_) => args.push(Value::String(value)),
        None => {
            args.push(Value::String(flag.to_owned()));
            args.push(Value::String(value));
        }
    }
}

/// Removes `flag` (and its value when it takes one) wherever it is in an argument list.
fn drop_flag(args: &mut Vec<Value>, flag: &str, with_value: bool) {
    while let Some(i) = args.iter().position(|a| a.as_str() == Some(flag)) {
        let end = if with_value { (i + 2).min(args.len()) } else { i + 1 };
        args.drain(i..end);
    }
}

/// Puts a placement into a run config, the way Strata's own settings say it
/// (`docs/MULTI_GPU.md`, `docs/SECOND_GPU.md`): `"gpu"` lists the cards with the leading one
/// last, kept in that order (`"gpu_order": "as_given"`); several cards are a layer split
/// (`"layer_split": "auto"`), with the draft head moved to the first card by the pipeline
/// (`--pipeline-windows 2`, `STRATA_MTP_STAGE0=1`) when asked; a second card as an expert cache
/// is `--peer-device 1`. Whatever the config said about cards before is replaced: the roles,
/// another split, the helper caches.
fn place(object: &mut serde_json::Map<String, Value>, args: &mut Vec<Value>, placement: &Placement) {
    for flag in ["--layer-split", "--split-device", "--peer-device", "--expert-cache-device1", "--expert-cache-device2", "--expert-cache-device3", "--expert-cache-remote-placement", "--pipeline-windows"] {
        drop_flag(args, flag, true);
    }
    for flag in ["--remote-expert-opt", "--split-skip-if-fits"] {
        drop_flag(args, flag, false);
    }
    object.remove("gpu_roles");
    object.remove("gpu_none");
    let mut env = object.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
    env.remove("STRATA_MTP_STAGE0");
    let mut cards = placement.gpus.clone();
    match placement.experts {
        Some(card) => {
            cards.truncate(1);
            cards.push(card);
            set_flag(args, "--peer-device", "1".into());
            object.remove("layer_split");
        }
        None if cards.len() >= 2 => {
            object.insert("layer_split".into(), Value::String("auto".into()));
            if let Some(Draft::Card(card)) = placement.draft
                && Some(card) == cards.first().copied()
            {
                set_flag(args, "--pipeline-windows", "2".into());
                env.insert("STRATA_MTP_STAGE0".into(), Value::String("1".into()));
            }
        }
        None => {
            object.remove("layer_split");
        }
    }
    object.insert("gpu".into(), Value::Array(cards.iter().map(|c| Value::from(*c)).collect()));
    object.insert("gpu_order".into(), Value::String("as_given".into()));
    // The config's own variable would win over the server's choice from "gpu": it says the same.
    env.insert("CUDA_DEVICE_ORDER".into(), Value::String("PCI_BUS_ID".into()));
    env.insert("CUDA_VISIBLE_DEVICES".into(), Value::String(cards.iter().map(u32::to_string).collect::<Vec<_>>().join(",")));
    object.insert("env".into(), Value::Object(env));
}

/// Writes the run config with the overrides into `out_dir`, and returns the copy's path.
///
/// The copy keeps every absolute path of the original (the engine, the model, the packs), its
/// `cwd` made explicit, and the settings file Strata keeps next to a config is copied too. The
/// engine's log goes next to the copy: nothing is written into Strata's folder.
pub fn overlay_config(config: &RunConfig, overrides: &Overrides, port: u16, out_dir: &Path) -> std::io::Result<PathBuf> {
    let original_dir = config.path.parent().unwrap_or(Path::new("."));
    let inside = |dir: &Path| {
        let a = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let b = original_dir.canonicalize().unwrap_or_else(|_| original_dir.to_path_buf());
        a.starts_with(&b)
    };
    std::fs::create_dir_all(out_dir)?;
    if inside(out_dir) {
        return Err(std::io::Error::other("the copy of a Strata config must not go into Strata's folder"));
    }
    let mut value = read_json(&config.path).ok_or_else(|| std::io::Error::other(format!("{} unreadable", config.path.display())))?;
    let object = value.as_object_mut().ok_or_else(|| std::io::Error::other("not a JSON object"))?;
    let mut args: Vec<Value> = object.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    if let Some(context) = overrides.context {
        set_flag(&mut args, "--max-context", context.to_string());
    }
    if let Some(kv) = &overrides.kv {
        set_flag(&mut args, "--kv", kv.clone());
    }
    if let Some(workers) = overrides.pool_workers {
        set_flag(&mut args, "--pool-workers", workers.to_string());
    }
    if let Some(placement) = overrides.placement.as_ref().filter(|p| !p.is_auto()) {
        place(object, &mut args, placement);
    }
    if let Some(preset) = &overrides.preset {
        apply_preset(object, &mut args, preset, &overrides.cards);
    }
    object.insert("args".into(), Value::Array(args));
    if let Some(choice) = overrides.template.as_ref().or(overrides.preset.as_ref().and_then(|p| p.template.as_ref())) {
        let file = template_file(choice, original_dir).ok_or_else(|| std::io::Error::other(format!("chat template not found: {choice}")))?;
        let stem = config.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "strata-po".into());
        with_template(object, &file, out_dir, &stem)?;
    }
    let mut sampling = object.get("sampling").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut put = |key: &str, v: Option<serde_json::Number>| {
        if let Some(v) = v {
            sampling.insert(key.into(), Value::Number(v));
        }
    };
    let num = |f: Option<f32>| f.and_then(|f| serde_json::Number::from_f64((f as f64 * 1000.0).round() / 1000.0));
    put("temperature", num(overrides.temperature));
    put("top_p", num(overrides.top_p));
    put("min_p", num(overrides.min_p));
    put("top_k", overrides.top_k.map(serde_json::Number::from));
    if !sampling.is_empty() {
        object.insert("sampling".into(), Value::Object(sampling));
    }
    if let Some(effort) = &overrides.reasoning_effort {
        object.insert("reasoning_effort".into(), Value::String(effort.clone()));
    }
    if object.get("cwd").and_then(Value::as_str).is_none_or(str::is_empty) {
        object.insert("cwd".into(), Value::String(original_dir.display().to_string()));
    }
    let name = config.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "strata-po.json".into());
    let copy = out_dir.join(&name);
    object.insert("log".into(), Value::String(copy.with_extension("engine.log").display().to_string()));
    object.insert("port".into(), Value::from(port));
    object.insert("open_browser".into(), Value::Bool(false));
    std::fs::write(&copy, serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?)?;
    // Strata keeps the settings changed from its page next to the config: the copy reads the same.
    let shared = |p: &Path| PathBuf::from(format!("{}.shared-settings.json", p.with_extension("").display()));
    if shared(&config.path).is_file() {
        let _ = std::fs::copy(shared(&config.path), shared(&copy));
    }
    Ok(copy)
}

/// The engine log a run config names (the `log` key), where its loading stages are written.
pub fn engine_log_of(config_path: &Path) -> Option<PathBuf> {
    read_json(config_path)?["log"].as_str().filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// What a loading Strata is doing, from the end of its logs, in the user's words.
pub fn progress(tail: &[String]) -> Option<String> {
    use po_i18n::t;
    let detail = |line: &str| line.split_once('(').and_then(|(_, r)| r.split_once(')')).map(|(d, _)| format!("({d})")).unwrap_or_default();
    for line in tail.iter().rev() {
        let l = line.as_str();
        if l.contains("[strata] almost ready") {
            return Some(t!("local.progress.strata_almost").to_owned());
        }
        if l.contains("filling the GPU's expert cache") {
            return Some(t!("local.progress.strata_gpu_cache", detail = detail(l)));
        }
        if l.contains("experts loaded:") {
            return Some(t!("local.progress.strata_experts_loaded").to_owned());
        }
        if l.contains("loading the experts into RAM") {
            return Some(t!("local.progress.strata_experts_ram", detail = detail(l)));
        }
        if l.contains("reading the model's weights") {
            return Some(t!("local.progress.strata_weights").to_owned());
        }
        if l.contains("loading the vision encoder") || l.contains("strata-vision") {
            return Some(t!("local.progress.strata_vision").to_owned());
        }
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{}", po_i18n::t!("local.strata.err.no_python"))]
    NoPython,
    #[error("{}", po_i18n::t!("local.strata.err.incomplete", files = .0))]
    Incomplete(String),
    #[error("{}", po_i18n::t!("local.strata.err.port_taken", port = .0))]
    PortTaken(u16),
    #[error("{}", po_i18n::t!("local.supervisor.err.spawn", detail = .0))]
    Spawn(String),
    #[error("{}", po_i18n::t!("local.strata.err.died", detail = .0))]
    Died(String),
    #[error("{}", po_i18n::t!("local.strata.err.timeout", log = .0))]
    Timeout(String),
    #[error("{}", po_i18n::t!("local.strata.err.cancelled"))]
    Cancelled,
}

/// A Strata server Po started. Stopping it stops everything it started (its engine and vision
/// processes included): on Windows a job object, on Unix a process group.
pub struct Running {
    child: tokio::process::Child,
    #[cfg(windows)]
    job: Option<job::Job>,
    pub port: u16,
    pub config: RunConfig,
    /// The server's own output.
    pub log: PathBuf,
    /// The engine's log, where the loading stages are written.
    pub engine_log: Option<PathBuf>,
    pub started_at: Instant,
}

impl Running {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
    /// Has the process quit on its own?
    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Stops the server and everything it started.
    pub async fn stop(mut self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.kill();
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // The server starts its engine as a child: the whole group goes.
            let _ = std::process::Command::new("kill").args(["-TERM", &format!("-{pid}")]).status();
        }
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// How long a model is given to load. Strata's own reload of a 47 GB model took 123 s on the
/// development machine; a slower disk must not be cut off.
pub const READY_TIMEOUT: Duration = Duration::from_secs(900);

/// Starts a run config's server on `port`, and waits until it says the model is loaded.
///
/// The caller checks first that no Strata already serves that port: starting a second one would
/// load a second copy of a model that fills the card.
pub async fn start(install: &Install, config: &RunConfig, port: u16, timeout: Duration) -> Result<Running, StartError> {
    start_with(install, config, port, timeout, &Overrides::default()).await
}

/// Where the logs and config copies of the Strata servers Po starts go.
pub fn work_dir() -> PathBuf {
    std::env::temp_dir().join("po-strata")
}

/// [`start`], with a profile's settings and game mode.
pub async fn start_with(install: &Install, config: &RunConfig, port: u16, timeout: Duration, overrides: &Overrides) -> Result<Running, StartError> {
    start_watch(install, config, port, timeout, overrides, None).await
}

/// Same, with a flag watched while the model loads: when the user asks to stop, the wait ends at
/// once and the server is stopped here, through the ordinary path (a launch future dropped
/// mid-wait would kill only the launcher and leave the engine it had started holding the card).
pub async fn start_watch(
    install: &Install,
    config: &RunConfig,
    port: u16,
    timeout: Duration,
    overrides: &Overrides,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Running, StartError> {
    let python = install.python.as_ref().ok_or(StartError::NoPython)?;
    if !config.missing.is_empty() {
        let files: Vec<String> = config.missing.iter().map(|f| f.display().to_string()).collect();
        return Err(StartError::Incomplete(files.join(", ")));
    }
    if !crate::supervisor::port_is_free(port) {
        return Err(StartError::PortTaken(port));
    }
    let log = std::env::temp_dir().join(format!("po-strata-{port}.log"));
    // Always through a copy, even without a profile: the config names its log inside Strata's
    // folder, and a start by Po must write nothing there. The copy logs next to itself.
    let copy = overlay_config(config, overrides, port, &work_dir().join(port.to_string())).map_err(|e| StartError::Spawn(e.to_string()))?;
    let engine_log = engine_log_of(&copy);
    if let Some(engine_log) = &engine_log {
        let _ = std::fs::remove_file(engine_log);
    }
    let config_path = copy;
    let log_file = std::fs::File::create(&log).map_err(|e| StartError::Spawn(e.to_string()))?;
    let log_err = log_file.try_clone().map_err(|e| StartError::Spawn(e.to_string()))?;
    let mut command = tokio::process::Command::new(python);
    command
        .args(launch_args(&install.dir, &config_path, port))
        .current_dir(&install.dir)
        // Its own setup sets it: a folder or user name in another alphabet stays intact.
        .env("PYTHONUTF8", "1")
        // Python would otherwise write its bytecode cache into `serve/__pycache__`: inside
        // Strata's folder, which a start by Po leaves untouched (observed).
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(std::process::Stdio::null())
        // Into a file rather than a pipe: the server writes a lot, and a pipe nobody reads
        // fills up and freezes it mid-load.
        .stdout(log_file)
        .stderr(log_err)
        .kill_on_drop(true);
    // Started suspended: the `python.exe` of a virtual environment is a launcher that starts the
    // real interpreter at once. Put in the job only after that, the interpreter (and the engine
    // it starts) would escape it and outlive a stop - holding the card's memory.
    #[cfg(windows)]
    command.creation_flags(crate::catalog::NO_CONSOLE | job::CREATE_SUSPENDED | if overrides.low_priority { job::BELOW_NORMAL } else { 0 });
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(unix)]
    if overrides.low_priority {
        // Run through `nice`: the same program, its arguments after.
        let std_command = command.as_std();
        let mut niced = tokio::process::Command::new("nice");
        niced.arg("-n").arg("10").arg(std_command.get_program()).args(std_command.get_args());
        niced.current_dir(&install.dir).env("PYTHONUTF8", "1").env("PYTHONDONTWRITEBYTECODE", "1").stdin(std::process::Stdio::null()).kill_on_drop(true).process_group(0);
        let out = std::fs::File::create(&log).map_err(|e| StartError::Spawn(e.to_string()))?;
        let err = out.try_clone().map_err(|e| StartError::Spawn(e.to_string()))?;
        niced.stdout(out).stderr(err);
        command = niced;
    }
    let child = command.spawn().map_err(|e| StartError::Spawn(e.to_string()))?;
    #[cfg(windows)]
    let job = {
        let job = job::Job::new().filter(|job| child.raw_handle().is_some_and(|h| job.assign(h)));
        if let Some(handle) = child.raw_handle() {
            job::resume(handle);
        }
        job
    };
    let mut running = Running {
        child,
        #[cfg(windows)]
        job,
        port,
        config: config.clone(),
        log: log.clone(),
        engine_log,
        started_at: Instant::now(),
    };
    match wait_ready(&mut running, timeout, cancel.as_ref()).await {
        Ok(()) => Ok(running),
        Err(err) => {
            running.stop().await;
            Err(err)
        }
    }
}

/// Last lines of the log, for an error message.
pub fn log_tail(log: &Path) -> String {
    let text = std::fs::read(log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(4)..].join(" | ")
}

async fn wait_ready(running: &mut Running, timeout: Duration, cancel: Option<&Arc<AtomicBool>>) -> Result<(), StartError> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().map_err(|e| StartError::Spawn(e.to_string()))?;
    let url = format!("http://127.0.0.1:{}/health", running.port);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if running.exited() {
            return Err(StartError::Died(log_tail(&running.log)));
        }
        if let Ok(response) = client.get(&url).send().await
            && response.status().is_success()
            && let Ok(body) = response.json::<Value>().await
            // The API answers before the model is in memory: ready means loaded.
            && body["loaded"].as_bool().unwrap_or(true)
        {
            return Ok(());
        }
        if cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err(StartError::Cancelled);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(StartError::Timeout(running.log.display().to_string()))
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };

    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;
    /// `BELOW_NORMAL_PRIORITY_CLASS`: inherited by the engine the server starts.
    pub const BELOW_NORMAL: u32 = 0x0000_4000;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        /// Resumes every thread of a process: a process started suspended has only its main
        /// thread, whose handle `std` does not keep.
        fn NtResumeProcess(process: HANDLE) -> i32;
    }

    pub fn resume(process: std::os::windows::io::RawHandle) {
        // SAFETY: the handle is the live process held by its `Child`.
        unsafe { NtResumeProcess(process as HANDLE) };
    }

    /// A job that ends its processes when closed: if Po dies, the server it started dies too
    /// instead of holding the card's memory.
    pub struct Job(HANDLE);

    // SAFETY: a job handle can be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Option<Self> {
            // SAFETY: an anonymous job with default security; the handle is checked.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            // SAFETY: a zeroed structure is a valid "no limit", only the flag is set.
            unsafe {
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
            }
            Some(Job(handle))
        }

        pub fn assign(&self, process: std::os::windows::io::RawHandle) -> bool {
            // SAFETY: both handles are alive: the job here, the process held by its `Child`.
            unsafe { AssignProcessToJobObject(self.0, process as HANDLE) != 0 }
        }

        pub fn kill(&self) {
            // SAFETY: the handle is alive until drop.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle was created by `new` and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The user's real run config, trimmed to what Po reads, with the model in `dir`.
    fn write_config(install: &Path, tag: &str, models: &Path, with_files: bool) -> PathBuf {
        let shard1 = models.join(format!("Qwen3.8-Flash-Next-GSQ-RCO-{}-00001-of-00002.gguf", tag.to_uppercase()));
        let shard2 = models.join(format!("Qwen3.8-Flash-Next-GSQ-RCO-{}-00002-of-00002.gguf", tag.to_uppercase()));
        if with_files {
            std::fs::create_dir_all(models).unwrap();
            std::fs::write(&shard1, vec![0u8; 3000]).unwrap();
            std::fs::write(&shard2, vec![0u8; 1000]).unwrap();
        }
        let config = serde_json::json!({
            "args": ["--pack", "x", "--native", shard1, "--ple-gguf", shard2, "--max-context", "131072"],
            "cwd": install,
            "env": {"CUDA_VISIBLE_DEVICES": "1"},
            "exe": install.join("engine").join("strata.exe"),
            "gpu": [1],
            "model_name": format!("qwen3.8-flash-next-{tag}"),
            "port": 8080,
            "slot_save_path": "E:\\LLM\\Strata-data\\sessions"
        });
        let path = install.join(format!("strata-{tag}.json"));
        std::fs::create_dir_all(install).unwrap();
        std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        path
    }

    fn fake_install(root: &Path) -> PathBuf {
        let install = root.join("Strata");
        std::fs::create_dir_all(install.join("serve")).unwrap();
        std::fs::write(install.join("serve").join("server.py"), "# fake").unwrap();
        let venv = if cfg!(windows) { install.join(".venv").join("Scripts").join("python.exe") } else { install.join(".venv").join("bin").join("python3") };
        std::fs::create_dir_all(venv.parent().unwrap()).unwrap();
        std::fs::write(&venv, "").unwrap();
        std::fs::create_dir_all(install.join("engine")).unwrap();
        std::fs::write(install.join("engine").join("BUILD.json"), "{\"version\": \"0.1.47\", \"src\": \"main-v0.2.2\"}").unwrap();
        install
    }

    fn places_for(root: &Path) -> Places {
        Places { install_dir_var: None, app_data: Some(root.join("Roaming")), local_app_data: Some(root.join("Local")), home: Some(root.join("home")) }
    }

    #[test]
    fn a_run_config_gives_its_model_quant_size_and_port() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        let path = write_config(&install, "iq3_xxs", &dir.path().join("data"), true);
        let configs = run_configs_in(&install);
        assert_eq!(configs.len(), 1);
        let config = &configs[0];
        assert_eq!(config.path, path);
        assert_eq!(config.tag, "iq3_xxs");
        assert_eq!(config.model_name, "qwen3.8-flash-next-iq3_xxs");
        assert_eq!(config.quant.as_deref(), Some("IQ3_XXS"));
        assert_eq!(config.port, 8080);
        assert_eq!(config.shard1_bytes, 3000);
        assert_eq!(config.size_bytes, 4000);
        assert_eq!(config.gpus, vec![1]);
        assert!(config.is_complete());
    }

    #[test]
    fn a_config_whose_files_are_gone_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        write_config(&install, "iq2_xs", &dir.path().join("nowhere"), false);
        let config = &run_configs_in(&install)[0];
        assert!(!config.is_complete());
        assert_eq!(config.missing.len(), 2);
    }

    #[test]
    fn files_that_are_not_run_configs_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        std::fs::write(install.join("strata-.json"), "{}").unwrap();
        std::fs::write(install.join("strata-x.shared-settings.json"), "{}").unwrap();
        std::fs::write(install.join("strata-broken.json"), "{ not json").unwrap();
        std::fs::write(install.join("strata-nomodel.json"), "{\"args\": [\"--kv\", \"int8\"]}").unwrap();
        std::fs::write(install.join("strata-array.json"), "[1, 2]").unwrap();
        std::fs::create_dir_all(install.join("strata-dir.json")).unwrap();
        std::fs::write(install.join("strata-huge.json"), vec![b' '; (MAX_CONFIG_BYTES + 1) as usize]).unwrap();
        assert!(run_configs_in(&install).is_empty());
    }

    #[test]
    fn the_install_is_found_from_strata_settings() {
        // The user's real `%APPDATA%\Strata\settings.json`.
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        write_config(&install, "iq3_xxs", &dir.path().join("data"), true);
        let settings = dir.path().join("Roaming").join("Strata");
        std::fs::create_dir_all(&settings).unwrap();
        std::fs::write(settings.join("settings.json"), serde_json::json!({"data_dir": dir.path().join("data"), "installs": [install]}).to_string()).unwrap();
        let found = detect(&Choice::default(), &places_for(dir.path()));
        let install_found = found.install.expect("found");
        assert_eq!(install_found.dir, install);
        assert_eq!(install_found.origin, Origin::StrataSettings);
        assert_eq!(install_found.version.as_deref(), Some("0.1.47"));
        assert!(install_found.launchable());
        assert_eq!(install_found.configs.len(), 1);
        assert_eq!(install_found.data_dir.as_deref(), Some(dir.path().join("data").as_path()));
    }

    #[test]
    fn nothing_is_invented_on_a_machine_without_strata() {
        let dir = tempfile::tempdir().unwrap();
        let found = detect(&Choice::default(), &places_for(dir.path()));
        assert!(found.install.is_none());
        assert!(found.others.is_empty());
    }

    #[test]
    fn the_users_folder_comes_first_even_one_level_up() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        let choice = Choice { search_dir: Some(dir.path().to_path_buf()), ..Default::default() };
        let found = detect(&choice, &places_for(dir.path()));
        assert_eq!(found.install.unwrap().origin, Origin::UserFolder);
        assert!(found.bad_search_dir.is_none());
        let _ = install;
    }

    #[test]
    fn a_backup_copy_is_not_preferred() {
        let dir = tempfile::tempdir().unwrap();
        let backup = fake_install(&dir.path().join("Strata-backup-v0.1.39"));
        let real = fake_install(&dir.path().join("x"));
        // Both under the given folder, the backup sorted first by name.
        let choice = Choice { search_dir: Some(dir.path().to_path_buf()), ..Default::default() };
        let found = detect(&choice, &places_for(dir.path()));
        assert_eq!(found.install.unwrap().dir, real);
        assert!(found.others.contains(&backup));
    }

    #[test]
    fn any_file_of_the_install_designates_it() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        let python = python_of(&install).unwrap();
        for path in [install.clone(), install.join("serve").join("server.py"), install.join("engine").join("BUILD.json"), python.clone()] {
            let (found, _) = install_from_path(&path).unwrap_or_else(|| panic!("{} not recognised", path.display()));
            assert_eq!(found, install);
        }
        assert_eq!(install_from_path(&python).unwrap().1.as_deref(), Some(python.as_path()), "a Python given is the Python used");
        assert!(install_from_path(&dir.path().join("nothing-here")).is_none());
        let choice = Choice { executable: Some(dir.path().join("nothing-here")), ..Default::default() };
        let found = detect(&choice, &places_for(dir.path()));
        assert!(found.bad_executable.is_some(), "a wrong path is reported, not silently ignored");
    }

    #[test]
    fn an_install_in_the_recycle_bin_is_not_an_install() {
        assert!(in_trash(Path::new("C:\\$Recycle.Bin\\S-1-5\\Strata")));
        assert!(in_trash(Path::new("/home/u/.local/share/Trash/.Trash-1000/Strata")) || !cfg!(unix));
        assert!(!in_trash(Path::new("E:\\LLM\\Strata")));
    }

    #[test]
    fn the_status_answer_is_read() {
        // The production server's real answer, trimmed.
        let body = r#"{"service": "strata", "model": "qwen3.8-flash-next-iq3_xxs", "loaded": true, "engine": "0.1.47", "activity": {"requests": 52, "in_flight": 0}}"#;
        let live = parse_status(8080, body).unwrap();
        assert_eq!(live.model, "qwen3.8-flash-next-iq3_xxs");
        assert!(live.loaded);
        assert_eq!(live.engine.as_deref(), Some("0.1.47"));
        assert!(parse_status(8080, r#"{"service": "other"}"#).is_none());
        assert!(parse_status(8080, "<html>").is_none());
    }

    #[test]
    fn the_launch_is_the_one_its_setup_writes() {
        let args = launch_args(Path::new("E:\\LLM\\Strata"), Path::new("E:\\LLM\\Strata\\strata-iq3_xxs.json"), 8080);
        let joined = args.join(" ");
        assert!(args[0].ends_with("server.py"));
        assert!(joined.contains("--engine strata"));
        assert!(joined.contains("--config E:\\LLM\\Strata\\strata-iq3_xxs.json"));
        assert!(joined.contains("--port 8080"));
    }

    #[test]
    fn detections_running_at_the_same_time_agree() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        for tag in ["iq3_xxs", "iq2_xs", "coder-iq1_m"] {
            write_config(&install, tag, &dir.path().join("data"), true);
        }
        let llama = dir.path().join("bin").join(crate::llama::exe_name());
        std::fs::create_dir_all(llama.parent().unwrap()).unwrap();
        std::fs::write(&llama, "").unwrap();
        let root = dir.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    let mut seen = Vec::new();
                    for _ in 0..10 {
                        let strata = detect(&Choice { search_dir: Some(root.clone()), ..Default::default() }, &places_for(&root));
                        let llama = crate::llama::detect(&crate::llama::Choice { search_dir: Some(root.join("bin")), ..Default::default() }, &crate::llama::Env::default());
                        seen.push((strata.install.map(|i| i.configs.len()), llama.located.is_some()));
                    }
                    seen
                })
            })
            .collect();
        for handle in handles {
            for (configs, llama) in handle.join().unwrap() {
                assert_eq!(configs, Some(3));
                assert!(llama);
            }
        }
    }

    #[tokio::test]
    async fn a_busy_port_is_never_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = fake_install(dir.path());
        write_config(&install_dir, "iq3_xxs", &dir.path().join("data"), true);
        let install = detect(&Choice { search_dir: Some(install_dir.clone()), ..Default::default() }, &places_for(dir.path())).install.unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let result = start(&install, &install.configs[0], port, Duration::from_secs(1)).await;
        assert!(matches!(result, Err(StartError::PortTaken(p)) if p == port));
    }

    #[tokio::test]
    async fn an_incomplete_config_is_not_started() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = fake_install(dir.path());
        write_config(&install_dir, "iq2_xs", &dir.path().join("gone"), false);
        let install = detect(&Choice { search_dir: Some(install_dir), ..Default::default() }, &places_for(dir.path())).install.unwrap();
        let result = start(&install, &install.configs[0], 1, Duration::from_secs(1)).await;
        assert!(matches!(result, Err(StartError::Incomplete(_))));
    }

    /// A fake Strata server: answers `/health` (loaded after a second), `/v1/status` and
    /// `/v1/models` on the port it is given, and starts a child of its own - the way the real one
    /// starts its engine - whose process id it writes next to the config.
    const FAKE_SERVER: &str = r#"
import json, subprocess, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
args = sys.argv[1:]
port = int(args[args.index("--port") + 1])
config = args[args.index("--config") + 1]
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
open(config + ".child", "w").write(str(child.pid))
started = time.time()
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        loaded = time.time() - started > 1
        body = {"/health": {"status": "ok", "loaded": loaded, "service": "strata"},
                "/v1/status": {"service": "strata", "model": "fake-flash", "loaded": loaded, "engine": "0.0.1"},
                "/v1/models": {"data": [{"id": "fake-flash", "max_model_len": 4096}]}}.get(self.path)
        data = json.dumps(body or {}).encode()
        self.send_response(200 if body else 404); self.send_header("content-length", str(len(data))); self.end_headers(); self.wfile.write(data)
print("fake strata on", port, flush=True)
ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
"#;

    fn python_on_path() -> Option<PathBuf> {
        let name = if cfg!(windows) { "python.exe" } else { "python3" };
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|c| c.is_file()))
    }

    fn alive(pid: u32) -> bool {
        if cfg!(windows) {
            std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
                .unwrap_or(false)
        } else {
            std::path::Path::new(&format!("/proc/{pid}")).exists()
        }
    }

    #[tokio::test]
    async fn po_starts_its_own_strata_and_stops_everything_it_started() {
        let Some(python) = python_on_path() else { return };
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Strata");
        std::fs::create_dir_all(install.join("serve")).unwrap();
        std::fs::write(install.join("serve").join("server.py"), FAKE_SERVER).unwrap();
        // A real interpreter at the place Strata's setup puts it: a virtual environment.
        let made = std::process::Command::new(&python).args(["-m", "venv", "--without-pip"]).arg(install.join(".venv")).status();
        if !made.is_ok_and(|s| s.success()) {
            return;
        }
        let config_path = write_config(&install, "fake", &dir.path().join("data"), true);
        let found = detect(&Choice { search_dir: Some(install.clone()), ..Default::default() }, &places_for(dir.path())).install.unwrap();
        assert!(found.launchable(), "{found:?}");
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let running = start(&found, &found.configs[0], port, Duration::from_secs(60)).await.expect("the fake server starts");
        assert_eq!(running.port, port);
        let live = probe(&[port]).await;
        assert_eq!(live.len(), 1);
        assert!(live[0].loaded, "ready means loaded");
        // Even without a profile the server reads a copy of the config, outside the install: the
        // config's own log is inside it, and a start by Po writes nothing there.
        let copy = work_dir().join(port.to_string()).join("strata-fake.json");
        let child: u32 = std::fs::read_to_string(format!("{}.child", copy.display())).unwrap().trim().parse().unwrap();
        assert!(!PathBuf::from(format!("{}.child", config_path.display())).exists());
        let written: Vec<PathBuf> = std::fs::read_dir(&install).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file()).collect();
        assert_eq!(written, vec![config_path.clone()], "nothing new in the install's folder");
        assert!(alive(child));
        running.stop().await;
        // The engine the server started goes with it: otherwise it would keep the card's memory.
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(child) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(!alive(child), "the server's child {child} outlived the stop");
        assert!(probe(&[port]).await.is_empty(), "nothing answers any more");
    }

    #[test]
    fn a_profile_goes_into_a_copy_and_the_users_config_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        let path = write_config(&install, "iq3_xxs", &dir.path().join("data"), true);
        std::fs::write(install.join("strata-iq3_xxs.shared-settings.json"), "{\"temperature\": 0.5}").unwrap();
        let before = std::fs::read(&path).unwrap();
        let config = run_configs_in(&install).remove(0);
        let overrides = Overrides {
            context: Some(65_536),
            kv: Some("q4_0".into()),
            reasoning_effort: Some("high".into()),
            temperature: Some(0.6),
            top_p: Some(0.95),
            top_k: Some(20),
            min_p: Some(0.0),
            pool_workers: Some(2),
            low_priority: true,
            ..Default::default()
        };
        assert!(overrides.touches_config());
        let out = dir.path().join("po-work");
        let copy = overlay_config(&config, &overrides, 18_123, &out).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before, "the original is byte for byte the same");
        assert!(copy.starts_with(&out));
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&copy).unwrap()).unwrap();
        let args: Vec<&str> = value["args"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        let after = |flag: &str| args.iter().position(|a| *a == flag).map(|i| args[i + 1]);
        assert_eq!(after("--max-context"), Some("65536"), "replaced, not added twice: {args:?}");
        assert_eq!(args.iter().filter(|a| **a == "--max-context").count(), 1);
        assert_eq!(after("--kv"), Some("q4_0"));
        assert_eq!(after("--pool-workers"), Some("2"));
        assert!(after("--native").is_some(), "the model's files are kept");
        assert_eq!(value["sampling"]["temperature"], 0.6);
        assert_eq!(value["sampling"]["top_k"], 20);
        assert_eq!(value["reasoning_effort"], "high");
        assert_eq!(value["port"], 18_123);
        assert!(value["log"].as_str().unwrap().starts_with(&out.display().to_string()), "the engine logs next to the copy");
        assert_eq!(value["exe"], serde_json::json!(install.join("engine").join("strata.exe")));
        assert!(out.join("strata-iq3_xxs.shared-settings.json").is_file(), "the page's settings follow");
        // Never into Strata's own folder.
        assert!(overlay_config(&config, &overrides, 18_123, &install.join("tmp")).is_err());
        assert!(!Overrides { low_priority: true, ..Default::default() }.touches_config(), "priority alone needs no copy");
    }

    #[test]
    fn the_cards_chosen_go_into_the_copy_the_way_strata_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        let path = write_config(&install, "iq3_xxs", &dir.path().join("data"), true);
        // What a config may already say about cards: roles, helper caches, a split of its own.
        let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["gpu_roles"] = serde_json::json!({"1": "main", "0": "off"});
        value["layer_split"] = "off".into();
        value["env"]["STRATA_RING_BYTES"] = "1".into();
        let args = value["args"].as_array_mut().unwrap();
        for a in ["--expert-cache-device1", "auto", "--remote-expert-opt", "--layer-split", "18", "--adapt-async", "1"] {
            args.push(a.into());
        }
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let config = run_configs_in(&install).remove(0);
        let out = dir.path().join("po-work");
        let copy_for = |text: &str| -> Value {
            let overrides = Overrides { placement: Some(Placement::parse(text).unwrap()), ..Default::default() };
            assert!(overrides.touches_config());
            let copy = overlay_config(&config, &overrides, 18_124, &out).unwrap();
            serde_json::from_str(&std::fs::read_to_string(copy).unwrap()).unwrap()
        };
        let flags = |v: &Value| -> Vec<String> { v["args"].as_array().unwrap().iter().filter_map(|a| a.as_str().map(str::to_owned)).collect() };
        let after = |v: &Value, flag: &str| -> Option<String> {
            let args = flags(v);
            args.iter().position(|a| a == flag).map(|i| args[i + 1].clone())
        };

        // One card: that card alone, seen by the engine under the bus order.
        let one = copy_for("0");
        assert_eq!(one["gpu"], serde_json::json!([0]));
        assert_eq!(one["env"]["CUDA_VISIBLE_DEVICES"], "0");
        assert_eq!(one["env"]["CUDA_DEVICE_ORDER"], "PCI_BUS_ID");
        assert_eq!(one["env"]["STRATA_RING_BYTES"], "1", "the config's other settings stay");
        assert!(one.get("gpu_roles").is_none() && one.get("layer_split").is_none(), "{one}");
        let args = flags(&one);
        for gone in ["--expert-cache-device1", "--remote-expert-opt", "--layer-split", "--peer-device"] {
            assert!(!args.iter().any(|a| a == gone), "{gone} left in {args:?}");
        }
        assert_eq!(after(&one, "--adapt-async").as_deref(), Some("1"), "flags about other things stay");

        // Two cards, the 4070 SUPER (1) leading: a split in that order, kept as given.
        let split = copy_for("0,1");
        assert_eq!(split["gpu"], serde_json::json!([0, 1]));
        assert_eq!(split["layer_split"], "auto");
        assert_eq!(split["gpu_order"], "as_given");
        assert_eq!(split["env"]["CUDA_VISIBLE_DEVICES"], "0,1");
        assert!(split["env"].get("STRATA_MTP_STAGE0").is_none() && after(&split, "--pipeline-windows").is_none(), "the draft head stays on the leading card");

        // The draft head on the split's first card: the pipeline puts it there.
        let drafted = copy_for("0,1/mtp=0");
        assert_eq!(after(&drafted, "--pipeline-windows").as_deref(), Some("2"));
        assert_eq!(drafted["env"]["STRATA_MTP_STAGE0"], "1");

        // The 3070 Ti as a second expert cache of a model on the 4070 SUPER.
        let peer = copy_for("1/experts=0");
        assert_eq!(peer["gpu"], serde_json::json!([1, 0]));
        assert_eq!(after(&peer, "--peer-device").as_deref(), Some("1"), "the second card of the list");
        assert!(peer.get("layer_split").is_none());
        assert_eq!(peer["env"]["CUDA_VISIBLE_DEVICES"], "1,0");

        assert_eq!(std::fs::read(&path).unwrap(), before, "the user's config is never touched");
        // Automatic: the config's own cards, untouched.
        let auto = copy_for("auto");
        assert_eq!(auto["gpu"], serde_json::json!([1]));
        assert_eq!(auto["gpu_roles"], serde_json::json!({"1": "main", "0": "off"}));
    }

    #[test]
    fn the_tuned_settings_and_another_template_go_into_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let install = fake_install(dir.path());
        let path = write_config(&install, "iq3_xxs", &dir.path().join("data"), true);
        // A config as Strata's setup writes it: its own draft threshold and reserve, the
        // model's tokenizer with its template, vision on the engine's card.
        let tokenizer = dir.path().join("pack").join("tokenizer");
        std::fs::create_dir_all(&tokenizer).unwrap();
        std::fs::write(tokenizer.join("tokenizer.json"), "{}").unwrap();
        std::fs::write(tokenizer.join("chat_template.jinja"), "the model's").unwrap();
        std::fs::write(install.join("serve").join("chat_template.jinja"), "{%- set template_version = \"qwen3.8-froggeric-v22.5.0\" %}").unwrap();
        let mut value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        value["tokenizer"] = tokenizer.display().to_string().into();
        value["vision"] = serde_json::json!({"gpu": true});
        for a in ["--expert-profile", "E:\\p.bin", "--spec-min-p", "0.5", "--vision", "--vram-reserve-mib", "700"] {
            value["args"].as_array_mut().unwrap().push(a.into());
        }
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let config = run_configs_in(&install).remove(0);
        let out = dir.path().join("po-work");
        // A preset file as the user writes it; none: the config as Strata wrote it.
        let presets = dir.path().join("presets");
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::write(
            presets.join("mine.json"),
            r#"{"description": "measured", "args": {"--spec-min-p": "0.7", "--pcie-frac": "0.55", "--adapt-async": "1", "--ple-io": "direct"},
                "vision_args": {"--vram-reserve-mib": "450"}, "env": {"STRATA_RING_BYTES": "1"},
                "learn_profile": true, "vision_on_spare_card": true, "template": "depot"}"#,
        )
        .unwrap();
        std::fs::write(presets.join("broken.json"), "{").unwrap();
        let found = presets_in(&presets);
        assert_eq!(found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["mine"], "an unreadable file is left out");
        let overrides = Overrides { preset: Some(found[0].1.clone()), cards: vec![0, 1], ..Default::default() };
        assert!(overrides.touches_config());
        let copy: Value = serde_json::from_str(&std::fs::read_to_string(overlay_config(&config, &overrides, 18_125, &out).unwrap()).unwrap()).unwrap();
        let args: Vec<String> = copy["args"].as_array().unwrap().iter().filter_map(|a| a.as_str().map(str::to_owned)).collect();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].clone());
        assert_eq!(after("--spec-min-p").as_deref(), Some("0.7"));
        assert_eq!(after("--vram-reserve-mib").as_deref(), Some("450"));
        assert_eq!(after("--adapt-async").as_deref(), Some("1"));
        assert_eq!(after("--ple-io").as_deref(), Some("direct"));
        assert_eq!(after("--pcie-frac").as_deref(), Some("0.55"));
        assert_eq!(after("--expert-profile-save").as_deref(), Some("E:\\p.bin"), "the profile keeps learning");
        assert_eq!(args.iter().filter(|a| *a == "--spec-min-p").count(), 1);
        assert_eq!(copy["env"]["STRATA_RING_BYTES"], "1");
        assert_eq!(copy["vision"]["cuda_device"], 0, "the encoder on the card the engine leaves");
        let own = PathBuf::from(copy["tokenizer"].as_str().unwrap());
        assert!(own.starts_with(&out), "{own:?}");
        assert!(own.join("tokenizer.json").is_file());
        assert_eq!(template_name(&own.join("chat_template.jinja")).as_deref(), Some("qwen3.8-froggeric-v22.5.0"));
        assert_eq!(std::fs::read_to_string(tokenizer.join("chat_template.jinja")).unwrap(), "the model's", "the pack is untouched");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the user's config is never touched");
        // The engine on both cards: no spare one, vision stays as it was.
        let both = Overrides { placement: Placement::parse("0,1"), ..overrides.clone() };
        let copy: Value = serde_json::from_str(&std::fs::read_to_string(overlay_config(&config, &both, 18_125, &out).unwrap()).unwrap()).unwrap();
        assert!(copy["vision"].get("cuda_device").is_none());
        // A template that is not there: said, not silently skipped.
        assert!(overlay_config(&config, &Overrides { template: Some("E:\\none.jinja".into()), ..Default::default() }, 18_125, &out).is_err());
        // No preset: the developers' config, its own values kept.
        let plain: Value = serde_json::from_str(&std::fs::read_to_string(overlay_config(&config, &Overrides { context: Some(32_768), ..Default::default() }, 18_125, &out).unwrap()).unwrap()).unwrap();
        let args: Vec<&str> = plain["args"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(args.windows(2).any(|w| w == ["--spec-min-p", "0.5"]) && !args.contains(&"--adapt-async"), "{args:?}");
        assert_eq!(plain["tokenizer"], tokenizer.display().to_string());
    }

    #[test]
    fn the_loading_stages_are_read_from_the_engine_log() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ram = progress(&lines(&["[strata] starting the engine: reading the model's weights", "[strata] loading the experts into RAM (12.3 of 43.0 GB)"])).unwrap();
        assert!(ram.contains("12.3 of 43.0 GB"), "{ram}");
        let cache = progress(&lines(&["[strata] filling the GPU's expert cache (40%)"])).unwrap();
        assert!(cache.contains("40%"), "{cache}");
        assert!(progress(&lines(&["[strata] almost ready"])).is_some());
        assert_eq!(progress(&lines(&["nothing to see"])), None);
    }

    #[tokio::test]
    async fn a_profile_and_game_mode_reach_the_server_through_the_copy() {
        let Some(python) = python_on_path() else { return };
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Strata");
        std::fs::create_dir_all(install.join("serve")).unwrap();
        std::fs::write(install.join("serve").join("server.py"), FAKE_SERVER).unwrap();
        if !std::process::Command::new(&python).args(["-m", "venv", "--without-pip"]).arg(install.join(".venv")).status().is_ok_and(|s| s.success()) {
            return;
        }
        let original = write_config(&install, "fake", &dir.path().join("data"), true);
        let found = detect(&Choice { search_dir: Some(install.clone()), ..Default::default() }, &places_for(dir.path())).install.unwrap();
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let overrides = Overrides { context: Some(32_768), pool_workers: Some(2), low_priority: true, ..Default::default() };
        let running = start_with(&found, &found.configs[0], port, Duration::from_secs(60), &overrides).await.expect("starts");
        // The fake server wrote its child's id next to the config it was given: the copy.
        let copy = work_dir().join(port.to_string()).join("strata-fake.json");
        assert!(PathBuf::from(format!("{}.child", copy.display())).is_file(), "the server read the copy");
        assert!(!PathBuf::from(format!("{}.child", original.display())).exists(), "not the original");
        assert!(running.engine_log.as_ref().is_some_and(|l| l.starts_with(work_dir())));
        running.stop().await;
    }

    #[tokio::test]
    async fn a_server_that_dies_while_loading_says_why() {
        let Some(python) = python_on_path() else { return };
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("Strata");
        std::fs::create_dir_all(install.join("serve")).unwrap();
        std::fs::write(install.join("serve").join("server.py"), "import sys\nprint('CUDA error: out of memory', file=sys.stderr)\nsys.exit(3)\n").unwrap();
        if !std::process::Command::new(&python).args(["-m", "venv", "--without-pip"]).arg(install.join(".venv")).status().is_ok_and(|s| s.success()) {
            return;
        }
        write_config(&install, "fake", &dir.path().join("data"), true);
        let found = detect(&Choice { search_dir: Some(install.clone()), ..Default::default() }, &places_for(dir.path())).install.unwrap();
        let port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        match start(&found, &found.configs[0], port, Duration::from_secs(60)).await {
            Err(StartError::Died(detail)) => assert!(detail.contains("out of memory"), "{detail}"),
            other => panic!("expected a death with its reason, got {:?}", other.err()),
        }
    }
}
