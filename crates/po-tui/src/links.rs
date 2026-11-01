//! Links in the model's answers: `[text](https://…)`, or an address written as is. They are
//! drawn underlined; a click on one opens it in the browser.
//!
//! The screen keeps only text: what a link points to is remembered here when it is drawn, by
//! its text, and found again from the underlined cells under the click.

use std::sync::Mutex;

/// Links remembered, the most recent last; the oldest go beyond this.
const KEPT: usize = 400;

static SEEN: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Notes that `label` is drawn as a link to `url`.
pub fn remember(label: &str, url: &str) {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.last().is_some_and(|(l, u)| l == label && u == url) {
        return;
    }
    seen.retain(|(l, _)| l != label);
    seen.push((label.to_owned(), url.to_owned()));
    if seen.len() > KEPT {
        let extra = seen.len() - KEPT;
        seen.drain(..extra);
    }
}

/// The address of the link whose text (or a piece of it, when the line wrapped) is `fragment`.
pub fn find(fragment: &str) -> Option<String> {
    let fragment = fragment.trim();
    if fragment.chars().count() < 2 {
        return None;
    }
    let seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    seen.iter()
        .rev()
        .find(|(l, _)| l == fragment)
        .or_else(|| seen.iter().rev().find(|(l, _)| l.contains(fragment)))
        .map(|(_, u)| u.clone())
}

/// An address worth opening: the web only, nothing a program could take for an option.
pub fn safe(url: &str) -> Option<String> {
    let url = url.trim();
    let full = if url.starts_with("www.") { format!("https://{url}") } else { url.to_owned() };
    let ok = (full.starts_with("https://") || full.starts_with("http://"))
        && full.len() > "https://".len() + 2
        && full.len() < 2048
        && !full.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '\\'));
    ok.then_some(full)
}

/// Opens `url` in the default browser (no shell in between).
pub fn open(url: &str) -> Result<(), String> {
    let url = safe(url).ok_or_else(|| po_i18n::t!("tui.link.refused").to_owned())?;
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", &url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// A bare address at the start of `text` (`https://…`, `http://…`, `www.…`): its length in
/// bytes, the punctuation that ends a sentence left out.
pub fn bare_at(text: &str) -> Option<usize> {
    let starts = text.starts_with("https://") || text.starts_with("http://") || text.starts_with("www.");
    if !starts {
        return None;
    }
    let mut end = text.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '\'')).unwrap_or(text.len());
    // A closing parenthesis belongs to the address only if it opened one there (Wikipedia).
    loop {
        let candidate = &text[..end];
        let last = candidate.chars().last()?;
        let unbalanced_paren = last == ')' && candidate.matches('(').count() < candidate.matches(')').count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ']' | '}') || unbalanced_paren {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    let min = if text.starts_with("www.") { 7 } else { 10 };
    (end >= min).then_some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_address_stops_at_the_sentence_and_a_label_finds_its_link() {
        assert_eq!(bare_at("https://example.com/a?b=1. Next"), Some("https://example.com/a?b=1".len()));
        assert_eq!(bare_at("www.google.com, then"), Some("www.google.com".len()));
        assert_eq!(bare_at("https://en.wikipedia.org/wiki/Rust_(language))"), Some("https://en.wikipedia.org/wiki/Rust_(language)".len()));
        assert_eq!(bare_at("hello"), None);
        assert_eq!(bare_at("https://"), None);
        remember("the docs", "https://docs.rs");
        assert_eq!(find("the docs").as_deref(), Some("https://docs.rs"));
        assert_eq!(find("docs").as_deref(), Some("https://docs.rs"), "a piece of a wrapped label");
        assert_eq!(safe("www.google.com").as_deref(), Some("https://www.google.com"));
        assert!(safe("file:///etc/passwd").is_none());
        assert!(safe("https://x.y/\" & calc").is_none());
    }
}
