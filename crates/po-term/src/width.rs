//! Display width. Being off by one column on an emoji is enough to shift a whole frame and
//! produce the text overlaps seen in existing harnesses.
//!
//! We reason in **graphemes**, not `char`s: `👨‍👩‍👧` is a single grapheme of width 2, made of
//! five code points. And we take the negotiated Unicode level into account: on a terminal that
//! does not render emoji at double width, pretending otherwise breaks the layout.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::caps::UnicodeLevel;

/// Removes emoji from a model reply. The model is instructed not to use them, but a local model
/// does not always follow the instruction (observed: "I am here 🙂"); the user does not want
/// them. Typographic symbols (✓, →, •) stay: they are not emoji, and they are useful. An emoji
/// followed by a space takes that space with it, to avoid leaving a double space.
pub fn strip_emoji(text: &str) -> String {
    let is_emoji = |c: char| {
        matches!(c as u32,
            0x1F000..=0x1FAFF   // pictograms, faces, objects, flags, skin tones
            | 0x2600..=0x26FF   // miscellaneous symbols: ☀ ⚠ ⚡ ☕
            | 0x2700..=0x2712 | 0x2715 | 0x2719..=0x27BF // dingbats, except ✓ ✔ ✗ ✘
            | 0x2B00..=0x2BFF   // ⭐ ⬛ ⭕
            | 0x231A | 0x231B | 0x23E9..=0x23FA // ⌚ ⌛ ⏩ ⏳
            | 0xFE0F | 0x200D | 0x20E3) // variation selector, joiner, keycap
    };
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if is_emoji(c) {
            if out.ends_with(' ') && chars.peek() == Some(&' ') {
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Neutralises what, in text of external origin, would break the display.
///
/// The text displayed by the harness comes from a model, a tool output or a third-party MCP
/// server - never from us. Left untreated, a reply containing ANSI escape sequences would
/// repaint the user's screen, erase lines or move the cursor; a tab would be enough to
/// misalign a frame. We therefore replace control characters with the Unicode replacement
/// character, which keeps the text visible while making the injection harmless.
///
/// Line breaks are not handled here: it is up to the caller to decide whether to split or
/// flatten, depending on whether it renders a line or a block.
pub fn sanitize_line(text: &str) -> String {
    text.chars()
        .filter(|c| !is_ambiguous_width(*c))
        .map(|c| match c {
            '\t' => ' ',
            '\n' | '\r' => c,
            c if c.is_control() => '\u{fffd}',
            c => c,
        })
        .collect()
}

/// Characters that make the width computed by the application diverge from the one the
/// terminal draws, or that mislead reading.
///
/// A composed emoji (👨‍👩‍👧) takes two columns for a width table and six for a terminal that draws
/// each component; the text that follows is then overwritten, which was observed on screen
/// (`end` turned into `en…d`). Removing the joiner (ZWJ), variation selectors, skin tones and
/// tags leaves simple emoji, whose width is the same everywhere. We lose a graphical nuance,
/// never text.
///
/// Writing-direction controls are removed for another reason: they visually reorder the text,
/// and a model or a file could use them to make people read something other than what is
/// written.
fn is_ambiguous_width(c: char) -> bool {
    matches!(c as u32,
        0x200B          // zero-width space
        | 0x200D        // zero-width joiner (ZWJ)
        | 0x2060        // word joiner
        | 0xFEFF        // byte order mark
        | 0xFE00..=0xFE0F   // variation selectors
        | 0x1F3FB..=0x1F3FF // skin tones
        | 0xE0020..=0xE007F // tags (composed regional flags)
        | 0x202A..=0x202E   // bidirectional controls
        | 0x2066..=0x2069
    )
}

/// Display width of a string, in columns.
pub fn display_width(text: &str, level: UnicodeLevel) -> usize {
    text.graphemes(true).map(|g| grapheme_width(g, level)).sum()
}

/// Width of a single grapheme.
pub fn grapheme_width(grapheme: &str, level: UnicodeLevel) -> usize {
    // A grapheme composed through a zero-width joiner (ZWJ) occupies the width of its base, not
    // the sum of its components - hence the computation on the first visible code point.
    if grapheme.contains('\u{200d}') {
        let base = grapheme.chars().next().map(String::from).unwrap_or_default();
        return grapheme_width(&base, level);
    }

    let width = UnicodeWidthStr::width(grapheme);
    match level {
        // Without emoji support, a non-BMP character will be rendered by a one-column substitute.
        UnicodeLevel::Ascii | UnicodeLevel::Bmp if is_wide_emoji(grapheme) => 1,
        _ => width,
    }
}

fn is_wide_emoji(grapheme: &str) -> bool {
    grapheme.chars().any(|c| {
        matches!(c as u32,
            0x1F300..=0x1FAFF   // pictograms, emoticons, extended symbols
            | 0x2600..=0x27BF   // miscellaneous symbols and dingbats
            | 0x1F000..=0x1F2FF // tiles, cards, enclosed ideographs
        )
    })
}

/// Truncates to `max_cols` columns without ever cutting a grapheme in two.
/// Returns the string as is if it already fits.
pub fn truncate(text: &str, max_cols: usize, level: UnicodeLevel) -> &str {
    if display_width(text, level) <= max_cols {
        return text;
    }
    let mut used = 0;
    let mut end = 0;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let width = grapheme_width(grapheme, level);
        if used + width > max_cols {
            break;
        }
        used += width;
        end = offset + grapheme.len();
    }
    &text[..end]
}

/// Truncates while reserving room for a cut indicator, adapted to the Unicode level.
pub fn truncate_with_ellipsis(text: &str, max_cols: usize, level: UnicodeLevel) -> String {
    let ellipsis = if level >= UnicodeLevel::Bmp { "…" } else { "..." };
    let ellipsis_width = display_width(ellipsis, level);

    if display_width(text, level) <= max_cols {
        return text.to_owned();
    }
    if max_cols <= ellipsis_width {
        return truncate(text, max_cols, level).to_owned();
    }
    let head = truncate(text, max_cols - ellipsis_width, level);
    format!("{head}{ellipsis}")
}

/// Pads on the right up to `cols` columns. Does not truncate: a line that is too long is
/// returned intact, it being up to the caller to have cut it already.
pub fn pad_to(text: &str, cols: usize, level: UnicodeLevel) -> String {
    let width = display_width(text, level);
    if width >= cols {
        return text.to_owned();
    }
    format!("{text}{}", " ".repeat(cols - width))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_disappear_but_not_useful_symbols() {
        assert_eq!(strip_emoji("I am here 🙂 - yes"), "I am here - yes");
        assert_eq!(strip_emoji("✅ done ✓ and → next ⚠️ careful 👍🏽"), " done ✓ and → next careful ");
        assert_eq!(strip_emoji("• bullet - dash “quotes”"), "• bullet - dash “quotes”");
    }

    #[test]
    fn sanitizing_defuses_ansi_sequences() {
        // A model reply containing "\x1b[2J" would clear the user's screen.
        let dangerous = "text\u{1b}[2Jvisible\u{7}";
        let clean = sanitize_line(dangerous);
        assert!(!clean.contains('\u{1b}'));
        assert!(!clean.contains('\u{7}'));
        assert!(clean.contains("text") && clean.contains("visible"));
    }

    #[test]
    fn sanitizing_preserves_ordinary_text() {
        for text in ["simple", "accents éàï", "日本語", "emoji 🚀"] {
            assert_eq!(sanitize_line(text), text);
        }
    }

    #[test]
    fn sanitizing_leaves_line_breaks_to_the_caller() {
        assert_eq!(sanitize_line("a\nb"), "a\nb");
        assert_eq!(sanitize_line("column\tvalue"), "column value");
    }

    #[test]
    fn ascii_counts_one_per_character() {
        assert_eq!(display_width("hello", UnicodeLevel::Full), 5);
    }

    #[test]
    fn ideographs_count_double() {
        assert_eq!(display_width("日本語", UnicodeLevel::Full), 6);
    }

    #[test]
    fn composed_accents_count_only_once() {
        // Decomposed "é": e + U+0301. A single grapheme, a single column.
        assert_eq!(display_width("e\u{0301}", UnicodeLevel::Full), 1);
        assert_eq!(display_width("café", UnicodeLevel::Full), 4);
    }

    #[test]
    fn the_zwj_family_stays_one_grapheme() {
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(display_width(family, UnicodeLevel::Full), 2);
    }

    #[test]
    fn without_emoji_support_the_width_falls_back_to_one() {
        assert_eq!(display_width("🚀", UnicodeLevel::Full), 2);
        assert_eq!(display_width("🚀", UnicodeLevel::Bmp), 1);
        assert_eq!(display_width("🚀", UnicodeLevel::Ascii), 1);
    }

    #[test]
    fn truncation_does_not_cut_a_grapheme() {
        // Cutting at 3 columns in "日本語" must leave a whole ideograph, not a stray byte.
        assert_eq!(truncate("日本語", 3, UnicodeLevel::Full), "日");
        assert_eq!(truncate("日本語", 4, UnicodeLevel::Full), "日本");
        assert_eq!(truncate("hello", 99, UnicodeLevel::Full), "hello");
    }

    #[test]
    fn the_ellipsis_fits_in_the_budget() {
        let out = truncate_with_ellipsis("path/very/long/to/file.rs", 10, UnicodeLevel::Full);
        assert_eq!(display_width(&out, UnicodeLevel::Full), 10);
        assert!(out.ends_with('…'));

        let ascii = truncate_with_ellipsis("path/very/long", 10, UnicodeLevel::Ascii);
        assert_eq!(display_width(&ascii, UnicodeLevel::Ascii), 10);
        assert!(ascii.ends_with("..."));
    }

    #[test]
    fn padding_reaches_the_wanted_width() {
        assert_eq!(display_width(&pad_to("日本", 6, UnicodeLevel::Full), UnicodeLevel::Full), 6);
        assert_eq!(pad_to("too long", 3, UnicodeLevel::Full), "too long");
    }

    #[test]
    fn a_composed_emoji_is_reduced_to_simple_emoji() {
        let clean = sanitize_line("👨\u{200d}👩\u{200d}👧 end");
        assert_eq!(clean, "👨👩👧 end");
    }

    #[test]
    fn variations_and_skin_tones_are_removed() {
        assert_eq!(sanitize_line("❤\u{fe0f}"), "❤");
        assert_eq!(sanitize_line("👍\u{1f3fd}"), "👍");
    }

    #[test]
    fn writing_direction_controls_are_removed() {
        assert_eq!(sanitize_line("a\u{202e}b"), "ab");
    }

    #[test]
    fn ordinary_text_stays_intact() {
        let text = "Hello, 日本語 - café ✅ 🚀";
        assert_eq!(sanitize_line(text), text);
    }
}
