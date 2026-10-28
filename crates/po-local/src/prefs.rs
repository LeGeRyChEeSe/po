//! Local preferences saved by Po itself: model folders and the `llama-server` binary chosen by
//! the user during a session (`/local dossier`, `/local serveur`).
//!
//! A separate file that Po owns entirely: rewriting it cannot damage the configuration the
//! user wrote by hand.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalPrefs {
    /// GGUF model folders added by the user, scanned in addition to the usual locations.
    pub models_dirs: Vec<String>,
    /// Forced `llama-server` binary.
    pub llama_server: Option<String>,
}

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("po").join("local.json"))
}

impl LocalPrefs {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = path().ok_or_else(|| std::io::Error::other(po_i18n::t!("local.prefs.no_config_dir")))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, text + "\n")?;
        Ok(path)
    }

    /// Adds a model folder if it is not already there. Returns true if it was new.
    pub fn add_models_dir(&mut self, dir: &str) -> bool {
        let dir = dir.trim().trim_matches('"').to_owned();
        if dir.is_empty() || self.models_dirs.iter().any(|d| d.eq_ignore_ascii_case(&dir)) {
            return false;
        }
        self.models_dirs.push(dir);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_added_only_once() {
        let mut prefs = LocalPrefs::default();
        assert!(prefs.add_models_dir("\"E:\\LLM\\models\""));
        assert!(!prefs.add_models_dir("e:\\llm\\MODELS"), "Windows ignores case");
        assert_eq!(prefs.models_dirs, vec!["E:\\LLM\\models".to_owned()]);
    }

    #[test]
    fn the_format_reads_back_and_tolerates_missing_fields() {
        let prefs: LocalPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs, LocalPrefs::default());
        let prefs = LocalPrefs { models_dirs: vec!["a".into()], llama_server: Some("b".into()) };
        let reread: LocalPrefs = serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(reread, prefs);
    }
}
