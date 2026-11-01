//! Interface language: French or English, switchable on the fly.
//!
//! Every text shown to the user goes through a key (`t!("tui.toast.copied")`), looked up in TOML
//! catalogs embedded in the binary (`locales/*.toml`), one file per area of the code. The
//! language is a global value read on every draw: changing it mid-session (`/langue en`)
//! changes the screen on the next frame, without restarting.
//!
//! A text written for the model (system prompt, tool description) is not part of it: it is the
//! model that reads it, not the user.
//!
//! Format of an entry:
//!
//! ```toml
//! [tui.toast.mouse_on]
//! fr = "molette et doigt font défiler"
//! en = "wheel and finger scroll"
//! ```
//!
//! A parameter is written `{name}` and given to the macro: `t!("tui.agents.count", n = 3)`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// Embedded catalogs, one per area of the code: each can be filled without touching the others.
const CATALOGS: &[(&str, &str)] = &[
    ("common", include_str!("../locales/common.toml")),
    ("tui", include_str!("../locales/tui.toml")),
    ("tui_view", include_str!("../locales/tui_view.toml")),
    ("commands", include_str!("../locales/commands.toml")),
    ("chat", include_str!("../locales/chat.toml")),
    ("agent", include_str!("../locales/agent.toml")),
    ("labs", include_str!("../locales/labs.toml")),
    ("router", include_str!("../locales/router.toml")),
    ("local", include_str!("../locales/local.toml")),
    ("provider", include_str!("../locales/provider.toml")),
    ("tools", include_str!("../locales/tools.toml")),
    ("term", include_str!("../locales/term.toml")),
    ("core", include_str!("../locales/core.toml")),
    ("mcp", include_str!("../locales/mcp.toml")),
    ("menus", include_str!("../locales/menus.toml")),
    ("engines", include_str!("../locales/engines.toml")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Fr,
    En,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fr" | "français" | "francais" | "french" => Some(Lang::Fr),
            "en" | "anglais" | "english" => Some(Lang::En),
            _ => None,
        }
    }

    /// Name of the language in that language itself.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
        }
    }
}

/// 0: not chosen yet (first read), 1: French, 2: English.
static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Current language. On the first read: the saved one, otherwise the system's, otherwise
/// English.
pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::Fr,
        2 => Lang::En,
        _ => {
            // `PO_LANG` wins: scripts, tests, one-off trials.
            let lang = std::env::var("PO_LANG")
                .ok()
                .and_then(|l| Lang::parse(&l))
                .or_else(|| prefs::get_str("language").and_then(|l| Lang::parse(&l)))
                .unwrap_or_else(system_lang);
            set_lang(lang);
            lang
        }
    }
}

/// Changes the language for the whole interface, immediately.
pub fn set_lang(lang: Lang) {
    CURRENT.store(if lang == Lang::Fr { 1 } else { 2 }, Ordering::Relaxed);
}

/// Changes the language and remembers it for later sessions.
pub fn choose(lang: Lang) -> std::io::Result<()> {
    set_lang(lang);
    prefs::set_str("language", lang.code())
}

/// System language: `LANG`/`LC_ALL` on Unix, the user's language on Windows.
pub fn system_lang() -> Lang {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
            && value != "C"
            && value != "POSIX"
        {
            return if value.to_ascii_lowercase().starts_with("fr") { Lang::Fr } else { Lang::En };
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
        let mut buffer = [0u16; 85];
        // SAFETY: the buffer has the maximum size of a locale name (85).
        let len = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
        if len > 1 {
            let name = String::from_utf16_lossy(&buffer[..len as usize - 1]);
            if name.to_ascii_lowercase().starts_with("fr") {
                return Lang::Fr;
            }
        }
    }
    Lang::En
}

type Catalog = HashMap<String, [&'static str; 2]>;

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut out = Catalog::new();
        for (_, text) in CATALOGS {
            let Ok(value) = text.parse::<toml::Table>() else { continue };
            flatten(&value, String::new(), &mut out);
        }
        out
    })
}

/// Nested tables become dotted keys: `[tui.toast]` + `copied` = `tui.toast.copied`.
fn flatten(table: &toml::Table, prefix: String, out: &mut Catalog) {
    if let (Some(fr), Some(en)) = (table.get("fr").and_then(|v| v.as_str()), table.get("en").and_then(|v| v.as_str())) {
        let leak = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
        out.insert(prefix.clone(), [leak(fr), leak(en)]);
    }
    for (key, value) in table {
        if let Some(sub) = value.as_table() {
            let name = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
            flatten(sub, name, out);
        }
    }
}

/// Text of a key in the current language. An unknown key is displayed as is: it shows on
/// screen, and a test flags it beforehand.
pub fn text(key: &'static str) -> &'static str {
    match catalog().get(key) {
        Some(pair) => pair[usize::from(lang() == Lang::En)],
        None => key,
    }
}

/// Text of a key in a given language.
pub fn text_in(key: &str, lang: Lang) -> Option<&'static str> {
    catalog().get(key).map(|pair| pair[usize::from(lang == Lang::En)])
}

/// Text with parameters: each `{name}` is replaced by its value.
pub fn format(key: &'static str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = text(key).to_owned();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), &value.to_string());
    }
    out
}

/// `t!("key")` gives a `&'static str`; `t!("key", n = 3)` a `String`.
#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::text($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::format($key, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

/// Interface preferences (language, options), in `%APPDATA%\po\ui.json`.
pub mod prefs {
    use super::*;

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("po").join("ui.json"))
    }

    fn load() -> serde_json::Map<String, serde_json::Value> {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    fn store(map: serde_json::Map<String, serde_json::Value>) -> std::io::Result<()> {
        let path = path().ok_or_else(|| std::io::Error::other("configuration directory not found"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(&serde_json::Value::Object(map)).unwrap_or_default())
    }

    pub fn get_str(key: &str) -> Option<String> {
        load().get(key).and_then(|v| v.as_str()).map(str::to_owned)
    }

    pub fn set_str(key: &str, value: &str) -> std::io::Result<()> {
        let mut map = load();
        map.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));
        store(map)
    }

    pub fn get_bool(key: &str) -> Option<bool> {
        load().get(key).and_then(|v| v.as_bool())
    }

    pub fn set_bool(key: &str, value: bool) -> std::io::Result<()> {
        let mut map = load();
        map.insert(key.to_owned(), serde_json::Value::Bool(value));
        store(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{name}` parameters of a text.
    fn params(text: &str) -> Vec<String> {
        let mut out: Vec<String> = text
            .match_indices('{')
            .filter_map(|(i, _)| text[i + 1..].find('}').map(|end| text[i + 1..i + 1 + end].to_owned()))
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn every_catalog_parses_and_every_key_exists_in_both_languages() {
        for (name, text) in CATALOGS {
            let table = text.parse::<toml::Table>().unwrap_or_else(|e| panic!("locales/{name}.toml unreadable: {e}"));
            let mut keys = Catalog::new();
            flatten(&table, String::new(), &mut keys);
            for (key, [fr, en]) in keys {
                assert!(!fr.trim().is_empty() && !en.trim().is_empty(), "{name}: {key} is empty in one language");
                assert_eq!(params(fr), params(en), "{name}: {key} does not have the same parameters in fr and en");
            }
        }
    }

    #[test]
    fn no_key_is_defined_twice() {
        let mut seen: HashMap<String, &str> = HashMap::new();
        for (name, text) in CATALOGS {
            let table = text.parse::<toml::Table>().unwrap();
            let mut keys = Catalog::new();
            flatten(&table, String::new(), &mut keys);
            for key in keys.into_keys() {
                if let Some(other) = seen.insert(key.clone(), name) {
                    panic!("{key} defined in {other} and in {name}");
                }
            }
        }
    }

    /// Every key used in the code exists: otherwise a typo would show up on screen.
    #[test]
    fn every_key_used_in_the_code_exists() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut missing = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // The crate itself cites example keys in its documentation.
                    if path.file_name().is_some_and(|n| n != "target" && n != "po-i18n") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap_or_default();
                let mut rest = source.as_str();
                while let Some(at) = rest.find("t!(\"") {
                    // `format!(`, `print!(`… also end with "t!(": only the macro counts.
                    let before = &rest[..at];
                    let is_macro = !before.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
                    let after = &rest[at + 4..];
                    let end = after.find('"').unwrap_or(0);
                    let key = &after[..end];
                    if is_macro && !key.is_empty() && !key.contains(' ') && !catalog().contains_key(key) {
                        missing.push(format!("{} : {key}", path.display()));
                    }
                    rest = &after[end..];
                }
            }
        }
        assert!(missing.is_empty(), "keys missing from the catalogs:\n{}", missing.join("\n"));
    }

    #[test]
    fn the_language_changes_on_the_fly() {
        set_lang(Lang::Fr);
        assert_eq!(text("common.yes"), "oui");
        set_lang(Lang::En);
        assert_eq!(text("common.yes"), "yes");
        assert_eq!(text("unknown.key"), "unknown.key", "an unknown key shows");
        assert_eq!(t!("common.count_items", n = 3), "3 items");
    }
}
