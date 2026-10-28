//! The status line under the input: a list of segments the user arranges with `/statusline`.
//!
//! Each segment is a built-in value (time, project, branch, diff, context…), a fixed text, or
//! the output of a shell command, with an optional color. The list is kept in the interface
//! preferences (`ui.json`, key `statusline`) and applies right away. Git and command values are
//! computed in the background and cached: drawing never waits on a process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// One element of the line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// A built-in id (see [`BUILTINS`]), `text` or `command`.
    pub id: String,
    /// Fixed text (`text`), or a label shown before a built-in value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Shell command whose first output line is shown (`command`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Color name: green, red, yellow, blue, magenta, cyan, gray, white.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl Segment {
    pub fn builtin(id: &str) -> Self {
        Self { id: id.to_owned(), text: None, command: None, color: None }
    }
}

/// Built-in values, with what they show. The descriptions are given to the model that edits
/// the line, hence in English.
pub const BUILTINS: &[(&str, &str)] = &[
    ("time", "current time, HH:MM"),
    ("project", "name of the folder Po was started in"),
    ("branch", "current git branch"),
    ("diff", "lines added (green) and removed (red) since the last commit"),
    ("context", "context used by the conversation, out of the model's window"),
    ("prefill", "prompt reading speed of the last call, tokens/s"),
    ("generation", "generation speed, tokens/s"),
    ("tokens", "tokens used by the session"),
    ("cost", "money spent on cloud models in this session"),
    ("model", "model of the session"),
    ("local_share", "share of the work done by local models"),
];

pub const COLORS: &[&str] = &["green", "red", "yellow", "blue", "magenta", "cyan", "gray", "white"];

/// The line before any change by the user.
pub fn default_segments() -> Vec<Segment> {
    ["time", "project", "branch", "diff", "context", "prefill", "generation", "tokens"]
        .iter()
        .map(|id| Segment::builtin(id))
        .collect()
}

static CURRENT: Mutex<Option<Vec<Segment>>> = Mutex::new(None);

/// The segments in force: the user's, or the default line.
pub fn current() -> Vec<Segment> {
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    current
        .get_or_insert_with(|| {
            po_i18n::prefs::get_str("statusline")
                .filter(|s| !s.is_empty())
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(default_segments)
        })
        .clone()
}

/// Replaces the line right away and keeps it; `None` goes back to the default line.
pub fn set(segments: Option<Vec<Segment>>) -> std::io::Result<()> {
    let stored = segments.as_ref().map(|s| serde_json::to_string(s).unwrap_or_default()).unwrap_or_default();
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(segments.unwrap_or_else(default_segments));
    po_i18n::prefs::set_str("statusline", &stored)
}

/// Checks a line proposed by the model: known ids and colors, a command for `command`, a text
/// for `text`. Returns what is wrong, in English, for the model to correct.
pub fn validate(segments: &[Segment]) -> Result<(), String> {
    if segments.is_empty() {
        return Err("the line has no segment".into());
    }
    for segment in segments {
        match segment.id.as_str() {
            "text" if segment.text.as_deref().is_none_or(str::is_empty) => {
                return Err("a `text` segment needs a `text`".into());
            }
            "command" if segment.command.as_deref().is_none_or(str::is_empty) => {
                return Err("a `command` segment needs a `command`".into());
            }
            "text" | "command" => {}
            id if BUILTINS.iter().any(|(b, _)| *b == id) => {}
            id => return Err(format!("unknown segment id `{id}`")),
        }
        if let Some(color) = &segment.color
            && !COLORS.contains(&color.as_str())
        {
            return Err(format!("unknown color `{color}`"));
        }
    }
    Ok(())
}

/// Instruction for the model that turns a request into a new line.
pub fn editing_prompt(current: &[Segment], request: &str) -> (String, String) {
    let builtins: String = BUILTINS.iter().map(|(id, what)| format!("- `{id}`: {what}\n")).collect();
    let system = format!(
        "You edit the status line of Po, a terminal coding agent. The line is a JSON array of segments, shown left \
         to right, separated by ` · `. A segment is an object:\n\
         - {{\"id\": \"<built-in>\"}} with an optional \"text\" label shown before the value;\n\
         - {{\"id\": \"text\", \"text\": \"…\"}} for fixed text;\n\
         - {{\"id\": \"command\", \"command\": \"…\"}} for the first line printed by a shell command (refreshed \
         every 10 seconds; keep it fast and read-only);\n\
         and an optional \"color\" among: {colors}.\n\
         Built-in ids:\n{builtins}\
         Apply the user's request to the current line, keeping everything they did not mention. Answer with the \
         JSON array only, no prose, no code fence.",
        colors = COLORS.join(", ")
    );
    let prompt = format!(
        "Current line:\n{}\n\nRequest: {request}",
        serde_json::to_string_pretty(current).unwrap_or_default()
    );
    (system, prompt)
}

/// Reads the model's answer: the JSON array, even wrapped in a code fence or a sentence.
pub fn parse_answer(answer: &str) -> Result<Vec<Segment>, String> {
    let start = answer.find('[').ok_or("no JSON array in the answer")?;
    let end = answer.rfind(']').ok_or("no JSON array in the answer")?;
    let segments: Vec<Segment> =
        serde_json::from_str(&answer[start..=end]).map_err(|e| format!("invalid JSON: {e}"))?;
    validate(&segments)?;
    Ok(segments)
}

// ---------------------------------------------------------------------------
// Values computed in the background
// ---------------------------------------------------------------------------

/// The git state of the working folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitState {
    pub branch: Option<String>,
    pub added: usize,
    pub removed: usize,
}

struct Cached<T> {
    value: Option<T>,
    at: Option<Instant>,
    running: bool,
}

static GIT: Mutex<Cached<GitState>> = Mutex::new(Cached { value: None, at: None, running: false });
static COMMANDS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);
static RUNNING: Mutex<Vec<String>> = Mutex::new(Vec::new());

const GIT_EVERY: Duration = Duration::from_secs(3);
const COMMAND_EVERY: Duration = Duration::from_secs(10);

/// Name of the folder Po works in.
pub fn project() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
}

/// Last known git state; a refresh starts in the background when it is getting old.
pub fn git() -> Option<GitState> {
    let mut cached = GIT.lock().unwrap_or_else(|e| e.into_inner());
    let stale = cached.at.is_none_or(|at| at.elapsed() > GIT_EVERY);
    if stale && !cached.running {
        cached.running = true;
        std::thread::spawn(|| {
            let state = read_git(&std::env::current_dir().unwrap_or_default());
            let mut cached = GIT.lock().unwrap_or_else(|e| e.into_inner());
            cached.value = state;
            cached.at = Some(Instant::now());
            cached.running = false;
        });
    }
    cached.value.clone()
}

fn git_output(dir: &PathBuf, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new("git");
    command.args(args).current_dir(dir).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = command.output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Branch and lines changed since the last commit (staged or not).
pub fn read_git(dir: &PathBuf) -> Option<GitState> {
    let branch = git_output(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_owned();
    let numstat = git_output(dir, &["diff", "HEAD", "--numstat"]).unwrap_or_default();
    let (added, removed) = parse_numstat(&numstat);
    Some(GitState { branch: Some(branch).filter(|b| !b.is_empty()), added, removed })
}

/// Sums `git diff --numstat` (binary files show `-` and count nothing).
pub fn parse_numstat(text: &str) -> (usize, usize) {
    text.lines().fold((0, 0), |(a, r), line| {
        let mut parts = line.split_whitespace();
        let x = parts.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        let y = parts.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        (a + x, r + y)
    })
}

/// Last output of a `command` segment; it is (re)run in the background when needed.
pub fn command_output(command: &str) -> Option<String> {
    let mut outputs = COMMANDS.lock().unwrap_or_else(|e| e.into_inner());
    let outputs = outputs.get_or_insert_with(HashMap::new);
    let known = outputs.get(command).cloned();
    let stale = known.as_ref().is_none_or(|(_, at)| at.elapsed() > COMMAND_EVERY);
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if stale && !running.iter().any(|c| c == command) {
        running.push(command.to_owned());
        let command = command.to_owned();
        std::thread::spawn(move || {
            let text = run_shell(&command).unwrap_or_default();
            COMMANDS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(HashMap::new)
                .insert(command.clone(), (text, Instant::now()));
            RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| *c != command);
        });
    }
    known.map(|(text, _)| text)
}

fn run_shell(command: &str) -> Option<String> {
    let mut process = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", command]);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x0800_0000);
    }
    let output = process.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or("").trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_line_is_valid_and_shows_the_project_branch_and_diff() {
        let line = default_segments();
        assert!(validate(&line).is_ok());
        for id in ["project", "branch", "diff"] {
            assert!(line.iter().any(|s| s.id == id), "{id}");
        }
    }

    #[test]
    fn the_answer_of_the_model_is_read_even_wrapped() {
        let answer = "Here it is:\n```json\n[{\"id\": \"branch\", \"color\": \"magenta\"}, {\"id\": \"text\", \"text\": \"🐼\"}]\n```";
        let line = parse_answer(answer).unwrap();
        assert_eq!(line[0].color.as_deref(), Some("magenta"));
        assert_eq!(line[1].text.as_deref(), Some("🐼"));
        assert!(parse_answer("[{\"id\": \"weather\"}]").unwrap_err().contains("weather"));
        assert!(parse_answer("[{\"id\": \"time\", \"color\": \"plaid\"}]").is_err());
        assert!(parse_answer("[{\"id\": \"command\"}]").is_err());
        assert!(parse_answer("sorry").is_err());
    }

    #[test]
    fn numstat_sums_text_files_and_skips_binaries() {
        assert_eq!(parse_numstat("3\t1\ta.rs\n-\t-\tlogo.png\n10\t0\tb.rs\n"), (13, 1));
    }

    #[test]
    fn the_prompt_lists_every_builtin_and_the_current_line() {
        let (system, prompt) = editing_prompt(&default_segments(), "add the model in cyan");
        assert!(BUILTINS.iter().all(|(id, _)| system.contains(id)));
        assert!(prompt.contains("\"project\"") && prompt.contains("add the model in cyan"));
    }
}
