//! Images in the transcript: a small thumbnail drawn with half blocks (`▀`, the top pixel in the
//! foreground color, the bottom one in the background color), which every color terminal shows
//! (Windows Terminal, WezTerm, Termius, tmux) without an image protocol. Below it, a line that
//! opens the image in the system's viewer when clicked.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use ratatui::style::{Color, Style};
use ratatui::text::{Line as TuiLine, Span};

/// Largest thumbnail, in cells: wide enough to recognize a screenshot, short enough not to push
/// the conversation away.
pub const MAX_COLS: u32 = 48;
pub const MAX_ROWS: u32 = 18;

/// Colors of a decoded thumbnail: `rows` rows of `cols` cells, two pixels each (top, bottom).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pixels {
    pub cols: u32,
    pub rows: u32,
    pub cells: Vec<([u8; 3], [u8; 3])>,
}

/// Decoded once per path and size: the screen is drawn many times a second.
type Decoded = HashMap<(String, u32), Option<Pixels>>;
static CACHE: Mutex<Option<Decoded>> = Mutex::new(None);

/// The thumbnail of the image at `path`, at most `max_cols` wide; `None` when the file cannot be
/// read as an image.
pub fn pixels(path: &str, max_cols: u32) -> Option<Pixels> {
    let key = (path.to_owned(), max_cols);
    if let Ok(mut cache) = CACHE.lock()
        && let Some(found) = cache.get_or_insert_with(HashMap::new).get(&key)
    {
        return found.clone();
    }
    let decoded = decode(Path::new(path), max_cols);
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(key, decoded.clone());
    }
    decoded
}

fn decode(path: &Path, max_cols: u32) -> Option<Pixels> {
    // A huge file is not decoded on the interface's thread for a thumbnail.
    if std::fs::metadata(path).ok()?.len() > 40 * 1024 * 1024 {
        return None;
    }
    let image = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?.into_rgb8();
    from_rgb(&image, max_cols)
}

/// Scales an image into cells: a terminal cell is about twice as tall as wide, so one cell holds
/// two pixels stacked, and the proportions stay right.
pub fn from_rgb(image: &image::RgbImage, max_cols: u32) -> Option<Pixels> {
    let (w, h) = image.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    let max_cols = max_cols.clamp(4, MAX_COLS);
    let max_px_h = MAX_ROWS * 2;
    let scale = (max_cols as f64 / w as f64).min(max_px_h as f64 / h as f64).min(1.0);
    let cols = ((w as f64 * scale).round() as u32).max(1);
    let px_h = ((h as f64 * scale).round() as u32).max(2);
    let small = image::imageops::resize(image, cols, px_h, image::imageops::FilterType::Triangle);
    let rows = px_h.div_ceil(2);
    let mut cells = Vec::with_capacity((cols * rows) as usize);
    for row in 0..rows {
        for col in 0..cols {
            let top = small.get_pixel(col, row * 2).0;
            let bottom = if row * 2 + 1 < px_h { small.get_pixel(col, row * 2 + 1).0 } else { top };
            cells.push((top, bottom));
        }
    }
    Some(Pixels { cols, rows, cells })
}

/// The color a terminal can show: exact in true color, the nearest of the 256 otherwise.
fn color(rgb: [u8; 3], truecolor: bool) -> Color {
    if truecolor {
        return Color::Rgb(rgb[0], rgb[1], rgb[2]);
    }
    let level = |v: u8| -> u8 { if v < 48 { 0 } else if v < 115 { 1 } else { ((v - 35) / 40).min(5) } };
    let (r, g, b) = (level(rgb[0]), level(rgb[1]), level(rgb[2]));
    Color::Indexed(16 + 36 * r + 6 * g + b)
}

/// The rows of the thumbnail, each after `indent`.
pub fn lines(pixels: &Pixels, indent: &str, truecolor: bool) -> Vec<TuiLine<'static>> {
    (0..pixels.rows)
        .map(|row| {
            let mut spans = vec![Span::raw(indent.to_owned())];
            for col in 0..pixels.cols {
                let (top, bottom) = pixels.cells[(row * pixels.cols + col) as usize];
                spans.push(Span::styled("▀", Style::default().fg(color(top, truecolor)).bg(color(bottom, truecolor))));
            }
            TuiLine::from(spans)
        })
        .collect()
}

/// How the link under a thumbnail is remembered among the links (see [`crate::links`]).
pub const LINK: &str = "po-image:";

/// The images Po itself showed: only those open from a click, never a path a model wrote in a
/// link of its own.
static SHOWN: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn remember_shown(path: &str) {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    if !shown.iter().any(|p| p == path) {
        shown.push(path.to_owned());
    }
}

/// An image file Po showed, with an image's extension: nothing that would run if opened.
fn openable(path: &str) -> bool {
    let image = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"));
    image && SHOWN.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p == path) && Path::new(path).is_file()
}

/// Opens the image in the system's viewer, without waiting for it.
pub fn open(path: &str) -> std::io::Result<()> {
    if !openable(path) {
        return Err(std::io::Error::other(po_i18n::t!("tui.image.refused")));
    }
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        // `start` takes its first quoted argument as a window title: an empty one comes first.
        c.args(["/C", "start", "", path]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_image_keeps_its_proportions_in_cells() {
        // 200×100 pixels: twice as wide as tall. At 40 columns, 20 pixels tall = 10 rows.
        let image = image::RgbImage::from_fn(200, 100, |x, _| if x < 100 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let p = from_rgb(&image, 40).unwrap();
        assert_eq!((p.cols, p.rows), (40, 10));
        assert_eq!(p.cells[0].0, [255, 0, 0], "left is red");
        assert_eq!(p.cells[39].0, [0, 0, 255], "right is blue");
    }

    #[test]
    fn a_tall_image_is_limited_in_height() {
        let image = image::RgbImage::from_pixel(100, 1000, image::Rgb([10, 200, 10]));
        let p = from_rgb(&image, 48).unwrap();
        assert_eq!(p.rows, MAX_ROWS);
        assert!(p.cols < 10);
    }

    #[test]
    fn a_small_image_is_not_enlarged_and_a_missing_file_has_no_thumbnail() {
        let image = image::RgbImage::from_pixel(6, 4, image::Rgb([1, 2, 3]));
        let p = from_rgb(&image, 48).unwrap();
        assert_eq!((p.cols, p.rows), (6, 2));
        assert!(pixels("/nowhere/at/all.png", 40).is_none());
    }

    #[test]
    fn without_true_color_the_nearest_of_the_256_is_used() {
        assert_eq!(color([255, 0, 0], false), Color::Indexed(196));
        assert_eq!(color([0, 0, 0], false), Color::Indexed(16));
        assert_eq!(color([1, 2, 3], true), Color::Rgb(1, 2, 3));
    }
}
