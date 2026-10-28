//! Po Labs: the user shapes Po by simply talking to it, and sees the change live.
//!
//! Disabled by default. Once enabled, a message about Po itself — "make the tool calls orange",
//! "the status bar should show the model" — is a task on Po's own source, done right away like a
//! developer listening to feedback would: the model records the request (`po_labs`), finds the
//! place with a map of the source, edits it (Po's source is open for writing, without approval
//! requests), then calls `po_apply`, which builds Po, installs it and restarts the session on the
//! new binary once the turn ends — falling back to the previous binary if the new one does not
//! start. Requests are kept in `%APPDATA%\po\labs\requests.jsonl`, always in English whatever the
//! interface language: they feed issues and pull requests on the public repository.

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

pub const APPLY_TOOL: &str = "po_apply";
/// Heading of the guide in the system prompt.
pub const GUIDE_TITLE: &str = "## Po Labs";

/// Where Po's source can be changed: `PO_SOURCE`, else the folder this binary was built from,
/// if it is still there. An installed binary built elsewhere has none: Po Labs then records
/// requests only.
pub fn source_root() -> Option<PathBuf> {
    let candidate = std::env::var_os("PO_SOURCE").map(PathBuf::from).or_else(|| {
        // crates/po at build time: the workspace is two levels up.
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent()?.parent().map(PathBuf::from)
    })?;
    (candidate.join("Cargo.toml").is_file() && candidate.join("crates").join("po").is_dir()).then_some(candidate)
}

/// What the model must know to change Po by itself: the steps, where the source is, and a map
/// of it, so that it neither searches nor guesses. Part of the system prompt while Po Labs is on.
pub fn guide(source: Option<&std::path::Path>) -> String {
    let Some(source) = source else {
        return "## Po Labs\nThe user enabled Po Labs, but Po's source code is not on this machine. When a message \
                concerns Po itself (its interface, a bug, how it works, an idea), call po_labs with a clear summary \
                of that part, then handle the rest of the message normally."
            .to_owned();
    };
    format!(
        "## Po Labs\n\
         You are Po, and the user enabled Po Labs: they shape Po by talking to it while they use it. When a message \
         asks for a change to Po itself — how it looks, how it behaves, a bug, a text, a command (for example \"make \
         the tool calls orange\") — it is a task on your own source code. Do it right away, from start to finish, \
         without asking for confirmation or for permission, like a developer applying live feedback:\n\
         1. Call po_labs with the request, in one English sentence.\n\
         2. Find the place with the map below; to locate a symbol, call `grep` with `path` set to the source \
         root. Read only the file you need.\n\
         3. Make the change with `edit`/`write` and ABSOLUTE paths under {root}. Changes there need no approval.\n\
         4. Call po_apply with a one-line summary. It builds Po, installs it, and restarts this very session on the \
         new build as soon as your turn ends; the conversation is kept. If it returns compiler errors, fix them \
         and call po_apply again.\n\
         5. End your turn with one or two sentences saying what changed. Never ask the user to build, install or \
         restart: po_apply does it.\n\
         Keep each change minimal, in the style of the surrounding code. A text shown to the user goes through \
         `po_i18n::t!` with a French and an English entry. A request about the user's own project is not a Po \
         change: handle it normally.\n\
         Po's source root: {root}\n\n{map}",
        root = source.display(),
        map = include_str!("labs_map.md")
    )
}

/// Switches Po Labs on or off in a running session: tools, write access to the source, and the
/// guide (in the system prompt at startup, as a note when switched on mid-session).
///
/// The tools stay declared whether Po Labs is on or off, and refuse to work while it is off:
/// adding or removing a tool rewrites the start of the prompt, and the server then rereads the
/// whole conversation (measured on Strata: 5,102 tokens out of 5,102 at `/labs on`).
pub fn register_tools(agent: &mut po_agent::Agent) {
    agent.registry_mut().register(std::sync::Arc::new(LabsTool));
    if let Some(source) = source_root() {
        agent.registry_mut().register(std::sync::Arc::new(ApplyTool::for_source(source)));
    }
}

/// Switches Po Labs on or off in a running session: write access to the source, and the guide
/// (in the system prompt of a new session; as a note, which appends to the conversation and so
/// keeps the cache, when switched on later or on a resumed session).
pub fn activate(agent: &mut po_agent::Agent, on: bool, at_startup: bool) {
    let source = source_root();
    if let Some(source) = &source {
        agent.close_for_changes(source);
    }
    if !on {
        if !at_startup {
            agent.add_context("[Po Labs disabled by the user] Do not call po_labs or po_apply anymore, and do not change Po's source.");
        }
        return;
    }
    if let Some(source) = &source {
        agent.open_for_changes(source);
    }
    let guide = guide(source.as_deref());
    if at_startup {
        agent.session.system.push_str(&format!("\n\n{guide}"));
    } else {
        agent.add_context(format!(
            "[Po Labs enabled by the user]\n{guide}\nAlso reread the conversation so far: a request about Po already \
             expressed is handled the same way."
        ));
    }
}

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
             idea), separate from the rest of their request. One call per request. Only while the user has Po \
             Labs enabled.",
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
        if !enabled() {
            return ToolOutcome::error(id, "Po Labs is off: the user has not enabled it. Do not call po_labs.");
        }
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

/// The binary that `po` launches: `PO_INSTALL` if set (a trial install elsewhere), Cargo's
/// binary folder when Po was installed there, else the running one. The restart relaunches it.
pub fn installed_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("PO_INSTALL") {
        return PathBuf::from(path);
    }
    let name = if cfg!(windows) { "po.exe" } else { "po" };
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|home| PathBuf::from(home).join(".cargo").join("bin").join(name))
        .filter(|path| path.is_file())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The binary replaced by the last `po_apply`: the restart goes back to it if the new one does
/// not start.
static PREVIOUS: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn previous_binary() -> Option<PathBuf> {
    PREVIOUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Builds Po from its source, installs the result and asks for a restart on it.
pub struct ApplyTool {
    source: PathBuf,
    /// Build command, run in the source root.
    build: Vec<String>,
    /// What the build produces.
    artifact: PathBuf,
    /// Where it goes.
    install: PathBuf,
    /// Notes each applied change next to the requests (off in tests: that file is the user's).
    log: bool,
}

impl ApplyTool {
    pub fn for_source(source: PathBuf) -> Self {
        let name = if cfg!(windows) { "po.exe" } else { "po" };
        Self {
            build: ["cargo", "build", "--release", "-p", "po"].iter().map(|s| (*s).to_owned()).collect(),
            // Where cargo puts it: CARGO_TARGET_DIR when the user shares one target folder.
            artifact: std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| source.join("target"))
                .join("release")
                .join(name),
            install: installed_binary(),
            source,
            log: true,
        }
    }

    /// Runs the build; on failure, the compiler's errors, trimmed for the model.
    async fn build(&self) -> Result<(), String> {
        let (program, args) = self.build.split_first().ok_or("empty build command")?;
        let output = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&self.source)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("could not run `{}`: {e}", self.build.join(" ")))?;
        if output.status.success() {
            return Ok(());
        }
        Err(compiler_errors(&String::from_utf8_lossy(&output.stderr)))
    }

    /// Puts the new binary in place. A running binary cannot be overwritten on Windows but can be
    /// renamed: the current one steps aside (and stays as the fallback), the new one is copied.
    fn install(&self) -> Result<Option<PathBuf>, String> {
        let previous = if self.install.exists() {
            let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let stem = self.install.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = self.install.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let aside = self.install.with_file_name(format!("{stem}.old-{stamp}{ext}"));
            std::fs::rename(&self.install, &aside).map_err(|e| format!("{}: {e}", self.install.display()))?;
            Some(aside)
        } else {
            None
        };
        if let Err(err) = std::fs::copy(&self.artifact, &self.install) {
            // Put the previous one back: the user must never be left without a working `po`.
            if let Some(previous) = &previous {
                let _ = std::fs::rename(previous, &self.install);
            }
            return Err(format!("{} → {}: {err}", self.artifact.display(), self.install.display()));
        }
        Ok(previous)
    }
}

/// The `error…` blocks of cargo's output, up to a size a model reads at once.
fn compiler_errors(stderr: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in stderr.lines() {
        if line.starts_with("error") {
            inside = true;
        } else if line.starts_with("warning") || line.trim_start().starts_with("Compiling") {
            inside = false;
        }
        if inside {
            out.push(line);
        }
        if out.len() >= 120 {
            out.push("[… more errors cut]");
            break;
        }
    }
    if out.is_empty() {
        stderr.lines().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    } else {
        out.join("\n")
    }
}

#[async_trait]
impl Tool for ApplyTool {
    fn name(&self) -> &str {
        APPLY_TOOL
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            APPLY_TOOL,
            "Builds Po from its source after your changes, installs it, and restarts this session on the new build \
             as soon as your turn ends (the conversation is kept). Call it once your edits to Po are done. Returns \
             the compiler errors if the build fails. Only while the user has Po Labs enabled.",
            json!({
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "What changed, in one English sentence"}
                },
                "required": ["summary"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        // The user switched Po Labs on precisely so that Po applies its changes by itself.
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, _ctx: &ToolContext) -> ToolOutcome {
        if self.log && !enabled() {
            return ToolOutcome::error(id, "Po Labs is off: the user has not enabled it. Do not change Po.");
        }
        let summary = input["summary"].as_str().unwrap_or("").trim().to_owned();
        if let Err(errors) = self.build().await {
            return ToolOutcome::error(id, format!("The build failed; nothing was installed. Fix these errors, then call po_apply again.\n{errors}"))
                .with_display(t!("labs.apply.failed"));
        }
        match self.install() {
            Ok(previous) => {
                *PREVIOUS.lock().unwrap_or_else(|e| e.into_inner()) = previous;
                po_tui::request_restart();
                if self.log {
                    record_applied(&summary);
                }
                ToolOutcome::ok(
                    id,
                    "Built and installed. Po restarts on this session with the new build as soon as your turn ends. \
                     Finish now with one or two sentences on what changed.",
                )
                .with_display(t!("labs.apply.done", summary = summary))
            }
            Err(err) => ToolOutcome::error(id, format!("The build succeeded but could not be installed: {err}"))
                .with_display(t!("labs.apply.failed")),
        }
    }
}

/// Keeps track of what Po Labs changed, next to the requests.
fn record_applied(summary: &str) {
    let Some(path) = requests_file() else { return };
    let entry = json!({
        "date": chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        "summary": summary,
        "kind": "applied",
        "core": true,
        "version": format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("PO_COMMIT")),
    });
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{entry}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use po_agent::{Agent, ReadOnly};
    use po_core::{Capabilities, Completion, CompletionResponse, ContentBlock, Message, ModelSpec, Origin, Pricing, Role, StopReason, Tier, Usage};
    use po_provider::{Provider, ProviderError};
    use po_router::{BudgetMode, Router};
    use po_tools::{Registry, Workspace};
    use std::sync::{Arc, Mutex};

    struct Script {
        replies: Mutex<Vec<CompletionResponse>>,
        seen: Arc<Mutex<Vec<Completion>>>,
    }

    #[async_trait]
    impl Provider for Script {
        fn name(&self) -> &str {
            "strata"
        }
        async fn complete(&self, request: &Completion) -> Result<CompletionResponse, ProviderError> {
            self.seen.lock().unwrap().push(request.clone());
            let mut replies = self.replies.lock().unwrap();
            if replies.is_empty() {
                return Err(ProviderError::Transport("script exhausted".into()));
            }
            Ok(replies.remove(0))
        }
    }

    fn call(id: &str, name: &str, input: Value) -> CompletionResponse {
        CompletionResponse {
            message: Message { role: Role::Assistant, content: vec![ContentBlock::ToolUse { id: id.into(), name: name.into(), input }] },
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            model_id: "local".into(),
        }
    }

    fn say(text: &str) -> CompletionResponse {
        CompletionResponse { message: Message::assistant(text), stop_reason: StopReason::EndTurn, usage: Usage::default(), model_id: "local".into() }
    }

    fn shell(script: &str) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), script.into()]
        } else {
            vec!["sh".into(), "-c".into(), script.into()]
        }
    }

    /// A user project, a copy of Po's source with the line to change, and the places the build
    /// and the install use — all temporary.
    struct Bench {
        _dirs: Vec<tempfile::TempDir>,
        project: PathBuf,
        source: PathBuf,
        artifact: PathBuf,
        install: PathBuf,
    }

    fn bench() -> Bench {
        let (project, source, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let render = source.path().join("crates").join("po-tui").join("src");
        std::fs::create_dir_all(&render).unwrap();
        std::fs::create_dir_all(source.path().join("crates").join("po")).unwrap();
        std::fs::write(source.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(render.join("render.rs"), "let icon_style = colors.style(Color::Green, false);\n").unwrap();
        let artifact = bin.path().join("built-po");
        std::fs::write(&artifact, "new build").unwrap();
        let install = bin.path().join("po-installed");
        std::fs::write(&install, "old build").unwrap();
        Bench {
            project: project.path().canonicalize().unwrap(),
            source: source.path().canonicalize().unwrap(),
            artifact,
            install,
            _dirs: vec![project, source, bin],
        }
    }

    fn agent_for(bench: &Bench, build: Vec<String>, replies: Vec<CompletionResponse>) -> (Agent, Arc<Mutex<Vec<Completion>>>) {
        let model = ModelSpec {
            id: "local".into(),
            display_name: "local".into(),
            origin: Origin::Local { backend: "llama.cpp".into(), endpoint: "http://x".into() },
            tier: Tier::Strong,
            capabilities: Capabilities::AGENTIC,
            context_window: 131_072,
            max_output_tokens: 4096,
            pricing: Pricing::FREE,
        };
        let router = Router::new(vec![model], BudgetMode::Balanced);
        let ctx = ToolContext::new(Workspace::new(&bench.project));
        // An approver that refuses everything: what goes through was pre-approved.
        let mut agent = Agent::new(router, Registry::with_builtins(), ctx, "system", Arc::new(ReadOnly));
        let seen = Arc::new(Mutex::new(Vec::new()));
        agent.attach("local", Arc::new(Script { replies: Mutex::new(replies), seen: Arc::clone(&seen) }));
        agent.open_for_changes(&bench.source);
        agent.registry_mut().register(Arc::new(ApplyTool {
            source: bench.source.clone(),
            build,
            artifact: bench.artifact.clone(),
            install: bench.install.clone(),
            log: false,
        }));
        (agent, seen)
    }

    #[tokio::test]
    async fn a_request_about_po_is_changed_built_installed_and_restarted_without_asking() {
        let bench = bench();
        let file = bench.source.join("crates").join("po-tui").join("src").join("render.rs");
        let path = file.display().to_string();
        let (mut agent, seen) = agent_for(
            &bench,
            shell("echo built"),
            vec![
                call("1", "read", json!({"path": path})),
                call("2", "edit", json!({"path": path, "old": "Color::Green", "new": "Color::Rgb(255, 165, 0)"})),
                call("3", APPLY_TOOL, json!({"summary": "tool call icons are orange"})),
                say("The tool calls are orange now; Po restarts with it."),
            ],
        );
        let _ = po_tui::take_restart_request();
        agent.turn("make the tool calls orange", &mut |_| {}).await.unwrap();

        assert!(std::fs::read_to_string(&file).unwrap().contains("Color::Rgb(255, 165, 0)"), "edited without approval");
        assert_eq!(std::fs::read_to_string(&bench.install).unwrap(), "new build", "installed");
        let previous = previous_binary().expect("the replaced build is kept");
        assert_eq!(std::fs::read_to_string(&previous).unwrap(), "old build", "and stays as the fallback");
        assert!(po_tui::take_restart_request(), "the session restarts on the new build");
        let seen = seen.lock().unwrap();
        let results: String = seen[3].messages.last().unwrap().content.iter().map(|b| b.as_text().to_owned()).collect();
        assert!(results.contains("Built and installed"), "{results}");
    }

    #[tokio::test]
    async fn a_failed_build_hands_the_errors_back_and_installs_nothing() {
        let bench = bench();
        let (mut agent, seen) = agent_for(
            &bench,
            shell("echo error[E0308]: mismatched types 1>&2 && exit 1"),
            vec![call("1", APPLY_TOOL, json!({"summary": "x"})), say("I will fix it.")],
        );
        agent.turn("make the tool calls orange", &mut |_| {}).await.unwrap();
        assert_eq!(std::fs::read_to_string(&bench.install).unwrap(), "old build");
        let seen = seen.lock().unwrap();
        let result = seen[1].messages.last().unwrap().content[0].as_text().to_owned();
        assert!(result.contains("E0308") && result.contains("call po_apply again"), "{result}");
    }

    #[tokio::test]
    async fn outside_po_s_source_changes_still_need_approval() {
        let bench = bench();
        let (mut agent, _) = agent_for(
            &bench,
            shell("echo built"),
            vec![call("1", "write", json!({"path": "notes.txt", "content": "x"})), say("done")],
        );
        agent.turn("write a note in my project", &mut |_| {}).await.unwrap();
        assert!(!bench.project.join("notes.txt").exists(), "the user's project keeps its approvals");
    }

    #[test]
    fn the_guide_gives_the_steps_the_source_and_the_map() {
        let guide = guide(Some(std::path::Path::new("/src/po")));
        for needed in ["/src/po", "po_apply", "po_labs", "without asking", "render_tools", "po_i18n::t!", "ABSOLUTE"] {
            assert!(guide.contains(needed), "{needed} missing");
        }
        assert!(guide.starts_with(GUIDE_TITLE));
        let without = super::guide(None);
        assert!(without.contains("po_labs") && !without.contains("po_apply"), "no source: record only");
    }

    #[test]
    fn the_source_of_this_build_is_found() {
        let root = source_root().expect("tests run from the source tree");
        assert!(root.join("crates").join("po").join("src").join("labs.rs").is_file());
    }

    #[test]
    fn compiler_errors_keep_the_error_blocks_only() {
        let stderr = "   Compiling po v0.1.0\nwarning: unused variable\n  --> a.rs:1\nerror[E0425]: cannot find value `x`\n  --> src/a.rs:3:5\n   |\n3  |     x\nerror: aborting due to 1 previous error\n";
        let kept = compiler_errors(stderr);
        assert!(kept.contains("E0425") && kept.contains("src/a.rs:3:5"));
        assert!(!kept.contains("unused variable") && !kept.contains("Compiling"));
    }
}
