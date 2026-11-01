//! A fake GitHub on localhost, for the tests (and `examples/fake_github.rs`, for trying the app
//! by hand): the device flow, accounts, and secret gists with their ETags.
//!
//! Like the real one, a secret gist is readable by anyone who has its id, but only its owner
//! writes it: the tests of a leaked address rely on that.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Gist {
    pub owner: String,
    pub description: String,
    pub files: BTreeMap<String, String>,
    pub version: u64,
}

#[derive(Debug, Default)]
pub struct State {
    /// token → account.
    pub tokens: HashMap<String, String>,
    pub gists: BTreeMap<String, Gist>,
    /// Device codes waiting, and the account that approves them (`None`: still pending).
    pub devices: HashMap<String, Option<String>>,
    /// Who approves the next sign-ins.
    pub approve_as: Option<String>,
    /// Requests answered, and how many of them were "not modified".
    pub requests: u64,
    pub not_modified: u64,
    /// The next requests fail with 503 (GitHub down).
    pub failing: u32,
    /// Each request answered: method, path, status (for watching a device by hand).
    pub log: Vec<String>,
    next: u64,
}

/// A running fake GitHub.
#[derive(Clone)]
pub struct FakeGitHub {
    pub url: String,
    pub state: Arc<Mutex<State>>,
}

impl FakeGitHub {
    pub async fn start() -> Self {
        Self::start_on("127.0.0.1:0").await
    }

    pub async fn start_on(addr: &str) -> Self {
        let listener = TcpListener::bind(addr).await.expect("a local port");
        let url = format!("http://{}", listener.local_addr().expect("an address"));
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = shared.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, state).await;
                });
            }
        });
        Self { url, state }
    }

    /// Where Po and the app must send: API and web on the same server.
    pub fn config(&self) -> crate::github::Config {
        crate::github::Config { api: self.url.clone(), web: self.url.clone(), client_id: "fake".into() }
    }

    /// A token of `login`, as if it had signed in.
    pub fn token_for(&self, login: &str) -> String {
        let mut state = self.state.lock().unwrap();
        state.next += 1;
        let token = format!("gho_fake_{login}_{}", state.next);
        state.tokens.insert(token.clone(), login.to_owned());
        token
    }

    /// The next device sign-ins are approved as `login`.
    pub fn approve_as(&self, login: &str) {
        self.state.lock().unwrap().approve_as = Some(login.to_owned());
    }

    pub fn revoke(&self, token: &str) {
        self.state.lock().unwrap().tokens.remove(token);
    }

    pub fn fail_next(&self, n: u32) {
        self.state.lock().unwrap().failing = n;
    }

    /// The gists of `login`.
    pub fn gists_of(&self, login: &str) -> Vec<(String, Gist)> {
        self.state.lock().unwrap().gists.iter().filter(|(_, g)| g.owner == login).map(|(i, g)| (i.clone(), g.clone())).collect()
    }

    pub fn counts(&self) -> (u64, u64) {
        let state = self.state.lock().unwrap();
        (state.requests, state.not_modified)
    }
}

async fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let (method, target) = (first.next().unwrap_or("").to_owned(), first.next().unwrap_or("").to_owned());
    let headers: HashMap<String, String> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned())).collect();
    let length = headers.get("content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    while buffer.len() < head_end + length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buffer[head_end..(head_end + length).min(buffer.len())]).into_owned();
    let (status, extra, reply) = route(&state, &method, &target, &headers, &body);
    if let Ok(mut s) = state.lock() {
        s.log.push(format!("{method} {target} {status}"));
    }
    let text = if reply.is_null() { String::new() } else { reply.to_string() };
    let reason = match status {
        200 => "OK",
        201 => "Created",
        304 => "Not Modified",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let mut response = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n", text.len());
    for (k, v) in extra {
        response.push_str(&format!("{k}: {v}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&text);
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    // The client closes first: closing here at once can reset the connection before the
    // answer is read (seen through the emulator's network).
    let mut rest = [0u8; 256];
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut rest)).await;
    Ok(())
}

fn form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.replace('+', " ").replace("%3A", ":").replace("%20", " ")))
        .collect()
}

fn etag(gist: &Gist) -> String {
    format!("W/\"{}-{}\"", gist.files.len(), gist.version)
}

fn gist_json(id: &str, gist: &Gist, contents: bool) -> Value {
    let files: serde_json::Map<String, Value> = gist
        .files
        .iter()
        .map(|(n, c)| {
            let mut file = json!({"filename": n, "size": c.len(), "truncated": false});
            if contents {
                file["content"] = json!(c);
            }
            (n.clone(), file)
        })
        .collect();
    json!({"id": id, "description": gist.description, "public": false, "owner": {"login": gist.owner}, "files": files})
}

type Answer = (u16, Vec<(String, String)>, Value);

fn route(state: &Mutex<State>, method: &str, target: &str, headers: &HashMap<String, String>, body: &str) -> Answer {
    let mut state = state.lock().unwrap();
    state.requests += 1;
    if state.failing > 0 {
        state.failing -= 1;
        return (503, vec![], json!({"message": "Service Unavailable"}));
    }
    let path = target.split('?').next().unwrap_or("");
    // The device flow, approved by whoever `approve_as` names.
    if path == "/login/device/code" && method == "POST" {
        state.next += 1;
        let device = format!("dev{}", state.next);
        state.devices.insert(device.clone(), None);
        return (200, vec![], json!({"device_code": device, "user_code": format!("ABCD-{:04}", state.next), "verification_uri": "https://github.com/login/device", "interval": 1, "expires_in": 60}));
    }
    if path == "/login/oauth/access_token" && method == "POST" {
        let device = form(body).get("device_code").cloned().unwrap_or_default();
        let approver = state.approve_as.clone();
        return match (state.devices.get(&device).cloned(), approver) {
            (None, _) => (200, vec![], json!({"error": "expired_token"})),
            (Some(_), Some(login)) if login == "deny" => (200, vec![], json!({"error": "access_denied"})),
            (Some(_), Some(login)) => {
                state.devices.remove(&device);
                state.next += 1;
                let token = format!("gho_fake_{login}_{}", state.next);
                state.tokens.insert(token.clone(), login);
                (200, vec![], json!({"access_token": token, "token_type": "bearer", "scope": "gist"}))
            }
            (Some(_), None) => (200, vec![], json!({"error": "authorization_pending"})),
        };
    }
    let token = headers.get("authorization").and_then(|a| a.strip_prefix("Bearer ").or(a.strip_prefix("token "))).unwrap_or("");
    let Some(login) = state.tokens.get(token).cloned() else {
        return (401, vec![], json!({"message": "Bad credentials"}));
    };
    let scopes = vec![("X-OAuth-Scopes".to_owned(), "gist, public_repo".to_owned())];
    if path == "/user" {
        return (200, scopes, json!({"login": login, "id": login.len() as u64 * 1000 + 7}));
    }
    if path == "/gists" && method == "GET" {
        let list: Vec<Value> = state.gists.iter().rev().filter(|(_, g)| g.owner == login).map(|(i, g)| gist_json(i, g, false)).collect();
        return (200, vec![], Value::Array(list));
    }
    if path == "/gists" && method == "POST" {
        let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        state.next += 1;
        let id = format!("{:020x}", state.next.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut files = BTreeMap::new();
        for (name, file) in request["files"].as_object().into_iter().flatten() {
            files.insert(name.clone(), file["content"].as_str().unwrap_or_default().to_owned());
        }
        let gist = Gist { owner: login, description: request["description"].as_str().unwrap_or_default().to_owned(), files, version: 1 };
        let out = gist_json(&id, &gist, true);
        state.gists.insert(id, gist);
        return (201, vec![], out);
    }
    if let Some(id) = path.strip_prefix("/gists/") {
        let Some(gist) = state.gists.get(id).cloned() else {
            return (404, vec![], json!({"message": "Not Found"}));
        };
        if method == "GET" {
            let tag = etag(&gist);
            if headers.get("if-none-match") == Some(&tag) {
                state.not_modified += 1;
                return (304, vec![("ETag".into(), tag)], Value::Null);
            }
            return (200, vec![("ETag".into(), tag)], gist_json(id, &gist, true));
        }
        if method == "PATCH" {
            if gist.owner != login {
                return (404, vec![], json!({"message": "Not Found"}));
            }
            let request: Value = serde_json::from_str(body).unwrap_or(Value::Null);
            let entry = state.gists.get_mut(id).expect("checked");
            for (name, file) in request["files"].as_object().into_iter().flatten() {
                match file["content"].as_str() {
                    Some(content) => {
                        entry.files.insert(name.clone(), content.to_owned());
                    }
                    None => {
                        entry.files.remove(name);
                    }
                }
            }
            entry.version += 1;
            let out = gist_json(id, entry, true);
            return (200, vec![], out);
        }
        if method == "DELETE" && gist.owner == login {
            state.gists.remove(id);
            return (204, vec![], Value::Null);
        }
    }
    (404, vec![], json!({"message": "Not Found"}))
}
