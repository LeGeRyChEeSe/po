//! Project instructions: `AGENTS.md`, `CLAUDE.md`, `PO.md`, and the user's memory.
//!
//! `AGENTS.md` has become the common format of agent harnesses; `CLAUDE.md` is the one many
//! repositories already have. Reading them spares the user from repeating their guidelines,
//! and Po from inventing yet another format. Read once, at startup: the system prompt must stay
//! identical from one turn to the next so that the server reuses its cache.

use std::path::{Path, PathBuf};

const NAMES: &[&str] = &["AGENTS.md", "CLAUDE.md", "PO.md"];
/// Beyond this, the instructions would cost more context than they bring.
const MAX_TOTAL: usize = 40 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instructions {
    pub files: Vec<PathBuf>,
    pub text: String,
    pub truncated: bool,
}

/// Files kept, from the most general to the closest: the user, then the repository root,
/// then each folder down to the workspace. The closest speaks last, so it prevails in case of
/// contradiction.
pub fn discover(workspace: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for candidate in [home.join(".po").join("AGENTS.md"), home.join(".claude").join("CLAUDE.md")] {
            if candidate.is_file() {
                files.push(candidate);
            }
        }
        if let Some(memory) = memory_index(&home, workspace) {
            files.push(memory);
        }
    }
    // Walk up to the repository root (the folder containing `.git`) without going past it:
    // an AGENTS.md found above would belong to another project.
    let mut chain = Vec::new();
    let mut dir = Some(workspace);
    while let Some(current) = dir {
        chain.push(current.to_path_buf());
        if current.join(".git").exists() {
            break;
        }
        dir = current.parent();
    }
    if !chain.last().is_some_and(|d| d.join(".git").exists()) {
        // No repository: only the workspace counts.
        chain.truncate(1);
    }
    chain.reverse();
    for dir in chain {
        for name in NAMES {
            for candidate in [dir.join(name), dir.join(".claude").join(name)] {
                if candidate.is_file() && !files.contains(&candidate) {
                    files.push(candidate);
                }
            }
        }
    }
    files
}

/// Index of the memory that Claude Code keeps for this folder
/// (`~/.claude/projects/<folder>/memory/MEMORY.md`). The user already relies on it: reading
/// what it learned about their machine and projects avoids rediscovering everything. Observed:
/// launched in the home folder, Claude Code knew where to find Strata thanks to it, while Po
/// searched the disk.
/// Memory folder of this working folder, if it exists.
pub fn memory_dir(workspace: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    memory_index(&home, workspace).and_then(|index| index.parent().map(Path::to_path_buf))
}

fn memory_index(home: &Path, workspace: &Path) -> Option<PathBuf> {
    let index = home.join(".claude").join("projects").join(memory_slug(workspace)).join("memory").join("MEMORY.md");
    index.is_file().then_some(index)
}

/// Name of the project folder in Claude Code: every character other than letters and digits
/// becomes a dash (`C:\Users\alice` → `C--Users-alice`).
fn memory_slug(workspace: &Path) -> String {
    workspace.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

pub fn load(workspace: &Path) -> Instructions {
    let files = discover(workspace);
    let mut text = String::new();
    let mut truncated = false;
    let mut kept = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else { continue };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        let is_memory = file.file_name().is_some_and(|n| n == "MEMORY.md");
        let block = if is_memory {
            let dir = file.parent().map(|d| d.display().to_string()).unwrap_or_default();
            format!(
                "\n\n## User memory (notes from their previous sessions)\nEach line points to a detailed note in \
                 {dir}: read it when the topic comes up.\n{content}"
            )
        } else {
            format!("\n\n## Instructions — {}\n{content}", file.display())
        };
        if text.len() + block.len() > MAX_TOTAL {
            truncated = true;
            break;
        }
        text.push_str(&block);
        kept.push(file);
    }
    Instructions { files: kept, text, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closest_speaks_last_and_discovery_stops_at_the_repository() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("crates").join("x");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "outside the repository").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "root").unwrap();
        std::fs::write(nested.join("CLAUDE.md"), "closest").unwrap();

        let files = discover(&nested);
        let local: Vec<_> = files.iter().filter(|f| f.starts_with(root.path())).collect();
        assert_eq!(local, vec![&repo.join("AGENTS.md"), &nested.join("CLAUDE.md")]);
        let text = load(&nested).text;
        assert!(text.find("root").unwrap() < text.find("closest").unwrap());
        assert!(!text.contains("outside the repository"));
    }

    #[test]
    fn folder_name_follows_claude_code() {
        assert_eq!(memory_slug(Path::new(r"C:\Users\alice")), "C--Users-alice");
        assert_eq!(memory_slug(Path::new(r"D:\Documents\Projets\Po")), "D--Documents-Projets-Po");
        assert_eq!(memory_slug(Path::new("/home/k/po")), "-home-k-po");
    }

    #[test]
    fn memory_is_attached_with_its_folder() {
        let home = tempfile::tempdir().unwrap();
        let ws = home.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let dir = home.path().join(".claude").join("projects").join(memory_slug(&ws)).join("memory");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("MEMORY.md"), "- [Strata](strata.md) — server on 8080").unwrap();
        assert_eq!(memory_index(home.path(), &ws), Some(dir.join("MEMORY.md")));
    }

    #[test]
    fn without_a_repository_only_the_workspace_counts() {
        let root = tempfile::tempdir().unwrap();
        let ws = root.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "parent").unwrap();
        std::fs::write(ws.join("AGENTS.md"), "here").unwrap();
        let local: Vec<_> = discover(&ws).into_iter().filter(|f| f.starts_with(root.path())).collect();
        assert_eq!(local, vec![ws.join("AGENTS.md")]);
    }
}
