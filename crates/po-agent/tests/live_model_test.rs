use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use po_agent::work_mode::WorkMode;
use po_agent::{Agent, AgentEvent, Approver};
use po_core::{Capabilities, ModelSpec, Origin, Pricing, Tier};
use po_provider::OpenAiProvider;
use po_router::{BudgetMode, Router};
use po_tools::{Registry, ToolContext, Workspace};
use serde_json::Value;

struct DenyAll;

#[async_trait::async_trait]
impl Approver for DenyAll {
    async fn approve(&self, _name: &str, _input: &Value) -> bool {
        false
    }
}

/// Live tests talk to the user's own model server and project: opt-in only (PO_LIVE_TESTS=1),
/// never as a side effect of `cargo test`.
fn live_server_available() -> bool {
    if std::env::var("PO_LIVE_TESTS").ok().as_deref() != Some("1") {
        return false;
    }
    TcpStream::connect_timeout(
        &"127.0.0.1:8080".parse().unwrap(),
        Duration::from_millis(500),
    )
    .is_ok()
}

fn create_live_agent() -> Agent {
    let workspace_path = PathBuf::from(r"D:\Documents\Programmation\Projets\LuaVault");
    let ctx = ToolContext::new(Workspace::new(&workspace_path));

    let model_id = "qwen3.8-flash-next-iq3_xxs";
    let model = ModelSpec {
        id: model_id.into(),
        display_name: model_id.into(),
        origin: Origin::Local {
            backend: "Strata".into(),
            endpoint: "http://127.0.0.1:8080/v1".into(),
        },
        tier: Tier::Strong,
        capabilities: Capabilities::AGENTIC,
        context_window: 131072,
        max_output_tokens: 4096,
        pricing: Pricing::FREE,
    };

    let router = Router::new(vec![model], BudgetMode::Offline);
    let mut agent = Agent::new(
        router,
        Registry::with_builtins(),
        ctx,
        "You are an AI assistant in plan mode. Perform the requested inspections with the bash tool.",
        Arc::new(DenyAll),
    );

    agent.work_mode().set(WorkMode::Plan);

    let provider = OpenAiProvider::local("Strata", "http://127.0.0.1:8080/v1");
    agent.attach(model_id, Arc::new(provider));
    agent
}

#[tokio::test]
async fn test_live_qwen_git_branch_and_status_in_plan_mode() {
    if !live_server_available() {
        println!("Live tests are opt-in (PO_LIVE_TESTS=1) and need the server on port 8080: skipped");
        return;
    }

    let mut agent = create_live_agent();
    let mut tool_starts = Vec::new();
    let mut tool_denials = Vec::new();

    let prompt = "Vérifie les branches distantes avec `git branch -r --merged origin/main` et le statut avec `git status --short`. Réponds brièvement.";

    let turn_result = agent
        .turn(prompt, &mut |e| {
            match &e {
                AgentEvent::ToolStarted { name, summary } => {
                    println!("[TOOL STARTED] {name}: {summary}");
                    tool_starts.push((name.clone(), summary.clone()));
                }
                AgentEvent::ToolDenied { name, summary } => {
                    println!("[TOOL DENIED] {name}: {summary}");
                    tool_denials.push((name.clone(), summary.clone()));
                }
                AgentEvent::ToolFinished { name, summary, is_error } => {
                    println!("[TOOL FINISHED] {name}: is_error={is_error} summary={summary}");
                }
                _ => {}
            }
        })
        .await;

    assert!(turn_result.is_ok(), "turn failed: {:?}", turn_result.err());
    // The two commands asked for must run unrefused; what the model adds on its own is not judged.
    let asked = |text: &str| text.contains("git branch -r") || text.contains("git status");
    let denied: Vec<_> = tool_denials.iter().filter(|(_, summary)| asked(summary)).collect();
    assert!(denied.is_empty(), "read-only commands denied: {denied:?}");
    assert!(tool_starts.iter().any(|(_, s)| s.contains("git branch -r")), "git branch -r ran: {tool_starts:?}");
    assert!(tool_starts.iter().any(|(_, s)| s.contains("git status")), "git status ran: {tool_starts:?}");
}

#[tokio::test]
async fn test_live_qwen_gh_and_rev_list_in_plan_mode() {
    if !live_server_available() {
        println!("Live tests are opt-in (PO_LIVE_TESTS=1) and need the server on port 8080: skipped");
        return;
    }

    let mut agent = create_live_agent();
    let mut tool_starts = Vec::new();
    let mut tool_denials = Vec::new();

    let prompt = "Exécute `gh repo view LeGeRyChEeSe/luavault --json name` ainsi que `git rev-list --count origin/main..HEAD` pour inspecter le repo.";

    let turn_result = agent
        .turn(prompt, &mut |e| {
            match &e {
                AgentEvent::ToolStarted { name, summary } => {
                    println!("[TOOL STARTED] {name}: {summary}");
                    tool_starts.push((name.clone(), summary.clone()));
                }
                AgentEvent::ToolDenied { name, summary } => {
                    println!("[TOOL DENIED] {name}: {summary}");
                    tool_denials.push((name.clone(), summary.clone()));
                }
                AgentEvent::ToolFinished { name, summary, is_error } => {
                    println!("[TOOL FINISHED] {name}: is_error={is_error} summary={summary}");
                }
                _ => {}
            }
        })
        .await;

    assert!(turn_result.is_ok(), "turn failed: {:?}", turn_result.err());
    // Only the two commands asked for are judged: the model may add inspections of its own, and
    // plan mode rightly refuses those that are not read-only.
    let asked = |text: &str| text.contains("gh repo view") || text.contains("git rev-list");
    let denied: Vec<_> = tool_denials.iter().filter(|(_, summary)| asked(summary)).collect();
    assert!(denied.is_empty(), "gh repo view and git rev-list should not be denied: {denied:?}");
    assert!(tool_starts.iter().any(|(_, s)| s.contains("gh repo view")), "gh repo view ran: {tool_starts:?}");
    assert!(tool_starts.iter().any(|(_, s)| s.contains("git rev-list")), "git rev-list ran: {tool_starts:?}");
}

#[tokio::test]
async fn test_live_qwen_mutations_properly_refused_in_plan_mode() {
    if !live_server_available() {
        println!("Live tests are opt-in (PO_LIVE_TESTS=1) and need the server on port 8080: skipped");
        return;
    }

    let mut agent = create_live_agent();
    let mut tool_denials = Vec::new();
    let mut tool_starts = Vec::new();

    let prompt = "Crée une nouvelle branche nommée 'test-plan-branch-must-fail' en exécutant la commande bash `git branch test-plan-branch-must-fail`.";

    let _ = agent
        .turn(prompt, &mut |e| {
            if let AgentEvent::ToolStarted { name, summary } = &e {
                println!("[UNEXPECTED TOOL STARTED] {name}: {summary}");
                tool_starts.push((name.clone(), summary.clone()));
            }
            if let AgentEvent::ToolDenied { name, summary } = &e {
                println!("[EXPECTED TOOL DENIED] {name}: {summary}");
                tool_denials.push((name.clone(), summary.clone()));
            }
        })
        .await;

    println!("Last message: {:?}", agent.session.messages.last().map(|m| m.visible_text()));
    println!("Tool starts: {}, Tool denials: {}", tool_starts.len(), tool_denials.len());

    // In plan mode:
    // 1. None of the started tools must be the mutating branch creation command:
    for (name, cmd) in &tool_starts {
        assert_ne!(cmd, "git branch test-plan-branch-must-fail", "Mutating command must not run: {name} {cmd}");
    }
    // 2. The classifier itself must strictly require approval for branch creation:
    assert_eq!(
        po_tools::shell::classify("git branch test-plan-branch-must-fail"),
        po_core::tool::Permission::Ask,
        "Creating a branch must require approval"
    );
}
