//! Drawing the panel as terminal escape sequences. Every row is overwritten
//! in full (no clearing) so redraws don't flicker.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use std::path::{Path, PathBuf};

use crate::input::Key;
use crate::keymap::{format_key, Cmd, Keymap};
use crate::media::{self, Gfx, MediaKind};
use crate::panel::{human_size, tilde_path, Entry, Kind, Panel, Preview};

const RESET: &str = "\x1b[0m";
const HEADER: &str = "\x1b[1m";
const FOOTER: &str = "\x1b[7m";
const MESSAGE: &str = "\x1b[1;33m";
const CURSOR: &str = "\x1b[7m";
const DIR: &str = "\x1b[1;34m";
const LINK: &str = "\x1b[36m";
const EXEC: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const MARK: &str = "\x1b[1;33m";
const ERROR: &str = "\x1b[31m";
const SEPARATOR: &str = "\u{2502}";

/// Footer hints: pairs of commands share one label ("j/k move").
const HINTS: &[(&[Cmd], &str)] = &[
    (&[Cmd::Down, Cmd::Up], "move"),
    (&[Cmd::Parent, Cmd::Enter], "out/in"),
    (&[Cmd::Activate], "cd/edit"),
    (&[Cmd::OpenExternal], "open in app"),
    (&[Cmd::CdHere], "cd here"),
    (&[Cmd::Mark], "mark"),
    (&[Cmd::Insert], "insert"),
    (&[Cmd::ToggleHidden], "hidden"),
    (&[Cmd::Home], "home"),
    (&[Cmd::Search], "search"),
    (&[Cmd::Clear], "close"),
];

/// How the Alt modifier is shown: macOS keyboards call it Option.
const ALT_LABEL: &str = if cfg!(target_os = "macos") { "\u{2325}" } else { "alt+" };

/// Footer text built from the current bindings, so rebinding keeps it true.
/// Alt bindings are grouped behind one "alt+" (macOS: "⌥", the Option key
/// the terminal sends as Alt) so the line stays short.
fn hints(keymap: &Keymap) -> String {
    let (mut alt, mut other) = (Vec::new(), Vec::new());
    for (cmds, label) in HINTS {
        let keys: Vec<Key> = cmds.iter().filter_map(|c| keymap.first_key(*c)).collect();
        if keys.is_empty() {
            continue;
        }
        let all_alt = keys.iter().all(|k| k.alt);
        let names: Vec<String> = keys
            .iter()
            .map(|k| match format_key(*k) {
                n if all_alt => n.trim_start_matches("alt+").to_string(),
                n if n == "enter" => "\u{23ce}".into(),
                n => n,
            })
            .collect();
        let item = format!("{} {label}", names.join("/"));
        if all_alt {
            alt.push(item);
        } else {
            other.push(item);
        }
    }
    let mut out = String::from(" type to filter");
    if !alt.is_empty() {
        out.push_str(&format!("  \u{2502} {ALT_LABEL} {}", alt.join("  ")));
    }
    if !other.is_empty() {
        out.push_str(&format!("  \u{2502} {}", other.join("  ")));
    }
    out
}

pub fn render(panel: &mut Panel, keymap: &Keymap, gfx: &mut Gfx, rows: u16, cols: u16) -> Vec<u8> {
    let (rows, cols) = (rows as usize, cols as usize);
    let mut out = String::new();
    if rows == 0 || cols == 0 {
        return Vec::new();
    }

    goto(&mut out, 0);
    let marked = match panel.marks.len() {
        0 => String::new(),
        n => format!(" {n} marked "),
    };
    let marked_width = marked.len().min(cols);
    out.push_str(HEADER);
    let title = if panel.in_search() {
        format!(" search folders you've visited ({} indexed)", panel.total())
    } else {
        format!(" {}", tilde_path(&panel.dir))
    };
    out.push_str(&fit_tail(&title, cols - marked_width));
    out.push_str(RESET);
    cell(&mut out, MARK, &marked, marked_width);
    if rows < 3 {
        out.push_str(&gfx.update(None));
        return out.into_bytes();
    }

    let height = rows - 2;
    panel.page = height;
    keep_cursor_visible(panel, height);

    // Narrow terminals get the list only.
    let left = if cols >= 40 { (cols * 2 / 5).max(20) } else { cols };
    let right = cols.saturating_sub(left + 1);
    let (preview, image) = if right > 0 { preview_lines(panel, gfx, height) } else { (Vec::new(), None) };

    for row in 0..height {
        goto(&mut out, row + 1);
        let idx = panel.scroll + row;
        match panel.get(idx) {
            Some(e) => {
                let cursor = if idx == panel.cursor { CURSOR } else { "" };
                // One-column mark gutter, then the name.
                if panel.is_marked(e) {
                    cell(&mut out, &format!("{MARK}{cursor}"), "*", 1.min(left));
                } else {
                    cell(&mut out, cursor, " ", 1.min(left));
                }
                let style = format!("{}{cursor}", entry_style(e));
                if let Some(w) = e.weight {
                    // Search result: the full path, keeping its end (the
                    // folder's own name) visible, then its weight on the right.
                    let w = format!(" {:>5} ", format_weight(w));
                    let w_width = if left > w.len() + 10 { w.len() } else { 0 };
                    out.push_str(&style);
                    out.push_str(&fit_tail(&entry_label(e), left.saturating_sub(1 + w_width)));
                    out.push_str(RESET);
                    cell(&mut out, &format!("{DIM}{cursor}"), &w, w_width);
                } else {
                    cell(&mut out, &style, &entry_label(e), left.saturating_sub(1));
                }
            }
            None if row == 0 && panel.total() == 0 && panel.in_search() => {
                cell(&mut out, DIM, "  no folders indexed yet: cd around and they'll show up here", left)
            }
            None if row == 0 && panel.total() == 0 => cell(&mut out, DIM, "  (empty)", left),
            None if row == 0 && panel.len() == 0 => cell(&mut out, DIM, "  (no matches)", left),
            None => cell(&mut out, "", "", left),
        }
        if right > 0 {
            out.push_str(DIM);
            out.push_str(SEPARATOR);
            out.push_str(RESET);
            let (style, text) = preview.get(row).map_or(("", ""), |(s, t)| (*s, t.as_str()));
            cell(&mut out, style, &format!(" {text}"), right);
        }
    }

    goto(&mut out, rows - 1);
    if let Some(msg) = &panel.message {
        cell(&mut out, MESSAGE, &format!(" {msg}"), cols);
    } else if panel.in_search() && panel.filter.is_empty() {
        cell(&mut out, FOOTER, " search: type part of a folder's path  \u{2502} \u{23ce} cd  tab browse  esc back", cols);
    } else if !panel.filter.is_empty() {
        let (prompt, hint) = if panel.in_search() {
            (" search / ", format!(" {}/{}  \u{23ce} cd  tab browse  esc clear ", panel.len(), panel.total()))
        } else {
            (" / ", format!(" {}/{}  \u{23ce} open  tab into  esc clear ", panel.len(), panel.total()))
        };
        let prompt = format!("{prompt}{}\u{2588}", panel.filter);
        let hint_width = UnicodeWidthStr::width(hint.as_str()).min(cols.saturating_sub(10));
        cell(&mut out, "", &prompt, cols - hint_width);
        cell(&mut out, DIM, &hint, hint_width);
    } else {
        cell(&mut out, FOOTER, &hints(keymap), cols);
    }

    // The image sits under the preview's info line, inside the pane with a
    // one-cell margin. The text pass above left those cells blank.
    let want = image.and_then(|(png, px)| {
        let (box_cols, box_rows) = (right.saturating_sub(2), height.saturating_sub(3));
        if box_cols < 4 || box_rows < 2 {
            return None;
        }
        let cols = media::fit_columns(px, box_cols, box_rows, gfx.cell);
        Some(media::place(&png, left + 2, 3, cols))
    });
    out.push_str(&gfx.update(want));
    out.into_bytes()
}

fn keep_cursor_visible(panel: &mut Panel, height: usize) {
    if panel.cursor < panel.scroll {
        panel.scroll = panel.cursor;
    } else if panel.cursor >= panel.scroll + height {
        panel.scroll = panel.cursor + 1 - height;
    }
    panel.scroll = panel.scroll.min(panel.len().saturating_sub(height));
}

type Lines = Vec<(&'static str, String)>;

/// Preview text, plus an image to draw (PNG path and pixel size) if any.
fn preview_lines(panel: &mut Panel, gfx: &Gfx, height: usize) -> (Lines, Option<(PathBuf, (u32, u32))>) {
    let path = panel.selected_path();
    let lines = match panel.preview() {
        None => Vec::new(),
        Some(Preview::Dir(entries)) => {
            entries.iter().take(height).map(|e| (entry_style(e), entry_label(e))).collect()
        }
        Some(Preview::Text(lines)) => text_lines(lines, height),
        Some(Preview::Binary(size)) => vec![(DIM, format!("binary file, {}", human_size(*size)))],
        Some(Preview::Media { kind, size, dims, text }) => {
            let (kind, size, dims, text) = (*kind, *size, *dims, text.clone());
            return media_lines(&path.unwrap_or_default(), kind, size, dims, &text, gfx, height);
        }
        Some(Preview::Empty) => vec![(DIM, "(empty)".into())],
        Some(Preview::Error(e)) => vec![(ERROR, e.clone())],
    };
    (lines, None)
}

fn text_lines(lines: &[String], height: usize) -> Lines {
    lines.iter().take(height).map(|l| ("", strip_escapes(&l.replace('\t', "    ")))).collect()
}

/// Info line ("PNG image · 1920×1080 · 1.2 M"), then the image (graphics
/// terminals) or whatever text we have (PDF text, SVG source).
fn media_lines(
    path: &Path,
    kind: MediaKind,
    size: u64,
    dims: Option<(u32, u32)>,
    text: &[String],
    gfx: &Gfx,
    height: usize,
) -> (Lines, Option<(PathBuf, (u32, u32))>) {
    // Images only need the worker when they'll be drawn; PDFs and videos
    // also get their page count / duration (and PDF text) from it.
    let wants_worker = gfx.enabled || matches!(kind, MediaKind::Pdf | MediaKind::Video);
    let thumb = if wants_worker { gfx.thumb(path, kind) } else { Some(Ok(Default::default())) };

    let mut info = vec![kind.label()];
    if let Some((w, h)) = dims {
        info.push(format!("{w}\u{d7}{h}"));
    }
    info.push(human_size(size));
    if let Some(Ok(media::Thumb { info: Some(extra), .. })) = &thumb {
        info.push(extra.clone());
    }
    let mut lines: Lines = vec![(DIM, info.join(" \u{b7} ")), (DIM, String::new())];
    let rest = height.saturating_sub(2);
    let mut image = None;
    match thumb {
        None => lines.push((DIM, "loading preview\u{2026}".into())),
        Some(Ok(t)) => match t.png {
            // Size by the original's pixels when known: thumbnailers enlarge
            // small images, and small images shouldn't be drawn bigger.
            Some(png) => image = Some((png, dims.unwrap_or(t.px))),
            None if !t.text.is_empty() => lines.extend(text_lines(&t.text, rest)),
            None => lines.extend(text_lines(text, rest)),
        },
        Some(Err(e)) => {
            lines.push((ERROR, e));
            lines.extend(text_lines(text, rest.saturating_sub(1)));
        }
    }
    (lines, image)
}

/// A frecency weight in at most five columns: "0.5", "12.3", "840", "1.2k".
fn format_weight(w: f64) -> String {
    if w < 100.0 {
        format!("{w:.1}")
    } else if w < 1_000.0 {
        format!("{w:.0}")
    } else if w < 99_950.0 {
        format!("{:.1}k", w / 1_000.0)
    } else if w < 999_500.0 {
        format!("{:.0}k", w / 1_000.0)
    } else {
        // Can't happen with the rank cap, but never overflow the column.
        "1M+".into()
    }
}

fn entry_style(e: &Entry) -> &'static str {
    match e.kind {
        Kind::Dir => DIR,
        Kind::Symlink { .. } => LINK,
        Kind::File if e.executable => EXEC,
        Kind::File => "",
    }
}

fn entry_label(e: &Entry) -> String {
    if e.is_dir() {
        format!("{}/", e.display)
    } else {
        e.display.clone()
    }
}

fn goto(out: &mut String, row: usize) {
    out.push_str(&format!("\x1b[{};1H", row + 1));
}

fn cell(out: &mut String, style: &str, text: &str, width: usize) {
    out.push_str(style);
    out.push_str(&fit(text, width));
    out.push_str(RESET);
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// Truncate (with `…`) or pad `s` to exactly `width` terminal columns,
/// dropping control characters and accounting for wide characters.
pub fn fit(s: &str, width: usize) -> String {
    let chars = || s.chars().filter(|c| !c.is_control());
    let total: usize = chars().map(char_width).sum();
    let overflow = total > width;
    let limit = if overflow { width.saturating_sub(1) } else { width };

    let mut out = String::with_capacity(s.len() + width);
    let mut used = 0;
    for c in chars() {
        let w = char_width(c);
        if used + w > limit {
            break;
        }
        out.push(c);
        used += w;
    }
    if overflow && width > 0 {
        out.push('\u{2026}');
        used += 1;
    }
    out.extend(std::iter::repeat(' ').take(width - used));
    out
}

/// Like `fit`, but when too wide keep the end (`…` + tail), so a long path
/// still shows the folder you're in.
fn fit_tail(s: &str, width: usize) -> String {
    let total: usize = s.chars().filter(|c| !c.is_control()).map(char_width).sum();
    if total <= width || width == 0 {
        return fit(s, width);
    }
    let mut tail = Vec::new();
    let mut used = 1; // the ellipsis
    for c in s.chars().rev().filter(|c| !c.is_control()) {
        let w = char_width(c);
        if used + w > width {
            break;
        }
        tail.push(c);
        used += w;
    }
    let mut out = String::from("\u{2026}");
    out.extend(tail.into_iter().rev());
    out.extend(std::iter::repeat(' ').take(width - used));
    out
}

/// Remove ANSI escape sequences (CSI `ESC [ ... final`, OSC `ESC ] ... BEL/ST`,
/// and two-byte `ESC x`) so file previews show text, not leftovers like `[31m`.
/// Remaining control characters are dropped later by `fit`.
fn strip_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_follow_bindings() {
        let h = hints(&Keymap::default());
        let start = format!(" type to filter  \u{2502} {ALT_LABEL} j/k move  h/l out/in  p open in app ");
        assert!(h.starts_with(&start), "{h}");
        assert!(h.contains("\u{2502} \u{23ce} cd/edit  esc close"), "{h}");
        let (k, _) = Keymap::from_toml("[panel]\ndown = \"alt+n\"\ninsert = []\nclear = \"ctrl+g\"\n");
        let h = hints(&k);
        assert!(h.contains(&format!("{ALT_LABEL} n/k move ")), "{h}");
        assert!(!h.contains("insert"), "{h}");
        assert!(h.contains("ctrl+g close"), "{h}");
    }

    #[test]
    fn weights_fit_five_columns() {
        for (w, s) in [(0.25, "0.2"), (0.5, "0.5"), (12.34, "12.3"), (99.94, "99.9"), (840.0, "840"), (1234.0, "1.2k"), (40_000.0, "40.0k"), (250_000.0, "250k")] {
            assert_eq!(format_weight(w), s);
        }
        for w in [0.0, 1.0, 99.99, 999.4, 99_949.0, 99_999_999.0] {
            assert!(format_weight(w).len() <= 5, "{w}: {}", format_weight(w));
        }
    }

    #[test]
    fn fit_tail_keeps_the_end() {
        assert_eq!(fit_tail("/short", 8), "/short  ");
        assert_eq!(fit_tail("/a/very/long/path", 8), "\u{2026}ng/path");
        assert_eq!(fit_tail("/x/日本語", 5), "\u{2026}本語");
        assert_eq!(fit_tail("/x/日本語", 4), "\u{2026}語 ");
    }

    #[test]
    fn escapes_are_stripped() {
        assert_eq!(strip_escapes("a\x1b[31mred\x1b[0m b"), "ared b");
        assert_eq!(strip_escapes("\x1b]0;title\x07x\x1b]8;;u\x1b\\y"), "xy");
        assert_eq!(strip_escapes("\x1b(Bz"), "Bz");
    }

    #[test]
    fn fit_pads_and_truncates() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 4), "abc\u{2026}");
        assert_eq!(fit("abcd", 4), "abcd");
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn fit_handles_wide_chars() {
        // Each CJK char / emoji is two columns.
        assert_eq!(fit("日本語", 6), "日本語");
        assert_eq!(fit("日本語", 5), "日本\u{2026}");
        assert_eq!(fit("日本語", 4), "日\u{2026} "); // no half-width split
        assert_eq!(fit("a🦀b", 6), "a🦀b  ");
        for w in 0..10 {
            assert_eq!(fit("日本🦀語abc", w).width(), w, "width {w}");
        }
    }

    #[test]
    fn fit_strips_control_chars() {
        assert_eq!(fit("a\x1b[31mb\x07", 6), "a[31mb");
    }
}
