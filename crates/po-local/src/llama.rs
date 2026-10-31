//! Finding `llama-server`: the user's own choices first, then every place an installer drops it.
//!
//! A beginner does not know where `winget`, `scoop` or a zip they unpacked put the binary, and
//! should not have to. The search therefore covers, in order: the exact path the user set, the
//! folder they named (searched a few levels deep), `PO_LLAMA_SERVER`, the `PATH`, the package
//! managers' folders (winget's packages and links, scoop, Chocolatey, Homebrew), the usual
//! build folders, the folders next to the models, and the running WSL distributions.
//!
//! The environment is gathered in an [`Env`] so that every rule is testable on a fake tree.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How the binary was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    UserPath,
    UserFolder,
    EnvVar,
    Path,
    PackageManager,
    Usual,
    NextToModels,
    Wsl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: PathBuf,
    pub origin: Origin,
}

/// What the user set for llama.cpp.
#[derive(Debug, Clone, Default)]
pub struct Choice {
    pub executable: Option<PathBuf>,
    pub search_dir: Option<PathBuf>,
}

impl Choice {
    pub fn from_prefs(prefs: &crate::prefs::LocalPrefs) -> Self {
        let engine = prefs.engine(crate::prefs::LLAMA_CPP);
        Self { executable: prefs.llama_executable().map(PathBuf::from), search_dir: engine.search_dir.map(PathBuf::from) }
    }
}

/// The places to look at, gathered once from the system (or given by a test).
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub explicit_var: Option<PathBuf>,
    pub path_dirs: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub program_dirs: Vec<PathBuf>,
    /// Model folders: a binary stored next to them is usually the one the user employs.
    pub model_roots: Vec<PathBuf>,
    /// Look into the running WSL distributions (slow: off in tests).
    pub wsl: bool,
}

impl Env {
    pub fn system(prefs: &crate::prefs::LocalPrefs) -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            explicit_var: var("PO_LLAMA_SERVER"),
            path_dirs: std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default(),
            home: dirs::home_dir(),
            local_app_data: var("LOCALAPPDATA"),
            program_dirs: ["ProgramFiles", "ProgramW6432", "ProgramData"].iter().filter_map(|v| var(v)).collect(),
            model_roots: crate::catalog::search_roots_with(prefs),
            wsl: true,
        }
    }
}

/// The binary's file name on this system.
pub fn exe_name() -> &'static str {
    if cfg!(windows) { "llama-server.exe" } else { "llama-server" }
}

/// The result of a search.
#[derive(Debug, Clone, Default)]
pub struct Detection {
    pub located: Option<Located>,
    /// The exact path the user set is not (or no longer) a `llama-server`.
    pub bad_executable: Option<PathBuf>,
    /// The folder the user gave holds none.
    pub bad_search_dir: Option<PathBuf>,
}

/// Looks for `llama-server` a few levels under `dir`, the shallowest first. Bounded in depth and
/// in entries: the user may well name a whole disk.
pub fn find_in_dir(dir: &Path, exe: &str, max_depth: usize) -> Option<PathBuf> {
    let direct = dir.join(exe);
    if direct.is_file() {
        return Some(direct);
    }
    let mut level = vec![dir.to_path_buf()];
    let mut budget = 50_000usize;
    for _ in 0..max_depth {
        let mut next = Vec::new();
        for folder in &level {
            let Ok(entries) = std::fs::read_dir(folder) else { continue };
            for entry in entries.flatten() {
                if budget == 0 {
                    return None;
                }
                budget -= 1;
                let Ok(kind) = entry.file_type() else { continue };
                let path = entry.path();
                if kind.is_dir() && !crate::catalog::skipped_dir(&entry.file_name()) {
                    let candidate = path.join(exe);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                    next.push(path);
                }
            }
        }
        level = next;
        if level.is_empty() {
            break;
        }
    }
    None
}

/// What the package managers install, for a home and a local data folder.
fn package_manager_places(env: &Env, exe: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(local) = &env.local_app_data {
        let winget = local.join("Microsoft").join("WinGet");
        out.push(winget.join("Links").join(exe));
        // `winget install ggml.llamacpp` unpacks into a folder whose name carries the source:
        // `ggml.llamacpp_Microsoft.Winget.Source_8wekyb3d8bbwe`.
        if let Ok(entries) = std::fs::read_dir(winget.join("Packages")) {
            for entry in entries.flatten().take(5_000) {
                if entry.file_name().to_string_lossy().to_ascii_lowercase().starts_with("ggml.llamacpp") {
                    out.push(entry.path().join(exe));
                    out.push(entry.path().join("bin").join(exe));
                }
            }
        }
        out.push(local.join("Programs").join("llama.cpp").join(exe));
    }
    if let Some(home) = &env.home {
        out.push(home.join("scoop").join("apps").join("llama.cpp").join("current").join(exe));
        out.push(home.join("scoop").join("shims").join(exe));
    }
    for program in &env.program_dirs {
        out.push(program.join("llama.cpp").join(exe));
        out.push(program.join("llama.cpp").join("bin").join(exe));
        out.push(program.join("chocolatey").join("bin").join(exe));
    }
    out
}

/// Finds `llama-server`, and says what went wrong with the user's choices.
pub fn detect(choice: &Choice, env: &Env) -> Detection {
    let exe = exe_name();
    let mut detection = Detection::default();
    let found = |path: PathBuf, origin: Origin| Some(Located { path, origin });

    if let Some(path) = &env.explicit_var
        && path.is_file()
    {
        detection.located = found(path.clone(), Origin::EnvVar);
        return detection;
    }
    if let Some(path) = &choice.executable {
        if path.is_file() {
            detection.located = found(path.clone(), Origin::UserPath);
            return detection;
        }
        // A folder given as "the executable" is searched like a folder.
        match path.is_dir().then(|| find_in_dir(path, exe, 3)).flatten() {
            Some(inside) => {
                detection.located = found(inside, Origin::UserPath);
                return detection;
            }
            None => detection.bad_executable = Some(path.clone()),
        }
    }
    if let Some(dir) = &choice.search_dir {
        match find_in_dir(dir, exe, 4) {
            Some(inside) => {
                detection.located = found(inside, Origin::UserFolder);
                return detection;
            }
            None => detection.bad_search_dir = Some(dir.clone()),
        }
    }
    let ordered = env
        .path_dirs
        .iter()
        .map(|dir| (dir.join(exe), Origin::Path))
        .chain(package_manager_places(env, exe).into_iter().map(|p| (p, Origin::PackageManager)))
        .chain(crate::supervisor::usual_locations(env.home.as_deref(), exe).into_iter().map(|p| (p, Origin::Usual)))
        .chain(
            env.model_roots
                .iter()
                .filter_map(|root| root.parent().map(Path::to_path_buf))
                .flat_map(|parent| crate::supervisor::model_neighbours(&parent, exe))
                .map(|p| (p, Origin::NextToModels)),
        );
    for (path, origin) in ordered {
        if path.is_file() {
            detection.located = found(path, origin);
            return detection;
        }
    }
    if env.wsl
        && let Some(path) = crate::supervisor::find_binary_in_wsl()
    {
        detection.located = found(path, Origin::Wsl);
    }
    detection
}

/// Reads the version line of `llama-server --version`:
/// `version: 0.5.0-dev (build 11342, commit f1cee9941)` → `0.5.0-dev (build 11342, commit f1cee9941)`,
/// `version: 4568 (1ee9eea0)` → `4568 (1ee9eea0)`.
pub fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| l.starts_with("version:"))?;
    let version = line["version:".len()..].trim();
    (!version.is_empty()).then(|| version.chars().take(80).collect())
}

/// Asks a binary its version, waiting at most `timeout`. A binary that does not answer (an old
/// build that starts serving instead, a corrupt file) is stopped, not waited for.
pub fn version(path: &Path, timeout: Duration) -> Option<String> {
    #[cfg(windows)]
    if crate::catalog::wsl_parts(path).is_some() {
        return None;
    }
    let mut command = std::process::Command::new(path);
    command.arg("--version").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::catalog::NO_CONSOLE);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    // llama.cpp writes it on stderr; some builds on stdout.
    parse_version(&String::from_utf8_lossy(&output.stderr)).or_else(|| parse_version(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    fn empty_env(root: &Path) -> Env {
        Env { home: Some(root.join("home")), local_app_data: Some(root.join("local")), ..Default::default() }
    }

    #[test]
    fn the_users_exact_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mine = dir.path().join("mine").join(exe_name());
        let in_path = dir.path().join("path").join(exe_name());
        touch(&mine);
        touch(&in_path);
        let env = Env { path_dirs: vec![in_path.parent().unwrap().to_path_buf()], ..empty_env(dir.path()) };
        let found = detect(&Choice { executable: Some(mine.clone()), ..Default::default() }, &env);
        assert_eq!(found.located, Some(Located { path: mine, origin: Origin::UserPath }));
        let found = detect(&Choice::default(), &env);
        assert_eq!(found.located.unwrap().origin, Origin::Path);
    }

    #[test]
    fn a_vanished_path_is_reported_and_detection_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let in_path = dir.path().join("path").join(exe_name());
        touch(&in_path);
        let env = Env { path_dirs: vec![in_path.parent().unwrap().to_path_buf()], ..empty_env(dir.path()) };
        let gone = dir.path().join("deleted").join(exe_name());
        let found = detect(&Choice { executable: Some(gone.clone()), ..Default::default() }, &env);
        assert_eq!(found.bad_executable, Some(gone));
        assert_eq!(found.located.unwrap().path, in_path, "the automatic search still finds one");
    }

    #[test]
    fn the_users_folder_is_searched_a_few_levels_deep() {
        let dir = tempfile::tempdir().unwrap();
        // The layout of an unpacked release zip, one folder down.
        let deep = dir.path().join("llama-b6000-bin-win-cuda-x64").join("build").join("bin").join(exe_name());
        touch(&deep);
        let found = detect(&Choice { search_dir: Some(dir.path().to_path_buf()), ..Default::default() }, &empty_env(dir.path()));
        assert_eq!(found.located, Some(Located { path: deep, origin: Origin::UserFolder }));
        let empty = tempfile::tempdir().unwrap();
        let found = detect(&Choice { search_dir: Some(empty.path().to_path_buf()), ..Default::default() }, &empty_env(empty.path()));
        assert_eq!(found.bad_search_dir.as_deref(), Some(empty.path()));
        assert!(found.located.is_none());
    }

    #[test]
    fn a_folder_given_as_the_executable_is_searched() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("bin").join(exe_name());
        touch(&exe);
        let found = detect(&Choice { executable: Some(dir.path().to_path_buf()), ..Default::default() }, &empty_env(dir.path()));
        assert_eq!(found.located.unwrap().path, exe);
    }

    #[test]
    fn winget_s_package_folder_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let env = empty_env(dir.path());
        let winget = dir.path().join("local").join("Microsoft").join("WinGet").join("Packages").join("ggml.llamacpp_Microsoft.Winget.Source_8wekyb3d8bbwe").join(exe_name());
        touch(&winget);
        let found = detect(&Choice::default(), &env);
        assert_eq!(found.located, Some(Located { path: winget, origin: Origin::PackageManager }));
    }

    #[test]
    fn a_binary_next_to_the_models_is_found() {
        // `E:\LLM\models` and `E:\LLM\llama-bin`: the development machine's layout.
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("LLM").join("models");
        std::fs::create_dir_all(&models).unwrap();
        let bin = dir.path().join("LLM").join("llama-bin").join(exe_name());
        touch(&bin);
        let env = Env { model_roots: vec![models], ..empty_env(dir.path()) };
        assert_eq!(detect(&Choice::default(), &env).located.unwrap().origin, Origin::NextToModels);
    }

    #[test]
    fn nothing_found_is_nothing_found() {
        let dir = tempfile::tempdir().unwrap();
        let found = detect(&Choice::default(), &empty_env(dir.path()));
        assert!(found.located.is_none());
        assert!(found.bad_executable.is_none() && found.bad_search_dir.is_none());
    }

    #[test]
    fn a_huge_folder_does_not_freeze_the_search() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            std::fs::create_dir_all(dir.path().join(format!("d{i}")).join("sub")).unwrap();
        }
        let started = Instant::now();
        assert!(find_in_dir(dir.path(), exe_name(), 4).is_none());
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn versions_are_read() {
        assert_eq!(parse_version("load_backend: x\nversion: 0.5.0-dev (build 11342, commit f1cee9941)\nbuilt with Clang").as_deref(), Some("0.5.0-dev (build 11342, commit f1cee9941)"));
        assert_eq!(parse_version("version: 4568 (1ee9eea0)").as_deref(), Some("4568 (1ee9eea0)"));
        assert_eq!(parse_version("usage: llama-server [options]"), None);
        assert_eq!(parse_version("version:   "), None);
    }

    #[test]
    fn a_binary_that_is_not_one_gives_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join(exe_name());
        std::fs::write(&fake, b"not an executable").unwrap();
        assert_eq!(version(&fake, Duration::from_secs(2)), None);
        assert_eq!(version(&dir.path().join("absent"), Duration::from_secs(2)), None);
    }
}
