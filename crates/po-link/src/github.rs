//! The little of GitHub's API the link needs: the device flow (signing in on a phone as on the
//! PC), the account, and one secret gist read with conditional requests.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

/// The OAuth App of Po (a client id is public).
pub const CLIENT_ID: &str = "Ov23liB8iA4k9ZMM0f5R";
/// What the link needs: the gist. Po's sharing of Po Labs contributions adds `public_repo`.
pub const SCOPE: &str = "gist";

/// Where GitHub is: GitHub itself, or a test server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub api: String,
    pub web: String,
    pub client_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { api: "https://api.github.com".into(), web: "https://github.com".into(), client_id: CLIENT_ID.into() }
    }
}

impl Config {
    /// GitHub, unless `PO_GITHUB_API`, `PO_GITHUB_WEB` or `PO_GITHUB_CLIENT_ID` say otherwise.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let default = Self::default();
        Self {
            api: var("PO_GITHUB_API").unwrap_or(default.api).trim_end_matches('/').to_owned(),
            web: var("PO_GITHUB_WEB").unwrap_or(default.web).trim_end_matches('/').to_owned(),
            client_id: var("PO_GITHUB_CLIENT_ID").unwrap_or(default.client_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// No answer (no network, DNS, timeout).
    Network(String),
    /// GitHub answered with an error.
    Status(u16, String),
    /// The user declined the sign-in.
    Denied,
    /// The code was not entered in time.
    Expired,
}

impl Error {
    /// The token is no longer valid (revoked, or another account's).
    pub fn unauthorized(&self) -> bool {
        matches!(self, Error::Status(401, _))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Network(e) => write!(f, "GitHub unreachable: {e}"),
            Error::Status(code, message) => write!(f, "GitHub {code}: {message}"),
            Error::Denied => write!(f, "sign-in declined"),
            Error::Expired => write!(f, "sign-in code expired"),
        }
    }
}

impl std::error::Error for Error {}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("po-link/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

fn network(e: reqwest::Error) -> Error {
    Error::Network(e.to_string())
}

/// The start of a device flow: the page to open and the code to type there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device: String,
    pub user_code: String,
    pub url: String,
    pub interval: u64,
    pub expires: u64,
}

pub async fn device_start(http: &reqwest::Client, config: &Config, scope: &str) -> Result<DeviceCode, Error> {
    let start: Value = http
        .post(format!("{}/login/device/code", config.web))
        .header("Accept", "application/json")
        .form(&[("client_id", config.client_id.as_str()), ("scope", scope)])
        .send()
        .await
        .map_err(network)?
        .json()
        .await
        .map_err(network)?;
    match (start["device_code"].as_str(), start["user_code"].as_str(), start["verification_uri"].as_str()) {
        (Some(device), Some(code), Some(url)) => Ok(DeviceCode {
            device: device.into(),
            user_code: code.into(),
            url: url.into(),
            interval: start["interval"].as_u64().unwrap_or(5).max(1),
            expires: start["expires_in"].as_u64().unwrap_or(900),
        }),
        _ => Err(Error::Status(400, start["error_description"].as_str().or(start["error"].as_str()).unwrap_or("no device code").to_owned())),
    }
}

/// Waits for the user to enter the code: the account's token.
pub async fn device_wait(http: &reqwest::Client, config: &Config, code: &DeviceCode) -> Result<String, Error> {
    let mut interval = code.interval;
    let deadline = std::time::Instant::now() + Duration::from_secs(code.expires);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let answer: Value = match http
            .post(format!("{}/login/oauth/access_token", config.web))
            .header("Accept", "application/json")
            .form(&[("client_id", config.client_id.as_str()), ("device_code", code.device.as_str()), ("grant_type", "urn:ietf:params:oauth:grant-type:device_code")])
            .send()
            .await
        {
            Ok(response) => response.json().await.map_err(network)?,
            // A phone in a tunnel while the user types the code: try again.
            Err(_) => continue,
        };
        if let Some(token) = answer["access_token"].as_str() {
            return Ok(token.to_owned());
        }
        match answer["error"].as_str() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("access_denied") => return Err(Error::Denied),
            Some("expired_token") => return Err(Error::Expired),
            Some(other) => return Err(Error::Status(400, other.to_owned())),
            None => return Err(Error::Status(400, "no token".into())),
        }
    }
    Err(Error::Expired)
}

/// The account behind a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub login: String,
    pub id: u64,
    /// The rights the token carries (`X-OAuth-Scopes`); `None` when GitHub does not say.
    pub scopes: Option<Vec<String>>,
}

impl User {
    /// The token can use gists (or GitHub did not say, and we will see).
    pub fn can_gist(&self) -> bool {
        self.scopes.as_ref().is_none_or(|s| s.iter().any(|x| x == "gist"))
    }
}

/// A gist as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetch {
    /// Unchanged since the ETag given: GitHub does not count it in the quota.
    NotModified,
    Fetched { etag: Option<String>, files: BTreeMap<String, String> },
    /// Deleted (or never was).
    Gone,
}

/// An account's API.
#[derive(Clone)]
pub struct GitHub {
    http: reqwest::Client,
    api: String,
    token: String,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GitHub({})", self.api)
    }
}

impl GitHub {
    pub fn new(config: &Config, token: impl Into<String>) -> Self {
        Self { http: http(), api: config.api.clone(), token: token.into() }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.api))
            .bearer_auth(&self.token)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<(reqwest::header::HeaderMap, Value), Error> {
        let response = request.send().await.map_err(network)?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let text = response.text().await.map_err(network)?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok((headers, value))
        } else {
            Err(Error::Status(status, value["message"].as_str().map(str::to_owned).unwrap_or(text)))
        }
    }

    pub async fn user(&self) -> Result<User, Error> {
        let (headers, user) = self.send(self.request(reqwest::Method::GET, "/user")).await?;
        let scopes = headers
            .get("x-oauth-scopes")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect());
        Ok(User { login: user["login"].as_str().unwrap_or_default().to_owned(), id: user["id"].as_u64().unwrap_or(0), scopes })
    }

    /// The account's gist with this description, if there is one.
    pub async fn find_gist(&self, description: &str) -> Result<Option<String>, Error> {
        for page in 1..=10 {
            let (_, list) = self.send(self.request(reqwest::Method::GET, &format!("/gists?per_page=100&page={page}"))).await?;
            let Some(list) = list.as_array() else { break };
            if let Some(found) = list.iter().find(|g| g["description"].as_str() == Some(description)) {
                return Ok(found["id"].as_str().map(str::to_owned));
            }
            if list.len() < 100 {
                break;
            }
        }
        Ok(None)
    }

    /// Makes a secret gist; its id.
    pub async fn create_gist(&self, description: &str, files: &[(&str, &str)]) -> Result<String, Error> {
        let files: serde_json::Map<String, Value> = files.iter().map(|(n, c)| ((*n).to_owned(), json!({"content": c}))).collect();
        let (_, gist) = self.send(self.request(reqwest::Method::POST, "/gists").json(&json!({"description": description, "public": false, "files": files}))).await?;
        gist["id"].as_str().map(str::to_owned).ok_or_else(|| Error::Status(500, "no gist id".into()))
    }

    /// Reads a gist, unless it has not changed since `etag`.
    pub async fn get_gist(&self, id: &str, etag: Option<&str>) -> Result<Fetch, Error> {
        let mut request = self.request(reqwest::Method::GET, &format!("/gists/{id}"));
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag);
        }
        let response = request.send().await.map_err(network)?;
        let status = response.status().as_u16();
        if status == 304 {
            return Ok(Fetch::NotModified);
        }
        if status == 404 {
            return Ok(Fetch::Gone);
        }
        let etag = response.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let text = response.text().await.map_err(network)?;
        let gist: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            return Err(Error::Status(status, gist["message"].as_str().map(str::to_owned).unwrap_or(text)));
        }
        let mut files = BTreeMap::new();
        if let Some(map) = gist["files"].as_object() {
            for (name, file) in map {
                let content = match (file["content"].as_str(), file["truncated"].as_bool(), file["raw_url"].as_str()) {
                    // Over a megabyte, GitHub cuts the content: the whole file is at its raw address.
                    (_, Some(true), Some(raw)) => self.http.get(raw).bearer_auth(&self.token).send().await.map_err(network)?.text().await.map_err(network)?,
                    (Some(content), _, _) => content.to_owned(),
                    _ => continue,
                };
                files.insert(name.clone(), content);
            }
        }
        Ok(Fetch::Fetched { etag, files })
    }

    /// Writes files of a gist (`None` deletes one); the others stay as they are.
    pub async fn write_files(&self, id: &str, files: &[(String, Option<String>)]) -> Result<(), Error> {
        let files: serde_json::Map<String, Value> = files
            .iter()
            .map(|(n, c)| (n.clone(), c.as_ref().map(|c| json!({"content": c})).unwrap_or(Value::Null)))
            .collect();
        self.send(self.request(reqwest::Method::PATCH, &format!("/gists/{id}")).json(&json!({"files": files}))).await.map(|_| ())
    }
}
