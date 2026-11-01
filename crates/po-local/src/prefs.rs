//! Local preferences saved by Po itself: model folders, and for each engine (llama.cpp, Strata)
//! where to look for it, the exact executable when detection does not find it, and its model
//! folders (`/moteurs`, and the older `/local dossier`, `/local serveur`).
//!
//! A separate file that Po owns entirely: rewriting it cannot damage the configuration the
//! user wrote by hand.
//!
//! The format grew without breaking: the first version had only `models_dirs` and
//! `llama_server` at the top level. They are still read (and folded into the llama.cpp engine),
//! and still written, mirrored from that engine, so that an older Po reading the file finds what
//! it knew. An older Po ignores the `engines` table it does not know, as serde does by default.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Key of llama.cpp in [`LocalPrefs::engines`].
pub const LLAMA_CPP: &str = "llama.cpp";
/// Key of Strata in [`LocalPrefs::engines`].
pub const STRATA: &str = "strata";

/// What the user chose for one engine. Every field is optional: empty means "detect by itself".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnginePrefs {
    /// A folder to search the engine in, before the usual places.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_dir: Option<String>,
    /// More folders to search, after `search_dir` (one engine installed in several places, or
    /// a second drive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more_search_dirs: Vec<String>,
    /// The exact executable, when no search finds it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Folders of models added for this engine.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models_dirs: Vec<String>,
}

impl EnginePrefs {
    pub fn is_empty(&self) -> bool {
        self.search_dir.is_none() && self.more_search_dirs.is_empty() && self.executable.is_none() && self.models_dirs.is_empty()
    }

    /// Every folder the engine is searched in first, in the order given.
    pub fn search_dirs(&self) -> Vec<String> {
        let mut out: Vec<String> = self.search_dir.iter().cloned().collect();
        for dir in &self.more_search_dirs {
            if !out.iter().any(|d| d.eq_ignore_ascii_case(dir)) {
                out.push(dir.clone());
            }
        }
        out
    }
}

/// How one model is started and sampled, as the user set it from `/moteurs`. Every field is
/// optional: empty means "what Po's launch plan, or the server, decides".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelProfile {
    /// Context window, in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u32>,
    /// KV cache type: `f16`, `q8_0`, `q4_0` for llama.cpp; `int8`, `q4_0`, `k8v4` for Strata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
    /// Reasoning: `auto`, `off`, `on`, `low`, `medium`, `high`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    /// Penalty on tokens already written (llama.cpp's `repeat_penalty`), 1.0 for none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f32>,
    /// Most tokens of one answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u32>,
    /// Instructions of this model's own, added to the system prompt when it answers (a style,
    /// a language, what it must not do).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The graphics cards it runs on, chosen before its start (`placement::Placement`, written
    /// `1`, `0,1`, `1/mtp=0`); none: Po chooses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<crate::placement::Placement>,
}

impl ModelProfile {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Sets one field from the words of a command (`context 32768`, `temperature 0.6`, `cache
    /// default`). Returns false when the key or the value is not one Po knows: nothing is
    /// changed then.
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        let value = value.trim();
        let clear = matches!(value.to_ascii_lowercase().as_str(), "defaut" | "défaut" | "default" | "auto-po" | "");
        let float = |range: std::ops::RangeInclusive<f32>| value.replace(',', ".").parse::<f32>().ok().filter(|v| range.contains(v));
        match key.to_ascii_lowercase().as_str() {
            "contexte" | "context" | "ctx" | "c" => {
                if clear {
                    self.context = None;
                    return true;
                }
                // "32k", "131072", "128K".
                let lower = value.to_ascii_lowercase();
                let parsed = match lower.strip_suffix('k') {
                    Some(k) => k.parse::<u32>().ok().map(|n| n * 1024),
                    None => lower.parse::<u32>().ok(),
                };
                match parsed.filter(|n| (2048..=1_048_576).contains(n)) {
                    Some(n) => self.context = Some(n),
                    None => return false,
                }
            }
            "cache" | "kv" => {
                if clear {
                    self.cache = None;
                    return true;
                }
                let v = value.to_ascii_lowercase();
                if !matches!(v.as_str(), "f16" | "q8_0" | "q4_0" | "int8" | "k8v4") {
                    return false;
                }
                self.cache = Some(v);
            }
            "raisonnement" | "reasoning" | "effort" => {
                if clear {
                    self.reasoning = None;
                    return true;
                }
                let v = value.to_ascii_lowercase();
                let v = match v.as_str() {
                    "non" | "none" => "off".to_owned(),
                    "oui" => "on".to_owned(),
                    "bas" | "faible" => "low".to_owned(),
                    "moyen" => "medium".to_owned(),
                    "haut" | "eleve" | "élevé" => "high".to_owned(),
                    other => other.to_owned(),
                };
                if !matches!(v.as_str(), "auto" | "off" | "on" | "low" | "medium" | "high") {
                    return false;
                }
                self.reasoning = Some(v);
            }
            "temperature" | "température" | "temp" => match (clear, float(0.0..=2.0)) {
                (true, _) => self.temperature = None,
                (_, Some(v)) => self.temperature = Some(v),
                _ => return false,
            },
            "top_p" | "top-p" | "topp" => match (clear, float(0.0..=1.0)) {
                (true, _) => self.top_p = None,
                (_, Some(v)) => self.top_p = Some(v),
                _ => return false,
            },
            "min_p" | "min-p" | "minp" => match (clear, float(0.0..=1.0)) {
                (true, _) => self.min_p = None,
                (_, Some(v)) => self.min_p = Some(v),
                _ => return false,
            },
            "top_k" | "top-k" | "topk" => {
                if clear {
                    self.top_k = None;
                    return true;
                }
                match value.parse::<u32>().ok().filter(|n| *n <= 1000) {
                    Some(n) => self.top_k = Some(n),
                    None => return false,
                }
            }
            "repeat_penalty" | "repetition" | "penalite" | "pénalité" => match (clear, float(0.5..=2.0)) {
                (true, _) => self.repeat_penalty = None,
                (_, Some(v)) => self.repeat_penalty = Some(v),
                _ => return false,
            },
            "max_output" | "sortie" | "output" | "max_tokens" => {
                if clear {
                    self.max_output = None;
                    return true;
                }
                let lower = value.to_ascii_lowercase();
                let parsed = match lower.strip_suffix('k') {
                    Some(k) => k.parse::<u32>().ok().map(|n| n * 1024),
                    None => lower.parse::<u32>().ok(),
                };
                match parsed.filter(|n| (256..=262_144).contains(n)) {
                    Some(n) => self.max_output = Some(n),
                    None => return false,
                }
            }
            "cartes" | "carte" | "gpus" | "gpu" | "cards" | "placement" => match crate::placement::Placement::parse(value) {
                Some(p) if p.is_auto() => self.placement = None,
                Some(p) => self.placement = Some(p),
                None => return false,
            },
            // Free text: everything after the key.
            "systeme" | "système" | "system" | "prompt" | "consigne" => {
                self.system = (!clear).then(|| value.to_owned());
            }
            _ => return false,
        }
        true
    }

    /// The reasoning level a request carries (`none`, `low`, `medium`, `high`), or none.
    pub fn request_effort(&self) -> Option<String> {
        match self.reasoning.as_deref()? {
            "off" => Some("none".into()),
            level @ ("low" | "medium" | "high") => Some(level.into()),
            _ => None,
        }
    }
}

/// The small model Po may start on a second card, for the subagents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkerPrefs {
    /// The model's file (GGUF), when the user chose one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The card, by its `nvidia-smi` index or UUID, when the user chose one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalPrefs {
    /// GGUF model folders added by the user, scanned in addition to the usual locations.
    /// Since the engines table, the mirror of llama.cpp's folders (kept for older versions).
    pub models_dirs: Vec<String>,
    /// Forced `llama-server` binary. Since the engines table, the mirror of llama.cpp's executable.
    pub llama_server: Option<String>,
    /// Settings per engine, by key ([`LLAMA_CPP`], [`STRATA`]).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub engines: BTreeMap<String, EnginePrefs>,
    /// How each model is started and sampled, by its name (file stem, or Strata's model name).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, ModelProfile>,
    /// Game mode: servers started by Po run at low priority, with fewer processor threads.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub game_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<WorkerPrefs>,
    /// The local model every session starts with when a message needs one (`/configurer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// The setup screen was seen once: it no longer opens by itself while a model answers.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub setup_seen: bool,
}

/// The file: `%APPDATA%\po\local.json` (`~/.config/po` elsewhere).
///
/// `APPDATA` is read first, as for Po's other files (`ui.json`, the sessions): a test, or a
/// trial run, that points it at a scratch folder must not write into the user's real settings.
/// `dirs::config_dir` alone asks Windows for the folder and ignores the variable.
///
/// `PO_LOCAL_PREFS` names the file itself, for trials that must leave every other file alone.
pub fn path() -> Option<PathBuf> {
    if let Some(file) = std::env::var_os("PO_LOCAL_PREFS").filter(|f| !f.is_empty()) {
        return Some(PathBuf::from(file));
    }
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(dirs::config_dir)
        .map(|dir| dir.join("po").join("local.json"))
}

/// Folds case and separators the way the file system does, to compare two folders.
fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().trim_matches('"').replace('\\', "/");
        let s = s.trim_end_matches('/').to_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

/// A path typed by the user: quotes and spaces around it are not part of it.
pub fn clean_path(raw: &str) -> String {
    raw.trim().trim_matches('"').trim_matches('\'').trim().to_owned()
}

impl LocalPrefs {
    pub fn load() -> Self {
        path().map(|p| Self::load_from(&p)).unwrap_or_default()
    }

    /// Reads a file; an absent or unreadable one gives the defaults, never an error: the
    /// preferences only refine detection, they must not prevent it.
    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path).ok().map(|text| Self::parse(&text)).unwrap_or_default()
    }

    /// Parses the file's text and folds the first version's fields into the engines table.
    pub fn parse(text: &str) -> Self {
        let mut prefs: Self = serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_default();
        prefs.migrate();
        prefs
    }

    /// The first version's `models_dirs` and `llama_server` were llama.cpp's: they join its entry,
    /// without duplicates, and without overriding what the engines table already says.
    fn migrate(&mut self) {
        let legacy_dirs = std::mem::take(&mut self.models_dirs);
        let legacy_server = self.llama_server.take();
        let llama = self.engines.entry(LLAMA_CPP.to_owned()).or_default();
        for dir in legacy_dirs {
            if !dir.trim().is_empty() && !llama.models_dirs.iter().any(|d| same_dir(d, &dir)) {
                llama.models_dirs.push(dir);
            }
        }
        if llama.executable.is_none() {
            llama.executable = legacy_server.filter(|s| !s.trim().is_empty());
        }
        self.engines.retain(|_, e| !e.is_empty());
        self.mirror();
    }

    /// Writes llama.cpp's entry back into the first version's fields, for an older Po.
    fn mirror(&mut self) {
        let llama = self.engines.get(LLAMA_CPP).cloned().unwrap_or_default();
        self.models_dirs = llama.models_dirs;
        self.llama_server = llama.executable;
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = path().ok_or_else(|| std::io::Error::other(po_i18n::t!("local.prefs.no_config_dir")))?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Writes atomically: a temporary file next to it, then a rename. Two Po windows saving at
    /// the same time, or a Po stopped mid-write, must not leave half a file that reads as empty
    /// preferences — every folder the user added would be lost without a word.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        let mut copy = self.clone();
        copy.engines.retain(|_, e| !e.is_empty());
        copy.models.retain(|_, m| !m.is_empty());
        copy.mirror();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&copy).map_err(std::io::Error::other)? + "\n";
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("json.{}.{unique}.tmp", std::process::id()));
        std::fs::write(&tmp, text)?;
        // On Windows a rename onto a file another process is reading can fail for a moment:
        // a few tries, then the error.
        let mut last = None;
        for _ in 0..20 {
            match std::fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    last = Some(err);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        let _ = std::fs::remove_file(&tmp);
        Err(last.unwrap_or_else(|| std::io::Error::other("rename")))
    }

    /// A model's profile (empty when none was saved).
    pub fn model(&self, name: &str) -> ModelProfile {
        self.models.get(name).cloned().unwrap_or_default()
    }

    pub fn model_mut(&mut self, name: &str) -> &mut ModelProfile {
        self.models.entry(name.to_owned()).or_default()
    }

    /// The settings of one engine (defaults when none was saved).
    pub fn engine(&self, key: &str) -> EnginePrefs {
        self.engines.get(key).cloned().unwrap_or_default()
    }

    pub fn engine_mut(&mut self, key: &str) -> &mut EnginePrefs {
        self.engines.entry(key.to_owned()).or_default()
    }

    /// Every GGUF folder added for llama.cpp (the first version's list included).
    pub fn llama_models_dirs(&self) -> Vec<String> {
        let mut dirs = self.engine(LLAMA_CPP).models_dirs;
        for dir in &self.models_dirs {
            if !dirs.iter().any(|d| same_dir(d, dir)) {
                dirs.push(dir.clone());
            }
        }
        dirs
    }

    /// The `llama-server` chosen by the user, wherever it was saved.
    pub fn llama_executable(&self) -> Option<String> {
        self.engine(LLAMA_CPP).executable.or_else(|| self.llama_server.clone())
    }

    /// Adds a llama.cpp model folder if it is not already there. Returns true if it was new.
    pub fn add_models_dir(&mut self, dir: &str) -> bool {
        self.add_engine_models_dir(LLAMA_CPP, dir)
    }

    /// Adds a model folder to an engine. Returns true if it was new.
    pub fn add_engine_models_dir(&mut self, key: &str, dir: &str) -> bool {
        let dir = clean_path(dir);
        if dir.is_empty() {
            return false;
        }
        let known = if key == LLAMA_CPP { self.llama_models_dirs() } else { self.engine(key).models_dirs };
        if known.iter().any(|d| same_dir(d, &dir)) {
            return false;
        }
        self.engine_mut(key).models_dirs.push(dir);
        if key == LLAMA_CPP {
            self.mirror();
        }
        true
    }

    /// Removes a model folder from an engine. Returns true if it was there.
    pub fn remove_engine_models_dir(&mut self, key: &str, dir: &str) -> bool {
        let dir = clean_path(dir);
        let entry = self.engine_mut(key);
        let before = entry.models_dirs.len();
        entry.models_dirs.retain(|d| !same_dir(d, &dir));
        let removed = entry.models_dirs.len() != before;
        if key == LLAMA_CPP {
            let before = self.models_dirs.len();
            self.models_dirs.retain(|d| !same_dir(d, &dir));
            let legacy = self.models_dirs.len() != before;
            self.mirror();
            return removed || legacy;
        }
        removed
    }

    /// Sets (or clears) an engine's exact executable.
    pub fn set_executable(&mut self, key: &str, executable: Option<&str>) {
        self.engine_mut(key).executable = executable.map(clean_path).filter(|s| !s.is_empty());
        if key == LLAMA_CPP {
            self.mirror();
        }
    }

    /// Sets (or clears) the folder an engine is searched in first.
    pub fn set_search_dir(&mut self, key: &str, dir: Option<&str>) {
        self.engine_mut(key).search_dir = dir.map(clean_path).filter(|s| !s.is_empty());
    }

    /// Adds a folder to search an engine in. Returns false if it was already there.
    pub fn add_search_dir(&mut self, key: &str, dir: &str) -> bool {
        let dir = clean_path(dir);
        let entry = self.engine_mut(key);
        if dir.is_empty() || entry.search_dirs().iter().any(|d| d.eq_ignore_ascii_case(&dir)) {
            return false;
        }
        match entry.search_dir {
            None => entry.search_dir = Some(dir),
            Some(_) => entry.more_search_dirs.push(dir),
        }
        true
    }

    /// Forgets one of an engine's search folders. Returns false if it was not there.
    pub fn remove_search_dir(&mut self, key: &str, dir: &str) -> bool {
        let dir = clean_path(dir);
        let entry = self.engine_mut(key);
        let before = entry.search_dirs().len();
        entry.more_search_dirs.retain(|d| !d.eq_ignore_ascii_case(&dir));
        if entry.search_dir.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&dir)) {
            entry.search_dir = (!entry.more_search_dirs.is_empty()).then(|| entry.more_search_dirs.remove(0));
        }
        entry.search_dirs().len() < before
    }

    /// Back to detection alone for an engine: folder and executable forgotten, models kept.
    pub fn reset_engine(&mut self, key: &str) {
        let entry = self.engine_mut(key);
        entry.search_dir = None;
        entry.more_search_dirs.clear();
        entry.executable = None;
        if key == LLAMA_CPP {
            self.mirror();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn search_folders_are_added_listed_and_forgotten() {
        let mut prefs = LocalPrefs::default();
        assert!(prefs.add_search_dir(STRATA, "D:\\Strata"));
        assert!(prefs.add_search_dir(STRATA, "E:\\Moteurs\\Strata"));
        assert!(!prefs.add_search_dir(STRATA, "d:\\strata"), "the same folder, written otherwise");
        assert_eq!(prefs.engine(STRATA).search_dirs(), ["D:\\Strata", "E:\\Moteurs\\Strata"]);
        // The first forgotten: the next takes its place, an older Po keeps reading one.
        assert!(prefs.remove_search_dir(STRATA, "D:\\Strata"));
        assert_eq!(prefs.engine(STRATA).search_dir.as_deref(), Some("E:\\Moteurs\\Strata"));
        assert!(!prefs.remove_search_dir(STRATA, "Z:\\nowhere"));
        prefs.reset_engine(STRATA);
        assert!(prefs.engine(STRATA).search_dirs().is_empty());
        // Read back from a file written before the list existed.
        let old: LocalPrefs = serde_json::from_str(r#"{"engines": {"strata": {"search_dir": "D:\\S"}}}"#).unwrap();
        assert_eq!(old.engine(STRATA).search_dirs(), ["D:\\S"]);
    }

    #[test]
    fn a_folder_is_added_only_once() {
        let mut prefs = LocalPrefs::default();
        assert!(prefs.add_models_dir("\"E:\\LLM\\models\""));
        if cfg!(windows) {
            assert!(!prefs.add_models_dir("e:\\llm\\MODELS"), "Windows ignores case");
            assert!(!prefs.add_models_dir("E:/LLM/models/"), "nor the separator or the final slash");
        }
        assert_eq!(prefs.llama_models_dirs(), vec!["E:\\LLM\\models".to_owned()]);
        assert_eq!(prefs.models_dirs, vec!["E:\\LLM\\models".to_owned()], "mirrored for an older Po");
    }

    #[test]
    fn the_format_reads_back_and_tolerates_missing_fields() {
        let prefs = LocalPrefs::parse("{}");
        assert_eq!(prefs, LocalPrefs::default());
        let mut prefs = LocalPrefs::default();
        prefs.add_models_dir("a");
        prefs.set_executable(LLAMA_CPP, Some("b"));
        prefs.set_search_dir(STRATA, Some("D:\\Strata"));
        let reread = LocalPrefs::parse(&serde_json::to_string(&prefs).unwrap());
        assert_eq!(reread, prefs);
    }

    #[test]
    fn the_first_version_is_folded_into_the_llama_engine() {
        // The exact file the user's machine holds, written by the first version.
        let old = "{\n  \"models_dirs\": [\n    \"E:\\\\LLM\\\\models\"\n  ],\n  \"llama_server\": \"E:\\\\LLM\\\\llama-bin\\\\llama-server.exe\"\n}\n";
        let prefs = LocalPrefs::parse(old);
        let llama = prefs.engine(LLAMA_CPP);
        assert_eq!(llama.models_dirs, vec!["E:\\LLM\\models".to_owned()]);
        assert_eq!(llama.executable.as_deref(), Some("E:\\LLM\\llama-bin\\llama-server.exe"));
        // Written back, an older Po still finds its two fields.
        let text = serde_json::to_string(&{
            let mut p = prefs.clone();
            p.mirror();
            p
        })
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["models_dirs"][0], "E:\\LLM\\models");
        assert_eq!(value["llama_server"], "E:\\LLM\\llama-bin\\llama-server.exe");
    }

    #[test]
    fn an_older_po_reads_the_new_file() {
        // The first version's structure, as an older Po declares it: it must read the new file.
        #[derive(Deserialize)]
        #[serde(default)]
        #[derive(Default)]
        struct Old {
            models_dirs: Vec<String>,
            llama_server: Option<String>,
        }
        let mut prefs = LocalPrefs::default();
        prefs.add_models_dir("E:\\m");
        prefs.set_executable(LLAMA_CPP, Some("E:\\bin\\llama-server.exe"));
        prefs.add_engine_models_dir(STRATA, "E:\\Strata-data");
        let old: Old = serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(old.models_dirs, vec!["E:\\m".to_owned()]);
        assert_eq!(old.llama_server.as_deref(), Some("E:\\bin\\llama-server.exe"));
    }

    #[test]
    fn a_broken_file_gives_the_defaults() {
        for text in ["", "{", "null", "[1,2]", "{\"models_dirs\": 3}", "\u{feff}{}", "{\"engines\": {\"x\": 1}}"] {
            assert!(LocalPrefs::parse(text).engines.is_empty(), "{text:?}");
        }
        // A byte order mark (Notepad) does not lose the content.
        let prefs = LocalPrefs::parse("\u{feff}{\"models_dirs\": [\"a\"]}");
        assert_eq!(prefs.llama_models_dirs(), vec!["a".to_owned()]);
    }

    #[test]
    fn engines_keep_their_settings_apart() {
        let mut prefs = LocalPrefs::default();
        prefs.set_executable(STRATA, Some("  \"D:\\Strata\\.venv\\Scripts\\python.exe\" "));
        prefs.add_engine_models_dir(STRATA, "D:\\Strata-data");
        assert_eq!(prefs.engine(STRATA).executable.as_deref(), Some("D:\\Strata\\.venv\\Scripts\\python.exe"));
        assert!(prefs.engine(LLAMA_CPP).is_empty());
        assert!(prefs.llama_executable().is_none(), "Strata's executable is not llama.cpp's");
        assert!(prefs.models_dirs.is_empty());
        prefs.reset_engine(STRATA);
        assert!(prefs.engine(STRATA).executable.is_none());
        assert_eq!(prefs.engine(STRATA).models_dirs.len(), 1, "a reset keeps the model folders");
        assert!(prefs.remove_engine_models_dir(STRATA, "d:\\strata-data\\") || !cfg!(windows));
    }

    #[test]
    fn removing_a_llama_folder_removes_its_legacy_copy() {
        let mut prefs = LocalPrefs::parse("{\"models_dirs\": [\"E:\\\\a\", \"E:\\\\b\"]}");
        assert!(prefs.remove_engine_models_dir(LLAMA_CPP, "E:\\a"));
        assert_eq!(prefs.llama_models_dirs(), vec!["E:\\b".to_owned()]);
        assert_eq!(prefs.models_dirs, vec!["E:\\b".to_owned()]);
        assert!(!prefs.remove_engine_models_dir(LLAMA_CPP, "E:\\zzz"));
    }

    #[test]
    fn a_model_profile_reads_the_users_words_and_refuses_the_rest() {
        let mut p = ModelProfile::default();
        assert!(p.set("contexte", "32k"));
        assert_eq!(p.context, Some(32_768));
        assert!(p.set("context", "131072"));
        assert!(!p.set("context", "12"), "a window too small to work in");
        assert!(!p.set("context", "beaucoup"));
        assert_eq!(p.context, Some(131_072), "a refused value changes nothing");
        assert!(p.set("cache", "Q8_0"));
        assert_eq!(p.cache.as_deref(), Some("q8_0"));
        assert!(!p.set("cache", "q3_k"));
        assert!(p.set("raisonnement", "haut"));
        assert_eq!(p.request_effort().as_deref(), Some("high"));
        assert!(p.set("reasoning", "off"));
        assert_eq!(p.request_effort().as_deref(), Some("none"));
        assert!(p.set("reasoning", "auto"));
        assert_eq!(p.request_effort(), None, "auto lets the server decide");
        assert!(p.set("temperature", "0,6"), "a French decimal comma");
        assert_eq!(p.temperature, Some(0.6));
        assert!(!p.set("temperature", "3"));
        assert!(p.set("top_p", "0.95") && p.set("top_k", "20") && p.set("min_p", "0"));
        assert!(!p.set("top_p", "1.5"));
        assert!(!p.set("couleur", "rouge"));
        assert!(p.set("temperature", "defaut"));
        assert_eq!(p.temperature, None);
        for key in ["context", "cache", "reasoning", "top_p", "top_k", "min_p"] {
            assert!(p.set(key, "default"));
        }
        assert!(p.is_empty());
    }

    #[test]
    fn model_profiles_and_game_mode_read_back_and_empty_ones_are_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("local.json");
        let mut prefs = LocalPrefs::default();
        prefs.model_mut("Qwen3.5-9B-Q4_K_M").set("temperature", "0.7");
        assert!(prefs.model_mut("Ornith").set("cartes", "0,1/mtp=0"));
        assert!(!prefs.model_mut("Ornith").set("cartes", "zéro"), "nothing changed by a wrong value");
        prefs.model_mut("untouched");
        prefs.game_mode = true;
        prefs.worker = Some(WorkerPrefs { model: Some("m.gguf".into()), gpu: Some("0".into()) });
        prefs.save_to(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("untouched"), "{text}");
        let reread = LocalPrefs::load_from(&file);
        assert_eq!(reread.model("Qwen3.5-9B-Q4_K_M").temperature, Some(0.7));
        assert_eq!(reread.model("Ornith").placement.map(|p| p.to_string()).as_deref(), Some("0,1/mtp=0"), "the cards chosen are remembered");
        let mut back = reread.model("Ornith");
        assert!(back.set("cartes", "auto") && back.placement.is_none(), "auto: Po chooses again");
        assert!(reread.game_mode);
        assert_eq!(reread.worker.unwrap().gpu.as_deref(), Some("0"));
        // Without these fields the file stays as it was: game mode off is not written.
        let plain = serde_json::to_string(&LocalPrefs::default()).unwrap();
        assert!(!plain.contains("game_mode") && !plain.contains("\"models\":{"), "{plain}");
    }

    #[test]
    fn saving_is_atomic_and_survives_concurrent_writers() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("po").join("local.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for j in 0..20 {
                        let mut prefs = LocalPrefs::default();
                        prefs.add_models_dir(&format!("E:\\models-{i}-{j}"));
                        let _ = prefs.save_to(&file);
                        // A reader never sees half a file.
                        let text = std::fs::read_to_string(&file).unwrap_or_default();
                        if !text.is_empty() {
                            assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok(), "torn file: {text}");
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let prefs = LocalPrefs::load_from(&file);
        assert_eq!(prefs.llama_models_dirs().len(), 1);
        let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap()).unwrap().flatten().filter(|e| e.path() != file).collect();
        assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
    }
}
