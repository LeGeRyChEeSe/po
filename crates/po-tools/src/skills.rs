//! Skills: specialized instructions that the model loads on demand.
//!
//! A skill is a folder containing a `SKILL.md`, whose header gives a name and a description.
//! Only these two fields are announced to the model, in the description of the `skill` tool;
//! the body is read only if the model asks for it. A session can thus carry dozens of skills
//! for the price of a few lines of prompt.
//!
//! The format is the one other harnesses already use: Po also reads their folders
//! (`.claude/skills`), so the user has nothing to copy over.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{ToolOutcome, ToolSpec};
use serde_json::{Value, json};

use crate::{Tool, ToolContext, required_str};

/// Description announced to the model, bounded: a verbose skill must not bloat the prompt of
/// every request.
const MAX_DESCRIPTION: usize = 200;
/// Beyond this, the list would cost more than it brings; the following skills remain loadable
/// by name.
const MAX_LISTED: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Path of the `SKILL.md`.
    pub path: PathBuf,
    /// Root folder that provided it, for `/skills`.
    pub source: String,
}

/// Folders scanned, from highest to lowest priority: for the same name, the project's skill
/// hides the user's.
pub fn skill_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = vec![workspace.join(".po").join("skills"), workspace.join(".claude").join("skills")];
    if let Some(config) = dirs::config_dir() {
        roots.push(config.join("po").join("skills"));
    }
    if let Some(home) = dirs::home_dir() {
        roots.push(home.join(".po").join("skills"));
        roots.push(home.join(".claude").join("skills"));
    }
    roots
}

pub fn discover(workspace: &Path) -> Vec<Skill> {
    discover_in(&skill_roots(workspace))
}

pub fn discover_in(roots: &[PathBuf]) -> Vec<Skill> {
    let mut skills: Vec<Skill> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        let mut dirs: Vec<PathBuf> =
            entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        for dir in dirs {
            let Some(file) = skill_file(&dir) else { continue };
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let fallback = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned();
            let (name, description) = parse_header(&text, &fallback);
            if name.is_empty() || skills.iter().any(|s| s.name == name) {
                continue;
            }
            skills.push(Skill { name, description, path: file, source: root.display().to_string() });
        }
    }
    skills
}

fn skill_file(dir: &Path) -> Option<PathBuf> {
    ["SKILL.md", "skill.md", "Skill.md"].iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Reads `name` and `description` from the YAML header, without depending on a YAML parser:
/// the two fields fit on one line each in every format encountered. Without a header, the
/// folder name and the first line of text will do.
pub fn parse_header(text: &str, fallback_name: &str) -> (String, String) {
    let mut name = String::new();
    let mut description = String::new();
    let body = text.trim_start_matches('\u{feff}');
    let mut rest = body;
    if let Some(after) = body.strip_prefix("---")
        && let Some(end) = after.find("\n---")
    {
        let lines: Vec<&str> = after[..end].lines().collect();
        for (index, raw) in lines.iter().enumerate() {
            let line = raw.trim();
            if let Some(v) = line.strip_prefix("name:") {
                name = unquote(v);
            } else if let Some(v) = line.strip_prefix("description:") {
                let v = v.trim();
                // Block scalar (`>-`, `|`): the value is on the indented lines that follow.
                // Observed in real skills.
                description = if v.starts_with('>') || v.starts_with('|') {
                    lines[index + 1..]
                        .iter()
                        .take_while(|l| l.starts_with(' ') || l.starts_with('\t') || l.trim().is_empty())
                        .map(|l| l.trim())
                        .collect::<Vec<_>>()
                        .join(" ")
                } else {
                    unquote(v)
                };
            }
        }
        rest = &after[end + 4..];
    }
    if name.is_empty() {
        name = fallback_name.to_owned();
    }
    if description.is_empty() {
        description = rest
            .lines()
            .map(|l| l.trim().trim_start_matches('#').trim())
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_owned();
    }
    (name, shorten(&description, MAX_DESCRIPTION))
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches('"').trim_matches('\'').trim().to_owned()
}

fn shorten(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// The `skill` tool: returns the content of a skill.
pub struct SkillTool {
    skills: Vec<Skill>,
}

impl SkillTool {
    pub fn new(skills: Vec<Skill>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn spec(&self) -> ToolSpec {
        let mut listing = String::new();
        for skill in self.skills.iter().take(MAX_LISTED) {
            listing.push_str(&format!("\n- {}: {}", skill.name, skill.description));
        }
        if self.skills.len() > MAX_LISTED {
            listing.push_str(&format!("\n… and {} more, loadable by name", self.skills.len() - MAX_LISTED));
        }
        ToolSpec::new(
            "skill",
            format!(
                "Loads the instructions of a skill when the task matches its description. Read them \
                 before acting, then follow them.\nAvailable skills:{listing}"
            ),
            json!({
                "type": "object",
                "properties": {"name": {"type": "string", "description": "Skill name"}},
                "required": ["name"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        // Reading instructions acts on nothing.
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let name = match required_str(&input, "name") {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err),
        };
        let Some(skill) = self.skills.iter().find(|s| s.name.eq_ignore_ascii_case(name.trim())) else {
            let known: Vec<_> = self.skills.iter().map(|s| s.name.as_str()).collect();
            return ToolOutcome::error(id, format!("unknown skill: {name}. Available: {}", known.join(", ")));
        };
        match std::fs::read_to_string(&skill.path) {
            Ok(text) => {
                let dir = skill.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
                ToolOutcome::ok(id, ctx.clamp(&format!("Skill folder: {dir}\n\n{text}")))
                    .with_display(po_i18n::t!("tools.skill.display", name = skill.name))
            }
            Err(err) => ToolOutcome::error(id, format!("cannot read {}: {err}", skill.path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_gives_the_name_and_description() {
        let (name, desc) = parse_header("---\nname: deploy\ndescription: \"Deploys the app\"\n---\n# Body", "x");
        assert_eq!(name, "deploy");
        assert_eq!(desc, "Deploys the app");
    }

    #[test]
    fn a_yaml_block_description_is_read() {
        let text = "---\nname: deploy\ndescription: >-\n  Deploys the project\n  to the cloud.\nlicense: MIT\n---\n";
        let (_, desc) = parse_header(text, "x");
        assert_eq!(desc, "Deploys the project to the cloud.");
    }

    #[test]
    fn without_a_header_the_folder_and_first_line_are_used() {
        let (name, desc) = parse_header("\n# Code review\nDetails", "review");
        assert_eq!(name, "review");
        assert_eq!(desc, "Code review");
    }

    #[test]
    fn a_long_description_is_bounded() {
        let long = "word ".repeat(200);
        let (_, desc) = parse_header(&format!("---\nname: a\ndescription: {long}\n---"), "a");
        assert!(desc.chars().count() <= MAX_DESCRIPTION);
        assert!(desc.ends_with('…'));
    }

    #[test]
    fn the_project_hides_the_user_for_the_same_name() {
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        for (root, text) in [(&project, "project version"), (&user, "user version")] {
            let dir = root.path().join("review");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("SKILL.md"), format!("---\nname: review\ndescription: {text}\n---\n")).unwrap();
        }
        let found = discover_in(&[project.path().to_path_buf(), user.path().to_path_buf()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "project version");
    }

    #[tokio::test]
    async fn the_tool_returns_the_content_and_the_folder() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("deploy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: deploy\ndescription: d\n---\nStep 1").unwrap();
        let tool = SkillTool::new(discover_in(&[root.path().to_path_buf()]));
        assert!(tool.spec().description.contains("deploy: d"));
        let ws = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(crate::Workspace::new(ws.path()));
        let output = tool.run("1", json!({"name": "Deploy"}), &ctx).await;
        assert!(!output.is_error, "{}", output.content);
        assert!(output.content.contains("Step 1") && output.content.contains("Skill folder"));
        let unknown = tool.run("2", json!({"name": "other"}), &ctx).await;
        assert!(unknown.is_error && unknown.content.contains("deploy"));
    }
}
