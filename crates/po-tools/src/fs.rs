//! File tools: read, write, edit, list.

use async_trait::async_trait;
use po_core::tool::Permission;
use po_core::{ToolOutcome, ToolSpec};
use serde_json::{Value, json};

use crate::{Tool, ToolContext, required_str};

/// Beyond this size, a file can no longer be read in one go without saturating the context.
const LARGE_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read",
            "Reads a file from the workspace. The content is returned with line numbers. Use \
             `offset` and `limit` for a large file. An image (or a video, a sound) is attached to \
             the result when the model can read it.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the workspace"},
                    "offset": {"type": "integer", "description": "First line (default 1)"},
                    "limit": {"type": "integer", "description": "Number of lines to read"}
                },
                "required": ["path"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        // Reading is confined to the workspace by construction: nothing to arbitrate.
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let path = match required_str(&input, "path") {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err),
        };
        let resolved = match ctx.workspace.resolve_for_read(path) {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err.to_string()),
        };

        let metadata = match tokio::fs::metadata(&resolved).await {
            Ok(value) => value,
            Err(_) => return ToolOutcome::error(id, format!("{path}: file not found")),
        };
        if metadata.is_dir() {
            return ToolOutcome::error(id, format!("{path} is a directory - use `list`"));
        }

        if let Some((kind, mime)) = crate::media::media_type(path) {
            return read_media(id, path, &resolved, metadata.len(), kind, mime, ctx).await;
        }

        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1);
        let limit = input.get("limit").and_then(Value::as_u64);

        if metadata.len() > LARGE_FILE_BYTES && limit.is_none() {
            return ToolOutcome::error(
                id,
                format!(
                    "{path} weighs {} MiB - specify `offset` and `limit` to read it in slices",
                    metadata.len() / (1024 * 1024)
                ),
            );
        }

        let content = match tokio::fs::read(&resolved).await {
            Ok(bytes) => bytes,
            Err(err) => return ToolOutcome::error(id, format!("{path} : {err}")),
        };
        if is_binary(&content) {
            return ToolOutcome::error(id, format!("{path} is a binary file"));
        }
        let text = String::from_utf8_lossy(&content);

        let total = text.lines().count();
        let numbered: String = text
            .lines()
            .enumerate()
            .skip(offset as usize - 1)
            .take(limit.unwrap_or(u64::MAX) as usize)
            .map(|(index, line)| format!("{:>6}\t{line}\n", index + 1))
            .collect();

        if numbered.is_empty() && total > 0 {
            return ToolOutcome::error(
                id,
                format!("{path}: line {offset} is past the end of the file ({total} lines)"),
            );
        }

        ToolOutcome::ok(id, ctx.clamp(&numbered))
            .with_display(po_i18n::t!("tools.read.display", path = path, n = total))
    }
}

/// Reads a media file: it goes to the model as a separate block, if the model can read it.
/// Otherwise the model is told so instead of receiving unreadable bytes.
async fn read_media(
    id: &str,
    path: &str,
    resolved: &std::path::Path,
    size: u64,
    kind: crate::media::Kind,
    mime: &str,
    ctx: &ToolContext,
) -> ToolOutcome {
    if !ctx.media.accepts(kind) {
        return ToolOutcome::error(
            id,
            format!(
                "{path} is a {} file: the server does not announce that this model can read it",
                kind.label()
            ),
        );
    }
    if size as usize > kind.max_bytes() {
        return ToolOutcome::error(
            id,
            format!("{path} weighs {} MiB - too large to attach ({} MiB at most)", size >> 20, kind.max_bytes() >> 20),
        );
    }
    match tokio::fs::read(resolved).await {
        Ok(bytes) => ToolOutcome::ok(
            id,
            format!("{} {path} ({} KiB) attached to this result", kind.label(), bytes.len().div_ceil(1024)),
        )
        .with_display(po_i18n::t!("tools.read.media", kind = kind_name(kind), path = path))
        .with_attachment(po_core::ContentBlock::Image { media_type: mime.to_owned(), data: crate::media::base64(&bytes) }),
        Err(err) => ToolOutcome::error(id, format!("{path} : {err}")),
    }
}

/// Name of a media kind, for the user.
fn kind_name(kind: crate::media::Kind) -> &'static str {
    match kind {
        crate::media::Kind::Image => po_i18n::t!("tools.media.image"),
        crate::media::Kind::Video => po_i18n::t!("tools.media.video"),
        crate::media::Kind::Audio => po_i18n::t!("tools.media.audio"),
    }
}

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write",
            "Writes a file, creating it or replacing it entirely. To modify part of an existing \
             file, prefer `edit`.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        // Writing changes the user's disk: it is up to them to decide.
        Permission::Ask
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let (path, content) = match (required_str(&input, "path"), required_str(&input, "content")) {
            (Ok(path), Ok(content)) => (path, content),
            (Err(err), _) | (_, Err(err)) => return ToolOutcome::error(id, err),
        };
        let resolved = match ctx.workspace.resolve(path) {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err.to_string()),
        };

        if let Some(parent) = resolved.parent()
            && let Err(err) = tokio::fs::create_dir_all(parent).await
        {
            return ToolOutcome::error(id, format!("{path}: parent directory - {err}"));
        }
        let existed = resolved.exists();
        let before = if existed { tokio::fs::read_to_string(&resolved).await.unwrap_or_default() } else { String::new() };
        // A file in CRLF stays in CRLF: rewritten in `\n`, every line would show as changed.
        let converted;
        let content = if before.contains("\r\n") && !content.contains("\r\n") {
            converted = content.replace('\n', "\r\n");
            converted.as_str()
        } else {
            content
        };
        match tokio::fs::write(&resolved, content).await {
            Ok(()) => {
                let verb = if existed { "replaced" } else { "created" };
                let mut outcome = ToolOutcome::ok(id, format!("{path} {verb} ({} bytes)", content.len()))
                    .with_display(if existed {
                        po_i18n::t!("tools.write.replaced", path = path)
                    } else {
                        po_i18n::t!("tools.write.created", path = path)
                    });
                outcome.diff = crate::diff::line_diff(&before, content, DIFF_LINES);
                outcome
            }
            Err(err) => ToolOutcome::error(id, format!("{path} : {err}")),
        }
    }
}

/// Lines of a change kept for display; the counts always cover all of it.
const DIFF_LINES: usize = 80;

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "edit",
            "Replaces an exact portion of a file. `old` must appear exactly once, unless `all` \
             is true.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old": {"type": "string", "description": "Text to replace, exactly"},
                    "new": {"type": "string"},
                    "all": {"type": "boolean", "description": "Replace all occurrences"}
                },
                "required": ["path", "old", "new"]
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        Permission::Ask
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        let path = match required_str(&input, "path") {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err),
        };
        let (old, new) = match (required_str(&input, "old"), required_str(&input, "new")) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(err), _) | (_, Err(err)) => return ToolOutcome::error(id, err),
        };
        if old.is_empty() {
            // Empty text is found between every two characters: with `all`, `new` went in
            // everywhere and the file was lost.
            return ToolOutcome::error(id, "`old` is empty - give the exact text to replace, or use `write` for a new file");
        }
        if old == new {
            return ToolOutcome::error(id, "`old` and `new` are identical - no effect");
        }

        let resolved = match ctx.workspace.resolve(path) {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err.to_string()),
        };
        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(value) => value,
            Err(_) => return ToolOutcome::error(id, format!("{path}: file not found or binary")),
        };

        // A file with Windows line endings (CRLF), and a model that writes `\n`: the exact text is
        // not found, and the model goes round in circles trying to understand why (observed: a
        // Po Labs session spent its 40 steps on it). The text is then matched with the file's
        // line endings, and the replacement takes them too.
        let crlf = content.contains("\r\n");
        let to_crlf = |text: &str| text.replace("\r\n", "\n").replace('\n', "\r\n");
        let old: std::borrow::Cow<str> =
            if !content.contains(old) && crlf && old.contains('\n') && !old.contains("\r\n") { to_crlf(old).into() } else { old.into() };
        // The lines added keep the file's endings, even when `old` held a single line.
        let new: std::borrow::Cow<str> = if crlf && new.contains('\n') { to_crlf(new).into() } else { new.into() };
        let (old, new) = (old.as_ref(), new.as_ref());
        let occurrences = content.matches(old).count();
        let all = input.get("all").and_then(Value::as_bool).unwrap_or(false);

        // An ambiguous match is refused: applying the replacement at random would produce
        // a change that neither the model nor the user asked for.
        match occurrences {
            0 => {
                // A frequent slip: the line numbers shown by `read` copied into `old`.
                let numbered = old.lines().next().is_some_and(|line| {
                    let line = line.trim_start();
                    let digits = line.chars().take_while(char::is_ascii_digit).count();
                    digits > 0 && line[digits..].starts_with('\t')
                });
                let hint = if numbered { " - `old` starts with a line number from `read`: give the file's text only" } else { "" };
                return ToolOutcome::error(id, format!("{path}: text not found, nothing modified{hint}"));
            }
            n if n > 1 && !all => {
                return ToolOutcome::error(
                    id,
                    format!("{path}: {n} occurrences found - be more specific or pass `all: true`"),
                );
            }
            _ => {}
        }

        let updated = if all { content.replace(old, new) } else { content.replacen(old, new, 1) };
        match tokio::fs::write(&resolved, &updated).await {
            Ok(()) => {
                let count = if all { occurrences } else { 1 };
                let mut outcome = ToolOutcome::ok(id, format!("{path}: {count} replacement(s) made"))
                    .with_display(po_i18n::t!("tools.edit.display", path = path));
                outcome.diff = crate::diff::line_diff(&content, &updated, DIFF_LINES);
                outcome
            }
            Err(err) => ToolOutcome::error(id, format!("{path} : {err}")),
        }
    }
}

pub struct ListTool;

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "list",
            "Lists the contents of a workspace folder.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "Folder (root by default)"}}
            }),
        )
    }

    fn permission(&self, _input: &Value) -> Permission {
        Permission::Allow
    }

    async fn run(&self, id: &str, input: Value, ctx: &ToolContext) -> ToolOutcome {
        // An empty path means the root: this is what models send when the description says
        // "root by default".
        let path = input
            .get("path")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .unwrap_or(".");
        let resolved = match ctx.workspace.resolve(path) {
            Ok(value) => value,
            Err(err) => return ToolOutcome::error(id, err.to_string()),
        };

        let mut entries = match tokio::fs::read_dir(&resolved).await {
            Ok(value) => value,
            Err(_) => return ToolOutcome::error(id, format!("{path}: directory not found")),
        };

        let mut lines = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            lines.push(if is_dir { format!("{name}/") } else { name });
        }
        // A stable order makes turns reproducible and the prompt cache effective.
        lines.sort();

        if lines.is_empty() {
            return ToolOutcome::ok(id, format!("{path}: empty directory"));
        }
        let count = lines.len();
        ToolOutcome::ok(id, ctx.clamp(&lines.join("\n")))
            .with_display(po_i18n::t!("tools.list.display", path = path, n = count))
    }
}

/// A null byte in the first kilobytes reliably gives away a binary file.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;

    fn context() -> (tempfile::TempDir, ToolContext) {
        po_i18n::set_lang(po_i18n::Lang::Fr);
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(Workspace::new(dir.path()));
        (dir, ctx)
    }

    async fn write_file(ctx: &ToolContext, path: &str, content: &str) {
        let outcome = WriteTool
            .run("w", json!({"path": path, "content": content}), ctx)
            .await;
        assert!(!outcome.is_error, "{}", outcome.content);
    }

    #[tokio::test]
    async fn read_numbers_the_lines() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.txt", "one\ntwo\nthree\n").await;

        let outcome = ReadTool.run("r", json!({"path": "a.txt"}), &ctx).await;
        assert!(!outcome.is_error);
        assert!(outcome.content.contains("     1\tone"));
        assert!(outcome.content.contains("     3\tthree"));
        assert_eq!(outcome.display.as_deref(), Some("lu a.txt (3 lignes)"));
    }

    #[tokio::test]
    async fn reading_an_image_attaches_it_if_the_model_sees() {
        let (_dir, mut ctx) = context();
        write_file(&ctx, "view.png", "ABC").await;
        let denial = ReadTool.run("r", json!({"path": "view.png"}), &ctx).await;
        assert!(denial.is_error && denial.content.contains("does not announce"), "{}", denial.content);
        assert!(denial.attachments.is_empty());
        ctx.media.image = true;
        let read_back = ReadTool.run("r", json!({"path": "view.png"}), &ctx).await;
        assert!(!read_back.is_error, "{}", read_back.content);
        assert!(matches!(&read_back.attachments[..], [po_core::ContentBlock::Image { media_type, data }]
            if media_type == "image/png" && data == "QUJD"));
        write_file(&ctx, "clip.mp4", "ABC").await;
        let video = ReadTool.run("r", json!({"path": "clip.mp4"}), &ctx).await;
        assert!(video.is_error && video.content.contains("video"), "no video for a model that can only see");
    }

    #[tokio::test]
    async fn read_honours_offset_and_limit() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.txt", "1\n2\n3\n4\n5\n").await;

        let outcome = ReadTool.run("r", json!({"path": "a.txt", "offset": 2, "limit": 2}), &ctx).await;
        assert!(outcome.content.contains("     2\t2"));
        assert!(outcome.content.contains("     3\t3"));
        assert!(!outcome.content.contains("     4\t"));
    }

    #[tokio::test]
    async fn reading_a_missing_file_is_an_actionable_error() {
        let (_dir, ctx) = context();
        let outcome = ReadTool.run("r", json!({"path": "missing.txt"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("not found"));
    }

    #[tokio::test]
    async fn reading_outside_the_workspace_is_refused() {
        let (_dir, ctx) = context();
        let outcome = ReadTool.run("r", json!({"path": "../../secret"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("escapes the workspace"));
    }

    #[tokio::test]
    async fn a_binary_is_refused_rather_than_rendered_as_gibberish() {
        let (_dir, ctx) = context();
        let path = ctx.workspace.resolve("bin.dat").unwrap();
        tokio::fs::write(&path, [0u8, 1, 2, 3]).await.unwrap();

        let outcome = ReadTool.run("r", json!({"path": "bin.dat"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("binary"));
    }

    #[tokio::test]
    async fn write_creates_parent_directories() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a/b/c.txt", "content").await;
        assert!(ctx.workspace.resolve("a/b/c.txt").unwrap().exists());
    }

    #[tokio::test]
    async fn write_distinguishes_creation_from_replacement() {
        let (_dir, ctx) = context();
        let first_write = WriteTool.run("w", json!({"path": "a.txt", "content": "1"}), &ctx).await;
        assert!(first_write.content.contains("created"));
        let second_write = WriteTool.run("w", json!({"path": "a.txt", "content": "2"}), &ctx).await;
        assert!(second_write.content.contains("replaced"));
    }

    #[tokio::test]
    async fn edit_and_write_keep_windows_line_endings() {
        let (_dir, ctx) = context();
        let path = ctx.workspace.resolve("a.rs").unwrap();
        std::fs::write(&path, "fn a() {\r\n    green();\r\n}\r\n").unwrap();
        // The model writes `\n`, the file has `\r\n`.
        let outcome = EditTool
            .run("e", json!({"path": "a.rs", "old": "fn a() {\n    green();", "new": "fn a() {\n    orange();\n    more();"}), &ctx)
            .await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn a() {\r\n    orange();\r\n    more();\r\n}\r\n");

        WriteTool.run("w", json!({"path": "a.rs", "content": "fn b() {}\n"}), &ctx).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn b() {}\r\n", "a CRLF file stays CRLF");
    }

    #[tokio::test]
    async fn edit_replaces_a_single_occurrence() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "fn main() { println!(\"a\"); }").await;

        let outcome = EditTool
            .run("e", json!({"path": "a.rs", "old": "\"a\"", "new": "\"b\""}), &ctx)
            .await;
        assert!(!outcome.is_error, "{}", outcome.content);

        let path = ctx.workspace.resolve("a.rs").unwrap();
        assert!(tokio::fs::read_to_string(path).await.unwrap().contains("\"b\""));
    }

    #[tokio::test]
    async fn edit_refuses_an_ambiguous_match() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "let x = 1;\nlet x = 1;\n").await;

        let outcome = EditTool
            .run("e", json!({"path": "a.rs", "old": "let x = 1;", "new": "let y = 2;"}), &ctx)
            .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("2 occurrences"));

        // The file must not have moved.
        let path = ctx.workspace.resolve("a.rs").unwrap();
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap().matches("let x").count(), 2);
    }

    #[tokio::test]
    async fn edit_replaces_all_occurrences_on_request() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "a\na\na\n").await;

        let outcome = EditTool
            .run("e", json!({"path": "a.rs", "old": "a", "new": "b", "all": true}), &ctx)
            .await;
        assert!(!outcome.is_error);
        assert!(outcome.content.contains("3 replacement"));
    }

    #[tokio::test]
    async fn edit_with_missing_text_touches_nothing() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "content").await;
        let outcome = EditTool
            .run("e", json!({"path": "a.rs", "old": "missing", "new": "x"}), &ctx)
            .await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("not found"));
    }

    #[tokio::test]
    async fn an_empty_old_text_touches_nothing() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "abc").await;
        let outcome = EditTool.run("e", json!({"path": "a.rs", "old": "", "new": "X", "all": true}), &ctx).await;
        assert!(outcome.is_error);
        assert_eq!(std::fs::read_to_string(ctx.workspace.resolve("a.rs").unwrap()).unwrap(), "abc");
    }

    #[tokio::test]
    async fn lines_added_to_one_keep_the_files_endings() {
        let (_dir, ctx) = context();
        let path = ctx.workspace.resolve("a.rs").unwrap();
        std::fs::write(&path, "a();\r\nb();\r\n").unwrap();
        let outcome = EditTool.run("e", json!({"path": "a.rs", "old": "a();", "new": "a();\nc();"}), &ctx).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a();\r\nc();\r\nb();\r\n");
    }

    #[tokio::test]
    async fn line_numbers_copied_from_read_are_pointed_out() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "let x = 1;\n").await;
        let outcome = EditTool.run("e", json!({"path": "a.rs", "old": "     1\tlet x = 1;", "new": "let x = 2;"}), &ctx).await;
        assert!(outcome.is_error);
        assert!(outcome.content.contains("line number"), "{}", outcome.content);
    }

    #[tokio::test]
    async fn edit_without_change_is_refused() {
        let (_dir, ctx) = context();
        write_file(&ctx, "a.rs", "x").await;
        let outcome = EditTool
            .run("e", json!({"path": "a.rs", "old": "x", "new": "x"}), &ctx)
            .await;
        assert!(outcome.is_error);
    }

    #[tokio::test]
    async fn list_sorts_and_marks_folders() {
        let (_dir, ctx) = context();
        write_file(&ctx, "z.txt", "").await;
        write_file(&ctx, "a.txt", "").await;
        write_file(&ctx, "folder/x.txt", "").await;

        let outcome = ListTool.run("l", json!({}), &ctx).await;
        let lines: Vec<_> = outcome.content.lines().collect();
        assert_eq!(lines, ["a.txt", "folder/", "z.txt"]);
    }

    #[tokio::test]
    async fn an_empty_path_lists_the_root() {
        // Observed on the edit bench: Strata sends `path: ""` to mean "the root".
        let (_dir, ctx) = context();
        write_file(&ctx, "a.txt", "").await;
        let outcome = ListTool.run("l", json!({"path": ""}), &ctx).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(outcome.content.trim(), "a.txt");
    }

    #[test]
    fn writes_ask_for_user_approval() {
        assert_eq!(ReadTool.permission(&Value::Null), Permission::Allow);
        assert_eq!(ListTool.permission(&Value::Null), Permission::Allow);
        assert_eq!(WriteTool.permission(&Value::Null), Permission::Ask);
        assert_eq!(EditTool.permission(&Value::Null), Permission::Ask);
    }
}
