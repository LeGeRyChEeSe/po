//! Po's service: a small process without a window, started at logon, that keeps the phone's
//! link alive even when no Po is open.
//!
//! It holds the link (QUIC endpoint, the account's directory), lists the projects (the folders
//! of the saved sessions) and the open sessions (the `live` registry), and bridges the phone to
//! each of them through their local control server. It can also open a session itself, without
//! a terminal (`po --headless`), in the project the phone chose. It keeps no conversation: the
//! sessions do.
//!
//! `po service install` registers it at logon (a scheduled task, else the user's Run key: no
//! administrator rights), `uninstall` removes it, `start`/`stop` start and stop it, `status`
//! says how it is, `run` is the service itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use po_i18n::t;
use po_link::link::Backend;
use po_link::tls::Peer;
use po_wire::{Feed, ProjectInfo, Reply, Request, SessionInfo, Subscribe};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

/// `%APPDATA%\po\link`: the PC's keys, its directory, the service's state.
pub fn link_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po").join("link"))
}

fn file(name: &str) -> Option<PathBuf> {
    link_dir().map(|d| d.join(name))
}

/// What the user chose for the link: the PC's name, its UDP port.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub name: Option<String>,
    pub port: Option<u16>,
}

impl Settings {
    pub fn load() -> Self {
        file("settings.json").and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = file("settings.json").ok_or_else(|| std::io::Error::other("no config folder"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default())
    }

    /// The PC's name: the one chosen, else the machine's.
    pub fn name(&self) -> String {
        self.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(hostname)
    }

    /// The port: chosen once at random, then kept (a router's opening and the phones' last known
    /// address stay valid) - unless it can no longer be opened: Windows reserves ranges of ports
    /// for Hyper-V and WSL, different after a restart, and another program may hold it. Then
    /// another is chosen and kept.
    pub fn port(&mut self) -> u16 {
        if let Some(port) = self.port {
            // A service restarting: the previous one may still be letting the port go. Changing
            // it would send every phone back through the directory before it reconnects.
            for _ in 0..12 {
                if port_free(port) {
                    return port;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
        let mut port = random_port();
        for _ in 0..50 {
            if port_free(port) {
                break;
            }
            port = random_port();
        }
        self.port = Some(port);
        let _ = self.save();
        port
    }
}

/// A port under Windows' dynamic range (49152 and up), where Hyper-V and WSL take theirs.
fn random_port() -> u16 {
    let random = u16::from_le_bytes(po_link::identity::random::<2>());
    30_000 + random % 19_000
}

/// Can the link open this UDP port, on IPv6 and IPv4 alike?
fn port_free(port: u16) -> bool {
    // One after the other (each closed at once): on Linux an IPv6 socket also takes the IPv4 port.
    let v6 = std::net::UdpSocket::bind((std::net::Ipv6Addr::UNSPECIFIED, port)).is_ok();
    let v4 = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).is_ok();
    // A machine without IPv6 still has a usable port if IPv4 opens it.
    v4 && (v6 || std::net::UdpSocket::bind((std::net::Ipv6Addr::LOCALHOST, 0)).is_err())
}

pub fn hostname() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok().filter(|h| !h.is_empty()).unwrap_or_else(|| "PC".into())
}

/// What the running service says about itself, rewritten every few seconds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub pid: u32,
    /// `running`, `signed_out`, `error`.
    pub state: String,
    pub updated: u64,
    pub started: u64,
    pub login: String,
    pub id: String,
    pub name: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub mapping: Option<String>,
    /// (id, name, kind, last seen).
    pub devices: Vec<(String, String, String, u64)>,
    pub error: Option<String>,
}

/// The service's state, if it is running (it wrote it recently).
pub fn running() -> Option<State> {
    // Its lock free: no service, even if one that ended abruptly wrote its state a moment ago.
    if let Some(path) = file("service.lock")
        && let Ok(lock) = std::fs::OpenOptions::new().write(true).open(path)
        && lock.try_lock().is_ok()
    {
        return None;
    }
    let state: State = serde_json::from_str(&std::fs::read_to_string(file("service.json")?).ok()?).ok()?;
    (po_link::directory::now().saturating_sub(state.updated) <= 20).then_some(state)
}

/// Something for the running service to do: it reads the folder every few seconds.
pub fn ask(op: &str, value: &str) -> std::io::Result<()> {
    let dir = link_dir().ok_or_else(|| std::io::Error::other("no config folder"))?.join("requests");
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}-{}.json", po_link::directory::now(), u32::from_le_bytes(po_link::identity::random::<4>()));
    std::fs::write(dir.join(name), json!({"op": op, "value": value}).to_string())
}

fn take_requests() -> Vec<(String, String)> {
    let Some(dir) = link_dir().map(|d| d.join("requests")) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
        {
            out.push((v["op"].as_str().unwrap_or_default().to_owned(), v["value"].as_str().unwrap_or_default().to_owned()));
        }
        let _ = std::fs::remove_file(path);
    }
    out
}

/// The GitHub account the link uses: Po's own remembered connection (not GitHub CLI's: the
/// phone must sign in with an account the user chose for Po).
pub fn token() -> Option<String> {
    crate::github::stored_token(&crate::github::Config::from_env())
}

pub fn github_config() -> po_link::github::Config {
    let c = crate::github::Config::from_env();
    po_link::github::Config { api: c.api, web: c.web, client_id: c.client_id }
}

/// `po service …`.
pub async fn command(arg: Option<&str>) -> Result<(), String> {
    match arg {
        Some("run") => run().await,
        Some("install" | "installer") => {
            let how = install()?;
            println!("{}", t!("service.installed", how = how));
            start()?;
            println!("{}", t!("service.started"));
            Ok(())
        }
        Some("uninstall" | "retirer" | "desinstaller" | "désinstaller") => {
            let _ = stop().await;
            uninstall();
            println!("{}", t!("service.uninstalled"));
            Ok(())
        }
        Some("start" | "demarrer" | "démarrer") => {
            if running().is_some() {
                println!("{}", t!("service.already"));
                return Ok(());
            }
            start()?;
            println!("{}", t!("service.started"));
            Ok(())
        }
        Some("stop" | "arreter" | "arrêter") => {
            stop().await?;
            println!("{}", t!("service.stopped"));
            Ok(())
        }
        None | Some("status" | "etat" | "état") => {
            for line in status_lines() {
                println!("{line}");
            }
            Ok(())
        }
        Some(other) => Err(t!("service.unknown", arg = other)),
    }
}

/// The service's state in a few lines, for `po service status` and `/rc`.
pub fn status_lines() -> Vec<String> {
    let mut out = Vec::new();
    match running() {
        Some(state) => {
            out.push(t!("service.status.running", pid = state.pid, port = state.port));
            match state.state.as_str() {
                "signed_out" => out.push(t!("service.status.signed_out").to_owned()),
                _ if !state.login.is_empty() => out.push(t!("service.status.account", login = state.login, name = state.name)),
                _ => {}
            }
            if !state.addresses.is_empty() {
                out.push(t!("service.status.addresses", list = state.addresses.join(", ")));
            }
            if let Some(mapping) = &state.mapping {
                out.push(t!("service.status.mapping", how = mapping));
            }
            for (_, name, kind, seen) in &state.devices {
                out.push(t!("service.status.device", name = name, kind = kind, ago = ago(*seen)));
            }
            if let Some(error) = &state.error {
                out.push(t!("service.status.error", error = error));
            }
        }
        None => out.push(t!("service.status.stopped").to_owned()),
    }
    out.push(if installed() { t!("service.status.at_logon").to_owned() } else { t!("service.status.not_at_logon").to_owned() });
    out
}

/// "3 min ago", for a device's last sign of life.
pub fn ago(when: u64) -> String {
    let secs = po_link::directory::now().saturating_sub(when);
    match secs {
        0..60 => t!("service.ago.now").to_owned(),
        60..3600 => t!("service.ago.minutes", n = secs / 60),
        3600..86_400 => t!("service.ago.hours", n = secs / 3600),
        _ => t!("service.ago.days", n = secs / 86_400),
    }
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Starts `po` with `args` in the background, without a window, its output in `log`.
fn spawn_background(args: &[&str], dir: Option<&Path>, log: &str) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut command = std::process::Command::new(exe);
    command.args(args).stdin(std::process::Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    if let Some(path) = file(log) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(out) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            if let Ok(err) = out.try_clone() {
                command.stderr(err);
            }
            command.stdout(out);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|c| c.id()).map_err(|e| e.to_string())
}

pub fn start() -> Result<(), String> {
    spawn_background(&["service", "run"], None, "service.log").map(|_| ())
}

/// Asks the service to stop, and waits for it.
pub async fn stop() -> Result<(), String> {
    if running().is_none() {
        return Ok(());
    }
    ask("stop", "").map_err(|e| e.to_string())?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if running().is_none() {
            return Ok(());
        }
    }
    Err(t!("service.stop_timeout").to_owned())
}

const TASK: &str = "Po service";
const RUN_VALUE: &str = "PoService";

fn quiet(program: &str, args: &[&str]) -> bool {
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.status().is_ok_and(|s| s.success())
}

/// Registers the service at logon; how it was done.
pub fn install() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        let line = format!("\"{}\" service start", exe.display());
        // A task at logon, if Windows lets a user make one; else the user's Run key, which needs
        // no rights at all.
        if quiet("schtasks", &["/Create", "/F", "/SC", "ONLOGON", "/TN", TASK, "/TR", &line, "/RL", "LIMITED"]) {
            return Ok(t!("service.how.task").to_owned());
        }
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        if quiet("reg", &["add", key, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &line, "/f"]) {
            return Ok(t!("service.how.run_key").to_owned());
        }
        Err(t!("service.install_failed").to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        let dir = PathBuf::from(std::env::var_os("HOME").ok_or("no HOME")?).join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>dev.po.service</string>\
             <key>ProgramArguments</key><array><string>{}</string><string>service</string><string>run</string></array>\
             <key>RunAtLoad</key><true/></dict></plist>\n",
            exe.display()
        );
        std::fs::write(dir.join("dev.po.service.plist"), plist).map_err(|e| e.to_string())?;
        Ok("LaunchAgent".into())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = PathBuf::from(std::env::var_os("HOME").ok_or("no HOME")?).join(".config/autostart");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let entry = format!("[Desktop Entry]\nType=Application\nName=Po service\nExec=\"{}\" service run\nNoDisplay=true\n", exe.display());
        std::fs::write(dir.join("po-service.desktop"), entry).map_err(|e| e.to_string())?;
        Ok("autostart".into())
    }
}

pub fn uninstall() {
    #[cfg(windows)]
    {
        quiet("schtasks", &["/Delete", "/F", "/TN", TASK]);
        quiet("reg", &["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", RUN_VALUE, "/f"]);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::fs::remove_file(PathBuf::from(home).join("Library/LaunchAgents/dev.po.service.plist"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(home) = std::env::var_os("HOME") {
        let _ = std::fs::remove_file(PathBuf::from(home).join(".config/autostart/po-service.desktop"));
    }
}

pub fn installed() -> bool {
    #[cfg(windows)]
    {
        quiet("schtasks", &["/Query", "/TN", TASK]) || quiet("reg", &["query", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", RUN_VALUE])
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").is_some_and(|h| PathBuf::from(h).join("Library/LaunchAgents/dev.po.service.plist").exists())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("HOME").is_some_and(|h| PathBuf::from(h).join(".config/autostart/po-service.desktop").exists())
    }
}

fn write_state(state: &State) {
    if let Some(path) = file("service.json") {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = po_link::write_private(&path, serde_json::to_string_pretty(state).unwrap_or_default().as_bytes());
    }
}

/// The service itself.
pub async fn run() -> Result<(), String> {
    if let Some(state) = running()
        && state.pid != std::process::id()
    {
        return Err(t!("service.already").to_owned());
    }
    let dir = link_dir().ok_or("no config folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // One service at a time, even while the first one is between two attempts (its state file
    // then a little old): the lock is the system's, released when the process ends however.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("service.lock")).map_err(|e| e.to_string())?;
    // `running()` takes it for an instant to check: a few tries before concluding another
    // service holds it.
    let mut held = false;
    for _ in 0..10 {
        if lock.try_lock().is_ok() {
            held = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !held {
        return Err(t!("service.already").to_owned());
    }
    // Old requests are not for this run.
    take_requests();
    let identity = po_link::Identity::load_or_create(&dir.join("identity.json")).map_err(|e| e.to_string())?;
    let mut settings = Settings::load();
    let started = po_link::directory::now();
    let mut state = State { pid: std::process::id(), started, id: identity.id(), name: settings.name(), ..State::default() };
    let backend = Arc::new(Service::new(settings.name()));
    let mut failures = 0u32;
    loop {
        let Some(token) = token() else {
            state.state = "signed_out".into();
            state.login.clear();
            state.devices.clear();
            if wait_for_requests(Duration::from_secs(5), &mut state).await {
                break;
            }
            continue;
        };
        // Checked at each attempt: the port may have been taken or reserved meanwhile.
        let port = settings.port();
        state.port = port;
        let options = po_link::pc::Options {
            port,
            name: settings.name(),
            state: Some(dir.join("directory.json")),
            extra: extra_addresses(port),
            ..po_link::pc::Options::default()
        };
        let gh = po_link::github::GitHub::new(&github_config(), token.clone());
        let pc = match po_link::pc::Pc::start(identity.clone(), gh, options, backend.clone()).await {
            Ok(pc) => pc,
            Err(err) => {
                failures += 1;
                state.state = "error".into();
                state.error = Some(err);
                // GitHub unreachable at logon (no network yet): try again, less and less often.
                if wait_for_requests(Duration::from_secs((5 * failures as u64).min(120)), &mut state).await {
                    break;
                }
                continue;
            }
        };
        failures = 0;
        state.error = None;
        let mut stop = false;
        // The account changed (signed out, another one): the link starts again with it.
        while self::token().as_deref() == Some(token.as_str()) {
            let status = pc.status();
            state.state = "running".into();
            state.login = status.login.clone();
            state.name = settings.name();
            state.port = status.port;
            state.addresses = status.addresses.candidates().iter().map(|a| a.to_string()).collect();
            state.mapping = status.mapping.clone();
            state.devices = status
                .devices
                .iter()
                .map(|(id, name, kind, seen)| (id.clone(), name.clone(), format!("{kind:?}").to_lowercase(), *seen))
                .collect();
            state.error = status.last_error.clone();
            state.updated = po_link::directory::now();
            write_state(&state);
            tokio::time::sleep(Duration::from_secs(2)).await;
            for (op, value) in take_requests() {
                match op.as_str() {
                    "stop" => stop = true,
                    "revoke" => {
                        if let Err(e) = pc.revoke(&value).await {
                            state.error = Some(e);
                        }
                    }
                    "rename" => {
                        settings = Settings::load();
                        backend.set_name(settings.name());
                        let _ = pc.rename(&settings.name()).await;
                    }
                    "sync" => {
                        let _ = pc.sync_now().await;
                    }
                    _ => {}
                }
            }
            if stop {
                break;
            }
        }
        drop(pc);
        if stop {
            break;
        }
    }
    if let Some(path) = file("service.json") {
        let _ = std::fs::remove_file(path);
    }
    drop(lock);
    Ok(())
}

/// Waits, reading the requests and saying every few seconds that the service is still there
/// (else `status` and `/rc` would think it stopped between two attempts); true if asked to stop.
async fn wait_for_requests(total: Duration, state: &mut State) -> bool {
    let steps = (total.as_millis() / 500).max(1);
    for step in 0..steps {
        if step % 10 == 0 {
            state.updated = po_link::directory::now();
            write_state(state);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        if take_requests().iter().any(|(op, _)| op == "stop") {
            return true;
        }
    }
    false
}

/// Addresses published on top of those found: `PO_LINK_EXTRA` (comma-separated hosts), for an
/// emulator reaching this PC through its own address for it (10.0.2.2) or a test on localhost.
fn extra_addresses(port: u16) -> Vec<std::net::SocketAddr> {
    std::env::var("PO_LINK_EXTRA")
        .unwrap_or_default()
        .split(',')
        .filter_map(|h| h.trim().parse::<std::net::IpAddr>().ok())
        .map(|ip| std::net::SocketAddr::new(ip, port))
        .collect()
}

/// An open session, from the `live` registry.
#[derive(Debug, Clone)]
struct Live {
    pid: u32,
    url: String,
    workspace: String,
    kind: String,
}

fn live_sessions() -> Vec<Live> {
    let Some(dir) = po_tui::control::live_dir() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .filter_map(|v| {
            Some(Live {
                pid: v["pid"].as_u64()? as u32,
                url: v["url"].as_str()?.to_owned(),
                workspace: v["workspace"].as_str().unwrap_or_default().to_owned(),
                kind: v["kind"].as_str().unwrap_or("tui").to_owned(),
            })
        })
        .collect()
}

/// Two spellings of one folder (Windows: case and separators do not matter).
fn same_folder(a: &str, b: &str) -> bool {
    let tidy = |p: &str| p.trim_end_matches(['\\', '/']).replace('\\', "/");
    if cfg!(windows) { tidy(a).eq_ignore_ascii_case(&tidy(b)) } else { tidy(a) == tidy(b) }
}

fn folder_name(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
}

/// What the service does with the phone's requests.
pub struct Service {
    name: std::sync::Mutex<String>,
    http: reqwest::Client,
}

impl Service {
    pub fn new(name: String) -> Self {
        Self { name: std::sync::Mutex::new(name), http: reqwest::Client::builder().no_proxy().build().unwrap_or_default() }
    }

    fn set_name(&self, name: String) {
        *self.name.lock().unwrap_or_else(|e| e.into_inner()) = name;
    }

    /// The open sessions that answer, with what they say about themselves. An entry whose
    /// session no longer listens at all (its process died without removing it) is removed: it
    /// would show as an open session forever.
    async fn live(&self) -> Vec<(Live, Value)> {
        let asks = live_sessions().into_iter().map(|live| async move {
            let base = live.url.clone();
            let response = match self.http.get(format!("{base}/info")).timeout(Duration::from_millis(1500)).send().await {
                Ok(response) => response,
                Err(e) => {
                    if e.is_connect()
                        && let Some(dir) = po_tui::control::live_dir()
                    {
                        let _ = std::fs::remove_file(dir.join(format!("{}.json", live.pid)));
                    }
                    return None;
                }
            };
            let info = response.json::<Value>().await.ok()?;
            Some((live, info))
        });
        futures_util::future::join_all(asks).await.into_iter().flatten().collect()
    }

    async fn find(&self, key: &str) -> Option<Live> {
        let pid: u32 = key.strip_prefix("live:")?.parse().ok()?;
        live_sessions().into_iter().find(|l| l.pid == pid)
    }

    async fn projects(&self) -> Vec<ProjectInfo> {
        let mut projects: Vec<ProjectInfo> = Vec::new();
        for saved in po_agent::SessionStore::default_location().list() {
            match projects.iter_mut().find(|p| same_folder(&p.path, &saved.workspace)) {
                Some(p) => {
                    p.saved += 1;
                    if saved.saved_at > p.last {
                        p.last = saved.saved_at.clone();
                    }
                }
                None => projects.push(ProjectInfo { name: folder_name(&saved.workspace), path: saved.workspace.clone(), saved: 1, last: saved.saved_at.clone(), ..Default::default() }),
            }
        }
        for (live, info) in self.live().await {
            let loading = serde_json::from_value::<po_wire::Loading>(info["status"]["loading"].clone()).ok();
            match projects.iter_mut().find(|p| same_folder(&p.path, &live.workspace)) {
                Some(p) => {
                    p.live += 1;
                    p.loading = p.loading.take().or(loading);
                }
                None if !live.workspace.is_empty() => projects.push(ProjectInfo {
                    name: folder_name(&live.workspace),
                    path: live.workspace.clone(),
                    live: 1,
                    loading,
                    ..Default::default()
                }),
                None => {}
            }
        }
        // A folder that no longer exists is no project to open a session in.
        projects.retain(|p| Path::new(&p.path).is_dir());
        projects.sort_by(|a, b| (b.live > 0).cmp(&(a.live > 0)).then(b.last.cmp(&a.last)));
        projects
    }

    async fn sessions(&self, project: Option<&str>) -> Vec<SessionInfo> {
        let mut out = Vec::new();
        let mut open_ids = Vec::new();
        for (live, info) in self.live().await {
            if project.is_some_and(|p| !same_folder(p, &live.workspace)) {
                continue;
            }
            let saved = info["session"].as_str().map(str::to_owned);
            if let Some(id) = &saved {
                open_ids.push(id.clone());
            }
            out.push(SessionInfo {
                key: format!("live:{}", live.pid),
                live: true,
                headless: live.kind == "headless",
                saved_id: saved,
                title: info["status"]["title"].as_str().unwrap_or_default().to_owned(),
                project: live.workspace.clone(),
                model: info["status"]["model"].as_str().unwrap_or_default().to_owned(),
                busy: info["busy"].as_bool().unwrap_or(false),
                loading: info["status"]["loading"].is_object(),
                updated: String::new(),
                exchanges: info["exchanges"].as_u64().unwrap_or(0) as u32,
            });
        }
        let store = po_agent::SessionStore::default_location();
        let saved = match project {
            Some(p) => store.list_for(Path::new(p)),
            None => store.list(),
        };
        for s in saved.into_iter().filter(|s| !open_ids.contains(&s.id)).take(60) {
            out.push(SessionInfo {
                key: format!("saved:{}", s.id),
                live: false,
                headless: false,
                saved_id: Some(s.id.clone()),
                title: s.title.clone(),
                project: s.workspace.clone(),
                model: s.settings.last_model.clone().or(s.settings.model.clone()).unwrap_or_default(),
                busy: false,
                loading: false,
                updated: s.saved_at.clone(),
                exchanges: s.exchange_count() as u32,
            });
        }
        out
    }

    /// Opens a session without a terminal in `dir`; its key once it answers.
    async fn open(&self, dir: &str, resume: Option<&str>) -> Result<String, String> {
        if !Path::new(dir).is_dir() {
            return Err(t!("service.no_folder", path = dir));
        }
        let mut args = vec!["--headless"];
        if let Some(id) = resume {
            args.push("--resume");
            args.push(id);
        }
        let pid = spawn_background(&args, Some(Path::new(dir)), "headless.log")?;
        // It probes the models and loads the project first: a few seconds, more for a cold disk.
        for _ in 0..120 {
            tokio::time::sleep(Duration::from_millis(250)).await;
            if live_sessions().iter().any(|l| l.pid == pid) {
                return Ok(format!("live:{pid}"));
            }
        }
        Err(t!("service.open_timeout").to_owned())
    }

    /// Writes a file from the phone into the session's folder, under `.po/uploads`; the mention
    /// that attaches it to a message.
    async fn upload(&self, session: &str, name: &str, data: &str) -> Result<String, String> {
        use base64::Engine;
        let workspace = match session.strip_prefix("saved:") {
            Some(id) => po_agent::SessionStore::default_location().load(id).map_err(|e| e.to_string())?.workspace,
            None => self.find(session).await.ok_or_else(|| t!("service.session_gone").to_owned())?.workspace,
        };
        if workspace.is_empty() || !Path::new(&workspace).is_dir() {
            return Err(t!("service.no_folder", path = workspace));
        }
        // Base64 is a third longer than what it carries: a longer text is refused unread.
        if data.len() > po_wire::MAX_UPLOAD / 3 * 4 + 8 {
            return Err(t!("service.upload_too_big", max = po_wire::MAX_UPLOAD / (1024 * 1024)));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|_| t!("service.upload_invalid").to_owned())?;
        if bytes.len() > po_wire::MAX_UPLOAD {
            return Err(t!("service.upload_too_big", max = po_wire::MAX_UPLOAD / (1024 * 1024)));
        }
        let dir = Path::new(&workspace).join(".po").join("uploads");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // What is sent from a phone stays out of the project's history.
        let ignore = dir.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
        let file = format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), safe_file_name(name));
        std::fs::write(dir.join(&file), &bytes).map_err(|e| e.to_string())?;
        Ok(format!("@.po/uploads/{file}"))
    }

    /// An image of a session's conversation, in base64, for the phone to show it. Only an image
    /// file (by its extension), of a size a phone can take; a relative path is from the
    /// session's folder.
    async fn image(&self, session: &str, path: &str) -> Result<String, String> {
        use base64::Engine;
        let is_image = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"));
        if !is_image {
            return Err(t!("service.image_refused").to_owned());
        }
        let full = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            let workspace = match session.strip_prefix("saved:") {
                Some(id) => po_agent::SessionStore::default_location().load(id).map_err(|e| e.to_string())?.workspace,
                None => self.find(session).await.ok_or_else(|| t!("service.session_gone").to_owned())?.workspace,
            };
            Path::new(&workspace).join(path)
        };
        let size = std::fs::metadata(&full).map_err(|e| format!("{}: {e}", full.display()))?.len();
        if size > po_wire::MAX_UPLOAD as u64 {
            return Err(t!("service.upload_too_big", max = po_wire::MAX_UPLOAD / (1024 * 1024)));
        }
        let bytes = tokio::fs::read(&full).await.map_err(|e| format!("{}: {e}", full.display()))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    async fn post(&self, live: &Live, route: &str, body: Value) -> Result<Value, String> {
        let response = self.http.post(format!("{}/{route}", live.url)).json(&body).timeout(Duration::from_secs(130)).send().await.map_err(|e| e.to_string())?;
        response.json::<Value>().await.map_err(|e| e.to_string())
    }
}

/// A file name from the phone, made harmless: letters, digits, `.`, `-` and `_` only (no path,
/// no space a mention would cut at), not too long, never empty.
fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut out: String = base.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    out = out.trim_start_matches('.').to_owned();
    if out.chars().count() > 80 {
        // The extension is kept: it says what the file is.
        let ext = out.rsplit_once('.').map(|(_, e)| e.to_owned()).filter(|e| e.len() <= 8).unwrap_or_default();
        out = out.chars().take(70).collect();
        if !ext.is_empty() {
            out = format!("{out}.{ext}");
        }
    }
    if out.is_empty() { "file".into() } else { out }
}

/// Speech to text with the PC's own whisper (the one `/voice install` set up): nothing leaves
/// the PC.
async fn transcribe(audio: &str) -> Result<String, String> {
    use base64::Engine;
    let settings = po_tui::voice::Settings::load();
    if let Some(missing) = settings.missing() {
        return Err(t!("service.voice_missing", what = missing));
    }
    // Three minutes of 16 kHz mono 16-bit sound, and some room.
    if audio.len() > 8 * 1024 * 1024 {
        return Err(t!("service.voice_too_long").to_owned());
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(audio.trim()).map_err(|_| t!("service.upload_invalid").to_owned())?;
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(t!("service.voice_not_wav").to_owned());
    }
    let path = std::env::temp_dir().join(format!("po-phone-{}-{}.wav", std::process::id(), chrono::Local::now().format("%H%M%S%3f")));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    let result = po_tui::voice::transcribe(&settings, &path).await;
    let _ = std::fs::remove_file(&path);
    let text = result?;
    if text.trim().is_empty() {
        return Err(t!("service.voice_nothing").to_owned());
    }
    Ok(text)
}

/// Moves a saved session to the bin of sessions next to them: out of the lists, but not lost.
fn forget(id: &str) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(t!("service.session_gone").to_owned());
    }
    // A session open somewhere is not taken from under it.
    let store = po_agent::SessionStore::default_location();
    let from = store.root().join(format!("{id}.json"));
    if !from.is_file() {
        return Err(t!("service.session_gone").to_owned());
    }
    let bin = store.root().with_file_name("sessions-bin");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    std::fs::rename(&from, bin.join(format!("{id}.json"))).map_err(|e| e.to_string())
}

fn done(answer: Result<Value, String>) -> Reply {
    match answer {
        Ok(v) if v["error"].as_bool() == Some(true) => Reply::Error { message: v["text"].as_str().unwrap_or_default().to_owned() },
        Ok(v) => Reply::Done { text: v["text"].as_str().unwrap_or_default().to_owned() },
        Err(e) => Reply::Error { message: e },
    }
}

#[async_trait]
impl Backend for Service {
    fn name(&self) -> String {
        self.name.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    async fn request(&self, _peer: &Peer, request: Request) -> Reply {
        let gone = || Reply::Error { message: t!("service.session_gone").to_owned() };
        match request {
            Request::Projects => Reply::Projects { projects: self.projects().await },
            Request::Sessions { project } => Reply::Sessions { sessions: self.sessions(project.as_deref()).await },
            Request::Input { session, input, .. } => match self.find(&session).await {
                Some(live) => done(self.post(&live, "input", serde_json::to_value(&input).unwrap_or_default()).await),
                None => gone(),
            },
            Request::Menu { session, line } => match self.find(&session).await {
                Some(live) => match self.post(&live, "menu", json!({"line": line})).await {
                    Ok(v) => Reply::Menu { menu: serde_json::from_value(v).ok() },
                    Err(e) => Reply::Error { message: e },
                },
                None => gone(),
            },
            Request::Commands { session } => match self.find(&session).await {
                Some(live) => match self.http.get(format!("{}/commands", live.url)).timeout(Duration::from_secs(10)).send().await {
                    Ok(r) => Reply::Commands { commands: r.json().await.unwrap_or_default() },
                    Err(e) => Reply::Error { message: e.to_string() },
                },
                None => gone(),
            },
            Request::NewSession { project, prompt } => match self.open(&project, None).await {
                Ok(key) => {
                    if let (Some(text), Some(live)) = (prompt.filter(|p| !p.trim().is_empty()), self.find(&key).await) {
                        let _ = self.post(&live, "input", json!({"kind": "prompt", "text": text})).await;
                    }
                    Reply::Session { key }
                }
                Err(message) => Reply::Error { message },
            },
            Request::Resume { saved } => {
                if let Some((live, _)) = self.live().await.into_iter().find(|(_, info)| info["session"].as_str() == Some(saved.as_str())) {
                    return Reply::Session { key: format!("live:{}", live.pid) };
                }
                match po_agent::SessionStore::default_location().load(&saved) {
                    Ok(session) => match self.open(&session.workspace, Some(&saved)).await {
                        Ok(key) => Reply::Session { key },
                        Err(message) => Reply::Error { message },
                    },
                    Err(e) => Reply::Error { message: e.to_string() },
                }
            }
            Request::Close { session } => match self.find(&session).await {
                Some(live) if live.kind == "headless" => done(self.post(&live, "close", json!({})).await),
                Some(_) => Reply::Error { message: t!("tui.control.terminal_session").to_owned() },
                None => gone(),
            },
            Request::Upload { session, name, data } => match self.upload(&session, &name, &data).await {
                Ok(mention) => Reply::Done { text: mention },
                Err(message) => Reply::Error { message },
            },
            Request::Image { session, path } => match self.image(&session, &path).await {
                Ok(data) => Reply::Done { text: data },
                Err(message) => Reply::Error { message },
            },
            Request::Transcribe { audio } => match transcribe(&audio).await {
                Ok(text) => Reply::Done { text },
                Err(message) => Reply::Error { message },
            },
            Request::Usage => {
                let measured = tokio::task::spawn_blocking(po_local::usage::measure).await.unwrap_or_default();
                let model = crate::background::loaded_model().await;
                Reply::Usage {
                    usage: po_wire::MachineUsage {
                        cpu_percent: measured.cpu_percent,
                        ram_used_mib: measured.ram_used_mib,
                        ram_total_mib: measured.ram_total_mib,
                        gpus: measured
                            .gpus
                            .into_iter()
                            .map(|g| po_wire::GpuUsage { name: g.name, percent: g.percent, vram_used_mib: g.vram_used_mib, vram_total_mib: g.vram_total_mib, temperature_c: g.temperature_c })
                            .collect(),
                        disks: measured.disks.into_iter().map(|d| po_wire::DiskUsage { name: d.name, free_gib: d.free_gib, total_gib: d.total_gib }).collect(),
                        model,
                    },
                }
            }
            Request::Forget { saved } if self.live().await.iter().any(|(_, info)| info["session"].as_str() == Some(saved.as_str())) => {
                Reply::Error { message: t!("service.forget_open").to_owned() }
            }
            Request::Forget { saved } => match forget(&saved) {
                Ok(()) => Reply::Done { text: t!("service.forgotten").to_owned() },
                Err(message) => Reply::Error { message },
            },
            Request::Unload => Reply::Done {
                text: if crate::background::unload_internal(true).await {
                    t!("chat.background.unloaded").to_owned()
                } else {
                    t!("chat.background.none_running").to_owned()
                },
            },
            Request::Hello { .. } | Request::Ping { .. } => Reply::Error { message: "handled by the link".into() },
        }
    }

    async fn subscribe(&self, _peer: &Peer, subscribe: Subscribe) -> mpsc::Receiver<Feed> {
        let (tx, rx) = mpsc::channel(1024);
        // A saved session, not open: shown as it was saved, to read before opening it again.
        if let Some(id) = subscribe.session.strip_prefix("saved:") {
            let feed = match po_agent::SessionStore::default_location().load(id) {
                Ok(saved) => {
                    // The whole conversation, archived part included: compaction must not cut it off.
                    let all: Vec<po_core::Message> = saved.archive.iter().chain(saved.messages.iter()).cloned().collect();
                    let mut conversation = po_wire::Conversation::new(po_tui::control::items_from_messages(&all));
                    conversation.status.title = Some(saved.title.clone());
                    conversation.status.workspace = saved.workspace.clone();
                    conversation.closed = true;
                    Feed::Snapshot { epoch: format!("saved-{id}"), seq: 0, conversation: Box::new(conversation) }
                }
                Err(e) => Feed::Gone { reason: e.to_string() },
            };
            let _ = tx.send(feed).await;
            return rx;
        }
        let Some(live) = self.find(&subscribe.session).await else {
            let _ = tx.send(Feed::Gone { reason: t!("service.session_gone").to_owned() }).await;
            return rx;
        };
        let url = format!("{}/events?epoch={}&since={}", live.url, subscribe.epoch.unwrap_or_default(), subscribe.since);
        let http = self.http.clone();
        tokio::spawn(async move {
            use futures_util::StreamExt;
            let response = match http.get(url).send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Feed::Gone { reason: e.to_string() }).await;
                    return;
                }
            };
            let mut stream = response.bytes_stream();
            let mut pending = Vec::new();
            loop {
                tokio::select! {
                    chunk = stream.next() => {
                        let Some(Ok(chunk)) = chunk else { break };
                        pending.extend_from_slice(&chunk);
                        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                            let line: Vec<u8> = pending.drain(..=end).collect();
                            if let Ok(feed) = serde_json::from_slice::<Feed>(&line)
                                && tx.send(feed).await.is_err()
                            {
                                return;
                            }
                        }
                    }
                    // The phone stopped following: the session's stream is let go too.
                    _ = tx.closed() => return,
                }
            }
            let _ = tx.send(Feed::Gone { reason: t!("service.session_ended").to_owned() }).await;
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_port_is_seen_and_new_ones_stay_under_the_dynamic_range() {
        let held = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(!port_free(port), "a port another program holds is not free");
        drop(held);
        for _ in 0..1000 {
            let p = random_port();
            assert!((30_000..49_000).contains(&p), "{p}");
        }
    }
}
