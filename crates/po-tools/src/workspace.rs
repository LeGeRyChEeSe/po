//! Confining paths to the workspace.
//!
//! An agent that runs tools writes to the user's disk. The rule is simple and non-negotiable:
//! every path supplied by the model is resolved, then checked to be under the workspace root.
//! A path that escapes it is denied, not corrected.
//!
//! The tricky part is that resolution must work on a path that **does not exist yet** — we
//! often write a new file. `canonicalize` would fail; so we normalize the components
//! ourselves, which neutralizes `..` before any disk access.

use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PathError {
    #[error("path {0} escapes the workspace")]
    Escapes(String),
    #[error("absolute path refused: {0}")]
    Absolute(String),
    #[error("empty path")]
    Empty,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    /// Folders readable in addition to the root, never writable: the user's memory, whose notes
    /// are quoted in the system prompt.
    read_roots: Vec<PathBuf>,
    /// Folders open for writing too, reached by absolute path: Po's own source under Po Labs.
    write_roots: Vec<PathBuf>,
}

impl Workspace {
    /// Creates a workspace. The root is normalized if it exists, so that the prefix comparison
    /// is not defeated by a symbolic link or a `\\?\` path on Windows.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = canonical(root);
        Self { root, read_roots: Vec::new(), write_roots: Vec::new() }
    }

    /// Opens a folder outside the workspace for reading and writing, by absolute path.
    pub fn allow_write(&mut self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        let dir = canonical(dir);
        if !self.write_roots.contains(&dir) {
            self.write_roots.push(dir);
        }
    }

    /// Closes a folder opened with [`allow_write`](Self::allow_write).
    pub fn forbid_write(&mut self, dir: &Path) {
        let dir = canonical(dir.to_path_buf());
        self.write_roots.retain(|d| *d != dir);
    }

    /// Allows reading (only) under a folder outside the workspace.
    pub fn allow_read(&mut self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        self.read_roots.push(canonical(dir));
    }

    /// Like [`resolve`](Self::resolve), but also accepts an absolute path located under a folder
    /// opened for reading. `..` is neutralized the same way there.
    pub fn resolve_for_read(&self, candidate: &str) -> Result<PathBuf, PathError> {
        let owned = simplified(PathBuf::from(candidate.trim()));
        let path = owned.as_path();
        if path.is_absolute() || has_windows_prefix(path) {
            for root in &self.read_roots {
                if let Ok(relative) = path.strip_prefix(root) {
                    let inner = Workspace { root: root.clone(), read_roots: Vec::new(), write_roots: Vec::new() };
                    return inner.resolve_relative(relative);
                }
            }
        }
        self.resolve(candidate)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves a path relative to the workspace, refusing any escape.
    ///
    /// Absolute paths are refused outright: a model that produces one has either hallucinated
    /// a tree or tried to reach the system. In both cases, refusing is safer than a silent
    /// rewrite.
    pub fn resolve(&self, candidate: &str) -> Result<PathBuf, PathError> {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            return Err(PathError::Empty);
        }

        let owned = simplified(PathBuf::from(candidate));
        let path = owned.as_path();
        if path.is_absolute() || has_windows_prefix(path) {
            // An absolute path already under the root is accepted: this is the case of a tool that
            // returns its results as absolute paths and whose output is fed back as is.
            if let Ok(relative) = path.strip_prefix(&self.root) {
                return self.resolve_relative(relative);
            }
            for root in &self.write_roots {
                if let Ok(relative) = path.strip_prefix(root) {
                    let inner = Workspace { root: root.clone(), read_roots: Vec::new(), write_roots: Vec::new() };
                    return inner.resolve_relative(relative);
                }
            }
            return Err(PathError::Absolute(candidate.to_owned()));
        }
        self.resolve_relative(path)
    }

    fn resolve_relative(&self, path: &Path) -> Result<PathBuf, PathError> {
        let mut resolved = self.root.clone();
        let mut depth = 0i32;

        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    resolved.push(part);
                    depth += 1;
                }
                Component::ParentDir => {
                    // Going above the root is an escape, even if the following components went back
                    // down: we refuse without trying to compensate.
                    depth -= 1;
                    if depth < 0 {
                        return Err(PathError::Escapes(path.display().to_string()));
                    }
                    resolved.pop();
                }
                Component::CurDir => {}
                // A leading `/`, or a drive prefix in the middle of the path: abnormal.
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PathError::Absolute(path.display().to_string()));
                }
            }
        }

        // Belt and braces: the normalization above is enough, but if the target exists and
        // points outside the root through a symbolic link, the final check catches it.
        if let Ok(real) = resolved.canonicalize()
            && !simplified(real).starts_with(&self.root)
        {
            return Err(PathError::Escapes(path.display().to_string()));
        }
        Ok(resolved)
    }

    /// Displayable path, relative to the root: this is what the user and the model see.
    pub fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/")
    }
}

/// Resolves links, but keeps the usual form of the path: on Windows, `canonicalize` returns
/// `\\?\C:\…`, which no path written by a model or a user starts with. Observed: Po Labs could not
/// read its own source, every absolute path being refused.
pub fn canonical(path: PathBuf) -> PathBuf {
    simplified(path.canonicalize().unwrap_or(path))
}

/// `\\?\C:\dir` → `C:\dir`; `\\?\UNC\server\share` → `\\server\share`; anything else unchanged.
pub fn simplified(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

fn has_windows_prefix(path: &Path) -> bool {
    matches!(path.components().next(), Some(Component::Prefix(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absolute_path_written_plainly_reaches_an_opened_folder() {
        let (dir, mut ws) = workspace();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("a.rs"), "x").unwrap();
        ws.allow_write(other.path());
        // The form a model writes: neither canonical nor with a `\\?\` prefix, and with `/`.
        let plain = simplified(other.path().canonicalize().unwrap()).join("a.rs");
        let slashed = plain.display().to_string().replace('\\', "/");
        for candidate in [plain.display().to_string(), slashed] {
            assert!(ws.resolve(&candidate).is_ok(), "{candidate}");
            assert!(ws.resolve_for_read(&candidate).is_ok(), "{candidate}");
        }
        let root_file = simplified(dir.path().canonicalize().unwrap()).join("b.rs");
        assert!(ws.resolve(&root_file.display().to_string()).is_ok(), "the workspace root too");
    }

    #[test]
    fn verbatim_prefixes_are_dropped() {
        assert_eq!(simplified(PathBuf::from(r"\\?\C:\a\b")), PathBuf::from(r"C:\a\b"));
        assert_eq!(simplified(PathBuf::from(r"\\?\UNC\srv\share")), PathBuf::from(r"\\srv\share"));
        assert_eq!(simplified(PathBuf::from("/home/a")), PathBuf::from("/home/a"));
    }

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        (dir, ws)
    }

    #[test]
    fn a_simple_relative_path_is_accepted() {
        let (_dir, ws) = workspace();
        let resolved = ws.resolve("src/main.rs").unwrap();
        assert!(resolved.starts_with(ws.root()));
        assert!(resolved.ends_with("main.rs"));
    }

    #[test]
    fn going_up_out_of_the_root_is_refused() {
        let (_dir, ws) = workspace();
        for attempt in ["../secret", "../../etc/passwd", "src/../../outside"] {
            assert!(
                matches!(ws.resolve(attempt), Err(PathError::Escapes(_))),
                "{attempt} should be refused"
            );
        }
    }

    #[test]
    fn a_compensated_climb_is_still_refused() {
        let (_dir, ws) = workspace();
        // `../root/file` would end up in the right place by chance; we refuse anyway, because
        // accepting would force us to reason about the root's name.
        assert!(ws.resolve("../other/../file").is_err());
    }

    #[test]
    fn an_internal_climb_is_accepted() {
        let (_dir, ws) = workspace();
        let resolved = ws.resolve("src/module/../main.rs").unwrap();
        assert!(resolved.ends_with("src/main.rs") || resolved.ends_with("src\\main.rs"));
    }

    #[test]
    fn the_current_dir_component_is_neutral() {
        let (_dir, ws) = workspace();
        assert_eq!(ws.resolve("./src/./main.rs").unwrap(), ws.resolve("src/main.rs").unwrap());
    }

    #[test]
    fn an_absolute_path_outside_the_root_is_refused() {
        let (_dir, ws) = workspace();
        let absolute = if cfg!(windows) { "C:\\Windows\\System32" } else { "/etc/passwd" };
        assert!(matches!(ws.resolve(absolute), Err(PathError::Absolute(_))));
    }

    #[test]
    fn an_absolute_path_under_the_root_is_accepted() {
        let (_dir, ws) = workspace();
        let absolute = ws.root().join("src").join("main.rs");
        let resolved = ws.resolve(&absolute.display().to_string()).unwrap();
        assert_eq!(resolved, absolute);
    }

    #[test]
    fn an_empty_path_is_refused() {
        let (_dir, ws) = workspace();
        assert_eq!(ws.resolve(""), Err(PathError::Empty));
        assert_eq!(ws.resolve("   "), Err(PathError::Empty));
    }

    #[test]
    fn a_nonexistent_file_still_resolves() {
        let (_dir, ws) = workspace();
        // Essential: we write new files, `canonicalize` would fail here.
        assert!(ws.resolve("new/folder/file.txt").is_ok());
    }

    #[test]
    fn the_displayed_path_is_relative_with_forward_slashes() {
        let (_dir, ws) = workspace();
        let resolved = ws.resolve("src/main.rs").unwrap();
        assert_eq!(ws.display_path(&resolved), "src/main.rs");
    }
}
