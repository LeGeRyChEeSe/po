//! `po dev`: a Po session plugged into a built-in fake server, to try out the sub-agent
//! interface without a model (card deck, sweep, large card).
//!
//! The fake server speaks the OpenAI API, advertises several slots, and always answers the
//! same way: the main agent launches as many sub-agents as asked ("launch 5 subagents", 3 by
//! default); each one thinks, writes its "Progress: ..." lines for about twenty seconds, then
//! returns an empty report. Sub-agents call no tool: nothing is read or modified. Strata and
//! other servers are not probed.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MODEL: &str = "fake-model";

/// Starts the fake server; returns its port.
pub async fn start(slots: usize) -> std::io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(async move {
                let _ = serve(stream, slots).await;
            });
        }
    });
    Ok(port)
}

/// Prepares a throwaway working folder whose `po.toml` points at the fake server.
pub fn prepare(port: u16) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("po-dev");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("po.toml"),
        format!(
            "# Written by `po dev`: built-in fake server, no discovery.\n\
             discovery = false\n\n\
             [[provider]]\nname = \"dev\"\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n\n\
             [[model]]\nid = \"{MODEL}\"\nprovider = \"dev\"\ntier = \"frontier\"\ncontext_window = 65536\n"
        ),
    )?;
    Ok(dir)
}

async fn serve(mut stream: TcpStream, slots: usize) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16384];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let length = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    match (method, path) {
        ("GET", p) if p.ends_with("/models") => {
            json_reply(&mut stream, &json!({"object": "list", "data": [{"id": MODEL, "object": "model", "max_model_len": 65536}]}))
                .await
        }
        ("GET", "/props") => {
            json_reply(&mut stream, &json!({"total_slots": slots, "model_alias": MODEL, "modalities": {"vision": false}})).await
        }
        ("POST", p) if p.ends_with("/chat/completions") => {
            let request: Value = serde_json::from_slice(&buffer[header_end..]).unwrap_or(Value::Null);
            complete(&mut stream, &request).await
        }
        _ => stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await,
    }
}

async fn json_reply(stream: &mut TcpStream, value: &Value) -> std::io::Result<()> {
    let body = value.to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await
}

/// Text of a message (string or list of parts).
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// What the fake model answers, depending on who calls it.
enum Script {
    Title,
    Subagent { seed: u64 },
    Delegate { count: usize },
    Conclude { count: usize },
}

fn script(request: &Value) -> Script {
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let users: Vec<&Value> = messages.iter().filter(|m| m["role"] == "user").collect();
    let first = users.first().map(|m| text_of(m)).unwrap_or_default();
    let last = users.last().map(|m| text_of(m)).unwrap_or_default();
    if last.contains("[Harness request") {
        return Script::Title;
    }
    if first.starts_with("[Subagent") {
        let seed = first.bytes().fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
        return Script::Subagent { seed };
    }
    if messages.last().is_some_and(|m| m["role"] == "tool") {
        let count = messages.iter().filter(|m| m["role"] == "tool").count();
        return Script::Conclude { count };
    }
    let count = last
        .split(|c: char| !c.is_ascii_digit())
        .find_map(|n| n.parse::<usize>().ok())
        .unwrap_or(3)
        .clamp(1, 6);
    Script::Delegate { count }
}

const STEPS: &[&str] = &[
    "browsing the project folders",
    "reading the configuration and dependencies",
    "comparing the modules with each other",
    "checking the existing tests",
    "gathering my observations",
    "writing a short report",
];

const AGENTS: &[&str] = &["explorer", "reviewer", "verifier", "planner", "builder", "summarizer"];

async fn complete(stream: &mut TcpStream, request: &Value) -> std::io::Result<()> {
    let streaming = request["stream"].as_bool().unwrap_or(false);
    let script = script(request);
    if !streaming {
        let text = match script {
            Script::Title => "Subagent sandbox".to_owned(),
            _ => "Fake model reply.".to_owned(),
        };
        return json_reply(
            stream,
            &json!({
                "model": MODEL,
                "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 10},
            }),
        )
        .await;
    }
    stream
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")
        .await?;
    let mut sse = Sse { stream };
    sse.delta(json!({"role": "assistant"}), None).await?;
    match script {
        Script::Title => sse.words("Subagent sandbox", 20).await?,
        Script::Subagent { seed } => {
            // Each fake sub-agent has its own pace: their cards advance out of step.
            let steps = 4 + (seed % 3) as usize;
            for step in 0..steps {
                let pause = 250 + (seed.rotate_left(step as u32 * 7) % 250);
                for _ in 0..(6 + (seed as usize + step) % 8) {
                    sse.delta(json!({"reasoning_content": "pretending to think. "}), None).await?;
                    tokio::time::sleep(Duration::from_millis(pause)).await;
                }
                let phrase = STEPS[(seed as usize + step) % STEPS.len()];
                sse.words(&format!("Progress: {phrase}, step {} of {steps}\n", step + 1), 60).await?;
            }
            sse.words("Report: fake subagent, nothing was read or modified.", 40).await?;
        }
        Script::Delegate { count } => {
            sse.words(&format!("Launching {count} fake subagents in parallel."), 30).await?;
            let calls: Vec<Value> = (0..count)
                .map(|i| {
                    json!({"index": i, "id": format!("fake-{i}"), "type": "function", "function": {
                        "name": "task",
                        "arguments": json!({
                            "agent": AGENTS[i % AGENTS.len()],
                            "task": format!("Dummy task #{}: pretend to work, touch nothing.", i + 1),
                        }).to_string(),
                    }})
                })
                .collect();
            sse.delta(json!({"tool_calls": calls}), None).await?;
            sse.delta(json!({}), Some("tool_calls")).await?;
            return sse.end().await;
        }
        Script::Conclude { count } => {
            sse.words(&format!("The {count} fake subagents are done. Nothing was modified: this is a sandbox."), 25)
                .await?
        }
    }
    sse.delta(json!({}), Some("stop")).await?;
    sse.end().await
}

struct Sse<'a> {
    stream: &'a mut TcpStream,
}

impl Sse<'_> {
    async fn delta(&mut self, delta: Value, finish: Option<&str>) -> std::io::Result<()> {
        let event = json!({"model": MODEL, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        self.stream.write_all(format!("data: {event}\n\n").as_bytes()).await?;
        self.stream.flush().await
    }

    async fn words(&mut self, text: &str, pause_ms: u64) -> std::io::Result<()> {
        for word in text.split_inclusive(' ') {
            self.delta(json!({"content": word}), None).await?;
            tokio::time::sleep(Duration::from_millis(pause_ms)).await;
        }
        Ok(())
    }

    async fn end(&mut self) -> std::io::Result<()> {
        let usage = json!({"choices": [], "usage": {"prompt_tokens": 500, "completion_tokens": 120}});
        self.stream.write_all(format!("data: {usage}\n\ndata: [DONE]\n\n").as_bytes()).await?;
        self.stream.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_model_recognizes_its_caller() {
        let request = json!({"messages": [{"role": "system", "content": "s"}, {"role": "user", "content": "launch 5 subagents"}]});
        assert!(matches!(script(&request), Script::Delegate { count: 5 }));
        let request_fr = json!({"messages": [{"role": "user", "content": "lance 5 sous-agents"}]});
        assert!(matches!(script(&request_fr), Script::Delegate { count: 5 }));
        let subagent = json!({"messages": [{"role": "user", "content": "[Subagent explorer] Dummy task"}]});
        assert!(matches!(script(&subagent), Script::Subagent { .. }));
        let tool_results = json!({"messages": [{"role": "user", "content": "x"}, {"role": "assistant", "content": null},
            {"role": "tool", "content": "a"}, {"role": "tool", "content": "b"}]});
        assert!(matches!(script(&tool_results), Script::Conclude { count: 2 }));
        assert!(matches!(script(&json!({"messages": [{"role": "user", "content": "hello"}]})), Script::Delegate { count: 3 }));
    }
}
