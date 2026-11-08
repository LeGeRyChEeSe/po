//! Information on a server left running in the background.

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackgroundServerInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub port: u16,
    pub engine: String,
    pub model: String,
    pub endpoint: String,
    pub started_at: u64,
}

impl BackgroundServerInfo {
    pub fn file_path() -> Option<PathBuf> {
        crate::prefs::path().and_then(|p| p.parent().map(|d| d.join("background_server.json")))
    }

    pub fn load() -> Option<Self> {
        let path = Self::file_path()?;
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::file_path() else {
            return Err(std::io::Error::other("no config directory"));
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)
    }

    pub fn forget() {
        if let Some(path) = Self::file_path() {
            let _ = std::fs::remove_file(path);
        }
    }

    pub async fn is_alive(&self) -> bool {
        let client = match reqwest::Client::builder().timeout(std::time::Duration::from_millis(500)).build() {
            Ok(c) => c,
            Err(_) => return false,
        };
        client.get(format!("{}/models", self.endpoint)).send().await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn background_server_info_serde() {
        let info = BackgroundServerInfo {
            pid: Some(12345),
            port: 18080,
            engine: "llama.cpp".to_string(),
            model: "qwen2.5".to_string(),
            endpoint: "http://127.0.0.1:18080/v1".to_string(),
            started_at: 1700000000,
        };
        let serialized = serde_json::to_string(&info).unwrap();
        let parsed: BackgroundServerInfo = serde_json::from_str(&serialized).unwrap();
        assert_eq!(info, parsed);
    }

    #[test]
    fn background_server_info_file_cycle() {
        let _guard = TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let prefs_file = dir.path().join("local.json");
        unsafe { std::env::set_var("PO_LOCAL_PREFS", &prefs_file); }

        assert!(BackgroundServerInfo::load().is_none());

        let info = BackgroundServerInfo {
            pid: Some(42),
            port: 8080,
            engine: "strata".to_string(),
            model: "flash".to_string(),
            endpoint: "http://127.0.0.1:8080/v1".to_string(),
            started_at: 1700000001,
        };
        info.save().unwrap();

        let loaded = BackgroundServerInfo::load().expect("should load saved info");
        assert_eq!(loaded, info);

        BackgroundServerInfo::forget();
        assert!(BackgroundServerInfo::load().is_none());

        unsafe { std::env::remove_var("PO_LOCAL_PREFS"); }
    }
}
