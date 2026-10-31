//! Strata: finding an install, reading the models it was set up for, and running one.
//!
//! Strata is not a single binary like `llama-server`: it is a folder (a source checkout that ran
//! its setup, or the desktop app's install) holding a Python server (`serve/server.py`), its
//! engine (`engine/strata.exe`) and one **run config** per model set up (`strata-<tag>.json`):
//! the engine's arguments, the model files, the port, the card. Launching a model is
//! `python serve/server.py --engine strata --config strata-<tag>.json --port <port>` from that
//! folder — exactly what the `run-<tag>.bat` files written by its setup do.
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
use std::time::{Duration, Instant};

use serde_json::Value;

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
/// then the folder's own environment — what `START-HERE.bat` picks.
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
    if let Some(dir) = &choice.search_dir {
        let under = installs_under(dir);
        if under.is_empty() {
            detection.bad_search_dir = Some(dir.clone());
        }
        candidates.extend(under.into_iter().map(|d| (d, Origin::UserFolder, None)));
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
}

/// A Strata server Po started. Stopping it stops everything it started (its engine and vision
/// processes included): on Windows a job object, on Unix a process group.
pub struct Running {
    child: tokio::process::Child,
    #[cfg(windows)]
    job: Option<job::Job>,
    pub port: u16,
    pub config: RunConfig,
    pub log: PathBuf,
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
    let python = install.python.as_ref().ok_or(StartError::NoPython)?;
    if !config.missing.is_empty() {
        let files: Vec<String> = config.missing.iter().map(|f| f.display().to_string()).collect();
        return Err(StartError::Incomplete(files.join(", ")));
    }
    if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        return Err(StartError::PortTaken(port));
    }
    let log = std::env::temp_dir().join(format!("po-strata-{port}.log"));
    let log_file = std::fs::File::create(&log).map_err(|e| StartError::Spawn(e.to_string()))?;
    let log_err = log_file.try_clone().map_err(|e| StartError::Spawn(e.to_string()))?;
    let mut command = tokio::process::Command::new(python);
    command
        .args(launch_args(&install.dir, &config.path, port))
        .current_dir(&install.dir)
        // Its own setup sets it: a folder or user name in another alphabet stays intact.
        .env("PYTHONUTF8", "1")
        .stdin(std::process::Stdio::null())
        // Into a file rather than a pipe: the server writes a lot, and a pipe nobody reads
        // fills up and freezes it mid-load.
        .stdout(log_file)
        .stderr(log_err)
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(crate::catalog::NO_CONSOLE);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn().map_err(|e| StartError::Spawn(e.to_string()))?;
    #[cfg(windows)]
    let job = job::Job::new().filter(|job| child.raw_handle().is_some_and(|h| job.assign(h)));
    let mut running = Running {
        child,
        #[cfg(windows)]
        job,
        port,
        config: config.clone(),
        log: log.clone(),
        started_at: Instant::now(),
    };
    match wait_ready(&mut running, timeout).await {
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

async fn wait_ready(running: &mut Running, timeout: Duration) -> Result<(), StartError> {
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
}
