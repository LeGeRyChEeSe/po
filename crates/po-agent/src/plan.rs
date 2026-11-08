//! The `plan` tool: a plan becomes a file, and its implementation starts from it alone.
//!
//! Planning a feature can fill most of a local model's window with exploration (files read,
//! searches). Implementing on top of that leaves little room. The plan is therefore written to a
//! file the user can read and edit; when they launch it, the conversation is archived (the
//! `recall` tool still searches it) and the implementation starts in a fresh context holding the
//! plan and the user's request only.

use std::path::{Path, PathBuf};

use po_core::ToolSpec;
use serde_json::{Value, json};

pub const TOOL: &str = "plan";

/// How the first message of an implementation starts: Po's note, not something the user typed.
pub const START_MARKER: &str = "[Po: the user approved your plan";

/// Bounds: a plan is read again by the model at every step of the implementation.
const MAX_TITLE: usize = 120;
const MAX_PLAN: usize = 24_000;

pub fn tool_spec() -> ToolSpec {
    ToolSpec::new(
        TOOL,
        "Saves your implementation plan to a file the user can read and edit. In plan mode, call it once the \
         plan is ready: Po saves it and asks the user to launch the implementation, which then starts in a \
         fresh context holding only this plan and the user's request - so the plan must stand on its own. \
         Write it in markdown: the goal; the decisions taken and why; numbered steps, each naming the files \
         and functions to change; acceptance criteria, each with how it is checked (a command, a test, an \
         observable behavior); risks. To change a saved plan, call it again with the whole new version.",
        json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "A short title, e.g. \"Export sessions to Markdown\""},
                "plan": {"type": "string", "description": "The whole plan, in markdown"}
            },
            "required": ["title", "plan"]
        }),
    )
}

/// The title and the plan of a call, checked.
pub fn parse(input: &Value) -> Result<(String, String), String> {
    let title = input.get("title").and_then(Value::as_str).unwrap_or("").trim();
    let plan = input.get("plan").and_then(Value::as_str).unwrap_or("").trim();
    if title.is_empty() || plan.is_empty() {
        return Err("give `title` and `plan` (the whole plan, in markdown)".into());
    }
    if plan.len() < 80 {
        return Err("this plan is too short to be implemented from alone: give the goal, the numbered steps \
                    with their files, and the acceptance criteria"
            .into());
    }
    if plan.len() > MAX_PLAN {
        return Err(format!("the plan is {} characters long: keep it under {MAX_PLAN}, it is reread at every step", plan.len()));
    }
    Ok((title.chars().take(MAX_TITLE).collect(), plan.to_owned()))
}

/// A file name from a title: lowercase ASCII words joined by dashes.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        let c = match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            other => other.to_ascii_lowercase(),
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 60 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() { "plan".into() } else { out }
}

/// Where the plans of a project go: outside the project (nothing to commit by mistake), in Po's
/// data folder, one folder per project. `PO_PLANS_DIR` replaces the base (tests).
pub fn dir_for(workspace: &Path) -> Option<PathBuf> {
    let base = match std::env::var_os("PO_PLANS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::data_dir()?.join("po").join("plans"),
    };
    let project = workspace.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    Some(base.join(slug(&project)))
}

/// Writes the plan. A plan saved again under the same title replaces the previous version: it
/// is the same plan, revised.
pub fn save(dir: &Path, title: &str, plan: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.md", slug(title)));
    let body = if plan.trim_start().starts_with("# ") { format!("{plan}\n") } else { format!("# {title}\n\n{plan}\n") };
    std::fs::write(&path, body)?;
    Ok(path)
}

/// The plan as the user may have edited it since it was saved, else as the model wrote it.
pub fn read_back(path: &Path, written: &str) -> String {
    std::fs::read_to_string(path).ok().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| written.to_owned())
}

/// The first message of the implementation, in its fresh context.
pub fn implement_prompt(path: &Path, plan: &str, requests: &[String], mode: &str) -> String {
    let mut text = format!(
        "{START_MARKER} and launched its implementation in {mode} mode. The exploration that \
         led to it was archived to free the context: the `recall` tool searches it word for word if a detail is \
         missing. Implement the plan below step by step, telling the user briefly as you go which step you are \
         on. Check each acceptance criterion for real (run the commands and tests it names) before you report. \
         If a step turns out wrong, adapt and say why.]\n"
    );
    if !requests.is_empty() {
        text.push_str("\nThe user's request:\n");
        for request in requests {
            text.push_str(&format!("> {}\n", request.trim().replace('\n', "\n> ")));
        }
    }
    text.push_str(&format!("\nPlan ({}):\n\n{}\n", path.display(), plan.trim()));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_is_checked_and_saved_under_its_title() {
        assert!(parse(&json!({"title": "x"})).is_err());
        assert!(parse(&json!({"title": "x", "plan": "do it"})).is_err(), "too short to stand on its own");
        let long = "## Goal\nExport sessions.\n## Steps\n1. Add `export` in session.rs\n## Criteria\n- `cargo test` passes";
        let (title, plan) = parse(&json!({"title": " Export des sessions en Markdown ", "plan": long})).unwrap();
        assert_eq!(title, "Export des sessions en Markdown");
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), &title, &plan).unwrap();
        assert_eq!(path.file_name().unwrap(), "export-des-sessions-en-markdown.md");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with("# Export des sessions en Markdown\n\n## Goal"), "{saved}");
        // Saved again: the same file, revised.
        assert_eq!(save(dir.path(), &title, "## Goal\nv2").unwrap(), path);
        // Edited by the user: the implementation starts from their version.
        std::fs::write(&path, "# mine").unwrap();
        assert_eq!(read_back(&path, &plan), "# mine");
    }

    #[test]
    fn the_implementation_prompt_carries_the_request_and_the_plan() {
        let text = implement_prompt(Path::new("p.md"), "## Steps\n1. a", &["ajoute l'export".into()], "builder");
        assert!(text.starts_with("[Po:"), "a note of Po, not something the user typed");
        assert!(text.contains("> ajoute l'export") && text.contains("1. a") && text.contains("recall"));
    }
}
