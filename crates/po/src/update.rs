//! Po updating itself from its GitHub releases.
//!
//! A build from the release workflow (`PO_RELEASE` set when it was compiled) checks the latest
//! release at most once a day, from a session or from the service, and installs it in the
//! background: the binary of this platform is downloaded, checked against the release's
//! `SHA256SUMS.txt`, asked for its version, then put in place of the running one (which steps
//! aside: Windows lets a running binary be renamed, not overwritten). The next start runs it and
//! says so. The service, which runs for days, restarts itself on it.
//!
//! A build from source is never replaced: it only hears that a newer version exists.
//! `po update` updates now, `po update check` only looks, `po update off` stops the automatic
//! checks (`PO_NO_UPDATE=1` too).

use std::path::PathBuf;
use std::time::Duration;

use po_i18n::t;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The platform this binary was built for: its release asset is `po-<target>[.exe]`.
pub const TARGET: &str = env!("PO_TARGET");

/// Built by the release workflow: only such a binary replaces itself.
pub fn release_build() -> bool {
    option_env!("PO_RELEASE").is_some_and(|v| !v.is_empty())
}

const DAY: u64 = 24 * 60 * 60;

/// `update.json` in Po's config folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// Automatic checks; on unless the user turned them off.
    auto: bool,
    /// When the last check was made (seconds since the epoch).
    last_check: u64,
    /// A version installed in the background, not yet announced.
    installed: Option<String>,
    /// A newer version that could not be installed here (a build from source, a folder that
    /// cannot be written): told at the next start.
    available: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self { auto: true, last_check: 0, installed: None, available: None }
    }
}

fn state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("po").join("update.json"))
}

impl State {
    fn load() -> Self {
        state_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = state_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `1.2.3` from `v1.2.3` or `1.2.3`; a pre-release (`1.2.3-rc.1`) is not one.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.trim().trim_start_matches('v');
    if text.contains('-') {
        return None;
    }
    let mut parts = text.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(version)
}

fn current() -> (u64, u64, u64) {
    parse_version(env!("CARGO_PKG_VERSION")).unwrap_or((0, 0, 0))
}

/// The binary's name in a release.
fn asset_name() -> String {
    format!("po-{TARGET}{}", std::env::consts::EXE_SUFFIX)
}

/// The latest release, as far as this binary is concerned.
#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub page: String,
    binary: Option<String>,
    sums: Option<String>,
}

impl Release {
    fn newer(&self) -> bool {
        parse_version(&self.version).is_some_and(|v| v > current())
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("po/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_default()
}

/// Reads the latest release from GitHub (published ones only: drafts and pre-releases are not
/// listed as latest).
pub fn release_from(value: &serde_json::Value) -> Option<Release> {
    let tag = value["tag_name"].as_str()?;
    let asset = |name: &str| {
        value["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(name))?["browser_download_url"]
            .as_str()
            .map(str::to_owned)
    };
    Some(Release {
        version: tag.trim_start_matches('v').to_owned(),
        page: value["html_url"].as_str().unwrap_or_default().to_owned(),
        binary: asset(&asset_name()),
        sums: asset("SHA256SUMS.txt"),
    })
}

async fn latest() -> Result<Release, String> {
    let config = crate::github::Config::from_env();
    let response = client()
        .get(format!("{}/repos/{}/releases/latest", config.api, config.upstream))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub {}", response.status().as_u16()));
    }
    let value: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    release_from(&value).ok_or_else(|| "no release".to_owned())
}

/// The SHA-256 the sums file gives for `name` (`<hex>  <name>` lines, as `sha256sum` writes).
pub fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        (file == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

async fn bytes(url: &str) -> Result<Vec<u8>, String> {
    let response = client().get(url).send().await.map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: {}", response.status().as_u16()));
    }
    response.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Downloads the release's binary, checks it, and puts it in place of the running one; where.
async fn install(release: &Release) -> Result<PathBuf, String> {
    let (Some(binary), Some(sums)) = (&release.binary, &release.sums) else {
        return Err(t!("update.no_asset", target = TARGET).to_owned());
    };
    let sums = String::from_utf8_lossy(&bytes(sums).await?).into_owned();
    let expected = expected_sum(&sums, &asset_name()).ok_or_else(|| t!("update.no_asset", target = TARGET).to_owned())?;
    let data = bytes(binary).await?;
    let actual = format!("{:x}", Sha256::digest(&data));
    if actual != expected {
        return Err(t!("update.bad_sum").to_owned());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let fresh = exe.with_file_name(format!("po.update-{}{}", release.version, std::env::consts::EXE_SUFFIX));
    std::fs::write(&fresh, &data).map_err(|e| format!("{}: {e}", fresh.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755));
    }
    // It must start and say its version before it replaces anything.
    let answers = std::process::Command::new(&fresh)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains(&release.version));
    if !answers {
        let _ = std::fs::remove_file(&fresh);
        return Err(t!("update.does_not_start").to_owned());
    }
    let placed = crate::labs::put_in_place(&fresh, &exe);
    let _ = std::fs::remove_file(&fresh);
    placed.map(|_| exe)
}

/// Removes the binaries an earlier update set aside, once nothing runs them any more.
fn clean_aside() {
    let Ok(exe) = std::env::current_exe() else { return };
    let (Some(dir), Some(stem)) = (exe.parent(), exe.file_stem()) else { return };
    let prefix = format!("{}.old-", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            // One still running stays: it cannot be removed.
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn disabled() -> bool {
    std::env::var("PO_NO_UPDATE").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The automatic check, when one is due: where the new version was installed, if one was.
async fn check_and_install(state: &mut State) -> Option<PathBuf> {
    state.last_check = now();
    state.save();
    let release = latest().await.ok()?;
    if !release.newer() {
        state.available = None;
        state.save();
        return None;
    }
    if !release_build() {
        state.available = Some(release.version);
        state.save();
        return None;
    }
    match install(&release).await {
        Ok(path) => {
            state.installed = Some(release.version);
            state.available = None;
            state.save();
            Some(path)
        }
        Err(err) => {
            tracing::warn!("update {}: {err}", release.version);
            state.available = Some(release.version);
            state.save();
            None
        }
    }
}

fn due(state: &State) -> bool {
    state.auto && !disabled() && now().saturating_sub(state.last_check) >= DAY
}

/// From a session's start: checks in the background when a check is due. What it finds is told
/// at the next start, not in the middle of this one.
pub fn spawn_check() {
    clean_aside();
    let mut state = State::load();
    if !due(&state) {
        return;
    }
    tokio::spawn(async move {
        check_and_install(&mut state).await;
    });
}

/// From the service: checks every few hours, and restarts on the new version once installed.
pub fn spawn_service_checks() {
    clean_aside();
    tokio::spawn(async {
        loop {
            let mut state = State::load();
            if due(&state)
                && let Some(installed) = check_and_install(&mut state).await
                // The new binary takes over: the service's lock is released when this process
                // ends, and the new one waits a moment for it.
                && crate::service::start_from(&installed).is_ok()
            {
                std::process::exit(0);
            }
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

/// What to tell at the start of a session, once: an update installed, or one available.
pub fn take_note() -> Option<String> {
    let mut state = State::load();
    if let Some(version) = state.installed.take() {
        state.save();
        // Announced only when it is indeed this binary.
        return (parse_version(&version) == Some(current())).then(|| t!("update.installed_note", version = version).to_owned());
    }
    let version = state.available.clone()?;
    if parse_version(&version).is_none_or(|v| v <= current()) {
        state.available = None;
        state.save();
        return None;
    }
    let config = crate::github::Config::from_env();
    Some(t!("update.available_note", version = version, page = format!("{}/{}/releases/latest", config.web, config.upstream)).to_owned())
}

/// `po update [check|on|off]`.
pub async fn command(argument: Option<&str>) -> Result<(), String> {
    let mut state = State::load();
    match argument.map(str::to_lowercase).as_deref() {
        Some("on") => {
            state.auto = true;
            state.save();
            println!("{}", t!("update.auto_on"));
            return Ok(());
        }
        Some("off") => {
            state.auto = false;
            state.save();
            println!("{}", t!("update.auto_off"));
            return Ok(());
        }
        None | Some("check") | Some("now") => {}
        Some(other) => return Err(t!("update.usage", arg = other).to_owned()),
    }
    let release = latest().await.map_err(|e| t!("update.unreachable", err = e).to_owned())?;
    state.last_check = now();
    state.save();
    if !release.newer() {
        println!("{}", t!("update.up_to_date", version = env!("CARGO_PKG_VERSION")));
        return Ok(());
    }
    if argument == Some("check") || !release_build() {
        println!("{}", t!("update.available_note", version = release.version, page = release.page));
        if !release_build() {
            println!("{}", t!("update.from_source"));
        }
        return Ok(());
    }
    println!("{}", t!("update.installing", version = release.version));
    let installed = install(&release).await?;
    state.available = None;
    state.save();
    println!("{}", t!("update.done", version = release.version));
    if crate::service::running().is_some() {
        crate::service::stop().await?;
        crate::service::start_from(&installed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_tags() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10.3"), Some((1, 10, 3)));
        assert_eq!(parse_version("v1.0.0-rc.1"), None, "a pre-release is not offered");
        assert_eq!(parse_version("v1.0"), None);
        assert!(parse_version("v0.10.0") > parse_version("v0.9.9"));
    }

    #[test]
    fn the_sum_of_this_platform_is_found() {
        let sums = format!("{}  po-v1.0.0-x86_64.tar.gz\n{}  po-{TARGET}{}\n", "a".repeat(64), "B".repeat(64), std::env::consts::EXE_SUFFIX);
        assert_eq!(expected_sum(&sums, &asset_name()), Some("b".repeat(64)));
        assert_eq!(expected_sum(&sums, "po-unknown"), None);
    }

    #[test]
    fn a_release_lists_this_platform_s_binary() {
        let value = serde_json::json!({
            "tag_name": "v9.0.0",
            "html_url": "https://example.test/r",
            "assets": [
                {"name": asset_name(), "browser_download_url": "https://example.test/bin"},
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://example.test/sums"}
            ]
        });
        let release = release_from(&value).unwrap();
        assert_eq!(release.version, "9.0.0");
        assert!(release.newer());
        assert_eq!(release.binary.as_deref(), Some("https://example.test/bin"));
        assert_eq!(release.sums.as_deref(), Some("https://example.test/sums"));
    }
}
