//! Subagent profiles: built-in roles (explorer, planner, builder, verifier, reviewer,
//! summarizer) and custom subagents, written in `.po/agents/<name>.md` (project-specific) or in
//! Po's configuration folder (permanent).
//!
//! A profile says **what the subagent does** (its rules), **with what** (read-only or full
//! tools) and, if needed, **on which model**. By default it is the session's model: a subagent
//! is a second thread of work of the same model, not a discount model.
//!
//! File format - a short header, then the rules as free text. The `tools` value is written
//! `lecture` or `complet` (kept for compatibility with existing files; `read` and `full` are
//! accepted too):
//!
//! ```text
//! ---
//! name: test-runner
//! description: Runs the test suite and summarizes the failures.
//! tools: complet
//! model: session
//! ---
//! Rules the subagent follows to the letter…
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use po_router::TaskClass;
use serde::{Deserialize, Serialize};

/// Tools granted to a subagent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolScope {
    /// Read, list, search: nothing changes on disk.
    ReadOnly,
    /// All the session's tools, including writing and the shell.
    Full,
}

impl ToolScope {
    /// Name shown to the user, in the current language.
    pub fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => po_i18n::t!("agent.subagents.scope_read"),
            Self::Full => po_i18n::t!("agent.subagents.scope_full"),
        }
    }

    /// Value written to profile files. `parse` still reads the French words of the first
    /// versions (`lecture`, `complet`).
    fn file_value(self) -> &'static str {
        match self {
            Self::ReadOnly => "read",
            Self::Full => "full",
        }
    }

    /// Name given to the model in the subagent catalog.
    pub(crate) fn model_label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Full => "full",
        }
    }

    /// Accepts `read`/`full` (and the French `lecture`/`complet`), and also a tool list in the
    /// style of other harnesses (`Read, Grep, Bash`): as soon as a tool writes or executes,
    /// access is full. Anything else, `read` included, is read-only.
    pub fn parse(raw: &str) -> Self {
        let lower = raw.to_lowercase();
        let full = ["complet", "full", "all", "tous", "write", "edit", "bash", "shell"];
        if full.iter().any(|w| lower.contains(w)) { Self::Full } else { Self::ReadOnly }
    }
}

/// Where a profile comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Builtin,
    /// The user's configuration folder: available in every project.
    Global(PathBuf),
    /// The workspace's `.po/agents`: specific to the project.
    Project(PathBuf),
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Builtin => po_i18n::t!("agent.subagents.source_builtin"),
            Self::Global(_) => po_i18n::t!("agent.subagents.source_global"),
            Self::Project(_) => po_i18n::t!("agent.subagents.source_project"),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Builtin => None,
            Self::Global(p) | Self::Project(p) => Some(p),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub description: String,
    /// The subagent's rules: its system instruction.
    pub rules: String,
    pub tools: ToolScope,
    /// Model written in the profile (`session`, `auto` or an identifier). The user's settings
    /// take precedence.
    pub model: Option<String>,
    pub source: Source,
}

impl Profile {
    /// Task class, for the router when the model is left at `auto`.
    pub fn class(&self) -> TaskClass {
        match self.name.as_str() {
            "explorer" => TaskClass::Search,
            "summarizer" => TaskClass::Summarize,
            "builder" => TaskClass::Edit,
            "verifier" => TaskClass::Test,
            "planner" | "reviewer" => TaskClass::Review,
            _ if self.tools == ToolScope::Full => TaskClass::Edit,
            _ => TaskClass::Review,
        }
    }

    /// Text of the file describing this profile.
    pub fn render(&self) -> String {
        let mut text = format!(
            "---\nname: {}\ndescription: {}\ntools: {}\n",
            self.name,
            self.description.replace('\n', " "),
            self.tools.file_value()
        );
        if let Some(model) = &self.model {
            text.push_str(&format!("model: {model}\n"));
        }
        text.push_str("---\n");
        text.push_str(self.rules.trim());
        text.push('\n');
        text
    }
}

fn builtin(name: &str, description: &str, tools: ToolScope, rules: &str) -> Profile {
    Profile {
        name: name.into(),
        description: description.into(),
        rules: rules.into(),
        tools,
        model: None,
        source: Source::Builtin,
    }
}

/// Built-in roles. Their rules are short: a local model follows three crisp instructions
/// better than a page of nuances.
pub fn builtins() -> Vec<Profile> {
    vec![
        builtin(
            "explorer",
            "Explores the code: finds files, definitions and usages, without modifying anything.",
            ToolScope::ReadOnly,
            "You explore the workspace to answer the question asked.\n\
             - Search with glob and grep before reading; read only the useful passages.\n\
             - Return verified facts, each with its location `path:line`.\n\
             - Do not interpret beyond what you read; say what you did not find.",
        ),
        builtin(
            "planner",
            "Studies the relevant code and returns a numbered implementation plan, without modifying anything.",
            ToolScope::ReadOnly,
            "You prepare an implementation plan for the requested task.\n\
             - Read the relevant code before proposing anything.\n\
             - Return numbered steps: file, change, reason.\n\
             - End with the risks and how to verify the result.\n\
             - Do not modify any file.",
        ),
        builtin(
            "builder",
            "Implements a precise change in the code, then verifies it.",
            ToolScope::Full,
            "You implement exactly the requested change.\n\
             - Read the code before editing it; respect its style and conventions.\n\
             - Change the minimum necessary; touch nothing else.\n\
             - Verify your work (build, tests) when possible.\n\
             - Return the list of modified files and what you verified.",
        ),
        builtin(
            "verifier",
            "Verifies by execution (build, tests, commands) and returns a proven verdict, without fixing.",
            ToolScope::Full,
            "You verify, you do not fix.\n\
             - Run the commands that prove the result (build, tests, launch).\n\
             - For each point: `OK: <point>` or `FAIL: <point> - <evidence>`.\n\
             - Quote the output that justifies each verdict; assume nothing.\n\
             - Do not modify any file.",
        ),
        builtin(
            "reviewer",
            "Reviews code or a diff: bugs, edge cases, regressions, ranked by severity.",
            ToolScope::ReadOnly,
            "You review the indicated code looking for real defects.\n\
             - Bugs, edge cases, regressions, inconsistencies with neighboring code.\n\
             - Each remark: severity, `path:line`, problem, scenario that triggers it.\n\
             - No style remarks without consequence; do not rewrite the code.",
        ),
        builtin(
            "summarizer",
            "Faithfully summarizes files or content, adding nothing.",
            ToolScope::ReadOnly,
            "You faithfully summarize what is requested.\n\
             - Keep the exact facts, decisions, figures and names.\n\
             - Add nothing that is not in the source.\n\
             - Return dense text, without preamble.",
        ),
    ]
}

/// Reads a profile file. `None` if the header is missing or the name is invalid.
pub fn parse(text: &str, source: Source) -> Option<Profile> {
    let body = text.trim_start_matches('\u{feff}');
    let after = body.strip_prefix("---")?;
    let end = after.find("\n---")?;
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for line in after[..end].lines() {
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim().trim_matches('"').trim_matches('\'').trim().to_owned();
            fields.insert(key.trim().to_lowercase(), value);
        }
    }
    let rules = after[end + 4..].trim_start_matches('-').trim().to_owned();
    // The French keys (`nom`, `modele`, `outils`) stay accepted for existing files.
    let name = fields.get("name").or_else(|| fields.get("nom"))?.to_lowercase();
    if !valid_name(&name) || rules.is_empty() {
        return None;
    }
    let model = fields
        .get("model")
        .or_else(|| fields.get("modele"))
        .filter(|m| !m.is_empty())
        .cloned();
    Some(Profile {
        name,
        description: fields.get("description").cloned().unwrap_or_default(),
        rules,
        tools: fields.get("tools").or_else(|| fields.get("outils")).map(|t| ToolScope::parse(t)).unwrap_or(ToolScope::ReadOnly),
        model,
        source,
    })
}

/// Subagent name: lowercase letters, digits and hyphens, a file name that is safe everywhere.
pub fn valid_name(name: &str) -> bool {
    (2..=40).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// Reduces free text to a valid name (`Test Runner` → `test-runner`).
pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().to_lowercase().chars() {
        let c = match c {
            'à' | 'â' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'î' | 'ï' => 'i',
            'ô' | 'ö' => 'o',
            'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(40).collect::<String>().trim_end_matches('-').to_owned()
}

/// Folder of the permanent subagents.
pub fn global_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("po").join("agents"))
}

/// Folder of the project's subagents.
pub fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join(".po").join("agents")
}

fn read_dir_profiles(dir: &Path, make: impl Fn(PathBuf) -> Source) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("md")))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            parse(&text, make(path))
        })
        .collect()
}

/// Available profiles: built-in roles, then permanent ones, then the project's. For equal
/// names the one closest to the project wins - this is how a built-in role is tuned for a
/// project.
pub fn discover(workspace: &Path, global: Option<&Path>) -> Vec<Profile> {
    let mut profiles = builtins();
    let mut layers = Vec::new();
    if let Some(dir) = global {
        layers.push(read_dir_profiles(dir, Source::Global));
    }
    layers.push(read_dir_profiles(&project_dir(workspace), Source::Project));
    for layer in layers {
        for profile in layer {
            match profiles.iter_mut().find(|p| p.name == profile.name) {
                Some(slot) => *slot = profile,
                None => profiles.push(profile),
            }
        }
    }
    profiles
}

/// Subagent settings, common to all sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubagentSettings {
    /// Simultaneous sessions allowed on the session's server, main agent included.
    /// `None`: the value announced by the server (its slots).
    pub max_parallel: Option<usize>,
    /// Model per role: `session`, `auto` or an identifier. Absent: the profile's, otherwise
    /// the session's model.
    pub models: BTreeMap<String, String>,
}

impl SubagentSettings {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("po").join("subagents.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path = Self::path().ok_or_else(|| std::io::Error::other(po_i18n::t!("agent.subagents.config_dir_missing")))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, text + "\n")?;
        Ok(path)
    }
}

/// Instruction given to the session's model to write a subagent from a description. The format
/// is imposed line by line: more robust than JSON for a local model.
pub const GENERATION_SYSTEM: &str = "You write the definition of a subagent for a development harness. \
The subagent will receive one task at a time, without seeing the main conversation, and will follow its rules to the letter. \
Answer ONLY in this format, with nothing before or after:\n\
name: <short lowercase name, words separated by hyphens>\n\
description: <one sentence: when to hand it a task>\n\
tools: <read if the subagent only needs to read and search, full if it must write files or run commands>\n\
---\n\
<rules: 3 to 8 bullets starting with \"- \", imperative, verifiable, faithful to the request, and the exact form of the result to return>";

/// Reads the model's reply. Tolerates a surrounding code block, capital letters and French
/// field names; returns `None` if the rules or a usable name are missing.
pub fn parse_generated(reply: &str, source: Source) -> Option<Profile> {
    let cleaned: String = reply
        .lines()
        .filter(|l| !l.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n");
    // Reasoning models sometimes leave their tags behind.
    let cleaned = match cleaned.rfind("</think>") {
        Some(i) => cleaned[i + 8..].to_owned(),
        None => cleaned,
    };
    let (head, rules) = cleaned.split_once("\n---")?;
    let mut name = None;
    let mut description = String::new();
    let mut tools = ToolScope::ReadOnly;
    for line in head.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches('*').trim_matches('`').trim();
        match key.trim().trim_matches('*').trim().to_lowercase().as_str() {
            "name" | "nom" => name = Some(slugify(value)),
            "description" => description = value.to_owned(),
            "tools" | "outils" => tools = ToolScope::parse(value),
            _ => {}
        }
    }
    let name = name.filter(|n| valid_name(n))?;
    let rules = rules.trim_start_matches('-').trim().to_owned();
    if rules.is_empty() {
        return None;
    }
    Some(Profile { name, description, rules, tools, model: None, source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_file_reads_back_identically() {
        let profile = Profile {
            name: "test-runner".into(),
            description: "Runs the tests.".into(),
            rules: "- run cargo test\n- summarize the failures".into(),
            tools: ToolScope::Full,
            model: Some("session".into()),
            source: Source::Builtin,
        };
        let reread = parse(&profile.render(), Source::Builtin).unwrap();
        assert_eq!(reread, profile);
    }

    #[test]
    fn another_harness_tool_list_is_understood() {
        assert_eq!(ToolScope::parse("Read, Grep, Glob"), ToolScope::ReadOnly);
        assert_eq!(ToolScope::parse("Read, Edit, Bash"), ToolScope::Full);
        assert_eq!(ToolScope::parse("lecture"), ToolScope::ReadOnly);
        assert_eq!(ToolScope::parse("read"), ToolScope::ReadOnly);
        assert_eq!(ToolScope::parse("full"), ToolScope::Full);
        assert_eq!(ToolScope::parse("complet"), ToolScope::Full);
    }

    #[test]
    fn a_project_profile_replaces_the_built_in_role_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let agents = project_dir(dir.path());
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("reviewer.md"), "---\nname: reviewer\ntools: read\n---\n- house rules\n").unwrap();
        std::fs::write(agents.join("doc.md"), "---\nname: doc-writer\ndescription: writes the docs\ntools: full\n---\n- write\n").unwrap();
        std::fs::write(agents.join("broken.md"), "no header").unwrap();
        let profiles = discover(dir.path(), None);
        let reviewer = profiles.iter().find(|p| p.name == "reviewer").unwrap();
        assert_eq!(reviewer.rules, "- house rules");
        assert!(matches!(reviewer.source, Source::Project(_)));
        assert!(profiles.iter().any(|p| p.name == "doc-writer" && p.tools == ToolScope::Full));
        assert_eq!(profiles.len(), builtins().len() + 1, "the invalid file is ignored");
    }

    #[test]
    fn the_model_reply_becomes_a_profile() {
        let reply = "<think>let me think</think>\n```\nName: Vérificateur SQL\ndescription: Reviews the migrations.\ntools: read\n---\n- read every migration\n- flag the DROPs\n```";
        let p = parse_generated(reply, Source::Builtin).unwrap();
        assert_eq!(p.name, "verificateur-sql");
        assert_eq!(p.tools, ToolScope::ReadOnly);
        assert_eq!(p.rules, "- read every migration\n- flag the DROPs");
        assert!(parse_generated("name: x\n---\n", Source::Builtin).is_none(), "name too short, empty rules");
    }

    #[test]
    fn names_are_safe_for_a_file() {
        assert!(valid_name("test-runner"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("A"));
        assert_eq!(slugify("  Écrivain de Docs !! "), "ecrivain-de-docs");
    }
}
