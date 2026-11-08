//! `@path` mentions in the user's request.
//!
//! Attaching the file to the message saves the model a tool round trip - reading what it was
//! pointed at - and on a local model a round trip costs an entire generation. An image, video
//! or sound goes out as a media block, if the server announces that the model can read it.

use po_core::{ContentBlock, Message, Role};
use po_tools::media::{Kind, media_type};
use po_tools::{Media, Workspace};

/// Beyond this, the file is truncated: a mention must not fill the window on its own.
const MAX_TEXT_BYTES: usize = 100 * 1024;

/// What the expansion attached, so it can be announced to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    Text { path: String, lines: usize, truncated: bool },
    Image { path: String, bytes: usize, kind: Kind },
    /// Media pointed at while the session's model cannot read it.
    SkippedImage { path: String, kind: Kind },
}

/// Builds the user's message from their input and the files it mentions.
pub fn build_user_message(input: &str, workspace: &Workspace, media: Media) -> (Message, Vec<Attachment>) {
    let mut text = input.to_owned();
    let mut images = Vec::new();
    let mut attachments = Vec::new();
    let mut seen = Vec::new();

    for raw in mentions(input) {
        if seen.contains(&raw) {
            continue;
        }
        seen.push(raw.clone());
        let Ok(path) = workspace.resolve(&raw) else { continue };
        if !path.is_file() {
            continue;
        }
        if let Some((kind, mime)) = media_type(&raw) {
            if !media.accepts(kind) {
                attachments.push(Attachment::SkippedImage { path: raw, kind });
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            if bytes.len() > kind.max_bytes() {
                continue;
            }
            attachments.push(Attachment::Image { path: raw, bytes: bytes.len(), kind });
            images.push(ContentBlock::Image { media_type: mime.into(), data: base64(&bytes) });
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        // A binary file has no place in a message's text.
        if bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let truncated = bytes.len() > MAX_TEXT_BYTES;
        let mut content = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_TEXT_BYTES)]).into_owned();
        if truncated {
            content.push_str("\n[… truncated: read the rest with the read tool]");
        }
        let lines = content.lines().count();
        text.push_str(&format!("\n\n<file path=\"{raw}\">\n{content}\n</file>"));
        attachments.push(Attachment::Text { path: raw, lines, truncated });
    }

    let mut content = vec![ContentBlock::text(text)];
    content.extend(images);
    (Message { role: Role::User, content }, attachments)
}

/// The `@path` mentions of a text. An `@` glued to a word (email address, `user@host`) is not one.
pub fn mentions(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let starts = chars[i] == '@' && (i == 0 || chars[i - 1].is_whitespace() || "([{\"'".contains(chars[i - 1]));
        if starts {
            let mut j = i + 1;
            while j < chars.len() && !chars[j].is_whitespace() && !"\"')]},;".contains(chars[j]) {
                j += 1;
            }
            let mut token: String = chars[i + 1..j].iter().collect();
            // Sentence-ending punctuation does not belong to the path.
            while token.ends_with(['.', ':', '!', '?']) {
                token.pop();
            }
            if !token.is_empty() {
                out.push(token);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// The mentions of a text that name an image file (by its extension): what an interface shows
/// as a thumbnail when it replays a conversation.
pub fn image_mentions(input: &str) -> Vec<String> {
    mentions(input)
        .into_iter()
        .filter(|m| {
            std::path::Path::new(m)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"))
        })
        .collect()
}

pub use po_tools::media::base64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_do_not_mistake_an_email_address() {
        assert_eq!(mentions("read @src/main.rs and write to me@example.com."), vec!["src/main.rs"]);
        assert_eq!(mentions("(@a.txt) then @b.md: end @c."), vec!["a.txt", "b.md", "c"]);
        assert!(mentions("no mention").is_empty());
    }

    #[test]
    fn only_image_files_are_shown_as_images() {
        assert_eq!(image_mentions("see @a.PNG, @b.rs and @c/d.webp."), vec!["a.PNG", "c/d.webp"]);
    }

    #[test]
    fn base64_follows_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_mentioned_file_is_attached_and_so_is_an_image() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "line 1\nline 2\n").unwrap();
        std::fs::write(dir.path().join("view.png"), [0x89, b'P', b'N', b'G']).unwrap();
        let ws = Workspace::new(dir.path());

        let (message, attached) = build_user_message("look at @note.txt and @view.png and @missing.rs", &ws, Media { image: true, ..Media::default() });
        assert_eq!(attached.len(), 2, "{attached:?}");
        let text = message.content[0].as_text();
        assert!(text.contains("<file path=\"note.txt\">") && text.contains("line 2"));
        assert!(matches!(&message.content[1], ContentBlock::Image { media_type, .. } if media_type == "image/png"));

        let (message, attached) = build_user_message("@view.png", &ws, Media::default());
        assert_eq!(message.content.len(), 1, "no image without a model that can see");
        assert!(matches!(attached[0], Attachment::SkippedImage { .. }));
    }

    #[test]
    fn a_mention_outside_the_workspace_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path());
        let (message, attached) = build_user_message("@../../etc/passwd", &ws, Media { image: true, ..Media::default() });
        assert!(attached.is_empty());
        assert_eq!(message.content.len(), 1);
    }
}
