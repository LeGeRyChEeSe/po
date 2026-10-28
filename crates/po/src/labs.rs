//! Po Labs: the user helps shape Po by simply talking.
//!
//! Disabled by default. Once enabled, the model spots in the messages whatever concerns Po
//! itself — badly displayed text, a color to change, a bug, an idea — and records it with the
//! `po_labs` tool, separately from the rest of the request, which it handles normally. Requests
//! are kept in `%APPDATA%\po\labs\requests.jsonl`, always in English whatever the interface language:
//! they feed issues and pull requests on the public repository. That file is the raw material for trial changes to Po and proposals to the community, which will come later.

use std::path::PathBuf;

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{ToolOutcome, ToolSpec};
use po_i18n::t;
use po_tools::{Tool, ToolContext};
use serde_json::{Value, json};

pub const TOOL: &str = "po_labs";

/// Enabled in the interface preferences.
pub fn enabled() -> bool {
    po_i18n::prefs::get_bool("labs").unwrap_or(false)
}

pub fn set_enabled(on: bool) -> std::io::Result<()> {
    po_i18n::prefs::set_bool("labs", on)
}

fn requests_file() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let dir = base.join("po").join("labs");
    let path = dir.join("requests.jsonl");
    migrate(&dir.join("demandes.jsonl"), &path);
    Some(path)
}

/// Converts the file of the first versions (`demandes.jsonl`, French keys) to the current one.
fn migrate(old: &std::path::Path, new: &std::path::Path) {
    if new.exists() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(old) else {
        return;
    };
    let lines: Vec<String> =
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).map(|v| normalize(v).to_string()).collect();
    if std::fs::write(new, lines.join("\n") + "\n").is_ok() {
        let _ = std::fs::remove_file(old);
    }
}

/// Reads an entry written with the French keys (`resume`, `nature`, `noyau`) as a current one.
fn normalize(mut entry: Value) -> Value {
    let Some(map) = entry.as_object_mut() else {
        return entry;
    };
    for (old, new) in [("resume", "summary"), ("nature", "kind"), ("noyau", "core")] {
        if let Some(value) = map.remove(old) {
            map.entry(new).or_insert(value);
        }
    }
    if let Some(kind) = map.get_mut("kind") {
        match kind.as_str() {
            Some("amelioration") => *kind = json!("improvement"),
            Some("idee") => *kind = json!("idea"),
            _ => {}
        }
    }
    entry
}

/// Recorded requests, from the oldest to the most recent.
pub fn requests() -> Vec<Value> {
    requests_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).map(normalize).collect())
        .unwrap_or_default()
}

/// Instruction given to the model on activation: spot requests, including in what was already said.
pub const ACTIVATION_NOTE: &str = "[Po Labs enabled by the user] When a user message concerns Po \
itself — its interface, a display issue, a bug, how it works, an improvement idea — call the po_labs tool \
with a clear summary of that part only, then handle the rest of the message normally. Also reread the \
conversation so far: call po_labs for each request about Po already expressed.";

pub const DEACTIVATION_NOTE: &str = "[Po Labs disabled by the user] Do not call po_labs anymore.";

/// The tool through which the model records a request about Po.
pub struct LabsTool;

#[async_trait]
impl Tool for LabsTool {
    fn name(&self) -> &str {
        TOOL
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            TOOL,
            "Records a user request that concerns Po itself (its interface, a bug, how it works, an improvement \
             idea), separate from the rest of their request. One call per request.",
            json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "The request about Po, clear and self-contained, in one or two sentences. Always in English, whatever language the user writes in: it feeds the project's public issues"},
                    "kind": {"type": "string", "enum": ["bug", "improvement", "idea"], "description": "bug = defect, improvement = better existing behavior, idea = something new"},
                    "core": {"type": "boolean", "description": "True if it affects Po's core behavior (agent, models, tools, sessions) rather than its appearance or an extension"}
                },
                "required": ["summary"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, _ctx: &ToolContext) -> ToolOutcome {
        let input = normalize(input);
        let summary = input["summary"].as_str().map(str::trim).unwrap_or("");
        if summary.is_empty() {
            return ToolOutcome::error(id, "missing `summary`");
        }
        let entry = json!({
            "date": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            "summary": summary,
            "kind": input["kind"].as_str().unwrap_or("idea"),
            "core": input["core"].as_bool().unwrap_or(false),
            "version": format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("PO_COMMIT")),
        });
        let Some(path) = requests_file() else {
            return ToolOutcome::error(id, "configuration directory not found");
        };
        let written = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
                writeln!(file, "{entry}")
            });
        match written {
            Ok(()) => {
                let count = requests().len();
                ToolOutcome::ok(id, format!("Po Labs request #{count} recorded; continue with the rest of the request"))
                    .with_display(t!("labs.recorded", n = count))
            }
            Err(err) => ToolOutcome::error(id, format!("{}: {err}", path.display())),
        }
    }
}
