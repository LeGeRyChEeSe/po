//! Sends a contribution of Po Labs to Po's repository on GitHub: the user's connection (GitHub
//! CLI's if it is there, else a device flow, remembered for every project) and a pull request
//! from their fork, made through GitHub's REST API — no `git` program.

use std::path::PathBuf;

use base64::Engine;
use serde_json::{Value, json};

use po_i18n::t;

/// The OAuth App of Po, for the device flow (a client id is public).
const CLIENT_ID: &str = "Ov23liB8iA4k9ZMM0f5R";
/// Po's repository, where contributions go.
const UPSTREAM: &str = "LeGeRyChEeSe/po";

/// Where to send: GitHub, unless the environment says otherwise (a test server).
#[derive(Debug, Clone)]
pub struct Config {
    pub api: String,
    pub web: String,
    pub client_id: String,
    pub upstream: String,
    /// Where the files of the repository are read as they are (the catalog's).
    pub raw: String,
}

impl Config {
    pub fn from_env() -> Self {
        let var = |name: &str, default: &str| std::env::var(name).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.to_owned());
        Self {
            api: var("PO_GITHUB_API", "https://api.github.com").trim_end_matches('/').to_owned(),
            web: var("PO_GITHUB_WEB", "https://github.com").trim_end_matches('/').to_owned(),
            client_id: var("PO_GITHUB_CLIENT_ID", CLIENT_ID),
            upstream: var("PO_UPSTREAM", UPSTREAM),
            raw: var("PO_GITHUB_RAW", "https://raw.githubusercontent.com").trim_end_matches('/').to_owned(),
        }
    }

    /// Where contributions go, for the user: `github.com/owner/po`.
    pub fn target(&self) -> String {
        let host = self.web.trim_start_matches("https://").trim_start_matches("http://");
        format!("{host}/{}", self.upstream)
    }

    /// GitHub itself: GitHub CLI's connection is only valid there.
    fn is_github(&self) -> bool {
        self.api == "https://api.github.com"
    }
}

/// Says to the user how it goes (and, for the device flow, what to do).
pub type Progress = po_tui::LabsProgress;

/// The connection remembered, in `%APPDATA%\po\github.json`: one for every project.
fn token_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po").join("github.json"))
}

pub(crate) fn stored_token(config: &Config) -> Option<String> {
    let text = std::fs::read_to_string(token_path()?).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    (value["api"].as_str() == Some(config.api.as_str())).then(|| value["token"].as_str().map(str::to_owned)).flatten()
}

pub(crate) fn store_token(config: &Config, token: &str) {
    let Some(path) = token_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, json!({"api": config.api, "token": token}).to_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
}

pub(crate) fn forget_token() {
    if let Some(path) = token_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// GitHub CLI's token, when it is installed and connected.
async fn gh_token() -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !token.is_empty()).then_some(token)
}

/// A call to GitHub's REST API.
pub(crate) struct Api {
    http: reqwest::Client,
    config: Config,
    token: String,
}

/// A failed call: its status (0 without an answer) and what GitHub said.
#[derive(Debug)]
pub(crate) struct Failure {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub {}: {}", self.status, self.message)
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("po/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

impl Api {
    /// Without an account: what anyone can read (GitHub allows fewer calls an hour then).
    pub(crate) fn anonymous(config: &Config) -> Self {
        Self { http: client(), config: config.clone(), token: String::new() }
    }

    pub(crate) async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, Failure> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.config.api))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if !self.token.is_empty() {
            request = request.bearer_auth(&self.token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|e| Failure { status: 0, message: e.to_string() })?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            let message = value["message"].as_str().map(str::to_owned).unwrap_or(text);
            Err(Failure { status, message })
        }
    }

    pub(crate) async fn get(&self, path: &str) -> Result<Value, Failure> {
        self.call(reqwest::Method::GET, path, None).await
    }

    pub(crate) async fn post(&self, path: &str, body: Value) -> Result<Value, Failure> {
        self.call(reqwest::Method::POST, path, Some(body)).await
    }

    /// A file of the repository as it is, from the raw files' server.
    pub(crate) async fn raw(&self, branch: &str, path: &str) -> Result<Vec<u8>, Failure> {
        let url = format!("{}/{}/{branch}/{path}", self.config.raw, self.config.upstream);
        let response = self.http.get(url).send().await.map_err(|e| Failure { status: 0, message: e.to_string() })?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(|e| Failure { status, message: e.to_string() })?;
        if (200..300).contains(&status) {
            Ok(bytes.to_vec())
        } else {
            Err(Failure { status, message: String::from_utf8_lossy(&bytes).chars().take(200).collect() })
        }
    }
}

/// The user's account: the connection remembered, else GitHub CLI's, else a new one through the
/// device flow (a code to enter on a GitHub page).
async fn connect(config: &Config, progress: &Progress) -> Result<(Api, Value), String> {
    if let Some(found) = connected(config).await? {
        return Ok(found);
    }
    let http = client();
    let start = device_start(&http, config).await?;
    progress(t!("labs.github_device", url = start.url, code = start.code));
    let token = device_wait(&http, config, &start).await?;
    let api = Api { http, config: config.clone(), token };
    let user = api.get("/user").await.map_err(|e| e.to_string())?;
    store_token(config, &api.token);
    Ok((api, user))
}

/// The user's account if already connected (remembered, or GitHub CLI's), without asking.
pub(crate) async fn connected(config: &Config) -> Result<Option<(Api, Value)>, String> {
    let http = client();
    let mut candidates = Vec::new();
    if let Some(token) = stored_token(config) {
        candidates.push((token, true));
    }
    if config.is_github()
        && let Some(token) = gh_token().await
    {
        candidates.push((token, false));
    }
    for (token, stored) in candidates {
        let api = Api { http: http.clone(), config: config.clone(), token };
        match api.get("/user").await {
            Ok(user) => return Ok(Some((api, user))),
            Err(Failure { status: 401, .. }) if stored => forget_token(),
            Err(err) if err.status == 0 => return Err(err.to_string()),
            Err(_) => {}
        }
    }
    Ok(None)
}

/// Starts a connection to GitHub without waiting for it: the page and the code to enter there;
/// Po waits for the go-ahead in the background and remembers the connection.
pub(crate) async fn login_in_background(config: &Config) -> Result<(String, String), String> {
    let http = client();
    let start = device_start(&http, config).await?;
    let (url, code) = (start.url.clone(), start.code.clone());
    let config = config.clone();
    tokio::spawn(async move {
        if let Ok(token) = device_wait(&http, &config, &start).await {
            store_token(&config, &token);
        }
    });
    Ok((url, code))
}

/// The start of GitHub's device flow: a page to open and a code to enter there.
struct DeviceStart {
    device: String,
    code: String,
    url: String,
    interval: u64,
    expires: u64,
}

async fn device_start(http: &reqwest::Client, config: &Config) -> Result<DeviceStart, String> {
    if config.client_id.is_empty() {
        return Err(t!("labs.github_no_login").to_owned());
    }
    let start: Value = http
        .post(format!("{}/login/device/code", config.web))
        .header("Accept", "application/json")
        // One connection for both: sharing Po Labs contributions, and the phone's directory.
        .form(&[("client_id", config.client_id.as_str()), ("scope", "public_repo gist")])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let (Some(device), Some(code), Some(url)) = (start["device_code"].as_str(), start["user_code"].as_str(), start["verification_uri"].as_str()) else {
        return Err(format!("GitHub: {}", start["error_description"].as_str().or(start["error"].as_str()).unwrap_or("no device code")));
    };
    Ok(DeviceStart {
        device: device.to_owned(),
        code: code.to_owned(),
        url: url.to_owned(),
        interval: start["interval"].as_u64().unwrap_or(5).max(1),
        expires: start["expires_in"].as_u64().unwrap_or(900),
    })
}

/// Waits until the user enters the code (or declines, or lets it expire): the account's token.
async fn device_wait(http: &reqwest::Client, config: &Config, start: &DeviceStart) -> Result<String, String> {
    let device = start.device.as_str();
    let mut interval = start.interval;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(start.expires);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let answer: Value = http
            .post(format!("{}/login/oauth/access_token", config.web))
            .header("Accept", "application/json")
            .form(&[("client_id", config.client_id.as_str()), ("device_code", device), ("grant_type", "urn:ietf:params:oauth:grant-type:device_code")])
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        if let Some(token) = answer["access_token"].as_str() {
            return Ok(token.to_owned());
        }
        match answer["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("access_denied") => return Err(t!("labs.github_denied").to_owned()),
            Some(other) => return Err(format!("GitHub: {other}")),
            None => return Err("GitHub: no token".to_owned()),
        }
    }
    Err(t!("labs.github_expired").to_owned())
}

/// The branch of contribution `id`, from its title: `po-labs/3-orange-tool-calls`.
pub fn branch_name(id: u32, title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    format!("po-labs/{id}-{}", slug.trim_end_matches('-'))
}

/// The text of the pull request: the contribution's description, then what kind it is.
pub fn pull_body(kept: &po_tui::labs::Kept) -> String {
    let draft = kept.draft.clone().unwrap_or_default();
    let kind = if kept.core { "Core change" } else { "Extension" };
    format!(
        "{}\n\n---\n**Category:** {} · **{kind}**\n\nTried live and kept by its author with Po Labs.",
        draft.description.trim(),
        if draft.category.is_empty() { "other" } else { &draft.category },
    )
}

/// The files whose version on GitHub is not the one the change started from: sending would undo
/// what changed there since.
pub fn conflicts(changes: &[crate::labs_git::FileChange], upstream: &[(String, Option<String>)]) -> Vec<String> {
    changes
        .iter()
        .filter(|c| upstream.iter().find(|(p, _)| *p == c.path).map(|(_, sha)| sha) != Some(&c.before))
        .map(|c| c.path.clone())
        .collect()
}

/// Sends contribution `kept`: its files, onto Po's latest version, in a branch of the user's
/// fork, and a pull request to Po. Returns the pull request's address.
pub async fn share(kept: po_tui::labs::Kept, progress: Progress) -> Result<String, String> {
    let config = Config::from_env();
    let draft = kept.draft.clone().ok_or("no text")?;
    let dir = crate::labs_git::workspace_dir().ok_or_else(|| t!("tui.labs.no_source").to_owned())?;
    let changes = crate::labs_git::file_changes(&dir, &kept.base, &kept.head).map_err(|e| e.to_string())?;
    let (api, user) = connect(&config, &progress).await?;
    let login = user["login"].as_str().ok_or("GitHub: no login")?.to_owned();
    progress(t!("labs.github_sending", login = login));
    let err = |e: Failure| e.to_string();
    let upstream = api.get(&format!("/repos/{}", config.upstream)).await.map_err(err)?;
    let branch = upstream["default_branch"].as_str().unwrap_or("main").to_owned();
    let owner = upstream["owner"]["login"].as_str().unwrap_or_default();

    // Where the branch goes: the user's fork (made if needed), or Po itself for its owner.
    let target = if owner.eq_ignore_ascii_case(&login) {
        config.upstream.clone()
    } else {
        let fork = api.post(&format!("/repos/{}/forks", config.upstream), json!({"default_branch_only": true})).await.map_err(err)?;
        let name = fork["full_name"].as_str().ok_or("GitHub: no fork")?.to_owned();
        // A new fork takes a moment to be ready.
        for _ in 0..30 {
            if api.get(&format!("/repos/{name}/git/ref/heads/{branch}")).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        // Up to date with Po, so that Po's latest version is there to build on.
        let _ = api.post(&format!("/repos/{name}/merge-upstream"), json!({"branch": branch})).await;
        name
    };

    let base = api.get(&format!("/repos/{}/git/ref/heads/{branch}", config.upstream)).await.map_err(err)?;
    let base = base["object"]["sha"].as_str().ok_or("GitHub: no base")?.to_owned();
    let base_tree = api.get(&format!("/repos/{}/git/commits/{base}", config.upstream)).await.map_err(err)?;
    let base_tree = base_tree["tree"]["sha"].as_str().ok_or("GitHub: no tree")?.to_owned();

    // Each file must be on GitHub as it was before the change: else sending would undo what
    // changed there since.
    let mut upstream_files = Vec::new();
    for change in &changes {
        let sha = match api.get(&format!("/repos/{}/contents/{}?ref={base}", config.upstream, change.path)).await {
            Ok(file) => file["sha"].as_str().map(str::to_owned),
            Err(Failure { status: 404, .. }) => None,
            Err(e) => return Err(e.to_string()),
        };
        upstream_files.push((change.path.clone(), sha));
    }
    let behind = conflicts(&changes, &upstream_files);
    if !behind.is_empty() {
        return Err(t!("labs.github_conflict", files = behind.join(", ")));
    }

    let mut tree = Vec::new();
    for change in &changes {
        let mode = if change.executable { "100755" } else { "100644" };
        match &change.after {
            Some(content) => {
                let blob = api
                    .post(&format!("/repos/{target}/git/blobs"), json!({"content": base64::engine::general_purpose::STANDARD.encode(content), "encoding": "base64"}))
                    .await
                    .map_err(err)?;
                tree.push(json!({"path": change.path, "mode": mode, "type": "blob", "sha": blob["sha"]}));
            }
            None => tree.push(json!({"path": change.path, "mode": mode, "type": "blob", "sha": Value::Null})),
        }
    }
    let tree = api.post(&format!("/repos/{target}/git/trees"), json!({"base_tree": base_tree, "tree": tree})).await.map_err(err)?;
    // Signed with GitHub's private address of the account: the user's own email stays theirs.
    let email = format!("{}+{login}@users.noreply.github.com", user["id"].as_u64().unwrap_or(0));
    let commit = api
        .post(
            &format!("/repos/{target}/git/commits"),
            json!({"message": draft.title, "tree": tree["sha"], "parents": [base], "author": {"name": login, "email": email}}),
        )
        .await
        .map_err(err)?;
    let sha = commit["sha"].as_str().ok_or("GitHub: no commit")?.to_owned();
    let name = branch_name(kept.id, &draft.title);
    if let Err(e) = api.post(&format!("/repos/{target}/git/refs"), json!({"ref": format!("refs/heads/{name}"), "sha": sha})).await {
        // Sent before: the branch takes the new version.
        if e.status != 422 {
            return Err(e.to_string());
        }
        api.call(reqwest::Method::PATCH, &format!("/repos/{target}/git/refs/heads/{name}"), Some(json!({"sha": sha, "force": true}))).await.map_err(err)?;
    }

    let head = if target == config.upstream { name.clone() } else { format!("{login}:{name}") };
    let pull = api
        .post(
            &format!("/repos/{}/pulls", config.upstream),
            json!({"title": draft.title, "head": head, "base": branch, "body": pull_body(&kept), "maintainer_can_modify": true}),
        )
        .await;
    match pull {
        Ok(pull) => pull["html_url"].as_str().map(str::to_owned).ok_or_else(|| "GitHub: no address".to_owned()),
        // Already open: it now shows the new version.
        Err(e) if e.status == 422 => {
            let open = api.get(&format!("/repos/{}/pulls?head={login}:{name}&state=open", config.upstream)).await.map_err(err)?;
            open[0]["html_url"].as_str().map(str::to_owned).ok_or_else(|| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labs_git::FileChange;

    #[test]
    fn the_branch_of_a_contribution_is_named_after_its_title() {
        assert_eq!(branch_name(3, "Make tool calls orange!"), "po-labs/3-make-tool-calls-orange");
        assert_eq!(branch_name(1, "« Lab » → Labo"), "po-labs/1-lab-labo");
        assert!(branch_name(2, &"long title ".repeat(20)).len() <= "po-labs/2-".len() + 41);
    }

    #[test]
    fn a_file_changed_on_github_since_is_not_overwritten() {
        let change = |path: &str, before: Option<&str>| FileChange { path: path.into(), before: before.map(str::to_owned), after: Some(b"x".to_vec()), executable: false };
        let changes = [change("a.rs", Some("111")), change("new.rs", None), change("b.rs", Some("222"))];
        let upstream = [("a.rs".to_owned(), Some("111".to_owned())), ("new.rs".to_owned(), None), ("b.rs".to_owned(), Some("999".to_owned()))];
        assert_eq!(conflicts(&changes, &upstream), vec!["b.rs".to_owned()]);
    }

    #[test]
    fn the_pull_request_says_what_kind_of_change_it_is() {
        let kept = po_tui::labs::Kept {
            ids: vec![1],
            summary: String::new(),
            base: String::new(),
            head: String::new(),
            core: false,
            date: String::new(),
            id: 1,
            draft: Some(po_tui::labs::Draft { title: "T".into(), description: "Does a thing.".into(), category: "appearance".into(), user_summary: String::new() }),
            share: po_tui::labs::Share::Ready,
            files: Vec::new(),
        };
        let body = pull_body(&kept);
        assert!(body.starts_with("Does a thing.") && body.contains("appearance") && body.contains("Extension"));
    }
}
