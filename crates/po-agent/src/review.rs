//! Rigor: how much method a turn follows, whatever the mode (the mode only says what the model
//! may do). And the review of a turn's changes by an independent reviewer.
//!
//! Measured on Po's benches: plans and self-written checks did not make a capable local model
//! more correct on small tasks, and cost two to three times the time. A fresh pair of eyes on
//! the diff is cheaper (one subagent pass) and catches what a single context misses on real,
//! multi-file changes: a forgotten piece, a regression elsewhere, a convention not followed. So
//! the default (`auto`) reviews only changes of some size, and small requests stay direct.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// The rigor a turn follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rigor {
    /// No checklist, no review: the model does the task.
    Direct,
    /// A short checklist for a change of several steps; a review of changes of some size.
    Standard,
    /// A checklist; every change reviewed, up to two rounds; what to test said at the end.
    Full,
    /// Standard, with a full review of large changes and of a plan launched from plan mode.
    Auto,
}

/// Preference key (Po's `ui.json`).
pub const PREF: &str = "rigor";

impl Rigor {
    pub const ALL: [Rigor; 4] = [Rigor::Auto, Rigor::Direct, Rigor::Standard, Rigor::Full];

    pub fn as_str(self) -> &'static str {
        match self {
            Rigor::Direct => "direct",
            Rigor::Standard => "standard",
            Rigor::Full => "full",
            Rigor::Auto => "auto",
        }
    }

    pub fn parse(raw: &str) -> Option<Rigor> {
        match raw.trim().to_lowercase().as_str() {
            "direct" | "directe" => Some(Rigor::Direct),
            "standard" => Some(Rigor::Standard),
            "full" | "complet" | "complete" | "complète" => Some(Rigor::Full),
            "auto" => Some(Rigor::Auto),
            _ => None,
        }
    }

    /// The rigor the user chose (`/rigor`), `auto` by default.
    pub fn current() -> Rigor {
        po_i18n::prefs::get_str(PREF).as_deref().and_then(Rigor::parse).unwrap_or(Rigor::Auto)
    }

    /// What the model must know of it. Attached to the next request when it changes, like the
    /// work mode's note: the system prompt stays the same for the server's cache.
    pub fn note(self) -> &'static str {
        match self {
            Rigor::Direct => "[Rigor: direct] Do the task directly, without a checklist. Po does not have your changes reviewed.",
            Rigor::Standard => {
                "[Rigor: standard] For a change of several steps, first state a short numbered checklist (3 to 7 items) and \
                 follow it. When you finish a change of some size, Po has it reviewed by an independent reviewer and gives \
                 you the findings. After a change, end your answer with a short \"To test\" list for the user (what to try, \
                 what they should see)."
            }
            Rigor::Full => {
                "[Rigor: full] Before changing anything beyond a trivial fix, state a numbered checklist with how each item \
                 will be checked, and follow it. When you finish, Po has every change reviewed by an independent reviewer \
                 (up to two rounds). End your answer with a short \"To test\" list for the user."
            }
            Rigor::Auto => {
                "[Rigor: auto] Match the method to the request: answer questions and make small fixes directly; for a change \
                 of several steps, first state a short numbered checklist (3 to 7 items) and follow it. When you finish a \
                 change of some size, Po has it reviewed by an independent reviewer and gives you the findings. After a \
                 change, end your answer with a short \"To test\" list for the user (what to try, what they should see)."
            }
        }
    }

    /// How many review rounds `changes` deserve; 0: none. `planned`: the turn implements a plan
    /// launched from plan mode.
    pub fn review_rounds(self, changes: &Changes, planned: bool) -> usize {
        if changes.files.is_empty() {
            return 0;
        }
        let sizable = changes.files.len() >= 2 || changes.lines >= 30;
        let large = changes.files.len() >= 5 || changes.lines >= 300;
        match self {
            Rigor::Direct => 0,
            Rigor::Standard => usize::from(sizable),
            Rigor::Full => 2,
            Rigor::Auto if planned || large => 2,
            Rigor::Auto => usize::from(sizable),
        }
    }
}

/// The working tree when a turn started: what its changes are measured against.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// A commit holding the tree as it was (`git stash create`, which changes nothing), or HEAD
    /// when it was clean.
    base: String,
    /// Untracked files then: a new file since is a change of the turn.
    untracked: HashSet<String>,
}

/// What a turn changed.
#[derive(Debug, Clone, Default)]
pub struct Changes {
    pub files: Vec<String>,
    /// Lines added and removed.
    pub lines: usize,
    /// The diff, cut to a size a reviewer reads in one go.
    pub diff: String,
}

const MAX_DIFF: usize = 24_000;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let out = command.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A file a build or a run produces, not something the model wrote.
fn generated(path: &str) -> bool {
    const DIRS: &[&str] = &["__pycache__", "node_modules", "target", ".pytest_cache", ".mypy_cache", "dist", "build", ".venv", "venv", ".po"];
    path.split(['/', '\\']).any(|part| DIRS.contains(&part)) || path.ends_with(".pyc")
}

/// The tree as it is, or `None` outside a git repository (no review then).
pub fn snapshot(root: &Path) -> Option<Snapshot> {
    git(root, &["rev-parse", "--is-inside-work-tree"])?;
    let stashed = git(root, &["stash", "create"]).map(|s| s.trim().to_owned()).unwrap_or_default();
    let base = if stashed.is_empty() { git(root, &["rev-parse", "HEAD"])?.trim().to_owned() } else { stashed };
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard"])?.lines().map(str::to_owned).collect();
    Some(Snapshot { base, untracked })
}

/// What changed since `snapshot`.
pub fn changes(root: &Path, snapshot: &Snapshot) -> Option<Changes> {
    let mut files: Vec<String> =
        git(root, &["diff", "--name-only", &snapshot.base, "--"])?.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let mut diff = git(root, &["diff", "--no-color", &snapshot.base, "--"])?;
    // New files the turn wrote: text ones only, outside generated folders (a Python run leaves
    // `__pycache__/*.pyc` in a repository without .gitignore: the reviewer spent its time on them).
    let mut new: Vec<(String, String)> = Vec::new();
    for file in git(root, &["ls-files", "--others", "--exclude-standard"])?.lines() {
        if file.is_empty() || snapshot.untracked.contains(file) || generated(file) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(root.join(file)) {
            new.push((file.to_owned(), text));
        }
    }
    for (file, text) in &new {
        diff.push_str(&format!("--- /dev/null\n+++ b/{file} (new file)\n"));
        for line in text.lines() {
            diff.push('+');
            diff.push_str(line);
            diff.push('\n');
        }
    }
    files.extend(new.into_iter().map(|(file, _)| file));
    let lines = diff
        .lines()
        .filter(|l| (l.starts_with('+') && !l.starts_with("+++")) || (l.starts_with('-') && !l.starts_with("---")))
        .count();
    if diff.len() > MAX_DIFF {
        let cut = (0..=MAX_DIFF).rev().find(|i| diff.is_char_boundary(*i)).unwrap_or(0);
        diff.truncate(cut);
        diff.push_str("\n[... diff cut: read the files themselves for the rest]\n");
    }
    Some(Changes { files, lines, diff })
}

/// The reviewer's task: the request, the plan if there was one, the diff.
pub fn task(requests: &[String], plan: Option<&str>, changes: &Changes) -> String {
    let mut text = String::from(
        "Review the changes below, made in this workspace for the user's request. You can read any file to check \
         them. Report only real defects: a bug, a regression elsewhere, a part of the request not done, a test that \
         was weakened or now fails, a convention of the surrounding code not followed. No style remarks without \
         consequence, no rewrites.\n\nAnswer with one finding per line, most severe first:\n\
         BLOCKING|MINOR path:line - the problem - the case that triggers it\n\
         If there is no real defect, answer exactly: NO ISSUES\n",
    );
    if !requests.is_empty() {
        text.push_str("\nThe user's request:\n");
        for request in requests {
            text.push_str(&format!("> {}\n", request.trim().replace('\n', "\n> ")));
        }
    }
    if let Some(plan) = plan {
        text.push_str(&format!("\nThe plan it followed:\n{}\n", plan.trim()));
    }
    text.push_str(&format!("\nFiles changed: {}\n\n```diff\n{}\n```\n", changes.files.join(", "), changes.diff.trim_end()));
    text
}

/// The reviewer's findings as the main model reads them, or `None` when there is nothing to do.
pub fn findings_prompt(report: &str, round: usize, rounds: usize) -> Option<String> {
    let report = report.trim();
    let clean = report.to_uppercase().contains("NO ISSUES") && !report.to_uppercase().contains("BLOCKING");
    if report.is_empty() || clean {
        return None;
    }
    Some(format!(
        "[Po had your changes read by an independent reviewer (round {round} of {rounds}). Its findings:\n\n{report}\n\n\
         Check each one against the code. Fix the real problems (then run the relevant tests again). For a finding \
         that is wrong or does not apply, say so in one line with the reason: the user reads it. Then finish.]"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").args(args).current_dir(dir).output().unwrap().status.success(), "git {args:?}");
    }

    #[test]
    fn a_turns_changes_are_measured_against_the_tree_it_started_from() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        run(root, &["init", "-q"]);
        std::fs::write(root.join("a.py"), "x = 1\n").unwrap();
        run(root, &["add", "-A"]);
        run(root, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "start"]);
        // The user's own uncommitted work before the turn is not the turn's.
        std::fs::write(root.join("a.py"), "x = 2\n").unwrap();
        std::fs::write(root.join("notes.txt"), "mine\n").unwrap();
        let snap = snapshot(root).unwrap();
        assert!(changes(root, &snap).unwrap().files.is_empty(), "nothing changed during the turn yet");

        std::fs::write(root.join("a.py"), "x = 3\n").unwrap();
        std::fs::write(root.join("b.py"), "y = 1\nz = 2\n").unwrap();
        std::fs::create_dir(root.join("__pycache__")).unwrap();
        std::fs::write(root.join("__pycache__").join("b.cpython-313.pyc"), [0u8, 159, 146, 150]).unwrap();
        let c = changes(root, &snap).unwrap();
        assert_eq!(c.files, ["a.py", "b.py"]);
        assert!(c.diff.contains("-x = 2") && c.diff.contains("+x = 3") && c.diff.contains("+z = 2"), "{}", c.diff);
        assert!(!c.diff.contains("notes.txt"), "an untracked file of before is not part of it");
        assert_eq!(c.lines, 4);
        // Nothing is touched by the snapshot itself.
        assert_eq!(std::fs::read_to_string(root.join("a.py")).unwrap(), "x = 3\n");
    }

    #[test]
    fn the_rounds_follow_the_rigor_and_the_size() {
        let small = Changes { files: vec!["a.py".into()], lines: 4, diff: String::new() };
        let medium = Changes { files: vec!["a.py".into(), "b.py".into()], lines: 40, diff: String::new() };
        let large = Changes { files: (0..6).map(|i| format!("f{i}")).collect(), lines: 400, diff: String::new() };
        assert_eq!(Rigor::Auto.review_rounds(&Changes::default(), true), 0, "nothing changed");
        assert_eq!(Rigor::Auto.review_rounds(&small, false), 0, "a small fix stays direct");
        assert_eq!(Rigor::Auto.review_rounds(&medium, false), 1);
        assert_eq!(Rigor::Auto.review_rounds(&large, false), 2);
        assert_eq!(Rigor::Auto.review_rounds(&small, true), 2, "a launched plan is reviewed fully");
        assert_eq!(Rigor::Direct.review_rounds(&large, true), 0);
        assert_eq!(Rigor::Full.review_rounds(&small, false), 2);
    }

    #[test]
    fn a_clean_report_ends_the_review() {
        assert!(findings_prompt("NO ISSUES", 1, 1).is_none());
        assert!(findings_prompt("  no issues found.  ", 1, 1).is_none());
        let p = findings_prompt("BLOCKING cli.py:12 - --json ignored - `cli.py --json 1`", 1, 2).unwrap();
        assert!(p.contains("round 1 of 2") && p.contains("cli.py:12"));
        assert!(Rigor::parse("complet") == Some(Rigor::Full) && Rigor::parse("x").is_none());
    }
}
