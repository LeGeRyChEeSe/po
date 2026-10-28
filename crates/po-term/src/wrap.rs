//! Text wrapping to the terminal width, without breaking words.
//!
//! A terminal wraps a line that is too long by itself, but to the exact column: on a phone in
//! portrait (40 columns), `param_two` would become `par` / `am_two`. The text was not lost, it
//! became painful to read. These functions wrap at spaces and only break a word if it is wider
//! than the whole screen.

use crate::UnicodeLevel;
use crate::width::display_width;

/// Wraps text that arrives in fragments. The current word is held until the space that ends it:
/// only then do we know whether it fits on the line.
#[derive(Debug, Clone)]
pub struct StreamWrapper {
    width: usize,
    level: UnicodeLevel,
    col: usize,
    word: String,
    /// Pending space: it is only written once we are sure the next word fits on the line, so as
    /// never to leave a space at the end of a line nor start one with a space.
    space: bool,
}

impl StreamWrapper {
    pub fn new(width: usize, level: UnicodeLevel) -> Self {
        Self { width: width.max(8), level, col: 0, word: String::new(), space: false }
    }

    /// Adds a fragment and returns what must be written now.
    pub fn push(&mut self, fragment: &str) -> String {
        let mut out = String::new();
        for c in fragment.chars() {
            match c {
                '\n' => {
                    self.flush_word(&mut out);
                    out.push('\n');
                    self.col = 0;
                    self.space = false;
                }
                ' ' => {
                    self.flush_word(&mut out);
                    if self.col > 0 {
                        self.space = true;
                    }
                }
                c => {
                    self.word.push(c);
                    // A word wider than the screen will never find its place: we write it as is
                    // and the terminal will cut it, for lack of anything better.
                    if display_width(&self.word, self.level) > self.width {
                        self.flush_word(&mut out);
                    }
                }
            }
        }
        out
    }

    /// Returns what is still pending, at the end of the text.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        self.flush_word(&mut out);
        self.space = false;
        out
    }

    fn flush_word(&mut self, out: &mut String) {
        if self.word.is_empty() {
            return;
        }
        let w = display_width(&self.word, self.level);
        let needed = w + usize::from(self.space);
        if self.col > 0 && self.col + needed > self.width {
            out.push('\n');
            self.col = 0;
        } else if self.space {
            out.push(' ');
            self.col += 1;
        }
        self.space = false;
        out.push_str(&self.word);
        // A word wider than the screen spills onto the next line; a word of exactly the width
        // fills its own, and the next one must go to a new line.
        self.col = if w > self.width { w % self.width } else { self.col + w };
        self.word.clear();
    }
}

/// Wraps a report line made of segments separated by ` · `: each following line takes the
/// indentation again, and no segment is cut in two.
pub fn wrap_segments(text: &str, width: usize, indent: &str, level: UnicodeLevel) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for segment in text.split(" · ") {
        let candidate = if current.is_empty() { segment.to_owned() } else { format!("{current} · {segment}") };
        if !current.is_empty() && display_width(&candidate, level) > width {
            lines.push(current);
            current = format!("{indent}{segment}");
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(width: usize, fragments: &[&str]) -> String {
        let mut w = StreamWrapper::new(width, UnicodeLevel::Full);
        let mut out: String = fragments.iter().map(|f| w.push(f)).collect();
        out.push_str(&w.finish());
        out
    }

    #[test]
    fn words_are_never_broken() {
        let text = render(20, &["let value = compute(param_one, para", "m_two);"]);
        for line in text.lines() {
            assert!(display_width(line, UnicodeLevel::Full) <= 20, "{line:?}");
        }
        assert!(text.contains("param_two);"), "{text}");
    }

    #[test]
    fn no_text_is_lost_or_added_apart_from_spaces() {
        let source = "Here is a reply 日本語のテキスト with 🚀 and a URL https://example.com/a/very/long/path/that/overflows";
        let rendered = render(24, &[source]);
        let a: String = source.split_whitespace().collect();
        let b: String = rendered.split_whitespace().collect();
        assert_eq!(a, b);
    }

    #[test]
    fn no_space_at_the_start_or_end_of_a_line() {
        let text = render(10, &["one two three four five six"]);
        for line in text.lines() {
            assert_eq!(line, line.trim(), "{text:?}");
        }
    }

    #[test]
    fn original_line_breaks_are_kept() {
        assert_eq!(render(40, &["a\nb\n\nc"]), "a\nb\n\nc");
    }

    #[test]
    fn segments_wrap_whole_with_indentation() {
        let lines = wrap_segments("170 tokens · $0.0000 · 100% local · 0.6 s local compute", 30, "  ", UnicodeLevel::Full);
        assert_eq!(lines, vec!["170 tokens · $0.0000", "  100% local", "  0.6 s local compute"]);
    }
}
