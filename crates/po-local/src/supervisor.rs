//! Lifecycle of a local inference server.
//!
//! This is the piece missing everywhere else: `pi-llama-cpp` drives a server that is **already
//! running**, `llama-swap` manages processes but ignores the agent. Here, selecting a model is
//! enough to make it run - Po finds the binary, picks a free port, derives the parameters from
//! the hardware, starts, waits until the server is really ready, and frees it when it is no
//! longer used.
//!
//! Three rules guide the implementation:
//! - we never declare "ready" before the server answers; a 20 GB model takes tens of seconds to
//!   load, and a request sent too early fails for no understandable reason;
//! - a process launched by Po is stopped by Po, even if Po dies abruptly;
//! - a server that has not served for a while gives its VRAM back without anyone having to
//!   think about it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::plan::LaunchPlan;

/// Maximum time allowed for a model to load. Deliberately generous: a big model on a
/// mechanical disk can really take several minutes.
const READY_TIMEOUT: Duration = Duration::from_secs(300);
/// Interval between two availability checks.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Idle time after which the VRAM is given back.
pub const DEFAULT_IDLE_TTL: Duration = Duration::from_secs(600);
/// Port range in which to look for a free slot.
const PORT_RANGE: std::ops::Range<u16> = 18080..18180;

#[derive(Debug, Clone, PartialEq)]
pub enum ServerState {
    Stopped,
    /// The process is launched but does not answer yet.
    Starting,
    Ready,
    /// Startup failed; relaunching identically will fail the same way.
    Failed(String),
}

impl ServerState {
    pub fn is_usable(&self) -> bool {
        *self == Self::Ready
    }

    pub fn label(&self) -> String {
        match self {
            Self::Stopped => po_i18n::t!("local.supervisor.state.stopped").to_owned(),
            Self::Starting => po_i18n::t!("local.supervisor.state.starting").to_owned(),
            Self::Ready => po_i18n::t!("local.supervisor.state.ready").to_owned(),
            Self::Failed(reason) => po_i18n::t!("local.supervisor.state.failed", reason = reason),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("{}", po_i18n::t!("local.supervisor.err.binary_not_found"))]
    BinaryNotFound,
    #[error("{}", po_i18n::t!("local.supervisor.err.no_free_port", start = PORT_RANGE.start, end = PORT_RANGE.end))]
    NoFreePort,
    #[error("{}", po_i18n::t!("local.supervisor.err.ready_timeout"))]
    ReadyTimeout,
    #[error("{}", po_i18n::t!("local.supervisor.err.died", detail = exit_code_suffix(.0), hint = hint(.1)))]
    Died(Option<i32>, String),
    #[error("{}", po_i18n::t!("local.supervisor.err.spawn", detail = .0))]
    Spawn(String),
    #[error("{}", po_i18n::t!("local.supervisor.err.unviable_plan"))]
    UnviablePlan,
    #[error("{}", po_i18n::t!("local.supervisor.err.not_running"))]
    NotRunning,
    #[error("{}", po_i18n::t!("local.supervisor.err.not_router"))]
    NotRouter,
    #[error("{}", po_i18n::t!("local.supervisor.err.request", detail = .0))]
    Request(String),
}

fn exit_code_suffix(code: &Option<i32>) -> String {
    match code {
        Some(code) => po_i18n::t!("local.supervisor.err.exit_code", code = code),
        None => String::new(),
    }
}

/// Last lines of the server's error output.
///
/// Without them, a startup failure boils down to "code 1", which helps no one: yet that is
/// where llama.cpp explains that a flag is invalid or that memory is short.
fn hint(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = trimmed.lines().rev().take(4).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    format!(" - {}", tail.join(" | "))
}

/// Server launched by Po.
pub struct RunningServer {
    child: tokio::process::Child,
    /// The server's output, in a file: the only place where llama.cpp explains a refusal to
    /// start, where its loading progress shows, and what the log view reads. A pipe nobody reads
    /// would fill up and freeze the server mid-load.
    pub log: PathBuf,
    pub port: u16,
    pub model_path: PathBuf,
    pub state: ServerState,
    started_at: Instant,
    last_used: Instant,
}

impl RunningServer {
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// How long has this server been doing nothing?
    pub fn idle_for(&self) -> Duration {
        self.last_used.elapsed()
    }

    /// To be called on every request served, to push back the release deadline.
    pub fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    pub fn model_label(&self) -> String {
        self.model_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| po_i18n::t!("local.supervisor.model_fallback").to_owned())
    }
}

/// How a server is started, beyond its launch plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    /// Arguments that replace the plan's (same flag) or are added: a model's profile.
    pub overrides: Vec<(String, Option<String>)>,
    /// The port to listen on, when free; otherwise one of Po's range.
    pub port: Option<u16>,
    /// Game mode: the process runs below normal priority, so that a game keeps the processor.
    pub low_priority: bool,
    /// Variables added to the launch (the card chosen for a worker).
    pub env: Vec<(String, String)>,
}

/// Flags that mean the same thing: an override of one replaces the other.
const SYNONYMS: &[&[&str]] = &[
    &["-c", "--ctx-size"],
    &["-ngl", "--n-gpu-layers", "--gpu-layers"],
    &["-t", "--threads"],
    &["--flash-attn", "-fa"],
    &["--cache-type-k", "-ctk"],
    &["--cache-type-v", "-ctv"],
    &["-b", "--batch-size"],
    &["-ub", "--ubatch-size"],
    &["-np", "--parallel"],
    &["--temp", "--temperature"],
    &["-ncmoe", "--n-cpu-moe"],
];

fn same_flag(a: &str, b: &str) -> bool {
    a == b || SYNONYMS.iter().any(|group| group.contains(&a) && group.contains(&b))
}

/// The plan's arguments with a profile's on top: a flag already there gets the new value, a new
/// one is appended. Pure, hence testable without a binary.
pub fn override_args(mut args: Vec<String>, overrides: &[(String, Option<String>)]) -> Vec<String> {
    for (flag, value) in overrides {
        match args.iter().position(|a| same_flag(a, flag)) {
            Some(i) => {
                args[i] = flag.clone();
                if let Some(value) = value {
                    let has_value = args.get(i + 1).is_some_and(|next| !next.starts_with('-') || next.parse::<f64>().is_ok());
                    if has_value {
                        args[i + 1] = value.clone();
                    } else {
                        args.insert(i + 1, value.clone());
                    }
                }
            }
            None => {
                args.push(flag.clone());
                if let Some(value) = value {
                    args.push(value.clone());
                }
            }
        }
    }
    args
}

/// A model's profile as `llama-server` arguments. The window is not among them: it goes into the
/// launch plan, which sizes the offload for it. Game mode leaves the processor to the game:
/// a quarter of the threads, at low priority.
pub fn profile_overrides(profile: &crate::prefs::ModelProfile, game: bool, cpu_threads: u32) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut put = |flag: &str, value: String| out.push((flag.to_owned(), Some(value)));
    if let Some(cache) = profile.cache.as_deref().filter(|c| matches!(*c, "f16" | "q8_0" | "q4_0")) {
        put("--cache-type-k", cache.to_owned());
        put("--cache-type-v", cache.to_owned());
    }
    match profile.reasoning.as_deref() {
        Some(level @ ("on" | "off" | "auto")) => put("--reasoning", level.to_owned()),
        Some(level @ ("low" | "medium" | "high")) => {
            put("--reasoning", "on".to_owned());
            put("--reasoning-effort", level.to_owned());
        }
        _ => {}
    }
    let num = |v: f32| format!("{}", (v * 1000.0).round() / 1000.0);
    if let Some(v) = profile.temperature {
        put("--temp", num(v));
    }
    if let Some(v) = profile.top_p {
        put("--top-p", num(v));
    }
    if let Some(v) = profile.top_k {
        put("--top-k", v.to_string());
    }
    if let Some(v) = profile.min_p {
        put("--min-p", num(v));
    }
    if game {
        put("-t", (cpu_threads / 4).max(2).to_string());
        put("--prio", "-1".to_owned());
    }
    out
}

/// Where a server started on `port` writes its output.
pub fn log_path(port: u16) -> PathBuf {
    std::env::temp_dir().join(format!("po-llama-{port}.log"))
}

/// The last `lines` non-empty lines of a log, read from its end (a log grows to gigabytes).
pub fn log_tail(path: &Path, lines: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let window = (lines as u64 * 400).clamp(4096, 512 * 1024);
    let _ = file.seek(SeekFrom::Start(size.saturating_sub(window)));
    let mut bytes = Vec::new();
    let _ = file.take(window).read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    // The first line read may be cut in the middle: dropped when the window did not reach the start.
    let skip = usize::from(size > window && all.len() > lines);
    let all = &all[skip..];
    all[all.len().saturating_sub(lines)..].iter().map(|l| l.to_string()).collect()
}

/// What a loading `llama-server` is doing, from the end of its log, in the user's words.
pub fn llama_progress(tail: &[String]) -> Option<String> {
    use po_i18n::t;
    for line in tail.iter().rev() {
        let l = line.as_str();
        if l.contains("server is listening") || l.contains("listening on") || l.contains("model loaded") || l.contains("all slots are idle") {
            return Some(t!("local.progress.ready").to_owned());
        }
        if l.contains("warming up the model") || l.contains("warmup") {
            return Some(t!("local.progress.warmup").to_owned());
        }
        if l.contains("llama_kv_cache") || l.contains("llama_context") || l.contains("KV buffer") {
            return Some(t!("local.progress.context").to_owned());
        }
        if l.contains("load_tensors") || l.contains("offloading") || l.contains("offloaded") {
            return Some(t!("local.progress.tensors").to_owned());
        }
        if l.contains("llama_model_loader") || l.contains("loading model") || l.contains("load_model") {
            return Some(t!("local.progress.reading").to_owned());
        }
        if l.contains("ggml_cuda_init") || l.contains("found ") && l.contains("CUDA devices") {
            return Some(t!("local.progress.devices").to_owned());
        }
    }
    None
}

/// Starts a process detached from Po's console, its output into `log`, below normal priority
/// when asked. `kill_on_drop`: if Po disappears, the server must not stay behind holding VRAM.
fn spawn_logged(mut command: tokio::process::Command, log: &Path, low_priority: bool) -> Result<tokio::process::Child, SupervisorError> {
    let out = std::fs::File::create(log).map_err(|e| SupervisorError::Spawn(e.to_string()))?;
    let err = out.try_clone().map_err(|e| SupervisorError::Spawn(e.to_string()))?;
    command.stdin(std::process::Stdio::null()).stdout(out).stderr(err).kill_on_drop(true);
    #[cfg(windows)]
    {
        /// `BELOW_NORMAL_PRIORITY_CLASS`, inherited by the processes the server starts.
        const BELOW_NORMAL: u32 = 0x0000_4000;
        let flags = crate::catalog::NO_CONSOLE | if low_priority { BELOW_NORMAL } else { 0 };
        command.creation_flags(flags);
    }
    #[cfg(unix)]
    if low_priority {
        // `nice` is a pre-exec in disguise: wrap the program instead of touching the process.
        let std_command = command.as_std();
        let program = std_command.get_program().to_owned();
        let args: Vec<_> = std_command.get_args().map(|a| a.to_owned()).collect();
        let mut niced = tokio::process::Command::new("nice");
        niced.arg("-n").arg("10").arg(program).args(args);
        niced.envs(std_command.get_envs().filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned()))));
        if let Some(dir) = std_command.get_current_dir() {
            niced.current_dir(dir);
        }
        let out = std::fs::File::create(log).map_err(|e| SupervisorError::Spawn(e.to_string()))?;
        let err = out.try_clone().map_err(|e| SupervisorError::Spawn(e.to_string()))?;
        niced.stdin(std::process::Stdio::null()).stdout(out).stderr(err).kill_on_drop(true);
        return niced.spawn().map_err(|e| SupervisorError::Spawn(e.to_string()));
    }
    #[cfg(not(any(windows, unix)))]
    let _ = low_priority;
    command.spawn().map_err(|e| SupervisorError::Spawn(e.to_string()))
}

/// Which route to take to run the binary that was found.
///
/// On Windows, "the same machine" includes WSL: that is where most users' CUDA-built llama.cpp
/// runs, and it is an ELF that Windows cannot launch. The supervisor must therefore know how to
/// cross the boundary - translate paths, go through `wsl.exe`, and above all find the Linux
/// process again to stop it, because killing the relay does not always kill what it relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launcher {
    /// A host executable, called directly.
    Native,
    /// An executable housed in a running WSL distribution.
    Wsl { distro: String },
}

impl Launcher {
    /// Infers the calling mode from the binary's path.
    pub fn for_binary(binary: &Path) -> Self {
        match wsl_distro_of(binary) {
            Some(distro) => Self::Wsl { distro },
            None => Self::Native,
        }
    }

    /// Path as the launched process will see it.
    pub fn path_for(&self, path: &Path) -> PathBuf {
        match self {
            Self::Native => path.to_path_buf(),
            Self::Wsl { .. } => to_linux_path(path)
                .map(PathBuf::from)
                .unwrap_or_else(|| path.to_path_buf()),

        }
    }
}

/// Distribution name when the path designates a file seen through WSL.
#[cfg(windows)]
fn wsl_distro_of(path: &Path) -> Option<String> {
    let distro = crate::catalog::wsl_parts(path)?.0;
    (!distro.is_empty()).then_some(distro)
}

#[cfg(not(windows))]
fn wsl_distro_of(_path: &Path) -> Option<String> {
    None
}

/// Translates a WSL share path into a Linux path; leaves others as they are.
#[cfg(windows)]
fn to_linux_path(path: &Path) -> Option<String> {
    crate::catalog::wsl_parts(path).map(|(_, linux)| linux).or_else(|| drive_to_mnt(path))
}

/// `E:\LLM\models` as seen from WSL: `/mnt/e/LLM/models`. Without this translation, a WSL binary
/// received the Windows path as is and answered that it did not exist (observed).
#[cfg(any(windows, test))]
fn drive_to_mnt(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let rest = text[2..].replace('\\', "/");
        let rest = rest.trim_start_matches('/');
        return Some(format!("/mnt/{}/{rest}", (bytes[0] as char).to_ascii_lowercase()));
    }
    None
}

#[cfg(not(windows))]
fn to_linux_path(_path: &Path) -> Option<String> {
    None
}

pub struct Supervisor {
    binary: PathBuf,
    launcher: Launcher,
    running: Option<RunningServer>,
    pub idle_ttl: Duration,
    /// True when the current server serves a model directory rather than a single model.
    router_mode: bool,
    /// Environment variables of each launch (choice of the card, notably).
    env: Vec<(String, String)>,
}

impl Supervisor {
    /// Locates the `llama-server` binary, or fails cleanly.
    pub fn discover() -> Result<Self, SupervisorError> {
        let binary = find_binary().ok_or(SupervisorError::BinaryNotFound)?;
        let launcher = Launcher::for_binary(&binary);
        Ok(Self {
            binary,
            launcher,
            running: None,
            idle_ttl: DEFAULT_IDLE_TTL,
            router_mode: false,
            env: Vec::new(),
        })
    }

    pub fn with_binary(binary: impl Into<PathBuf>) -> Self {
        let binary = binary.into();
        Self {
            launcher: Launcher::for_binary(&binary),
            binary,
            running: None,
            idle_ttl: DEFAULT_IDLE_TTL,
            router_mode: false,
            env: Vec::new(),
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn launcher(&self) -> &Launcher {
        &self.launcher
    }

    /// Variables applied to each following launch.
    pub fn set_env(&mut self, env: Vec<(String, String)>) {
        self.env = env;
    }

    /// Command ready to launch the server, whichever side of the boundary it lives on.
    fn command(&self) -> tokio::process::Command {
        match &self.launcher {
            Launcher::Native => {
                let mut cmd = tokio::process::Command::new(&self.binary);
                cmd.envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
                cmd
            }
            Launcher::Wsl { distro } => {
                let mut cmd = tokio::process::Command::new("wsl.exe");
                cmd.arg("-d").arg(distro).arg("--exec");
                // Windows' environment does not cross the boundary: `env` sets it on the Linux
                // side, right before the binary.
                if !self.env.is_empty() {
                    cmd.arg("env");
                    for (k, v) in &self.env {
                        cmd.arg(format!("{k}={v}"));
                    }
                }
                cmd.arg(self.launcher.path_for(&self.binary)).stdin(std::process::Stdio::null());
                detach_console(&mut cmd);
                cmd
            }
        }
    }

    pub fn running(&self) -> Option<&RunningServer> {
        self.running.as_ref()
    }

    pub fn running_mut(&mut self) -> Option<&mut RunningServer> {
        self.running.as_mut()
    }

    pub fn state(&self) -> ServerState {
        self.running.as_ref().map(|s| s.state.clone()).unwrap_or(ServerState::Stopped)
    }

    /// Starts a model, stopping the running one if necessary.
    ///
    /// This is the operation triggered by selecting a model in the interface: a single action,
    /// from the choice to a server ready to answer.
    pub async fn start(
        &mut self,
        model_path: &Path,
        plan: &LaunchPlan,
    ) -> Result<u16, SupervisorError> {
        self.start_with(model_path, plan, &LaunchOptions::default()).await
    }

    /// [`start`](Self::start), with a profile's arguments, a chosen port and a priority.
    pub async fn start_with(
        &mut self,
        model_path: &Path,
        plan: &LaunchPlan,
        options: &LaunchOptions,
    ) -> Result<u16, SupervisorError> {
        if !plan.viable {
            return Err(SupervisorError::UnviablePlan);
        }
        // Swapping means freeing first: launching the new server before stopping the old one
        // would make two models cohabit in a VRAM sized for one.
        self.stop().await;

        let port = options
            .port
            .filter(|p| port_is_free(*p))
            .or_else(free_port)
            .ok_or(SupervisorError::NoFreePort)?;
        // A server too old for the draft head: started without it rather than refused.
        let plan = if plan.mtp && plan.draft_layers > 0 && !knows_mtp(&self.binary) { std::borrow::Cow::Owned(plan.without_mtp()) } else { std::borrow::Cow::Borrowed(plan) };
        let args = override_args(command_args(&self.launcher.path_for(model_path), &plan, port), &options.overrides);

        let mut command = self.command();
        command.args(&args).envs(options.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let log = log_path(port);
        let child = spawn_logged(command, &log, options.low_priority)?;

        let now = Instant::now();
        self.running = Some(RunningServer {
            child,
            log,
            port,
            model_path: model_path.to_path_buf(),
            state: ServerState::Starting,
            started_at: now,
            last_used: now,
        });

        match self.wait_ready(port).await {
            Ok(()) => {
                if let Some(server) = &mut self.running {
                    server.state = ServerState::Ready;
                }
                Ok(port)
            }
            Err(err) => {
                if let Some(server) = &mut self.running {
                    server.state = ServerState::Failed(err.to_string());
                }
                Err(err)
            }
        }
    }

    /// Waits for the server to answer, also watching for its possible death.
    ///
    /// Probing the health state without watching the process would make us wait five minutes
    /// for a server that quit after two seconds for lack of memory.
    async fn wait_ready(&mut self, port: u16) -> Result<(), SupervisorError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|err| SupervisorError::Spawn(err.to_string()))?;
        let url = format!("http://127.0.0.1:{port}/health");
        let deadline = Instant::now() + READY_TIMEOUT;

        while Instant::now() < deadline {
            if let Some(server) = &mut self.running
                && let Ok(Some(status)) = server.child.try_wait()
            {
                let message = log_tail(&server.log, 8).join("\n");
                return Err(SupervisorError::Died(status.code(), message));
            }
            if let Ok(response) = client.get(&url).send().await
                && response.status().is_success()
            {
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Err(SupervisorError::ReadyTimeout)
    }

    /// Starts a server in router mode, serving every model in a directory.
    ///
    /// Unlike [`Supervisor::start`], which serves a single model, this mode keeps several models
    /// loaded simultaneously - up to `max_loaded` - and allows switching from one to another
    /// without restarting anything.
    pub async fn start_router(
        &mut self,
        models_dir: &Path,
        max_loaded: u32,
        threads: u32,
        context: u32,
    ) -> Result<u16, SupervisorError> {
        self.stop().await;

        let port = free_port().ok_or(SupervisorError::NoFreePort)?;
        // Same launcher as in single-model mode: launching the binary directly failed as soon as
        // it lived in WSL ("not a valid Win32 application", observed).
        let args = router_args(&self.launcher.path_for(models_dir), port, max_loaded, threads, context);

        let mut command = self.command();
        command.args(&args);
        let log = log_path(port);
        let child = spawn_logged(command, &log, false)?;

        let now = Instant::now();
        self.running = Some(RunningServer {
            child,
            log,
            port,
            model_path: models_dir.to_path_buf(),
            state: ServerState::Starting,
            started_at: now,
            last_used: now,
        });
        self.router_mode = true;

        match self.wait_ready(port).await {
            Ok(()) => {
                if let Some(server) = &mut self.running {
                    server.state = ServerState::Ready;
                }
                Ok(port)
            }
            Err(err) => {
                if let Some(server) = &mut self.running {
                    server.state = ServerState::Failed(err.to_string());
                }
                Err(err)
            }
        }
    }

    /// Does the current server serve several models?
    pub fn is_router(&self) -> bool {
        self.router_mode && self.state().is_usable()
    }

    /// Models known to the router, with their load state.
    pub async fn list_models(&self) -> Result<Vec<crate::server::RemoteModel>, SupervisorError> {
        let port = self.running.as_ref().ok_or(SupervisorError::NotRunning)?.port;
        let body = self
            .http()
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .send()
            .await
            .map_err(|e| SupervisorError::Request(e.to_string()))?
            .text()
            .await
            .map_err(|e| SupervisorError::Request(e.to_string()))?;
        crate::server::parse_models(&body).map_err(|e| SupervisorError::Request(e.to_string()))
    }

    /// Loads a model into the router, without touching the others.
    pub async fn load_model(&self, model_id: &str) -> Result<(), SupervisorError> {
        self.model_action("load", model_id).await
    }

    /// Unloads a model and gives its memory back, the others continuing to serve.
    pub async fn unload_model(&self, model_id: &str) -> Result<(), SupervisorError> {
        self.model_action("unload", model_id).await
    }

    async fn model_action(&self, action: &str, model_id: &str) -> Result<(), SupervisorError> {
        let server = self.running.as_ref().ok_or(SupervisorError::NotRunning)?;
        if !self.router_mode {
            return Err(SupervisorError::NotRouter);
        }
        let response = self
            .http()
            .post(format!("http://127.0.0.1:{}/models/{action}", server.port))
            .json(&serde_json::json!({"model": model_id}))
            .send()
            .await
            .map_err(|e| SupervisorError::Request(e.to_string()))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(SupervisorError::Request(po_i18n::t!(
                "local.supervisor.err.action_refused",
                action = action,
                status = response.status()
            )))
        }
    }

    fn http(&self) -> reqwest::Client {
        // A model load can be long; a status request must not be.
        reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_default()
    }

    /// Stops the current server, if there is one.
    pub async fn stop(&mut self) {
        if let Some(mut server) = self.running.take() {
            let port = server.port;
            let _ = server.child.kill().await;
            let _ = server.child.wait().await;
            // Killing `wsl.exe` kills the relay, not always the ELF at the end. Without this
            // second step, the VRAM would stay taken after a "stopped" that claims success. We
            // only target our own port - the range reserved for Po - to never touch a server
            // the user may have launched themselves.
            if let Launcher::Wsl { distro } = &self.launcher {
                let mut kill = tokio::process::Command::new("wsl.exe");
                kill
                    .args(["-d", distro, "--exec", "pkill", "-f"])
                    .arg(format!("llama-server.*--port {port}"))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                detach_console(&mut kill);
                let _ = kill.status().await;
            }
        }
        self.router_mode = false;
    }

    /// Frees the VRAM if the server has not served for `idle_ttl`.
    /// Returns true if a stop took place.
    pub async fn reap_if_idle(&mut self) -> bool {
        let expired = self
            .running
            .as_ref()
            .is_some_and(|server| server.state.is_usable() && server.idle_for() >= self.idle_ttl);
        if expired {
            self.stop().await;
        }
        expired
    }

    /// Is the current server already serving this model? Avoids a pointless swap, which would
    /// cost a full reload.
    pub fn already_serving(&self, model_path: &Path) -> bool {
        self.running
            .as_ref()
            .is_some_and(|server| server.state.is_usable() && server.model_path == model_path)
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        // `kill_on_drop` takes care of the process; we just mark the state for any observer
        // that would consult the supervisor while it is being destroyed.
        if let Some(server) = &mut self.running {
            server.state = ServerState::Stopped;
        }
    }
}

/// Arguments to launch `llama-server` in **router mode**: several models, loaded and unloaded
/// on the fly, each in its own process, requests being dispatched by the `model` field.
///
/// This mode is what allows several local models to coexist in one session. Po does not
/// reimplement this mechanism - llama.cpp does it well - but it brings what it lacks: starting
/// with hardware-derived parameters, and the link with agent routing.
pub fn router_args(models_dir: &Path, port: u16, max_loaded: u32, threads: u32, context: u32) -> Vec<String> {
    vec![
        // Without an explicit window, each model loaded by the router takes its whole training
        // window: a 0.8B model occupied 4.1 GB of VRAM instead of 1.8 (observed). The plan's
        // window applies to every model in the directory.
        "-c".to_owned(),
        context.to_string(),
        "--models-dir".to_owned(),
        models_dir.display().to_string(),
        "--models-max".to_owned(),
        max_loaded.to_string(),
        "--port".to_owned(),
        port.to_string(),
        "--host".to_owned(),
        "127.0.0.1".to_owned(),
        "-t".to_owned(),
        threads.to_string(),
        // Without autoload, the first call to a model would fail instead of loading it: the
        // router must decide, not the user.
        "--models-autoload".to_owned(),
        // As in single-model mode: without jinja, tool calls come back as plain text.
        "--jinja".to_owned(),
    ]
}

/// Does this `llama-server` know the draft head (`--spec-type draft-mtp`)? Its option names
/// are in the binary or in the library next to it (`llama-common`); read once per binary.
pub fn knows_mtp(binary: &Path) -> bool {
    static SEEN: std::sync::Mutex<Vec<(PathBuf, bool)>> = std::sync::Mutex::new(Vec::new());
    if let Some((_, known)) = SEEN.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(p, _)| p == binary) {
        return *known;
    }
    let has = |path: &Path| std::fs::read(path).is_ok_and(|bytes| bytes.windows(9).any(|w| w == b"draft-mtp"));
    let mut known = has(binary);
    if !known && let Some(dir) = binary.parent() {
        known = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("common"))).take(4).any(|p| has(&p));
    }
    SEEN.lock().unwrap_or_else(|e| e.into_inner()).push((binary.to_path_buf(), known));
    known
}

/// Builds the `llama-server` arguments. Pure function, hence testable without a binary.
pub fn command_args(model_path: &Path, plan: &LaunchPlan, port: u16) -> Vec<String> {
    let mut args = vec![
        "-m".to_owned(),
        model_path.display().to_string(),
        "--port".to_owned(),
        port.to_string(),
        "--host".to_owned(),
        // We only listen on the loopback: an inference server exposed to the network without
        // authentication is an open door, and nothing here needs one.
        "127.0.0.1".to_owned(),
    ];
    args.extend(plan.to_args());
    // Essential for the server to turn tool calls into structured `tool_calls`; without it the
    // model writes them as text and the agent has to catch them.
    args.push("--jinja".to_owned());
    args
}

/// `llama-server` locations relative to a home directory.
///
/// The list is shared by both searches - the one that inspects the local machine and the one
/// that inspects a WSL distribution. They had diverged, and the gap showed: from Windows, Po
/// found `~/llama-upstream/build/bin/llama-server` in WSL and offered to launch it; the same Po
/// compiled for Linux and run in that very distribution did not find it, because that path was
/// missing from its list. Same machine, same binary, two answers. A single list, from now on.
const HOME_LOCATIONS: [&str; 4] = [
    // LM Studio ships its own llama.cpp engines.
    ".lmstudio/bin",
    ".local/bin",
    "llama.cpp/build/bin",
    "llama-upstream/build/bin",
];

/// `llama-server` locations outside the home directory.
const SYSTEM_LOCATIONS: [&str; 3] = ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"];

/// Looks for `llama-server`: the user's choices, the PATH, the package managers, the usual
/// locations, next to the models, then WSL. The rules are in [`crate::llama::detect`].
pub fn find_binary() -> Option<PathBuf> {
    let prefs = crate::prefs::LocalPrefs::load();
    crate::llama::detect(&crate::llama::Choice::from_prefs(&prefs), &crate::llama::Env::system(&prefs)).located.map(|l| l.path)
}

/// `llama-server` locations next to a models folder.
pub(crate) fn model_neighbours(parent: &Path, exe: &str) -> Vec<PathBuf> {
    ["llama-bin", "bin", "llama.cpp/build/bin/Release", "llama.cpp/build/bin", "llama.cpp"]
        .iter()
        .map(|relative| parent.join(relative).join(exe))
        .collect()
}

/// Paths to try, in order, for a given home directory.
///
/// A pure function: it is the only way to check without a binary that both searches cover the
/// same locations.
pub(crate) fn usual_locations(home: Option<&Path>, exe: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        for relative in HOME_LOCATIONS {
            candidates.push(home.join(relative).join(exe));
        }
    }
    for absolute in SYSTEM_LOCATIONS {
        candidates.push(PathBuf::from(absolute).join(exe));
    }
    candidates
}

/// Detaches a command from Po's console. Meaningless outside Windows.
///
/// See [`crate::catalog::NO_CONSOLE`]: `wsl.exe` takes over the inherited console and leaves
/// its output mode altered.
fn detach_console(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    #[cfg(windows)]
    // `tokio::process::Command` carries its own method: no trait to import.
    command.creation_flags(crate::catalog::NO_CONSOLE);
    command
}

/// `sh` script that looks for `llama-server` in a distribution, at the same locations as
/// [`usual_locations`].
///
/// The distribution is the only one to know what is executable on its side, and the only one
/// reachable from an SSH session: the search is therefore done from the inside. The script is
/// generated from the same constants as the native search, so the two do not diverge again.
#[cfg_attr(not(windows), allow(dead_code))]
fn search_script() -> String {
    let join = |locations: &[&str], suffix: &str| {
        locations
            .iter()
            .map(|e| format!("{e}/{suffix}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let personal = join(&HOME_LOCATIONS, "llama-server");
    let system = join(&SYSTEM_LOCATIONS, "llama-server");
    format!(
        "for h in /root /home/*; do for p in {personal}; do [ -x \"$h/$p\" ] && echo \"$h/$p\" && exit 0; done; done; for p in {system}; do [ -x \"$p\" ] && echo \"$p\" && exit 0; done; exit 1"
    )
}

/// Looks for `llama-server` in the running WSL distributions.
#[cfg(windows)]
pub(crate) fn find_binary_in_wsl() -> Option<PathBuf> {
    use std::os::windows::process::CommandExt;

    let search = search_script();

    for distro in crate::catalog::running_distros() {
        let Ok(output) = std::process::Command::new("wsl.exe")
            .args(["-d", &distro, "--exec", "sh", "-c", search.as_str()])
            .creation_flags(crate::catalog::NO_CONSOLE)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(found) = text.lines().map(str::trim).find(|l| !l.is_empty()) {
            return Some(crate::catalog::unc_path(&distro, found));
        }
    }
    None
}

#[cfg(not(windows))]
pub(crate) fn find_binary_in_wsl() -> Option<PathBuf> {
    None
}

/// Finds a free port in the range reserved for Po.
///
/// We bind then release: it is an optimistic reservation, but it avoids colliding with a server
/// the user may have launched themselves on the default port.
pub fn free_port() -> Option<u16> {
    PORT_RANGE.clone().find(|port| port_is_free(*port))
}

/// Is nothing listening on this local port?
///
/// Binding alone is not proof: on Windows, binding `127.0.0.1:8080` succeeds while a server
/// listens on `0.0.0.0:8080` (the more specific address is allowed), and Po would then start a
/// second server on the port of the first - observed with Strata. A port someone answers on is
/// taken, whatever the bind says.
pub fn port_is_free(port: u16) -> bool {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok() {
        return false;
    }
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_knows_mtp_from_its_own_option_names() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("llama-server.exe");
        std::fs::write(&exe, b"MZ old server").unwrap();
        assert!(!knows_mtp(&exe), "nothing says draft-mtp");
        let new = dir.path().join("new");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("llama-server.exe"), b"MZ").unwrap();
        std::fs::write(new.join("llama-common.dll"), b"...none,draft-simple,draft-mtp,ngram...").unwrap();
        assert!(knows_mtp(&new.join("llama-server.exe")), "found in the library beside it");
    }
    use crate::gguf::GgufInfo;
    use crate::hardware::{Gpu, GpuBackend, Hardware};
    use crate::plan::{PlanRequest, plan};

    fn model() -> GgufInfo {
        GgufInfo {
            architecture: "llama".into(),
            name: "test".into(),
            block_count: 32,
            embedding_length: 4096,
            head_count: 32,
            head_count_kv: 8,
            context_length: 8192,
            file_size_bytes: 2 * 1024 * 1024 * 1024,
            tensor_count: 100,
            draft_layers: 0,
        }
    }

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

    #[test]
    fn local_search_covers_locations_outside_path() {
        // Measured: a hand-built `llama-server` lives in `~/llama-upstream/build/bin`, absent
        // from the PATH. From Windows, Po found it there in WSL and offered to launch it; the
        // same Po run inside that distribution did not find it, because the native search
        // ignored that path. The session then refused to open on a machine perfectly able to
        // serve a model.
        let home = Path::new("/home/trial");
        let paths = usual_locations(Some(home), "llama-server");
        let expected = home.join("llama-upstream/build/bin").join("llama-server");
        assert!(
            paths.contains(&expected),
            "\"{}\" is missing from the local search: {paths:?}",
            expected.display()
        );
    }

    #[test]
    fn both_searches_cover_the_same_locations() {
        // The WSL search goes through a shell script, the local machine's through `PathBuf`s.
        // Two writings of the same list diverge sooner or later; this case flags it at the
        // next compilation rather than at the next Android bench.
        let script = search_script();
        for location in HOME_LOCATIONS.iter().chain(SYSTEM_LOCATIONS.iter()) {
            assert!(
                script.contains(&format!("{location}/llama-server")),
                "\"{location}\" missing from the WSL search script"
            );
        }
    }

    #[test]
    fn the_command_line_reflects_the_plan() {
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let args = command_args(Path::new("/models/test.gguf"), &plan, 18080);
        let joined = args.join(" ");

        assert!(joined.contains("-m /models/test.gguf"));
        assert!(joined.contains("--port 18080"));
        assert!(joined.contains(&format!("-ngl {}", plan.n_gpu_layers)));
        assert!(joined.contains(&format!("-c {}", plan.context)));
    }

    #[test]
    fn the_server_only_listens_on_the_loopback() {
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let args = command_args(Path::new("m.gguf"), &plan, 18080);
        let host = args.iter().position(|a| a == "--host").map(|i| &args[i + 1]);
        assert_eq!(host.map(String::as_str), Some("127.0.0.1"));
    }

    #[test]
    fn jinja_is_always_enabled() {
        // Without this flag, the server returns tool calls as plain text - the defect observed
        // in real conditions, which forces the agent to catch them.
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let args = command_args(Path::new("m.gguf"), &plan, 18080);
        assert!(args.contains(&"--jinja".to_string()));
    }

    #[tokio::test]
    async fn a_non_viable_plan_is_never_launched() {
        // No GPU, 512 MiB of free RAM: the 2 GiB model will not fit.
        let cpu_only = Hardware {
            gpus: vec![],
            ram_total_mib: 1024,
            ram_free_mib: 512,
            cpu_threads: 4,
        };
        let plan = plan(&model(), &cpu_only, &PlanRequest::default());
        assert!(!plan.viable);

        let mut supervisor = Supervisor::with_binary("llama-server-nonexistent");
        let result = supervisor.start(Path::new("m.gguf"), &plan).await;
        assert!(matches!(result, Err(SupervisorError::UnviablePlan)));
        // Nothing must have been launched.
        assert_eq!(supervisor.state(), ServerState::Stopped);
    }

    #[tokio::test]
    async fn a_missing_binary_fails_cleanly() {
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let mut supervisor = Supervisor::with_binary("binary-that-does-not-exist-at-all");
        let result = supervisor.start(Path::new("m.gguf"), &plan).await;
        assert!(matches!(result, Err(SupervisorError::Spawn(_))));
    }

    #[test]
    fn router_mode_arguments_are_complete() {
        let args = router_args(Path::new("/models"), 18080, 3, 8, 32768).join(" ");
        assert!(args.contains("-c 32768"), "the plan's window must be passed on");
        assert!(args.contains("--models-dir /models"));
        assert!(args.contains("--models-max 3"), "the memory limit must be passed on");
        assert!(args.contains("--models-autoload"));
        assert!(args.contains("--jinja"), "otherwise tool calls come back as text");
        assert!(args.contains("--host 127.0.0.1"), "never exposed to the network");
    }

    #[tokio::test]
    async fn driving_an_absent_router_fails_cleanly() {
        let supervisor = Supervisor::with_binary("llama-server");
        assert!(matches!(supervisor.load_model("m").await, Err(SupervisorError::NotRunning)));
        assert!(matches!(supervisor.list_models().await, Err(SupervisorError::NotRunning)));
        assert!(!supervisor.is_router());
    }

    #[test]
    fn the_server_diagnostic_surfaces_its_error_output() {
        po_i18n::set_lang(po_i18n::Lang::En);
        // "code 1" alone helps no one; it is in stderr that llama.cpp explains that a flag is
        // invalid or that memory is short.
        let err = SupervisorError::Died(
            Some(1),
            "usage:\n-fa, --flash-attn [on|off|auto]\nto show complete usage, run with -h".into(),
        );
        let text = err.to_string();
        assert!(text.contains("code 1"));
        assert!(text.contains("flash-attn"), "the real cause must appear: {text}");
    }

    #[test]
    fn a_dead_server_without_message_stays_readable() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let err = SupervisorError::Died(Some(137), String::new());
        assert!(err.to_string().contains("137"));
        assert!(!err.to_string().ends_with("- "));
    }

    #[test]
    fn a_free_port_is_found_in_the_range() {
        let port = free_port().expect("at least one port must be free");
        assert!(PORT_RANGE.contains(&port));
    }

    #[test]
    fn states_describe_themselves() {
        po_i18n::set_lang(po_i18n::Lang::En);
        assert!(ServerState::Ready.is_usable());
        assert!(!ServerState::Starting.is_usable());
        assert!(!ServerState::Stopped.is_usable());
        assert!(ServerState::Failed("memory".into()).label().contains("memory"));
    }

    #[test]
    fn without_a_server_the_supervisor_is_stopped() {
        let supervisor = Supervisor::with_binary("llama-server");
        assert_eq!(supervisor.state(), ServerState::Stopped);
        assert!(supervisor.running().is_none());
        assert!(!supervisor.already_serving(Path::new("m.gguf")));
    }

    #[tokio::test]
    async fn stopping_a_stopped_supervisor_does_nothing() {
        let mut supervisor = Supervisor::with_binary("llama-server");
        supervisor.stop().await;
        assert!(!supervisor.reap_if_idle().await);
    }

    #[test]
    fn the_environment_variable_takes_precedence_to_locate_the_binary() {
        // We cannot mutate the environment safely in a parallel test; we check that the
        // function does not panic and returns either a real path or nothing.
        if let Some(path) = find_binary() {
            assert!(path.is_file());
        }
    }

    #[test]
    fn a_profile_s_arguments_replace_the_plan_s() {
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let base = command_args(Path::new("m.gguf"), &plan, 18080);
        let overrides = vec![
            ("-c".to_owned(), Some("16384".to_owned())),
            ("--ctx-size".to_owned(), Some("8192".to_owned())),
            ("-t".to_owned(), Some("4".to_owned())),
            ("--temp".to_owned(), Some("0.6".to_owned())),
            ("--min-p".to_owned(), Some("0".to_owned())),
            ("--reasoning".to_owned(), Some("off".to_owned())),
            ("--no-mmap".to_owned(), None),
        ];
        let args = override_args(base, &overrides);
        let joined = args.join(" ");
        assert!(joined.contains("--ctx-size 8192"), "the last word on the window wins: {joined}");
        assert_eq!(args.iter().filter(|a| *a == "-c" || *a == "--ctx-size").count(), 1, "one window, not two: {joined}");
        assert!(joined.contains("-t 4") && !joined.contains(&format!("-t {}", plan.threads)));
        assert!(joined.contains("--temp 0.6") && joined.contains("--min-p 0") && joined.contains("--reasoning off"));
        assert!(args.last().is_some_and(|a| a == "--no-mmap"), "a flag without value is appended as is");
        assert!(joined.contains("-m m.gguf"), "the model stays");
    }

    #[test]
    fn a_profile_becomes_llama_server_arguments() {
        let mut profile = crate::prefs::ModelProfile::default();
        assert!(profile_overrides(&profile, false, 24).is_empty(), "an empty profile changes nothing");
        for (key, value) in [("cache", "q4_0"), ("reasoning", "high"), ("temperature", "0.7"), ("top_p", "0.8"), ("top_k", "20"), ("min_p", "0")] {
            assert!(profile.set(key, value));
        }
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let args = override_args(command_args(Path::new("m.gguf"), &plan, 18080), &profile_overrides(&profile, true, 24));
        let joined = args.join(" ");
        for expected in ["--cache-type-k q4_0", "--cache-type-v q4_0", "--reasoning on", "--reasoning-effort high", "--temp 0.7", "--top-p 0.8", "--top-k 20", "--min-p 0", "-t 6", "--prio -1"] {
            assert!(joined.contains(expected), "{expected} missing: {joined}");
        }
        assert_eq!(args.iter().filter(|a| *a == "-t").count(), 1, "game mode replaces the thread count");
        // Strata's cache types mean nothing to llama.cpp: not passed.
        let strata_kv = crate::prefs::ModelProfile { cache: Some("int8".into()), ..Default::default() };
        assert!(profile_overrides(&strata_kv, false, 8).is_empty());
        let off = crate::prefs::ModelProfile { reasoning: Some("off".into()), ..Default::default() };
        assert_eq!(profile_overrides(&off, false, 8), [("--reasoning".to_owned(), Some("off".to_owned()))]);
        assert!(profile_overrides(&Default::default(), true, 4).contains(&("-t".to_owned(), Some("2".to_owned()))), "never fewer than two threads");
    }

    #[test]
    fn a_port_someone_answers_on_is_never_free() {
        // A server on the wildcard address: on Windows, binding the loopback alone still works,
        // and was taken for a free port - Po then started a second Strata on 8080.
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_is_free(port), "a port with a listener is taken");
        drop(listener);
        let loopback = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = loopback.local_addr().unwrap().port();
        assert!(!port_is_free(port));
        drop(loopback);
        assert!(port_is_free(port), "once released, free again");
    }

    #[test]
    fn a_negative_value_is_a_value_not_a_flag() {
        let args = override_args(vec!["--prio".into(), "0".into()], &[("--prio".into(), Some("-1".into()))]);
        assert_eq!(args, ["--prio", "-1"]);
        let again = override_args(args, &[("--prio".into(), Some("0".into()))]);
        assert_eq!(again, ["--prio", "0"], "-1 was recognised as the old value");
    }

    #[test]
    fn the_log_tail_reads_only_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("big.log");
        let mut text = String::new();
        for i in 0..200_000 {
            text.push_str(&format!("line {i}\n"));
        }
        text.push_str("\n\nlast words\n");
        std::fs::write(&log, text).unwrap();
        let tail = log_tail(&log, 3);
        assert_eq!(tail, ["line 199998", "line 199999", "last words"]);
        assert!(log_tail(&dir.path().join("absent.log"), 5).is_empty());
        std::fs::write(&log, "only\n").unwrap();
        assert_eq!(log_tail(&log, 5), ["only"]);
        // Bytes that are not text do not stop the reading.
        std::fs::write(&log, [0xffu8, 0xfe, b'\n', b'o', b'k', b'\n']).unwrap();
        assert_eq!(log_tail(&log, 1), ["ok"]);
    }

    #[test]
    fn the_loading_progress_is_read_from_the_log() {
        po_i18n::set_lang(po_i18n::Lang::En);
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Real lines of llama-server 11342, in their order.
        assert!(llama_progress(&lines(&["ggml_cuda_init: found 1 CUDA devices:"])).is_some());
        assert!(llama_progress(&lines(&["llama_model_loader: loaded meta data with 40 key-value pairs"])).is_some());
        let tensors = llama_progress(&lines(&["llama_model_loader: x", "load_tensors: offloaded 25/25 layers to GPU"])).unwrap();
        let context = llama_progress(&lines(&["load_tensors: offloaded 25/25", "llama_context: n_ctx = 32768"])).unwrap();
        assert_ne!(tensors, context, "the last stage reached is the one said");
        assert!(llama_progress(&lines(&["main: server is listening on http://127.0.0.1:18080"])).is_some());
        // The same moment, as build 11342 says it.
        let quiet = llama_progress(&lines(&["srv    load_model: loading model 'm.gguf'", "srv  llama_server: model loaded"])).unwrap();
        assert_ne!(Some(quiet), llama_progress(&lines(&["srv    load_model: loading model 'm.gguf'"])));
        assert_eq!(llama_progress(&lines(&["something else"])), None);
        assert_eq!(llama_progress(&[]), None);
    }

    #[tokio::test]
    async fn a_server_that_dies_says_why_from_its_log() {
        // A "binary" that writes on its error output and quits: the message must come back.
        let dir = tempfile::tempdir().unwrap();
        let script = if cfg!(windows) { dir.path().join("fake.cmd") } else { dir.path().join("fake.sh") };
        #[cfg(windows)]
        std::fs::write(&script, "@echo error: unknown argument --bogus 1>&2\r\n@exit /b 3\r\n").unwrap();
        #[cfg(unix)]
        {
            std::fs::write(&script, "#!/bin/sh\necho 'error: unknown argument --bogus' >&2\nexit 3\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let plan = plan(&model(), &machine(11_000), &PlanRequest::default());
        let mut supervisor = Supervisor::with_binary(&script);
        let options = LaunchOptions { low_priority: true, ..Default::default() };
        match supervisor.start_with(Path::new("m.gguf"), &plan, &options).await {
            Err(SupervisorError::Died(code, message)) => {
                assert_eq!(code, Some(3));
                assert!(message.contains("unknown argument"), "{message}");
            }
            Err(other) => panic!("expected a death with its reason, got {other}"),
            Ok(port) => panic!("a dead script answered on {port}"),
        }
    }

    #[test]
    fn a_windows_drive_is_translated_for_wsl() {
        assert_eq!(drive_to_mnt(Path::new(r"E:\LLM\models")).as_deref(), Some("/mnt/e/LLM/models"));
        assert_eq!(drive_to_mnt(Path::new("C:/a b/c")).as_deref(), Some("/mnt/c/a b/c"));
        assert_eq!(drive_to_mnt(Path::new("/already/linux")), None);
    }
}
