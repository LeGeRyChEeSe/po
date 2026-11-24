//! The workspace of Po Labs: a copy of Po's source, in a git repository of its own, where each
//! change Po Labs makes is a commit on the `labs` branch. A change on trial can then be kept or
//! undone exactly, and the kept ones become contributions. The user's own checkout of Po is never
//! touched, and only what is committed there is copied (never files kept out of the repository).
//!
//! Everything goes through gix, in Rust: no `git` program is needed, and none of the user's git
//! configuration or hooks runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::object::tree::EntryKind;

const BRANCH: &str = "refs/heads/labs";
/// The commit of the user's checkout the workspace was copied from, and its tree.
const UPSTREAM: &str = "po-upstream";

/// Where the workspace lives: next to the notebook.
pub fn workspace_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("po").join("labs").join("workspace"))
}

/// Files of a tree: path → (blob, kind).
type Files = BTreeMap<String, (ObjectId, EntryKind)>;

fn files(repo: &gix::Repository, tree: ObjectId) -> Result<Files> {
    fn walk(repo: &gix::Repository, tree: ObjectId, prefix: &str, out: &mut Files) -> Result<()> {
        let tree = repo.find_tree(tree)?;
        for entry in tree.decode()?.entries.iter() {
            let path = format!("{prefix}{}", entry.filename.to_str_lossy());
            if entry.mode.is_tree() {
                walk(repo, entry.oid.to_owned(), &format!("{path}/"), out)?;
            } else if entry.mode.is_blob() {
                out.insert(path, (entry.oid.to_owned(), entry.mode.kind()));
            }
        }
        Ok(())
    }
    let mut out = Files::new();
    walk(repo, tree, "", &mut out)?;
    Ok(out)
}

/// The Po Labs workspace, as Po Labs: the reflog of a moved branch needs a committer, which a
/// machine without a git identity (a fresh Linux, a CI runner) does not have.
fn open(dir: &Path) -> Result<gix::Repository> {
    let mut repo = gix::open(dir)?;
    let mut config = repo.config_snapshot_mut();
    config.set_value(&gix::config::tree::Committer::NAME, "Po Labs")?;
    config.set_value(&gix::config::tree::Committer::EMAIL, "labs@po.invalid")?;
    config.commit()?;
    Ok(repo)
}

fn tree_of(repo: &gix::Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repo.find_commit(commit)?.tree_id()?.detach())
}

fn branch_head(repo: &gix::Repository) -> Result<ObjectId> {
    Ok(repo.find_reference(BRANCH)?.id().detach())
}

fn commit_on_branch(repo: &gix::Repository, message: &str, tree: ObjectId, parents: Vec<ObjectId>) -> Result<ObjectId> {
    let signature =
        gix::actor::Signature { name: "Po Labs".into(), email: "labs@po.invalid".into(), time: gix::date::Time::now_local_or_utc() };
    let mut buf = gix::date::parse::TimeBuf::default();
    let who = signature.to_ref(&mut buf);
    let id = repo.commit_as(who, who, BRANCH, message, tree, parents)?.detach();
    write_index(repo, tree)?;
    Ok(id)
}

/// The index matching `tree`: `git status` in the workspace then shows what Po Labs did not
/// commit yet, instead of every file as deleted.
fn write_index(repo: &gix::Repository, tree: ObjectId) -> Result<()> {
    let mut index = repo.index_from_tree(&tree)?;
    index.write(gix::index::write::Options::default())?;
    Ok(())
}

/// Writes the files of `to` that differ from `from` (and removes those gone) in the work tree.
fn rewrite(repo: &gix::Repository, dir: &Path, from: &Files, to: &Files) -> Result<()> {
    for (path, (blob, _)) in to {
        if from.get(path).map(|(b, _)| b) != Some(blob) || !dir.join(path).is_file() {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, &repo.find_blob(*blob)?.data).with_context(|| path.clone())?;
        }
    }
    for path in from.keys().filter(|p| !to.contains_key(*p)) {
        let _ = std::fs::remove_file(dir.join(path));
    }
    Ok(())
}

/// The work tree as a tree: every file not ignored by the repository's `.gitignore`.
fn snapshot(repo: &gix::Repository, known: &Files) -> Result<ObjectId> {
    let dir = repo.workdir().context("the workspace has no work tree")?.to_owned();
    let options =
        repo.dirwalk_options()?.emit_tracked(true).emit_untracked(gix::dir::walk::EmissionMode::Matching);
    let mut editor = repo.edit_tree(ObjectId::empty_tree(repo.object_hash()))?;
    for item in repo.dirwalk_iter(repo.index_or_empty()?, None::<&str>, Default::default(), options)? {
        let entry = item?.entry;
        if entry.disk_kind != Some(gix::dir::entry::Kind::File) {
            continue;
        }
        let path = entry.rela_path.to_str_lossy().into_owned();
        let blob = repo.write_blob(std::fs::read(dir.join(&path))?)?.detach();
        let kind = known.get(&path).map_or(EntryKind::Blob, |(_, kind)| *kind);
        editor.upsert(path.as_str(), kind, blob)?;
    }
    Ok(editor.write()?.detach())
}

fn upstream(repo: &gix::Repository) -> Option<(String, ObjectId)> {
    let text = std::fs::read_to_string(repo.git_dir().join(UPSTREAM)).ok()?;
    let (commit, tree) = text.trim().split_once(' ')?;
    Some((commit.to_owned(), ObjectId::from_hex(tree.as_bytes()).ok()?))
}

fn set_upstream(repo: &gix::Repository, commit: &str, tree: ObjectId) -> Result<()> {
    Ok(std::fs::write(repo.git_dir().join(UPSTREAM), format!("{commit} {tree}\n"))?)
}

/// Copies a tree of the user's checkout into the workspace's objects; the same content gives the
/// same tree id.
fn copy_tree(source: &gix::Repository, repo: &gix::Repository, tree: ObjectId) -> Result<ObjectId> {
    let mut editor = repo.edit_tree(ObjectId::empty_tree(repo.object_hash()))?;
    for (path, (blob, kind)) in files(source, tree)? {
        let id = repo.write_blob(&source.find_blob(blob)?.data)?.detach();
        editor.upsert(path.as_str(), kind, id)?;
    }
    Ok(editor.write()?.detach())
}

/// Makes the workspace ready from the user's checkout of Po at `source`: created on first use;
/// brought up to the checkout's last commit as long as it holds no change of its own. Returns
/// its folder.
pub fn prepare(source: &Path, dir: &Path) -> Result<PathBuf> {
    let checkout = gix::open(source).with_context(|| format!("{} is not a git repository", source.display()))?;
    let head = checkout.head_commit().context("Po's checkout has no commit")?;
    let head_id = head.id.to_string();
    let head_tree = head.tree_id()?.detach();
    if !dir.join(".git").is_dir() {
        std::fs::create_dir_all(dir)?;
        let repo = gix::init(dir)?;
        std::fs::write(repo.git_dir().join("HEAD"), format!("ref: {BRANCH}\n"))?;
        let tree = copy_tree(&checkout, &repo, head_tree)?;
        commit_on_branch(&repo, &format!("Po {head_id}"), tree, Vec::new())?;
        rewrite(&repo, dir, &Files::new(), &files(&repo, tree)?)?;
        set_upstream(&repo, &head_id, tree)?;
        return Ok(dir.to_owned());
    }
    let repo = open(dir)?;
    let current = branch_head(&repo)?;
    let current_tree = tree_of(&repo, current)?;
    match upstream(&repo) {
        // Up to date, or changes of its own to keep: nothing to bring.
        Some((commit, _)) if commit == head_id => {}
        Some((_, tree)) if tree != current_tree => {}
        _ => {
            let known = files(&repo, current_tree)?;
            // Edits not committed yet (a session interrupted midway) stay where they are.
            if snapshot(&repo, &known)? == current_tree {
                let tree = copy_tree(&checkout, &repo, head_tree)?;
                if tree != current_tree {
                    commit_on_branch(&repo, &format!("Po {head_id}"), tree, vec![current])?;
                    rewrite(&repo, dir, &known, &files(&repo, tree)?)?;
                }
                set_upstream(&repo, &head_id, tree)?;
            }
        }
    }
    Ok(dir.to_owned())
}

/// Makes a repository of a folder as it is.
#[cfg(test)]
pub fn adopt(dir: &Path) -> Result<String> {
    let repo = gix::init(dir)?;
    std::fs::write(repo.git_dir().join("HEAD"), format!("ref: {BRANCH}\n"))?;
    let tree = snapshot(&repo, &Files::new())?;
    let id = commit_on_branch(&repo, "Po", tree, Vec::new())?;
    set_upstream(&repo, &id.to_string(), tree)?;
    Ok(id.to_string())
}

/// The last commit of the `labs` branch.
pub fn head(dir: &Path) -> Result<String> {
    Ok(branch_head(&open(dir)?)?.to_string())
}

/// Commits what changed in the work tree since the last commit; `None` if nothing did.
pub fn commit(dir: &Path, message: &str) -> Result<Option<String>> {
    let repo = open(dir)?;
    let parent = branch_head(&repo)?;
    let parent_tree = tree_of(&repo, parent)?;
    let tree = snapshot(&repo, &files(&repo, parent_tree)?)?;
    if tree == parent_tree {
        return Ok(None);
    }
    Ok(Some(commit_on_branch(&repo, message, tree, vec![parent])?.to_string()))
}

/// The files changed in the work tree since the last commit.
pub fn pending(dir: &Path) -> Result<Vec<String>> {
    let repo = open(dir)?;
    let known = files(&repo, tree_of(&repo, branch_head(&repo)?)?)?;
    let now = files(&repo, snapshot(&repo, &known)?)?;
    let mut changed: Vec<String> = now.iter().filter(|(p, e)| known.get(*p) != Some(e)).map(|(p, _)| p.clone()).collect();
    changed.extend(known.keys().filter(|p| !now.contains_key(*p)).cloned());
    changed.sort();
    Ok(changed)
}

/// Only the catalog's extensions differ between `files`: nothing to build, Po reads them.
pub fn catalog_only(files: &[String]) -> bool {
    !files.is_empty() && files.iter().all(|f| f.starts_with("catalog/"))
}

/// Brings the work tree and the branch back to the commit `to`: the changes made since are undone.
pub fn restore(dir: &Path, to: &str) -> Result<()> {
    let repo = open(dir)?;
    let target = ObjectId::from_hex(to.as_bytes()).context("not a commit id")?;
    let current_tree = tree_of(&repo, branch_head(&repo)?)?;
    let known = files(&repo, current_tree)?;
    // What is on disk, committed or not, is what has to go.
    let on_disk = files(&repo, snapshot(&repo, &known)?)?;
    let wanted = files(&repo, tree_of(&repo, target)?)?;
    rewrite(&repo, dir, &on_disk, &wanted)?;
    repo.reference(BRANCH, target, gix::refs::transaction::PreviousValue::Any, "po labs: undo")?;
    write_index(&repo, tree_of(&repo, target)?)
}

/// Replays the change `base`..`head` on top of commit `onto` (a three-way merge), as a new commit
/// that no branch points to yet; fails on a conflict. The work tree is not touched.
pub fn replay(dir: &Path, base: &str, head: &str, onto: &str, message: &str) -> Result<String> {
    let repo = open(dir)?;
    let id = |hex: &str| ObjectId::from_hex(hex.as_bytes()).context("not a commit id");
    let (base, head, onto) = (id(base)?, id(head)?, id(onto)?);
    let labels = gix::merge::blob::builtin_driver::text::Labels { ancestor: None, current: None, other: None };
    let mut outcome = repo.merge_trees(tree_of(&repo, base)?, tree_of(&repo, onto)?, tree_of(&repo, head)?, labels, repo.tree_merge_options()?)?;
    if outcome.has_unresolved_conflicts(gix::merge::tree::TreatAsUnresolved::default()) {
        bail!("conflict replaying “{message}”");
    }
    let tree = outcome.tree.write()?.detach();
    let signature =
        gix::actor::Signature { name: "Po Labs".into(), email: "labs@po.invalid".into(), time: gix::date::Time::now_local_or_utc() };
    let mut buf = gix::date::parse::TimeBuf::default();
    let who = signature.to_ref(&mut buf);
    // No reference moves: the notebook keeps the commit, and the branch points to it once restored.
    Ok(repo.new_commit_as(who, who, message, tree, [onto])?.id.to_string())
}

/// The change between two commits as text for a model: a small diff per file, up to
/// `max_lines` lines in all.
pub fn diff_text(dir: &Path, from: &str, to: &str, max_lines: usize) -> Result<String> {
    let repo = open(dir)?;
    let id = |hex: &str| ObjectId::from_hex(hex.as_bytes()).context("not a commit id");
    let a = files(&repo, tree_of(&repo, id(from)?)?)?;
    let b = files(&repo, tree_of(&repo, id(to)?)?)?;
    let text = |files: &Files, path: &str| -> String {
        files.get(path).and_then(|(blob, _)| repo.find_blob(*blob).ok()).map(|b| String::from_utf8_lossy(&b.data).into_owned()).unwrap_or_default()
    };
    let mut out = String::new();
    let mut left = max_lines;
    for path in changed_files(dir, from, to)? {
        if left == 0 {
            out.push_str("[… more files cut]\n");
            break;
        }
        let diff = po_tools::diff::line_diff(&text(&a, &path), &text(&b, &path), left).unwrap_or_default();
        left = left.saturating_sub(diff.lines().count() + 1);
        out.push_str(&format!("--- {path}\n{diff}\n"));
    }
    Ok(out)
}

/// Files that differ between two commits.
pub fn changed_files(dir: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let repo = open(dir)?;
    let id = |hex: &str| ObjectId::from_hex(hex.as_bytes()).context("not a commit id");
    let a = files(&repo, tree_of(&repo, id(from)?)?)?;
    let b = files(&repo, tree_of(&repo, id(to)?)?)?;
    let mut changed: Vec<String> = b.iter().filter(|(p, e)| a.get(*p) != Some(e)).map(|(p, _)| p.clone()).collect();
    changed.extend(a.keys().filter(|p| !b.contains_key(*p)).cloned());
    changed.sort();
    if changed.is_empty() && from != to {
        bail!("no difference between {from} and {to}");
    }
    Ok(changed)
}

/// A file a change touches: its blob before it (`None` when new) and its content after
/// (`None` when removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<Vec<u8>>,
    pub executable: bool,
}

/// The files that differ between two commits, with what it takes to send the change elsewhere.
pub fn file_changes(dir: &Path, from: &str, to: &str) -> Result<Vec<FileChange>> {
    let repo = open(dir)?;
    let id = |hex: &str| ObjectId::from_hex(hex.as_bytes()).context("not a commit id");
    let a = files(&repo, tree_of(&repo, id(from)?)?)?;
    let b = files(&repo, tree_of(&repo, id(to)?)?)?;
    let mut out = Vec::new();
    for path in changed_files(dir, from, to)? {
        let after = match b.get(&path) {
            Some((blob, _)) => Some(repo.find_blob(*blob)?.data.clone()),
            None => None,
        };
        out.push(FileChange {
            before: a.get(&path).map(|(blob, _)| blob.to_string()),
            executable: b.get(&path).is_some_and(|(_, kind)| *kind == EntryKind::BlobExecutable),
            after,
            path,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("view.rs"), "green\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "/target\n").unwrap();
        adopt(dir.path()).unwrap();
        dir
    }

    #[test]
    fn a_change_is_committed_then_undone_exactly() {
        let source = checkout();
        let ws = tempfile::tempdir().unwrap();
        let dir = ws.path().join("workspace");
        prepare(source.path(), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src").join("view.rs")).unwrap(), "green\n");
        let base = head(&dir).unwrap();
        assert_eq!(commit(&dir, "nothing").unwrap(), None, "no change, no commit");

        std::fs::write(dir.join("src").join("view.rs"), "orange\n").unwrap();
        std::fs::write(dir.join("src").join("new.rs"), "new\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target").join("po.exe"), "binary").unwrap();
        let change = commit(&dir, "orange").unwrap().expect("a commit");
        assert_eq!(changed_files(&dir, &base, &change).unwrap(), vec!["src/new.rs", "src/view.rs"], "the build output is ignored");

        restore(&dir, &base).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src").join("view.rs")).unwrap(), "green\n");
        assert!(!dir.join("src").join("new.rs").exists());
        assert_eq!(head(&dir).unwrap(), base);
        // A file deleted in the work tree, not committed yet, is seen as gone.
        std::fs::remove_file(dir.join("src").join("view.rs")).unwrap();
        let gone = commit(&dir, "gone").unwrap().expect("a commit");
        assert_eq!(changed_files(&dir, &base, &gone).unwrap(), vec!["src/view.rs"]);
        assert_eq!(std::fs::read_to_string(source.path().join("src").join("view.rs")).unwrap(), "green\n", "the checkout is untouched");
    }

    #[test]
    fn a_later_change_is_replayed_on_top_of_an_earlier_one_changed_since() {
        let source = checkout();
        let ws = tempfile::tempdir().unwrap();
        let dir = ws.path().join("workspace");
        prepare(source.path(), &dir).unwrap();
        std::fs::write(dir.join("src").join("view.rs"), "green
line two
line three
line four
").unwrap();
        let base = commit(&dir, "four lines").unwrap().unwrap();
        // Contribution 1 changes the first line, contribution 2 the last.
        std::fs::write(dir.join("src").join("view.rs"), "orange
line two
line three
line four
").unwrap();
        let first = commit(&dir, "orange").unwrap().unwrap();
        std::fs::write(dir.join("src").join("view.rs"), "orange
line two
line three
line 4
").unwrap();
        let second = commit(&dir, "4").unwrap().unwrap();
        // Contribution 1 is changed afterwards: contribution 2 is replayed on top of it.
        restore(&dir, &first).unwrap();
        std::fs::write(dir.join("src").join("view.rs"), "light orange
line two
line three
line four
").unwrap();
        let edited = commit(&dir, "light orange").unwrap().unwrap();
        let replayed = replay(&dir, &first, &second, &edited, "4").unwrap();
        restore(&dir, &replayed).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src").join("view.rs")).unwrap(), "light orange
line two
line three
line 4
");
        assert_eq!(changed_files(&dir, &edited, &replayed).unwrap(), vec!["src/view.rs"]);
        // A contribution changed a second time: replayed again.
        let again = replay(&dir, &first, &second, &first, "4").unwrap();
        assert_eq!(changed_files(&dir, &first, &again).unwrap(), vec!["src/view.rs"]);
        // The same line changed both ways is a conflict, reported.
        restore(&dir, &first).unwrap();
        std::fs::write(dir.join("src").join("view.rs"), "orange
line two
line three
line IV
").unwrap();
        let clash = commit(&dir, "IV").unwrap().unwrap();
        assert!(replay(&dir, &first, &second, &clash, "4").is_err());
        let _ = base;
    }

    #[test]
    fn the_workspace_follows_the_checkout_until_it_holds_changes_of_its_own() {
        let source = checkout();
        let ws = tempfile::tempdir().unwrap();
        let dir = ws.path().join("workspace");
        prepare(source.path(), &dir).unwrap();
        // The user commits in their checkout: the workspace takes it.
        std::fs::write(source.path().join("src").join("view.rs"), "blue\n").unwrap();
        let repo = gix::open(source.path()).unwrap();
        let tree = snapshot(&repo, &Files::new()).unwrap();
        commit_on_branch(&repo, "blue", tree, vec![branch_head(&repo).unwrap()]).unwrap();
        prepare(source.path(), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src").join("view.rs")).unwrap(), "blue\n");
        // A change of Po Labs is kept when the checkout moves again.
        std::fs::write(dir.join("src").join("view.rs"), "orange\n").unwrap();
        commit(&dir, "orange").unwrap();
        std::fs::write(source.path().join("src").join("view.rs"), "red\n").unwrap();
        let tree = snapshot(&repo, &Files::new()).unwrap();
        commit_on_branch(&repo, "red", tree, vec![branch_head(&repo).unwrap()]).unwrap();
        prepare(source.path(), &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("src").join("view.rs")).unwrap(), "orange\n");
    }
}
