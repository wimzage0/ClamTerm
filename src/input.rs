//! Turning raw terminal input bytes into keys, and spotting our hotkeys in
//! the stream that otherwise goes to the shell untouched.
//!
//! Terminals send Alt+x as `ESC x`. We treat `ESC x` arriving in one read as
//! Alt+x and a lone ESC at the end of a read as the Escape key, while keeping
//! `ESC [` (CSI) and `ESC O` (SS3) sequences intact as arrows, Home, etc.

use crate::keymap::{Keymap, ShellCmd};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    /// Ctrl+letter, as the lowercase letter (Ctrl+A -> 'a').
    Ctrl(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// Bracketed paste markers, `ESC[200~` and `ESC[201~`.
    PasteStart,
    PasteEnd,
    /// Anything we don't decode (function keys, mouse reports, partial data).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub alt: bool,
}

impl Key {
    pub const fn new(code: KeyCode) -> Key {
        Key { code, alt: false }
    }

    #[cfg(test)]
    pub const fn alt(code: KeyCode) -> Key {
        Key { code, alt: true }
    }

    fn with_alt(self) -> Key {
        Key { alt: true, ..self }
    }
}

/// Decode the key at the start of `buf` (which must not be empty) and return
/// it with the number of bytes it used.
pub fn parse_key(buf: &[u8]) -> (Key, usize) {
    debug_assert!(!buf.is_empty());
    if buf[0] != 0x1b || buf.len() == 1 {
        return parse_plain(buf);
    }
    match buf[1] {
        b'[' if buf.len() > 2 => parse_csi(buf),
        b'O' if buf.len() > 2 => parse_ss3(buf),
        // ESC followed by an escape sequence: Alt+arrow in some terminals.
        0x1b if buf.len() > 2 && matches!(buf[2], b'[' | b'O') => {
            let (k, n) = parse_key(&buf[1..]);
            (k.with_alt(), n + 1)
        }
        _ => {
            let (k, n) = parse_plain(&buf[1..]);
            (k.with_alt(), n + 1)
        }
    }
}

/// Decode every key in `buf`.
#[cfg(test)]
pub fn parse_keys(buf: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let (k, n) = parse_key(&buf[i..]);
        keys.push(k);
        i += n;
    }
    keys
}

/// A single byte or UTF-8 character, no ESC prefix handling.
fn parse_plain(buf: &[u8]) -> (Key, usize) {
    let b = buf[0];
    let code = match b {
        b'\r' | b'\n' => KeyCode::Enter,
        b'\t' => KeyCode::Tab,
        0x7f | 0x08 => KeyCode::Backspace,
        0x1b => KeyCode::Esc,
        0x00 => KeyCode::Ctrl(' '),
        0x01..=0x1a => KeyCode::Ctrl((b + 0x60) as char),
        0x1c..=0x1f => KeyCode::Unknown,
        0x20..=0x7e => KeyCode::Char(b as char),
        _ => {
            let len = match b {
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf7 => 4,
                _ => return (Key::new(KeyCode::Unknown), 1),
            };
            return match buf.get(..len).and_then(|s| std::str::from_utf8(s).ok()) {
                Some(s) => (Key::new(KeyCode::Char(s.chars().next().unwrap())), len),
                None => (Key::new(KeyCode::Unknown), 1),
            };
        }
    };
    (Key::new(code), 1)
}

/// `ESC [ params intermediates final`.
fn parse_csi(buf: &[u8]) -> (Key, usize) {
    let mut i = 2;
    while i < buf.len() && (0x30..=0x3f).contains(&buf[i]) {
        i += 1;
    }
    let params_end = i;
    while i < buf.len() && (0x20..=0x2f).contains(&buf[i]) {
        i += 1;
    }
    if i >= buf.len() {
        // Incomplete: swallow the rest so it isn't misread as keys.
        return (Key::new(KeyCode::Unknown), buf.len());
    }
    if !(0x40..=0x7e).contains(&buf[i]) {
        return (Key::new(KeyCode::Unknown), i + 1);
    }
    let len = i + 1;
    let mut params = buf[2..params_end]
        .split(|&c| c == b';')
        .map(|p| std::str::from_utf8(p).ok().and_then(|s| s.parse::<u32>().ok()));
    let first = params.next().flatten();
    let modifier = params.next().flatten().unwrap_or(1);

    let code = match buf[i] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => KeyCode::BackTab,
        b'~' => match first {
            Some(1) | Some(7) => KeyCode::Home,
            Some(2) => KeyCode::Insert,
            Some(3) => KeyCode::Delete,
            Some(4) | Some(8) => KeyCode::End,
            Some(5) => KeyCode::PageUp,
            Some(6) => KeyCode::PageDown,
            Some(200) => KeyCode::PasteStart,
            Some(201) => KeyCode::PasteEnd,
            _ => KeyCode::Unknown,
        },
        _ => KeyCode::Unknown,
    };
    // xterm modifier parameter: 1 + (shift=1 | alt=2 | ctrl=4 | meta=8).
    let alt = modifier > 1 && (modifier - 1) & 2 != 0;
    (Key { code, alt }, len)
}

/// `ESC O x`, sent for arrows/Home/End in application cursor mode.
fn parse_ss3(buf: &[u8]) -> (Key, usize) {
    let code = match buf[2] {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'M' => KeyCode::Enter,
        _ => KeyCode::Unknown,
    };
    (Key::new(code), 3)
}

// ---------------------------------------------------------------------------
// Hotkeys in shell mode

/// Watches shell-bound input for hotkeys. Bytes are never re-encoded: the
/// caller forwards the original slice, so the shell sees exactly what the
/// terminal sent.
#[derive(Default)]
pub struct ShellInput {
    in_paste: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Scan {
    /// Bytes at the start of the buffer to forward to the shell as-is.
    pub forward: usize,
    /// The first hotkey found, and the offset just past it.
    pub hotkey: Option<(ShellCmd, usize)>,
}

impl ShellInput {
    pub fn scan(&mut self, buf: &[u8], keymap: &Keymap) -> Scan {
        let mut i = 0;
        while i < buf.len() {
            let (key, n) = parse_key(&buf[i..]);
            match key.code {
                KeyCode::PasteStart => self.in_paste = true,
                KeyCode::PasteEnd => self.in_paste = false,
                _ if !self.in_paste => {
                    if let Some(h) = keymap.shell(key) {
                        return Scan { forward: i, hotkey: Some((h, i + n)) };
                    }
                }
                _ => {}
            }
            i += n;
        }
        Scan { forward: buf.len(), hotkey: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyCode::*;

    fn km() -> Keymap {
        Keymap::default()
    }

    fn one(bytes: &[u8]) -> Key {
        let (k, n) = parse_key(bytes);
        assert_eq!(n, bytes.len(), "whole input should be one key: {bytes:?}");
        k
    }

    #[test]
    fn plain_keys() {
        assert_eq!(one(b"j"), Key::new(Char('j')));
        assert_eq!(one(b"\r"), Key::new(Enter));
        assert_eq!(one(b"\t"), Key::new(Tab));
        assert_eq!(one(b"\x7f"), Key::new(Backspace));
        assert_eq!(one(b"\x03"), Key::new(Ctrl('c')));
        assert_eq!(one("é".as_bytes()), Key::new(Char('é')));
        assert_eq!(one("日".as_bytes()), Key::new(Char('日')));
        assert_eq!(one("🦀".as_bytes()), Key::new(Char('🦀')));
    }

    #[test]
    fn lone_esc_is_escape() {
        assert_eq!(one(b"\x1b"), Key::new(Esc));
        assert_eq!(parse_keys(b"jj\x1b"), vec![Key::new(Char('j')), Key::new(Char('j')), Key::new(Esc)]);
    }

    #[test]
    fn esc_x_is_alt() {
        assert_eq!(one(b"\x1bm"), Key::alt(Char('m')));
        assert_eq!(one(b"\x1bh"), Key::alt(Char('h')));
        assert_eq!(one(b"\x1bM"), Key::alt(Char('M')));
        assert_eq!(one(b"\x1b\r"), Key::alt(Enter));
        assert_eq!(one(b"\x1b\x1b"), Key::alt(Esc));
        // ESC [ or ESC O with nothing after it is Alt+[ / Alt+O.
        assert_eq!(one(b"\x1b["), Key::alt(Char('[')));
        assert_eq!(one(b"\x1bO"), Key::alt(Char('O')));
    }

    #[test]
    fn csi_and_ss3_keys() {
        assert_eq!(one(b"\x1b[A"), Key::new(Up));
        assert_eq!(one(b"\x1b[B"), Key::new(Down));
        assert_eq!(one(b"\x1bOC"), Key::new(Right));
        assert_eq!(one(b"\x1bOD"), Key::new(Left));
        assert_eq!(one(b"\x1b[H"), Key::new(Home));
        assert_eq!(one(b"\x1bOF"), Key::new(End));
        assert_eq!(one(b"\x1b[1~"), Key::new(Home));
        assert_eq!(one(b"\x1b[4~"), Key::new(End));
        assert_eq!(one(b"\x1b[5~"), Key::new(PageUp));
        assert_eq!(one(b"\x1b[6~"), Key::new(PageDown));
        assert_eq!(one(b"\x1b[3~"), Key::new(Delete));
        assert_eq!(one(b"\x1b[15~"), Key::new(Unknown));
    }

    #[test]
    fn alt_arrows() {
        assert_eq!(one(b"\x1b[1;3A"), Key::alt(Up));
        assert_eq!(one(b"\x1b[1;3D"), Key::alt(Left));
        assert_eq!(one(b"\x1b\x1b[B"), Key::alt(Down));
        assert_eq!(one(b"\x1b[1;5A"), Key::new(Up)); // Ctrl+Up: not alt
    }

    #[test]
    fn sequences_are_not_split_into_alt_keys() {
        assert_eq!(parse_keys(b"\x1b[Ak"), vec![Key::new(Up), Key::new(Char('k'))]);
        assert_eq!(parse_keys(b"\x1bmj"), vec![Key::alt(Char('m')), Key::new(Char('j'))]);
        // Incomplete CSI is swallowed whole, not read as Alt+[ then '1'.
        assert_eq!(parse_keys(b"\x1b[1;"), vec![Key::new(Unknown)]);
    }

    #[test]
    fn scan_forwards_everything_without_hotkey() {
        let mut s = ShellInput::default();
        let input = b"ls -la\r\x1b[A\x1bb\x03";
        assert_eq!(s.scan(input, &km()), Scan { forward: input.len(), hotkey: None });
    }

    #[test]
    fn scan_stops_at_hotkey() {
        let mut s = ShellInput::default();
        assert_eq!(
            s.scan(b"ab\x1bmjk", &km()),
            Scan { forward: 2, hotkey: Some((ShellCmd::OpenPanel, 4)) }
        );
        assert_eq!(s.scan(b"\x1bo", &km()), Scan { forward: 0, hotkey: Some((ShellCmd::OpenFilter, 2)) });
    }

    #[test]
    fn scan_ignores_lookalikes() {
        let mut s = ShellInput::default();
        // CSI ending in 'm' (SGR) and SS3 'm' (keypad minus) are not Alt+m.
        for input in [&b"\x1b[0m"[..], b"\x1bOm", b"\x1b[1;3m", b"m", b"\x1bM"] {
            assert_eq!(s.scan(input, &km()).hotkey, None, "{input:?}");
        }
    }

    #[test]
    fn scan_ignores_hotkeys_inside_bracketed_paste() {
        let mut s = ShellInput::default();
        assert_eq!(s.scan(b"\x1b[200~x\x1bmy", &km()).hotkey, None);
        assert_eq!(s.scan(b"more \x1bo", &km()).hotkey, None); // paste spans reads
        assert_eq!(s.scan(b"\x1b[201~", &km()).hotkey, None);
        assert!(s.scan(b"\x1bm", &km()).hotkey.is_some());
    }
}
