//! Images, videos and sounds: recognizing a media file and encoding it for the model.
//!
//! A media file goes to the model as a separate block, in base64, and only if the server
//! announces that the model can read it: a model that cannot see would otherwise receive
//! megabytes of noise.

/// Kind of a media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Image,
    Video,
    Audio,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
        }
    }
}

/// Beyond this size, a media file is not attached: it would saturate the window and the server's memory.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_VIDEO_BYTES: usize = 50 * 1024 * 1024;

impl Kind {
    pub fn max_bytes(self) -> usize {
        match self {
            Kind::Image => MAX_IMAGE_BYTES,
            Kind::Video | Kind::Audio => MAX_VIDEO_BYTES,
        }
    }
}

impl crate::Media {
    pub fn accepts(&self, kind: Kind) -> bool {
        match kind {
            Kind::Image => self.image,
            Kind::Video => self.video,
            Kind::Audio => self.audio,
        }
    }
}

/// Kind and MIME type of a file, based on its extension.
pub fn media_type(path: &str) -> Option<(Kind, &'static str)> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit('.').next()?;
    Some(match ext {
        "png" => (Kind::Image, "image/png"),
        "jpg" | "jpeg" => (Kind::Image, "image/jpeg"),
        "gif" => (Kind::Image, "image/gif"),
        "webp" => (Kind::Image, "image/webp"),
        "bmp" => (Kind::Image, "image/bmp"),
        "mp4" | "m4v" => (Kind::Video, "video/mp4"),
        "webm" => (Kind::Video, "video/webm"),
        "mov" => (Kind::Video, "video/quicktime"),
        "mkv" => (Kind::Video, "video/x-matroska"),
        "avi" => (Kind::Video, "video/x-msvideo"),
        "mp3" => (Kind::Audio, "audio/mpeg"),
        "wav" => (Kind::Audio, "audio/wav"),
        "ogg" => (Kind::Audio, "audio/ogg"),
        "flac" => (Kind::Audio, "audio/flac"),
        "m4a" => (Kind::Audio, "audio/mp4"),
        _ => return None,
    })
}

/// Standard base64 encoding. A few lines beat one more dependency.
pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(TABLE[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_is_recognized_by_extension() {
        assert_eq!(media_type("a/Photo.JPG"), Some((Kind::Image, "image/jpeg")));
        assert_eq!(media_type("clip.mp4"), Some((Kind::Video, "video/mp4")));
        assert_eq!(media_type("note.wav"), Some((Kind::Audio, "audio/wav")));
        assert_eq!(media_type("main.rs"), None);
    }

    #[test]
    fn base64_follows_the_standard() {
        assert_eq!(base64(b"ABC"), "QUJD");
        assert_eq!(base64(b"AB"), "QUI=");
        assert_eq!(base64(b"A"), "QQ==");
    }
}
