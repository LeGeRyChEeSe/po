//! Input completion: `/…` commands and `@…` paths.
//!
//! Suggestions appear above the input while typing; the arrows move through them, and Tab or
//! Enter (on a half-typed command) picks the highlighted one. Nothing is completed unless the
//! user asks: a completion that imposed itself would rewrite what they are in the middle of
//! typing.

use std::path::Path;

/// Suggestions visible at once; beyond that, the window follows the selection.
pub const MAX_SUGGESTIONS: usize = 6;
/// Most paths suggested: a huge folder must not slow typing down.
const MAX_PATHS: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Text that will replace the current word.
    pub replacement: String,
    /// Description shown next to it.
    pub detail: String,
}

/// Word being typed, right before the cursor, and its position (in characters).
fn current_word(text: &str, cursor: usize) -> (usize, String) {
    let before: Vec<char> = text.chars().take(cursor).collect();
    let start = before.iter().rposition(|c| c.is_whitespace()).map(|i| i + 1).unwrap_or(0);
    (start, before[start..].iter().collect())
}

/// Suggestions for the input, from the known commands and the workspace.
pub fn suggestions(text: &str, cursor: usize, commands: &[(String, String)], workspace: Option<&Path>) -> Vec<Suggestion> {
    let (start, word) = current_word(text, cursor);
    // A command is typed at the start of the input, alone.
    if start == 0 && word.starts_with('/') && !text.contains(char::is_whitespace) {
        let out: Vec<Suggestion> = commands
            .iter()
            .filter(|(name, _)| name.starts_with(&word) && name != &word)
            .map(|(name, detail)| Suggestion { replacement: name.clone(), detail: detail.clone() })
            .collect();
        return out;
    }
    if let (Some(partial), Some(root)) = (word.strip_prefix('@'), workspace) {
        return paths(partial, root);
    }
    Vec::new()
}

/// Files and folders whose path starts with `partial`, relative to `root`.
fn paths(partial: &str, root: &Path) -> Vec<Suggestion> {
    let normalized = partial.replace('\\', "/");
    let (dir, prefix) = match normalized.rfind('/') {
        Some(i) => (&normalized[..=i], &normalized[i + 1..]),
        None => ("", normalized.as_str()),
    };
    // Never above the workspace: the tools would refuse it anyway.
    if dir.split('/').any(|part| part == "..") {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else { return Vec::new() };
    let mut out: Vec<Suggestion> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Hidden and build folders add clutter and are never useful here.
            if name.starts_with('.') && !prefix.starts_with('.') || name == "target" || name == "node_modules" {
                return None;
            }
            if !name.to_lowercase().starts_with(&prefix.to_lowercase()) {
                return None;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            let replacement = format!("@{dir}{name}{}", if is_dir { "/" } else { "" });
            Some(Suggestion { replacement, detail: if is_dir { po_i18n::t!("tui.complete.folder").into() } else { String::new() } })
        })
        .collect();
    out.sort_by(|a, b| a.replacement.cmp(&b.replacement));
    out.truncate(MAX_PATHS);
    out
}

/// Replaces the current word with the chosen suggestion. Returns the text and the new cursor
/// position (in characters).
pub fn apply(text: &str, cursor: usize, suggestion: &Suggestion) -> (String, usize) {
    let (start, _) = current_word(text, cursor);
    let chars: Vec<char> = text.chars().collect();
    let before: String = chars[..start].iter().collect();
    let after: String = chars[cursor.min(chars.len())..].iter().collect();
    // After a command or a file we add a space; not after a folder, where typing goes on
    // descending.
    let sep = if suggestion.replacement.ends_with('/') { "" } else { " " };
    let new_text = format!("{before}{}{sep}{after}", suggestion.replacement);
    let new_cursor = before.chars().count() + suggestion.replacement.chars().count() + sep.len();
    (new_text, new_cursor)
}

/// Commands and descriptions extracted from a help text shaped like "`  /name [args]   description`".
pub fn commands_from_help(help: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in help.lines() {
        let line = line.trim_start();
        if !line.starts_with('/') {
            continue;
        }
        let name: String = line.chars().take_while(|c| !c.is_whitespace()).collect();
        let rest = line[name.len()..].trim();
        // The description follows the syntax after at least two spaces.
        let detail = rest.split("  ").map(str::trim).filter(|s| !s.is_empty()).last().unwrap_or("").to_owned();
        if !out.iter().any(|(n, _)| *n == name) {
            out.push((name, detail));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds() -> Vec<(String, String)> {
        commands_from_help("Session\n  /think [none|low]  thinking level\n  /resume   x\n  /mcp add <name>   connects\n  /mcp   servers")
    }

    #[test]
    fn help_yields_commands_without_duplicates() {
        let c = cmds();
        let names: Vec<_> = c.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["/think", "/resume", "/mcp"]);
        assert_eq!(c[0].1, "thinking level");
    }

    #[test]
    fn a_command_completes_from_its_prefix() {
        let s = suggestions("/re", 3, &cmds(), None);
        assert_eq!(s.len(), 1);
        let (text, cursor) = apply("/re", 3, &s[0]);
        assert_eq!(text, "/resume ");
        assert_eq!(cursor, 8);
        assert!(suggestions("/think low", 10, &cmds(), None).is_empty(), "nothing once the argument has started");
    }

    #[test]
    fn a_path_completes_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "").unwrap();
        std::fs::write(dir.path().join("README.md"), "").unwrap();
        let s = suggestions("read @sr", 8, &[], Some(dir.path()));
        assert_eq!(s[0].replacement, "@src/");
        let (text, cursor) = apply("read @sr", 8, &s[0]);
        assert_eq!(text, "read @src/");
        let s = suggestions(&text, cursor, &[], Some(dir.path()));
        assert_eq!(s[0].replacement, "@src/main.rs");
        assert!(suggestions("@../", 4, &[], Some(dir.path())).is_empty(), "never above");
    }
}
