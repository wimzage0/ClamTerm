//! Drawing the panel as terminal escape sequences. Every row is overwritten
//! in full (no clearing) so redraws don't flicker.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::keymap::{Cmd, Keymap};
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
    (&[Cmd::CdHere], "cd here"),
    (&[Cmd::Mark], "mark"),
    (&[Cmd::Insert], "insert"),
    (&[Cmd::Filter], "filter"),
    (&[Cmd::ToggleHidden], "hidden"),
    (&[Cmd::OpenExternal], "open in app"),
    (&[Cmd::Home], "home"),
    (&[Cmd::Close], "close"),
];

/// Footer text built from the current bindings, so rebinding keeps it true.
fn hints(keymap: &Keymap) -> String {
    let mut out = String::new();
    for (cmds, label) in HINTS {
        let keys: Vec<String> = cmds
            .iter()
            .filter_map(|c| keymap.key_for(*c))
            .map(|k| if k == "enter" { "\u{23ce}".into() } else { k })
            .collect();
        if !keys.is_empty() {
            out.push_str(&format!(" {} {label} ", keys.join("/")));
        }
    }
    out
}

pub fn render(panel: &mut Panel, keymap: &Keymap, rows: u16, cols: u16) -> Vec<u8> {
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
    out.push_str(&fit_tail(&format!(" {}", tilde_path(&panel.dir)), cols - marked_width));
    out.push_str(RESET);
    cell(&mut out, MARK, &marked, marked_width);
    if rows < 3 {
        return out.into_bytes();
    }

    let height = rows - 2;
    panel.page = height;
    keep_cursor_visible(panel, height);

    // Narrow terminals get the list only.
    let left = if cols >= 40 { (cols * 2 / 5).max(20) } else { cols };
    let right = cols.saturating_sub(left + 1);
    let preview = if right > 0 { preview_lines(panel, height) } else { Vec::new() };

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
                cell(&mut out, &style, &entry_label(e), left.saturating_sub(1));
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
    } else if let Some(f) = &panel.filter {
        let prompt = format!(" / {f}\u{2588}");
        let hint = format!(" {}/{}  \u{23ce} open  Tab into  Esc clear ", panel.len(), panel.total());
        let hint_width = UnicodeWidthStr::width(hint.as_str()).min(cols.saturating_sub(10));
        cell(&mut out, "", &prompt, cols - hint_width);
        cell(&mut out, DIM, &hint, hint_width);
    } else {
        cell(&mut out, FOOTER, &hints(keymap), cols);
    }
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

fn preview_lines(panel: &mut Panel, height: usize) -> Vec<(&'static str, String)> {
    match panel.preview() {
        None => Vec::new(),
        Some(Preview::Dir(entries)) => {
            entries.iter().take(height).map(|e| (entry_style(e), entry_label(e))).collect()
        }
        Some(Preview::Text(lines)) => {
            lines.iter().take(height).map(|l| ("", strip_escapes(&l.replace('\t', "    ")))).collect()
        }
        Some(Preview::Binary(size)) => vec![(DIM, format!("binary file, {}", human_size(*size)))],
        Some(Preview::Empty) => vec![(DIM, "(empty)".into())],
        Some(Preview::Error(e)) => vec![(ERROR, e.clone())],
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
        assert!(h.starts_with(" j/k move  h/l out/in  \u{23ce} cd/edit "), "{h}");
        let (k, _) = Keymap::from_toml("[panel]\ndown = \"n\"\ninsert = []\n");
        let h = hints(&k);
        assert!(h.starts_with(" n/k move "), "{h}");
        assert!(!h.contains("insert"), "{h}");
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
