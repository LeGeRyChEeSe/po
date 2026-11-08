//! The community catalog: extensions made by Po's users - skills, subagents, a status line -
//! each in a folder `catalog/<name>/` of Po's repository, with its `extension.toml`. They are
//! installed without updating Po, and voted for with a 👍 on their GitHub issue: GitHub keeps one
//! per account. No server: Po reads the repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use po_i18n::t;

use crate::github::{Api, Config};

/// The categories of the catalog (those of contributions).
pub const CATEGORIES: &[&str] = &["appearance", "commands", "tools", "models", "sessions", "performance", "accessibility", "other"];

/// `catalog/<name>/extension.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Short English title.
    pub title: String,
    /// What it does, in English.
    pub description: String,
    pub category: String,
    /// GitHub account of its author.
    #[serde(default)]
    pub author: String,
    /// Tried by Po's maintainers: set by them only, when they accept it.
    #[serde(default)]
    pub certified: bool,
    /// Its GitHub issue, where it is voted for: opened by the maintainers.
    #[serde(default)]
    pub issue: Option<u64>,
}

/// An extension of the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub name: String,
    pub manifest: Manifest,
    /// Its files, relative to its folder (`extension.toml` aside).
    pub files: Vec<String>,
    pub votes: u64,
}

/// What a file of an extension becomes once installed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    /// `skills/<skill>/…`: a skill, in Po's skills folder.
    Skill(String),
    /// `agents/<agent>.md`: a subagent, in Po's subagents folder.
    Agent(String),
    /// `statusline.json`: the status line.
    StatusLine,
    /// `strata/<preset>.json`: settings added to a Strata config (`po_local::strata::Preset`),
    /// chosen per model in `/model settings`.
    StrataPreset(String),
}

/// A name of extension, skill or subagent: lowercase letters, digits and dashes.
pub fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

fn part(file: &str) -> Option<Part> {
    if file.contains("..") || file.contains('\\') || file.contains(':') || file.starts_with('/') {
        return None;
    }
    if let Some(rest) = file.strip_prefix("skills/") {
        let (skill, inside) = rest.split_once('/')?;
        return (valid_name(skill) && !inside.is_empty() && !inside.ends_with('/')).then(|| Part::Skill(rest.to_owned()));
    }
    if let Some(rest) = file.strip_prefix("agents/") {
        let name = rest.strip_suffix(".md")?;
        return valid_name(name).then(|| Part::Agent(rest.to_owned()));
    }
    if let Some(rest) = file.strip_prefix("strata/") {
        let name = rest.strip_suffix(".json")?;
        return valid_name(name).then(|| Part::StrataPreset(rest.to_owned()));
    }
    (file == "statusline.json").then_some(Part::StatusLine)
}

/// Checks an extension before it is listed or installed: its name, its text, its category, files
/// Po knows how to install, and a status line Po accepts (running commands only once certified).
pub fn check(name: &str, manifest: &Manifest, files: &[String], read: impl Fn(&str) -> Option<Vec<u8>>) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("`{name}`: a name is made of lowercase letters, digits and dashes"));
    }
    if manifest.title.trim().is_empty() || manifest.description.trim().is_empty() {
        return Err(format!("`{name}`: a title and a description are needed"));
    }
    if !CATEGORIES.contains(&manifest.category.as_str()) {
        return Err(format!("`{name}`: unknown category `{}`", manifest.category));
    }
    if files.is_empty() {
        return Err(format!("`{name}`: no file"));
    }
    for file in files {
        match part(file) {
            None => return Err(format!("`{name}`: Po does not know what to do with `{file}`")),
            Some(Part::StatusLine) => {
                let text = read(file).ok_or_else(|| format!("`{name}`: `{file}` unreadable"))?;
                let segments: Vec<po_tui::statusline::Segment> = serde_json::from_slice(&text).map_err(|e| format!("`{name}`: `{file}`: {e}"))?;
                po_tui::statusline::validate(&segments).map_err(|e| format!("`{name}`: `{file}`: {e}"))?;
                if !manifest.certified && segments.iter().any(|s| s.id == "command") {
                    return Err(format!("`{name}`: a status line running commands must be certified"));
                }
            }
            Some(Part::StrataPreset(_)) => {
                let text = read(file).ok_or_else(|| format!("`{name}`: `{file}` unreadable"))?;
                serde_json::from_slice::<po_local::strata::Preset>(&text).map_err(|e| format!("`{name}`: `{file}`: {e}"))?;
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The catalog last read, for a few minutes: it is read again after a vote or once stale.
static CACHE: Mutex<Option<(Instant, String, Vec<Extension>)>> = Mutex::new(None);
const CACHE_TIME: Duration = Duration::from_secs(300);

fn forget_cache() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Reads the account if connected (more calls allowed an hour), else anonymously.
async fn api(config: &Config) -> Api {
    match crate::github::connected(config).await {
        Ok(Some((api, _))) => api,
        _ => Api::anonymous(config),
    }
}

/// The catalog as it is in Po's repository: certified first, then by votes. Also returns the
/// branch it was read from.
pub async fn list(config: &Config) -> Result<(String, Vec<Extension>), String> {
    if let Some((at, branch, list)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone()
        && at.elapsed() < CACHE_TIME
    {
        return Ok((branch, list));
    }
    let api = api(config).await;
    let err = |e: crate::github::Failure| e.to_string();
    let repo = api.get(&format!("/repos/{}", config.upstream)).await.map_err(err)?;
    let branch = repo["default_branch"].as_str().unwrap_or("main").to_owned();
    let tree = api.get(&format!("/repos/{}/git/trees/{branch}?recursive=1", config.upstream)).await.map_err(err)?;
    let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in tree["tree"].as_array().into_iter().flatten() {
        let (Some(path), Some("blob")) = (entry["path"].as_str(), entry["type"].as_str()) else { continue };
        let Some((name, file)) = path.strip_prefix("catalog/").and_then(|p| p.split_once('/')) else { continue };
        folders.entry(name.to_owned()).or_default().push(file.to_owned());
    }
    let mut out = Vec::new();
    for (name, mut files) in folders {
        if !files.iter().any(|f| f == "extension.toml") {
            continue;
        }
        files.retain(|f| f != "extension.toml");
        let Ok(text) = api.raw(&branch, &format!("catalog/{name}/extension.toml")).await else { continue };
        let Ok(manifest) = toml::from_str::<Manifest>(&String::from_utf8_lossy(&text)) else { continue };
        // A status line is read to be checked; the rest is checked by its path.
        let mut statusline = None;
        if files.iter().any(|f| f == "statusline.json") {
            statusline = api.raw(&branch, &format!("catalog/{name}/statusline.json")).await.ok();
        }
        if check(&name, &manifest, &files, |_| statusline.clone()).is_ok() {
            out.push(Extension { name, manifest, files, votes: 0 });
        }
    }
    // The votes: the 👍 of each issue of the catalog, read in one call.
    if let Ok(issues) = api.get(&format!("/repos/{}/issues?labels=catalog&state=all&per_page=100", config.upstream)).await {
        for issue in issues.as_array().into_iter().flatten() {
            let (Some(number), Some(votes)) = (issue["number"].as_u64(), issue["reactions"]["+1"].as_u64()) else { continue };
            for extension in out.iter_mut().filter(|e| e.manifest.issue == Some(number)) {
                extension.votes = votes;
            }
        }
    }
    out.sort_by(|a, b| b.manifest.certified.cmp(&a.manifest.certified).then(b.votes.cmp(&a.votes)).then(a.name.cmp(&b.name)));
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), branch.clone(), out.clone()));
    Ok((branch, out))
}

/// The extensions installed, in `po/extensions.json` next to Po's settings: what each one put
/// where, to take it all back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub title: String,
    pub files: Vec<PathBuf>,
    /// The status line before it, to go back to.
    #[serde(default)]
    pub statusline_before: Option<String>,
    /// Made by Po Labs (not from the catalog): the fingerprint of its files, to follow its
    /// workspace.
    #[serde(default)]
    pub labs: Option<String>,
}

fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("po"))
}

/// The extensions installed under Po's settings folder `root`.
pub fn installed_in(root: &Path) -> BTreeMap<String, Installed> {
    std::fs::read_to_string(root.join("extensions.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn installed() -> BTreeMap<String, Installed> {
    config_dir().map(|root| installed_in(&root)).unwrap_or_default()
}

fn save_installed(root: &Path, registry: &BTreeMap<String, Installed>) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    std::fs::write(root.join("extensions.json"), serde_json::to_string_pretty(registry).unwrap_or_default() + "\n").map_err(|e| e.to_string())
}

/// Where a file of an extension goes, under Po's settings folder `root`.
fn target(root: &Path, part: &Part) -> Option<PathBuf> {
    match part {
        Part::Skill(rest) => Some(root.join("skills").join(rest)),
        Part::Agent(file) => Some(root.join("agents").join(file)),
        Part::StrataPreset(file) => Some(root.join("strata").join(file)),
        Part::StatusLine => None,
    }
}

/// Puts extension `name` in place, its files given with their content: skills and subagents in
/// Po's folders (never over one of the user's), the status line in the preferences.
pub fn put_in_place(root: &Path, name: &str, title: &str, files: &[(String, Vec<u8>)]) -> Result<Installed, String> {
    let mut registry = installed_in(root);
    if registry.contains_key(name) {
        return Err(t!("cmd.catalog.already", name = name));
    }
    // Everything is checked before anything is written.
    let mut writes = Vec::new();
    let mut statusline = None;
    for (file, content) in files {
        let part = part(file).ok_or_else(|| format!("`{file}`"))?;
        match target(root, &part) {
            Some(path) if path.exists() => return Err(t!("cmd.catalog.taken", path = path.display())),
            Some(path) => writes.push((path, content)),
            None => statusline = Some(serde_json::from_slice::<Vec<po_tui::statusline::Segment>>(content).map_err(|e| e.to_string())?),
        }
    }
    let mut record = Installed { title: title.to_owned(), ..Installed::default() };
    for (path, content) in writes {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        if let Err(err) = std::fs::write(&path, content) {
            take_back(root, &record);
            return Err(format!("{}: {err}", path.display()));
        }
        record.files.push(path);
    }
    if let Some(segments) = statusline {
        record.statusline_before = Some(po_i18n::prefs::get_str("statusline").unwrap_or_default());
        po_tui::statusline::set(Some(segments)).map_err(|e| e.to_string())?;
    }
    registry.insert(name.to_owned(), record.clone());
    save_installed(root, &registry)?;
    Ok(record)
}

/// Removes what an extension put in place, and the folders it leaves empty.
fn take_back(root: &Path, record: &Installed) {
    for path in &record.files {
        let _ = std::fs::remove_file(path);
        let mut dir = path.parent();
        while let Some(d) = dir.filter(|d| d.starts_with(root.join("skills")) && *d != root.join("skills")) {
            if std::fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    if let Some(before) = &record.statusline_before {
        let segments = serde_json::from_str(before).ok();
        let _ = po_tui::statusline::set(segments);
    }
}

/// Uninstalls extension `name` from Po's settings folder `root`.
pub fn remove_in(root: &Path, name: &str) -> Result<Installed, String> {
    let mut registry = installed_in(root);
    let record = registry.remove(name).ok_or_else(|| t!("cmd.catalog.not_installed", name = name))?;
    take_back(root, &record);
    save_installed(root, &registry)?;
    Ok(record)
}

/// Uninstalls extension `name`.
pub fn remove(name: &str) -> Result<Installed, String> {
    remove_in(&config_dir().ok_or("no configuration folder")?, name)
}

/// Downloads extension `name` of the catalog and installs it.
pub async fn install(config: &Config, name: &str) -> Result<Installed, String> {
    let (branch, list) = list(config).await?;
    let extension = list.iter().find(|e| e.name == name).ok_or_else(|| t!("cmd.catalog.unknown", name = name))?;
    let api = api(config).await;
    let mut files = Vec::new();
    for file in &extension.files {
        let content = api.raw(&branch, &format!("catalog/{name}/{file}")).await.map_err(|e| e.to_string())?;
        files.push((file.clone(), content));
    }
    let contents = files.clone();
    check(name, &extension.manifest, &extension.files, |f| contents.iter().find(|(p, _)| p == f).map(|(_, c)| c.clone()))?;
    let root = config_dir().ok_or("no configuration folder")?;
    put_in_place(&root, name, &extension.manifest.title, &files)
}

/// The text of an extension's files, to read before installing it.
pub async fn contents(config: &Config, extension: &Extension, branch: &str) -> Vec<(String, String)> {
    let api = api(config).await;
    let mut out = Vec::new();
    for file in &extension.files {
        let text = api.raw(branch, &format!("catalog/{}/{file}", extension.name)).await.map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        out.push((file.clone(), text));
    }
    out
}

/// How a vote went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vote {
    /// 👍 given; the count now.
    Given(u64),
    /// 👍 taken back.
    Withdrawn(u64),
    /// Not connected to GitHub yet: a page to open and a code to enter, then vote again.
    Login { url: String, code: String },
}

/// Gives a 👍 to extension `name`, or takes it back if already given.
pub async fn vote(config: &Config, name: &str) -> Result<Vote, String> {
    let (_, list) = list(config).await?;
    let extension = list.iter().find(|e| e.name == name).ok_or_else(|| t!("cmd.catalog.unknown", name = name))?;
    let issue = extension.manifest.issue.ok_or_else(|| t!("cmd.catalog.no_votes", name = name))?;
    let Some((api, user)) = crate::github::connected(config).await? else {
        let (url, code, _) = crate::github::login_in_background(config, |_| {}).await?;
        return Ok(Vote::Login { url, code });
    };
    let me = user["login"].as_str().unwrap_or_default();
    let err = |e: crate::github::Failure| e.to_string();
    let path = format!("/repos/{}/issues/{issue}/reactions", config.upstream);
    let reactions = api.get(&format!("{path}?content=%2B1&per_page=100")).await.map_err(err)?;
    let mine = reactions.as_array().into_iter().flatten().find(|r| r["user"]["login"].as_str() == Some(me)).and_then(|r| r["id"].as_u64());
    forget_cache();
    match mine {
        Some(id) => {
            api.call(reqwest::Method::DELETE, &format!("{path}/{id}"), None).await.map_err(err)?;
            Ok(Vote::Withdrawn(extension.votes.saturating_sub(1)))
        }
        None => {
            api.post(&path, json!({"content": "+1"})).await.map_err(err)?;
            Ok(Vote::Given(extension.votes + 1))
        }
    }
}

/// Files of an extension with their content, by path in its folder.
type Contents = Vec<(String, Vec<u8>)>;

/// The files of the extension in `folder`, with their content (`extension.toml` included).
fn folder_files(folder: &Path) -> Contents {
    let mut out = Vec::new();
    let mut stack = vec![folder.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(content) = std::fs::read(&path) {
                let rel = path.strip_prefix(folder).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                out.push((rel, content));
            }
        }
    }
    out.sort();
    out
}

/// The extension in folder `catalog/<name>`, checked: its manifest and its files.
fn read_folder(name: &str, folder: &Path) -> Result<(Manifest, Contents), String> {
    let mut files = folder_files(folder);
    let manifest = files.iter().position(|(f, _)| f == "extension.toml").map(|i| files.remove(i).1).ok_or_else(|| format!("`{name}`: no extension.toml"))?;
    let manifest: Manifest = toml::from_str(&String::from_utf8_lossy(&manifest)).map_err(|e| format!("`{name}`: extension.toml: {e}"))?;
    let paths: Vec<String> = files.iter().map(|(f, _)| f.clone()).collect();
    check(name, &manifest, &paths, |f| files.iter().find(|(p, _)| p == f).map(|(_, c)| c.clone()))?;
    Ok((manifest, files))
}

/// The extensions of a `catalog/` folder, by name.
fn folders(dir: &Path) -> BTreeMap<String, PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return BTreeMap::new() };
    entries.flatten().filter(|e| e.path().is_dir()).map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect()
}

/// A short fingerprint of an extension's files, to see that it changed.
fn fingerprint(files: &[(String, Vec<u8>)]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    files.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Brings the extensions made by Po Labs in sync with its workspace: those of `workspace`'s
/// catalog that Po's checkout `checkout` does not have as they are get installed (again, when
/// changed); those gone from the workspace (a change undone) are removed. Installed under Po's
/// settings folder `root`. Returns what could not be installed, for the model to fix.
pub fn sync_labs_in(root: &Path, workspace: &Path, checkout: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let here = folders(&workspace.join("catalog"));
    let theirs = folders(&checkout.join("catalog"));
    let registry = installed_in(root);
    for (name, record) in &registry {
        if record.labs.is_some() && !here.contains_key(name) {
            let _ = remove_in(root, name);
        }
    }
    for (name, folder) in &here {
        let mine = folder_files(folder);
        if theirs.get(name).is_some_and(|f| folder_files(f) == mine) {
            // Po's own catalog: installed by the user, from `/catalog`, if they want it.
            continue;
        }
        let print = fingerprint(&mine);
        match registry.get(name) {
            Some(record) if record.labs.as_deref() == Some(print.as_str()) => continue,
            Some(record) if record.labs.is_none() => {
                problems.push(format!("`{name}`: an extension of the catalog with this name is installed; choose another name"));
                continue;
            }
            Some(_) => {
                let _ = remove_in(root, name);
            }
            None => {}
        }
        match read_folder(name, folder).and_then(|(manifest, files)| put_in_place(root, name, &manifest.title, &files)) {
            Ok(_) => {
                let mut registry = installed_in(root);
                if let Some(record) = registry.get_mut(name) {
                    record.labs = Some(print);
                }
                if let Err(err) = save_installed(root, &registry) {
                    problems.push(err);
                }
            }
            Err(err) => problems.push(err),
        }
    }
    problems
}

/// [`sync_labs_in`] under Po's settings folder.
pub fn sync_labs(workspace: &Path, checkout: &Path) -> Vec<String> {
    match config_dir() {
        Some(root) => sync_labs_in(&root, workspace, checkout),
        None => vec!["no configuration folder".to_owned()],
    }
}

/// Checks the extensions of a `catalog/` folder.
pub fn check_folder(dir: &Path) -> Result<usize, String> {
    let all = folders(dir);
    for (name, folder) in &all {
        read_folder(name, folder)?;
    }
    Ok(all.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest { title: "Commit messages".into(), description: "Writes them.".into(), category: "commands".into(), author: "a".into(), certified: false, issue: None }
    }

    #[test]
    fn only_files_po_knows_how_to_install_are_accepted() {
        let files = |list: &[&str]| list.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>();
        let none = |_: &str| None;
        assert!(check("commit", &manifest(), &files(&["skills/commit/SKILL.md", "agents/reviewer.md"]), none).is_ok());
        for bad in ["skills/../x.md", "skills/commit", "agents/sub/x.md", "agents/x.txt", "run.sh", "skills/Bad/SKILL.md", "/etc/x"] {
            assert!(check("commit", &manifest(), &files(&[bad]), none).is_err(), "{bad} accepted");
        }
        assert!(check("Bad Name", &manifest(), &files(&["agents/x.md"]), none).is_err());
        assert!(check("commit", &Manifest { category: "games".into(), ..manifest() }, &files(&["agents/x.md"]), none).is_err());
        // A Strata preset: read as one, an unknown key refused.
        let preset = |text: &'static str| move |_: &str| Some(text.as_bytes().to_vec());
        assert!(check("fast", &manifest(), &files(&["strata/fast.json"]), preset(r#"{"args": {"--spec-min-p": "0.7"}}"#)).is_ok());
        assert!(check("fast", &manifest(), &files(&["strata/fast.json"]), preset(r#"{"argz": {}}"#)).is_err());
        assert!(check("fast", &manifest(), &files(&["strata/Fast.json"]), preset("{}")).is_err());
        let root = tempfile::tempdir().unwrap();
        let installed = put_in_place(root.path(), "fast", "Fast", &[("strata/fast.json".into(), b"{}".to_vec())]).unwrap();
        assert_eq!(installed.files, vec![root.path().join("strata").join("fast.json")], "beside Po's settings, where the profile finds it");
        remove_in(root.path(), "fast").unwrap();
        assert!(!root.path().join("strata").join("fast.json").exists());
    }

    #[test]
    fn a_status_line_runs_commands_only_once_certified() {
        let line = br#"[{"id": "time"}, {"id": "command", "command": "whoami"}]"#.to_vec();
        let read = |_: &str| Some(line.clone());
        let files = vec!["statusline.json".to_owned()];
        assert!(check("whoami", &manifest(), &files, read).is_err());
        assert!(check("whoami", &Manifest { certified: true, ..manifest() }, &files, read).is_ok());
        assert!(check("bad", &manifest(), &files, |_| Some(br#"[{"id": "nope"}]"#.to_vec())).is_err());
    }

    #[test]
    fn an_extension_is_put_in_place_and_taken_back_without_touching_the_users_files() {
        let root = tempfile::tempdir().unwrap();
        let name = "commit-kit";
        std::fs::create_dir_all(root.path().join("agents")).unwrap();
        std::fs::write(root.path().join("agents").join("mine.md"), "the user's").unwrap();
        let files = vec![("skills/commit/SKILL.md".to_owned(), b"skill".to_vec()), ("agents/reviewer.md".to_owned(), b"agent".to_vec())];
        let record = put_in_place(root.path(), name, "T", &files).unwrap();
        assert!(root.path().join("skills").join("commit").join("SKILL.md").is_file());
        assert_eq!(record.files.len(), 2);
        // The same subagent as the user's is refused, nothing written.
        let clash = vec![("agents/mine.md".to_owned(), b"x".to_vec())];
        assert!(put_in_place(root.path(), "other", "T", &clash).is_err());
        assert!(put_in_place(root.path(), name, "T", &files).is_err(), "installed once");
        assert_eq!(std::fs::read_to_string(root.path().join("agents").join("mine.md")).unwrap(), "the user's");
        assert!(installed_in(root.path()).contains_key(name));
        remove_in(root.path(), name).unwrap();
        assert!(!root.path().join("skills").join("commit").exists(), "the skill's folder goes too");
        assert!(!root.path().join("agents").join("reviewer.md").exists());
        assert!(root.path().join("agents").join("mine.md").is_file());
        assert!(installed_in(root.path()).is_empty());
    }

    #[test]
    fn the_catalog_of_the_repository_is_valid() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("catalog");
        assert!(check_folder(&dir).unwrap() >= 1);
        for (name, folder) in folders(&dir) {
            let (manifest, _) = read_folder(&name, &folder).unwrap();
            assert!(!manifest.author.is_empty(), "{name}: who made it?");
        }
    }

    #[test]
    fn the_extensions_made_by_po_labs_follow_its_workspace() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        let write = |base: &Path, name: &str, rules: &str| {
            let folder = base.join("catalog").join(name);
            std::fs::create_dir_all(folder.join("agents")).unwrap();
            std::fs::write(folder.join("extension.toml"), "title = \"T\"\ndescription = \"D\"\ncategory = \"tools\"\nauthor = \"a\"\n").unwrap();
            std::fs::write(folder.join("agents").join(format!("{name}.md")), rules).unwrap();
        };
        // In Po's checkout as it is: not Po Labs'.
        write(workspace.path(), "shared", "same");
        write(checkout.path(), "shared", "same");
        write(workspace.path(), "tester", "v1");
        assert!(sync_labs_in(root.path(), workspace.path(), checkout.path()).is_empty());
        let agent = root.path().join("agents").join("tester.md");
        assert_eq!(std::fs::read_to_string(&agent).unwrap(), "v1");
        assert!(!root.path().join("agents").join("shared.md").exists());
        // Adjusted: installed again; undone: removed.
        write(workspace.path(), "tester", "v2");
        sync_labs_in(root.path(), workspace.path(), checkout.path());
        assert_eq!(std::fs::read_to_string(&agent).unwrap(), "v2");
        std::fs::remove_dir_all(workspace.path().join("catalog").join("tester")).unwrap();
        sync_labs_in(root.path(), workspace.path(), checkout.path());
        assert!(!agent.exists() && installed_in(root.path()).is_empty());
        // A wrong one is reported, not installed.
        std::fs::create_dir_all(workspace.path().join("catalog").join("bad")).unwrap();
        assert_eq!(sync_labs_in(root.path(), workspace.path(), checkout.path()).len(), 1);
    }
}
