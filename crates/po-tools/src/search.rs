//! File and content search.
//!
//! These two tools are the ones an agent calls most often, and the ones whose output fills the
//! context fastest. They are therefore bounded by default, and always announce what they left
//! out.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{ToolOutcome, ToolSpec};
use regex::RegexBuilder;
use serde_json::{Value, json};

use crate::{Tool, ToolContext, required_str};

const DEFAULT_LIMIT: usize = 100;
const MAX_DEPTH: usize = 24;
/// Limits of a walk. Started from a home folder, an unbounded walk froze Po (observed:
/// `C:\Users\<name>`, `AppData` and its looping junctions). Beyond them, we return what was
/// found and say why we stopped.
const MAX_ENTRIES: usize = 50_000;
const MAX_WALK: Duration = Duration::from_secs(8);
/// A larger file is not read by `grep`: it is almost always an artifact.
const MAX_GREP_FILE: u64 = 2 * 1024 * 1024;

/// Budget of a walk: time, number of entries, and interruption by the user.
struct Budget {
    started: Instant,
    seen: usize,
    cancel: Arc<AtomicBool>,
    stopped: Option<String>,
}

impl Budget {
    fn new(cancel: Arc<AtomicBool>) -> Self {
        Self { started: Instant::now(), seen: 0, cancel, stopped: None }
    }

    /// Counts an entry; false when the walk must stop.
    fn tick(&mut self) -> bool {
        self.seen += 1;
        if self.stopped.is_some() {
            return false;
        }
        if self.cancel.load(Ordering::Relaxed) {
            self.stopped = Some("interrupted".into());
        } else if self.seen > MAX_ENTRIES {
            self.stopped = Some(format!("more than {MAX_ENTRIES} entries scanned"));
        } else if self.started.elapsed() > MAX_WALK {
            self.stopped = Some(format!("{} s elapsed", MAX_WALK.as_secs()));
        }
        self.stopped.is_none()
    }

    fn note(&self) -> String {
        match &self.stopped {
            Some(reason) => {
                format!("\n\n[walk stopped ({reason}): partial results - narrow the pattern to a subfolder]")
            }
            None => String::new(),
        }
    }
}

/// Signals abandonment when the tool's future is dropped (Esc, Ctrl+C): the walk, which runs
/// on a separate thread, stops at the next entry.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Runs a walk off the interface thread: it stays alive and interruptible.
async fn blocking<F>(id: &str, work: F) -> ToolOutcome
where
    F: FnOnce(Arc<AtomicBool>) -> ToolOutcome + Send + 'static,
{
    let cancel = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancel.clone());
    match tokio::task::spawn_blocking(move || work(cancel)).await {
        Ok(outcome) => outcome,
        Err(err) => ToolOutcome::error(id, format!("search interrupted: {err}")),
    }
}

/// Folders always ignored: walking them costs time and never yields a useful result.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".cache",
    "vendor",
    ".idea",
    ".vscode",
];

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "glob",
            "Finds files by pattern, for example `**/*.rs` or `src/*.toml`. Dependency and build \
             folders are ignored.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "Folder to search instead of the workspace"},
                    "limit": {"type": "integer", "description": "Maximum number of results"}
                },
                "required": ["pattern"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let (id_owned, ctx) = (id.to_owned(), ctx.clone());
        blocking(id, move |cancel| glob_blocking(&id_owned, &input, &ctx, cancel)).await
    }
}

fn glob_blocking(id: &str, input: &Value, ctx: &ToolContext, cancel: Arc<AtomicBool>) -> ToolOutcome {
    let pattern = match required_str(input, "pattern") {
        Ok(value) => value,
        Err(err) => return ToolOutcome::error(id, err),
    };
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_LIMIT as u64) as usize;

    let matcher = match glob_to_regex(pattern) {
        Ok(value) => value,
        Err(err) => return ToolOutcome::error(id, format!("invalid pattern: {err}")),
    };

    let root = match search_root(input, ctx) {
        Ok(root) => root,
        Err(err) => return ToolOutcome::error(id, err),
    };
    let mut found = Vec::new();
    let mut budget = Budget::new(cancel);
    walk(&root, &mut budget, &mut |path| {
        if matcher.is_match(&relative_to(&root, path)) {
            found.push(ctx.workspace.display_path(path));
        }
        found.len() < limit * 4
    });

    if found.is_empty() {
        return ToolOutcome::ok(id, format!("no file matches `{pattern}`{}", budget.note()));
    }
    found.sort();
    let total = found.len();
    found.truncate(limit);

    let mut body = found.join("\n");
    if total > limit {
        body.push_str(&format!("\n\n[{} of {total} results shown]", limit));
    }
    body.push_str(&budget.note());
    ToolOutcome::ok(id, ctx.clamp(&body)).with_display(po_i18n::t!("tools.glob.display", n = total, pattern = pattern))
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "grep",
            "Searches the workspace files for a regular expression. Returns the matching lines with \
             their file and line number.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression"},
                    "glob": {"type": "string", "description": "Restrict to matching files"},
                    "path": {"type": "string", "description": "Folder or file to search instead of the workspace"},
                    "ignore_case": {"type": "boolean"},
                    "limit": {"type": "integer"}
                },
                "required": ["pattern"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let (id_owned, ctx) = (id.to_owned(), ctx.clone());
        blocking(id, move |cancel| grep_blocking(&id_owned, &input, &ctx, cancel)).await
    }
}

fn grep_blocking(id: &str, input: &Value, ctx: &ToolContext, cancel: Arc<AtomicBool>) -> ToolOutcome {
    let pattern = match required_str(input, "pattern") {
        Ok(value) => value,
        Err(err) => return ToolOutcome::error(id, err),
    };
    let ignore_case = input.get("ignore_case").and_then(Value::as_bool).unwrap_or(false);
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(DEFAULT_LIMIT as u64) as usize;

    let regex = match RegexBuilder::new(pattern).case_insensitive(ignore_case).build() {
        Ok(value) => value,
        Err(err) => return ToolOutcome::error(id, format!("invalid expression: {err}")),
    };
    let file_filter = match input.get("glob").and_then(Value::as_str) {
        Some(glob) => match glob_to_regex(glob) {
            Ok(value) => Some(value),
            Err(err) => return ToolOutcome::error(id, format!("invalid pattern: {err}")),
        },
        None => None,
    };

    let root = match search_root(input, ctx) {
        Ok(root) => root,
        Err(err) => return ToolOutcome::error(id, err),
    };
    let mut candidates = Vec::new();
    let mut budget = Budget::new(cancel);
    walk(&root, &mut budget, &mut |path| {
        if file_filter.as_ref().is_none_or(|f| f.is_match(&relative_to(&root, path))) {
            candidates.push((path.to_path_buf(), ctx.workspace.display_path(path)));
        }
        true
    });
    candidates.sort_by(|a, b| a.1.cmp(&b.1));

    let mut hits = Vec::new();
    let mut truncated = false;
    for (path, relative) in candidates {
        // Reading counts against the same budget as the walk.
        if !budget.tick() {
            break;
        }
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_GREP_FILE) {
            continue;
        }
        let Ok(content) = std::fs::read(&path) else { continue };
        if content.iter().take(8000).any(|b| *b == 0) {
            continue; // binary
        }
        for (index, line) in String::from_utf8_lossy(&content).lines().enumerate() {
            if regex.is_match(line) {
                if hits.len() >= limit {
                    truncated = true;
                    break;
                }
                // A minified line can be megabytes long: we bound it here, otherwise a single
                // result is enough to saturate the context window.
                let line = if line.len() > 300 { &line[..clamp_boundary(line, 300)] } else { line };
                hits.push(format!("{relative}:{}: {}", index + 1, line.trim_end()));
            }
        }
        if truncated {
            break;
        }
    }

    if hits.is_empty() {
        return ToolOutcome::ok(id, format!("no match for `{pattern}`{}", budget.note()));
    }
    let count = hits.len();
    let mut body = hits.join("\n");
    if truncated {
        body.push_str(&format!("\n\n[limit of {limit} results reached]"));
    }
    body.push_str(&budget.note());
    ToolOutcome::ok(id, ctx.clamp(&body)).with_display(po_i18n::t!("tools.grep.display", n = count))
}

fn clamp_boundary(text: &str, mut at: usize) -> usize {
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Walks the tree like ripgrep: `.gitignore` and `.ignore` are honoured (even outside a
/// repository), hidden files and folders are skipped, links and junctions are not followed,
/// depth is bounded. On top of that come the dependency folders and, on Windows, hidden or
/// system folders. `visit` returns `false` to stop the walk; the budget stops it too.
/// Where a search starts: the `path` given (a folder the workspace may read, such as Po's own
/// source under Po Labs), else the workspace.
fn search_root(input: &Value, ctx: &ToolContext) -> Result<std::path::PathBuf, String> {
    match input.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) {
        Some(path) => ctx.workspace.resolve_for_read(path).map_err(|e| e.to_string()),
        None => Ok(ctx.workspace.root().to_path_buf()),
    }
}

/// Path of a file under the search root, with `/`: what patterns are matched against.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string().replace('\\', "/")
}

fn walk(root: &Path, budget: &mut Budget, visit: &mut impl FnMut(&Path) -> bool) {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .require_git(false)
        .follow_links(false)
        .max_depth(Some(MAX_DEPTH))
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| {
            if !entry.file_type().is_some_and(|t| t.is_dir()) || entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&name.as_ref()) && !hidden(entry)
        })
        .build();
    for entry in walker {
        if !budget.tick() {
            return;
        }
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_some_and(|t| t.is_file()) && !visit(entry.path()) {
            return;
        }
    }
}

/// Hidden or system folder in the Windows sense (`AppData`, `Application Data`...), or reparse
/// point (junction). Elsewhere, a leading dot in the name is enough: `hidden(true)` handles it.
#[cfg(windows)]
fn hidden(entry: &ignore::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const HIDDEN: u32 = 0x2;
    const SYSTEM: u32 = 0x4;
    const REPARSE_POINT: u32 = 0x400;
    entry.metadata().is_ok_and(|m| m.file_attributes() & (HIDDEN | SYSTEM | REPARSE_POINT) != 0)
}

#[cfg(not(windows))]
fn hidden(_entry: &ignore::DirEntry) -> bool {
    false
}

/// Translates a glob pattern into an anchored regular expression.
///
/// `**` crosses folders, `*` stops at the separator, `?` matches one character.
/// The other metacharacters are escaped: a dot in `*.rs` must remain a dot.
pub fn glob_to_regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    let mut out = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        match chars[index] {
            '*' => {
                let double = chars.get(index + 1) == Some(&'*');
                if double {
                    // `**/` must also accept zero folders, so that `**/*.rs` finds a file at
                    // the root.
                    if chars.get(index + 2) == Some(&'/') {
                        out.push_str("(?:.*/)?");
                        index += 3;
                        continue;
                    }
                    out.push_str(".*");
                    index += 2;
                    continue;
                }
                out.push_str("[^/]*");
                index += 1;
            }
            '?' => {
                out.push_str("[^/]");
                index += 1;
            }
            '{' => {
                // `{a,b}` becomes an alternation.
                let closing = chars[index..].iter().position(|c| *c == '}');
                match closing {
                    Some(offset) => {
                        let inner: String = chars[index + 1..index + offset].iter().collect();
                        let branches: Vec<String> = inner.split(',').map(regex::escape).collect();
                        out.push_str(&format!("(?:{})", branches.join("|")));
                        index += offset + 1;
                    }
                    None => {
                        out.push_str(&regex::escape("{"));
                        index += 1;
                    }
                }
            }
            other => {
                out.push_str(&regex::escape(&other.to_string()));
                index += 1;
            }
        }
    }
    out.push('$');
    regex::Regex::new(&out)
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

    fn write_file(ctx: &ToolContext, path: &str, content: &str) {
        let resolved = ctx.workspace.resolve(path).unwrap();
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(resolved, content).unwrap();
    }

    #[test]
    fn glob_distinguishes_one_star_from_two() {
        let simple = glob_to_regex("src/*.rs").unwrap();
        assert!(simple.is_match("src/main.rs"));
        assert!(!simple.is_match("src/a/b.rs"), "one star does not cross folders");

        let deep = glob_to_regex("**/*.rs").unwrap();
        assert!(deep.is_match("src/a/b.rs"));
        assert!(deep.is_match("main.rs"), "`**/` must accept zero folders");
    }

    #[test]
    fn the_dot_stays_literal() {
        let matcher = glob_to_regex("*.rs").unwrap();
        assert!(matcher.is_match("main.rs"));
        assert!(!matcher.is_match("mainXrs"), "the dot must not be a wildcard");
    }

    #[test]
    fn alternatives_are_supported() {
        let matcher = glob_to_regex("**/*.{rs,toml}").unwrap();
        assert!(matcher.is_match("src/main.rs"));
        assert!(matcher.is_match("Cargo.toml"));
        assert!(!matcher.is_match("README.md"));
    }

    #[test]
    fn a_pattern_without_closing_brace_does_not_panic() {
        assert!(glob_to_regex("src/{a").is_ok());
    }

    #[tokio::test]
    async fn glob_finds_files_and_ignores_dependencies() {
        let (_dir, ctx) = context();
        write_file(&ctx, "src/main.rs", "");
        write_file(&ctx, "src/lib.rs", "");
        write_file(&ctx, "target/debug/build.rs", "");
        write_file(&ctx, "node_modules/package/index.rs", "");

        let outcome = GlobTool.run("g", json!({"pattern": "**/*.rs"}), &ctx).await;
        assert!(outcome.content.contains("src/main.rs"));
        assert!(outcome.content.contains("src/lib.rs"));
        assert!(!outcome.content.contains("target/"), "build artifacts are excluded");
        assert!(!outcome.content.contains("node_modules"));
    }

    #[tokio::test]
    async fn glob_without_result_says_so_clearly() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.txt", "");
        let outcome = GlobTool.run("g", json!({"pattern": "**/*.rs"}), &ctx).await;
        assert!(!outcome.is_error);
        assert!(outcome.content.contains("no file"));
    }

    #[tokio::test]
    async fn grep_locates_each_match() {
        let (_dir, ctx) = context();
        write_file(&ctx, "src/main.rs", "fn main() {}\nfn other() {}\n");
        write_file(&ctx, "src/lib.rs", "pub fn main_lib() {}\n");

        let outcome = GrepTool.run("g", json!({"pattern": r"fn \w+\("}), &ctx).await;
        assert!(outcome.content.contains("src/main.rs:1:"));
        assert!(outcome.content.contains("src/main.rs:2:"));
        assert!(outcome.content.contains("src/lib.rs:1:"));
    }

    #[tokio::test]
    async fn grep_and_glob_search_an_opened_folder_by_its_plain_path() {
        let (_dir, mut ctx) = context();
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(other.path().join("src")).unwrap();
        std::fs::write(other.path().join("src").join("render.rs"), "let icon_style = 1;\n").unwrap();
        ctx.workspace.allow_write(other.path());
        let plain = crate::workspace::simplified(other.path().canonicalize().unwrap()).display().to_string();

        let outcome = GrepTool.run("g", json!({"pattern": "icon_style", "path": plain}), &ctx).await;
        assert!(outcome.content.contains("render.rs:1:"), "{}", outcome.content);
        let outcome = GlobTool.run("g", json!({"pattern": "src/*.rs", "path": plain}), &ctx).await;
        assert!(outcome.content.contains("render.rs"), "{}", outcome.content);
        let nowhere = if cfg!(windows) { "C:/nowhere/at/all" } else { "/nowhere/at/all" };
        let outcome = GrepTool.run("g", json!({"pattern": "x", "path": nowhere}), &ctx).await;
        assert!(outcome.is_error, "a folder not opened stays closed");
    }

    #[tokio::test]
    async fn grep_can_be_restricted_by_glob() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "needle\n");
        write_file(&ctx, "b.txt", "needle\n");

        let outcome = GrepTool.run("g", json!({"pattern": "needle", "glob": "**/*.rs"}), &ctx).await;
        assert!(outcome.content.contains("a.rs"));
        assert!(!outcome.content.contains("b.txt"));
    }

    #[tokio::test]
    async fn grep_ignores_case_on_request() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.txt", "Hello\n");

        let sensitive = GrepTool.run("g", json!({"pattern": "hello"}), &ctx).await;
        assert!(sensitive.content.contains("no match"));

        let insensitive = GrepTool.run("g", json!({"pattern": "hello", "ignore_case": true}), &ctx).await;
        assert!(insensitive.content.contains("a.txt:1:"));
    }

    #[tokio::test]
    async fn grep_bounds_an_endless_line() {
        let (_dir, ctx) = context();
        write_file(&ctx, "min.js", &format!("var x=\"{}\";needle\n", "a".repeat(50_000)));

        let outcome = GrepTool.run("g", json!({"pattern": "needle"}), &ctx).await;
        assert!(outcome.content.len() < 1000, "a minified line must not drown the context");
    }

    #[test]
    fn an_interrupted_walk_returns_what_it_has_and_says_so() {
        let (dir, ctx) = context();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        let outcome = glob_blocking("g", &json!({"pattern": "**/*"}), &ctx, cancel);
        assert!(outcome.content.contains("walk stopped (interrupted)"), "{}", outcome.content);
    }

    #[test]
    fn gitignore_is_honoured_even_outside_a_repository() {
        let (dir, ctx) = context();
        std::fs::write(dir.path().join(".gitignore"), "generated/\n*.log\n").unwrap();
        std::fs::create_dir_all(dir.path().join("generated")).unwrap();
        std::fs::write(dir.path().join("generated").join("needle.txt"), "needle").unwrap();
        std::fs::write(dir.path().join("trace.log"), "needle").unwrap();
        std::fs::write(dir.path().join("real.txt"), "needle").unwrap();
        let outcome = grep_blocking("g", &json!({"pattern": "needle"}), &ctx, Arc::new(AtomicBool::new(false)));
        assert!(outcome.content.contains("real.txt"), "{}", outcome.content);
        assert!(!outcome.content.contains("generated") && !outcome.content.contains("trace.log"), "{}", outcome.content);
    }

    #[cfg(windows)]
    #[test]
    fn a_junction_to_a_parent_is_not_followed() {
        let (dir, ctx) = context();
        std::fs::create_dir_all(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a").join("f.txt"), "x").unwrap();
        // A junction pointing to the root: following it would make the walk loop.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(dir.path().join("a").join("looping"))
            .arg(dir.path())
            .output()
            .unwrap();
        if !status.status.success() {
            return; // mklink unavailable: nothing to check here
        }
        let outcome = glob_blocking("g", &json!({"pattern": "**/f.txt"}), &ctx, Arc::new(AtomicBool::new(false)));
        assert_eq!(outcome.content.lines().filter(|l| l.ends_with("f.txt")).count(), 1, "{}", outcome.content);
    }

    #[tokio::test]
    async fn grep_honours_the_limit_and_announces_it() {
        let (_dir, ctx) = context();
        let lines: String = (0..50).map(|i| format!("needle {i}\n")).collect();
        write_file(&ctx, "a.txt", &lines);

        let outcome = GrepTool.run("g", json!({"pattern": "needle", "limit": 5}), &ctx).await;
        assert!(outcome.content.contains("limit of 5"));
        assert_eq!(outcome.content.lines().filter(|l| l.contains("a.txt:")).count(), 5);
    }

    #[tokio::test]
    async fn an_invalid_expression_is_reported_without_panicking() {
        let (_dir, ctx) = context();
        let outcome = GrepTool.run("g", json!({"pattern": "[unclosed"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("invalid"));
    }

    #[tokio::test]
    async fn grep_skips_binaries() {
        let (_dir, ctx) = context();
        let path = ctx.workspace.resolve("bin.dat").unwrap();
        std::fs::write(&path, [b'n', b'e', b'e', b'd', b'l', b'e', 0, 1, 2]).unwrap();

        let outcome = GrepTool.run("g", json!({"pattern": "needle"}), &ctx).await;
        assert!(outcome.content.contains("no match"));
    }
}
