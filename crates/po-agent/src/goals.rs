//! Goals: what the model commits to in auto mode, and how Po checks it.
//!
//! A local model left to itself declares a task done when it believes so. Here it states, before
//! changing anything, a few concrete goals, each with a shell command that only succeeds when the
//! goal is really met. When the model wants to hand back, Po runs those commands itself: the
//! model cannot tick a box, only make the check pass.
//!
//! What would let it cheat is guarded against: checks that cannot fail (`echo ok`, `|| true`) are
//! refused, the test files that existed when the goals were set are fingerprinted (changing one
//! must be justified, and the user is told), and goals replaced along the way need a reason.

use std::path::{Path, PathBuf};

use po_core::tool::ToolSpec;
use serde_json::{Value, json};

/// Name of the tool.
pub const TOOL: &str = "goals";
/// Goals at most: a handful of concrete checks, not a test plan.
pub const MAX_GOALS: usize = 6;
/// Verification rounds after a failed check before the turn ends and the user is told.
pub const MAX_ROUNDS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub text: String,
    /// Shell command, run in the workspace, that exits 0 only when the goal is met.
    pub check: String,
}

/// The goals of the current task.
#[derive(Debug, Clone, Default)]
pub struct Goals {
    pub items: Vec<Goal>,
    /// Test files as they were when the goals were set.
    pub protected: Vec<Protected>,
    /// Protected files the model changed, with its reason.
    pub justified: Vec<(String, String)>,
    /// Reasons given each time the goals were replaced.
    pub revisions: Vec<String>,
    /// All checks passed: the task is done.
    pub verified: bool,
    /// Verification rounds already run.
    pub rounds: usize,
}

/// A test file as it was when the goals were set. Its text is kept (small files only) so that a
/// change can be shown line by line: an untracked file has no `git diff`, and a model told only
/// "it changed" tends to answer that it did not.
#[derive(Debug, Clone)]
pub struct Protected {
    pub path: PathBuf,
    pub print: u64,
    pub text: Option<String>,
}

/// One check as Po ran it.
#[derive(Debug, Clone)]
pub struct CheckResult {
    pub goal: String,
    pub check: String,
    pub passed: bool,
    /// What the command printed, cut.
    pub output: String,
}

pub fn tool_spec() -> ToolSpec {
    ToolSpec::new(
        TOOL,
        "Commit to the goals of the current task and how Po will verify them. In auto mode, call it \
         after exploring and before any change. action=set: 1 to 6 goals, each a concrete outcome \
         with a `check`: a shell command run in the workspace that exits 0 only when the goal is \
         really met (run the relevant tests, the program with its expected output checked by grep, \
         a file content check...). For several cases, first write a new test file (allowed before \
         the goals) and use it as the check: `python -m pytest -q test_x.py`. That test must cover \
         every rule, example and rejected input the request or its spec names, one assertion each: \
         what it does not test, Po cannot verify. Po runs the checks \
         itself when you finish and tells you what fails: no need to run them all yourself first. Never weaken a test or a check to make it pass. action=revise: replace the goals \
         (give `reason`). action=justify: you had to change an existing test file (give `path` and \
         `reason`: why the test itself was wrong).",
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["set", "revise", "justify"]},
                "goals": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "goal": {"type": "string", "description": "the outcome, in a short sentence"},
                            "check": {"type": "string", "description": "shell command that exits 0 only when the goal is met"}
                        },
                        "required": ["goal", "check"]
                    }
                },
                "reason": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["action"]
        }),
    )
}

/// The goals of a `set` or `revise` call, or why they are refused.
pub fn parse_goals(input: &Value) -> Result<Vec<Goal>, String> {
    // Some models send the list as a JSON string: read it rather than refuse a round.
    let parsed;
    let items = match input.get("goals") {
        Some(Value::String(text)) => {
            parsed = serde_json::from_str::<Value>(text).map_err(|_| "give `goals`: a list of {goal, check}")?;
            parsed.as_array().ok_or("give `goals`: a list of {goal, check}")?
        }
        other => other.and_then(Value::as_array).ok_or("give `goals`: a list of {goal, check}")?,
    };
    if items.is_empty() {
        return Err("give at least one goal".into());
    }
    if items.len() > MAX_GOALS {
        return Err(format!("at most {MAX_GOALS} goals: keep the few that prove the task is done"));
    }
    let mut goals = Vec::new();
    for item in items {
        let text = item.get("goal").and_then(Value::as_str).unwrap_or("").trim().to_owned();
        let check = item.get("check").and_then(Value::as_str).unwrap_or("").trim().to_owned();
        if text.is_empty() || check.is_empty() {
            return Err("each goal needs a `goal` and a `check`".into());
        }
        if let Some(why) = hollow_check(&check) {
            return Err(format!("check `{check}` refused: {why}. A check must be able to fail when the goal is not met"));
        }
        goals.push(Goal { text, check });
    }
    Ok(goals)
}

/// Why a check could never fail, if it could not.
pub fn hollow_check(check: &str) -> Option<&'static str> {
    let c = check.trim();
    let lower = c.to_lowercase();
    let squashed: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    for masked in ["|| true", "||true", "|| exit 0", "||exit 0", "; true", ";true", "; exit 0", "|| :", "||:"] {
        if squashed.ends_with(masked) || squashed.contains(&format!("{masked} ")) {
            return Some("it hides its own failure");
        }
    }
    // Only printing or a constant: the outcome does not depend on the work.
    let first = squashed.split([' ', ';', '&', '|']).next().unwrap_or("");
    let constant = matches!(first, "echo" | "printf" | "true" | ":" | "exit")
        && !squashed.contains("&&")
        && !squashed.contains('|')
        && !squashed.contains(';');
    if constant || squashed == "exit 0" {
        return Some("it succeeds whatever the state of the work");
    }
    None
}

/// Whether a file name looks like a test: what a change to would make a check pass for the wrong
/// reason.
pub fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
    let in_tests = path.components().any(|c| matches!(c.as_os_str().to_str(), Some("tests" | "test" | "__tests__" | "spec")));
    name.starts_with("test_")
        || name.starts_with("test.")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_tests.rs")
        || (in_tests && !name.is_empty())
}

fn fingerprint(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    Some(hasher.finish())
}

/// The test files of the workspace as they are now: what a check could be made to pass by. The
/// files a check merely runs (the implementation itself) are not protected: fixing them is the
/// work. Bounded: a huge tree is not walked whole.
pub fn protect(workspace: &Path, _goals: &[Goal]) -> Vec<Protected> {
    const SKIP: &[&str] = &[".git", "target", "node_modules", ".venv", "venv", "__pycache__", "dist", "build", ".po"];
    let mut out = Vec::new();
    let mut stack = vec![(workspace.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > 20_000 {
                return out;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                if depth < 8 && !SKIP.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if is_test_file(path.strip_prefix(workspace).unwrap_or(&path))
                && let Some(print) = fingerprint(&path)
            {
                let text = std::fs::read_to_string(&path).ok().filter(|t| t.len() <= 64 * 1024);
                out.push(Protected { path, print, text });
            }
        }
    }
    out
}

/// Protected files changed or removed since the goals were set, and not justified.
pub fn tampered(goals: &Goals, workspace: &Path) -> Vec<String> {
    goals
        .protected
        .iter()
        .filter(|p| fingerprint(&p.path) != Some(p.print))
        .map(|p| p.path.strip_prefix(workspace).unwrap_or(&p.path).display().to_string().replace('\\', "/"))
        .filter(|shown| !goals.justified.iter().any(|(p, _)| p.replace('\\', "/").trim_start_matches("./") == shown))
        .collect()
}

/// Said right after a change to a test file the goals rely on: the model knows why it made the
/// change at that moment, while at the final check it no longer does (measured: a small model
/// spent three rounds answering "unchanged" about a test it had corrected).
pub fn edit_note(goals: &Goals, workspace: &Path, path: &str) -> Option<String> {
    let wanted = workspace.join(path);
    let wanted = wanted.canonicalize().unwrap_or(wanted);
    let hit = goals.protected.iter().find(|p| p.path.canonicalize().unwrap_or_else(|_| p.path.clone()) == wanted)?;
    let shown = hit.path.strip_prefix(workspace).unwrap_or(&hit.path).display().to_string().replace('\\', "/");
    if goals.justified.iter().any(|(p, _)| p.replace('\\', "/").trim_start_matches("./") == shown) {
        return None;
    }
    Some(format!(
        "\n[Po: {shown} is a test file your goals rely on. If the test itself was wrong, call goals now with \
         {{\"action\": \"justify\", \"path\": \"{shown}\", \"reason\": \"<why>\"}}. If you changed it to make it pass, undo it.]"
    ))
}

/// The lines of a protected file removed and added since the goals were set, bounded.
pub fn what_changed(goals: &Goals, workspace: &Path, shown: &str) -> Option<String> {
    let kept = goals.protected.iter().find(|p| {
        p.path.strip_prefix(workspace).unwrap_or(&p.path).display().to_string().replace('\\', "/") == shown
    })?;
    let before = kept.text.as_deref()?;
    let Ok(now) = std::fs::read_to_string(&kept.path) else { return Some("  (the file was deleted)\n".into()) };
    let mut left: Vec<&str> = now.lines().collect();
    let mut removed = Vec::new();
    for line in before.lines() {
        match left.iter().position(|l| *l == line) {
            Some(i) => {
                left.remove(i);
            }
            None => removed.push(line),
        }
    }
    let mut out = String::new();
    for (sign, lines) in [("-", &removed), ("+", &left)] {
        for line in lines.iter().filter(|l| !l.trim().is_empty()).take(12) {
            out.push_str(&format!("  {sign} {}\n", line.chars().take(160).collect::<String>()));
        }
    }
    Some(if out.is_empty() { "  (only blank lines or line order changed)\n".into() } else { out })
}

/// What the model reads after a verification that did not pass. `tampered`: each changed test
/// file with, when Po kept its text, the lines that changed.
pub fn failure_prompt(results: &[CheckResult], tampered: &[(String, Option<String>)], round: usize) -> String {
    let mut text = format!(
        "[Po ran your goal checks (round {round} of {MAX_ROUNDS}) and the task is not done yet. Fix the work, not the checks, then finish again.]\n"
    );
    for r in results.iter().filter(|r| !r.passed) {
        text.push_str(&format!("\nFAILED - {}\n$ {}\n{}\n", r.goal, r.check, r.output));
    }
    for (path, diff) in tampered {
        text.push_str(&format!(
            "\nTEST FILE CHANGED - {path} is not the same as when you set your goals (Po kept a copy; git cannot show it if the file is untracked). "
        ));
        match diff {
            Some(diff) => text.push_str(&format!("Lines removed (-) and added (+) since then:\n{diff}")),
            None => text.push('\n'),
        }
        text.push_str(&format!(
            "Either undo that change, or, if the test itself was wrong, call goals with {{\"action\": \"justify\", \"path\": \"{path}\", \"reason\": \"<why the old test was wrong>\"}} - the user will read the reason. Saying the file is unchanged does not resolve it.\n"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_that_cannot_fail_are_refused() {
        for hollow in ["echo ok", "true", "exit 0", "pytest || true", "cargo test ||true", "make; true", "npm test || exit 0", "printf done"] {
            assert!(hollow_check(hollow).is_some(), "{hollow}");
        }
        for real in [
            "python -m pytest -q",
            "cargo test",
            "python cli.py 2 8 | grep -q 'gmean 4'",
            "test -f README.md && grep -q 'Usage' README.md",
            "echo 3 | python sum.py | grep -qx 6",
        ] {
            assert!(hollow_check(real).is_none(), "{real}");
        }
    }

    #[test]
    fn goals_are_parsed_and_bounded() {
        let ok = json!({"action": "set", "goals": [{"goal": "tests pass", "check": "pytest -q"}]});
        assert_eq!(parse_goals(&ok).unwrap().len(), 1);
        assert!(parse_goals(&json!({"goals": []})).is_err());
        assert!(parse_goals(&json!({"goals": [{"goal": "x"}]})).is_err(), "a goal without a check");
        let many: Vec<Value> = (0..7).map(|i| json!({"goal": format!("g{i}"), "check": "pytest"})).collect();
        assert!(parse_goals(&json!({"goals": many})).is_err());
        assert!(parse_goals(&json!({"goals": [{"goal": "x", "check": "echo done"}]})).is_err());
    }

    #[test]
    fn goals_sent_as_a_json_string_are_read() {
        let input = json!({"action": "set", "goals": "[{\"goal\": \"ok\", \"check\": \"pytest -q\"}]"});
        assert_eq!(parse_goals(&input).unwrap()[0].check, "pytest -q");
    }

    #[test]
    fn the_implementation_a_check_runs_is_not_protected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("csvsum.py"), "old").unwrap();
        let goal = Goal { text: "sum".into(), check: "python csvsum.py sample.csv amount".into() };
        let goals = Goals { protected: protect(dir.path(), std::slice::from_ref(&goal)), items: vec![goal], ..Default::default() };
        std::fs::write(dir.path().join("csvsum.py"), "fixed").unwrap();
        assert!(tampered(&goals, dir.path()).is_empty(), "fixing the implementation is the work, not tampering");
    }

    #[test]
    fn test_files_are_recognized() {
        for t in ["test_stats.py", "tests/a.rs", "src/app.test.ts", "x_test.go", "spec/model.rb", "lib.spec.js"] {
            assert!(is_test_file(Path::new(t)), "{t}");
        }
        for f in ["stats.py", "src/lib.rs", "contest.py", "latest.md"] {
            assert!(!is_test_file(Path::new(f)), "{f}");
        }
    }

    #[test]
    fn a_changed_test_file_is_caught_unless_justified() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("test_a.py"), "assert f() == 2").unwrap();
        std::fs::write(dir.path().join("a.py"), "def f(): return 1").unwrap();
        let goal = Goal { text: "f works".into(), check: "python -m pytest test_a.py".into() };
        let mut goals = Goals { protected: protect(dir.path(), std::slice::from_ref(&goal)), items: vec![goal], ..Default::default() };
        assert!(tampered(&goals, dir.path()).is_empty());
        // The code is fixed: nothing to say.
        std::fs::write(dir.path().join("a.py"), "def f(): return 2").unwrap();
        assert!(tampered(&goals, dir.path()).is_empty());
        // The test is weakened: caught.
        std::fs::write(dir.path().join("test_a.py"), "assert True").unwrap();
        assert_eq!(tampered(&goals, dir.path()), ["test_a.py"]);
        let diff = what_changed(&goals, dir.path(), "test_a.py").unwrap();
        assert!(diff.contains("- assert f() == 2") && diff.contains("+ assert True"), "{diff}");
        assert!(edit_note(&goals, dir.path(), "test_a.py").unwrap().contains("justify"));
        assert!(edit_note(&goals, dir.path(), "a.py").is_none(), "the implementation is not a protected file");
        let prompt = failure_prompt(&[], &[("test_a.py".into(), Some(diff))], 1);
        assert!(prompt.contains("\"action\": \"justify\", \"path\": \"test_a.py\""), "{prompt}");
        goals.justified.push(("test_a.py".into(), "the expected value was wrong".into()));
        assert!(tampered(&goals, dir.path()).is_empty(), "a justified change passes, the user is told");
    }
}
