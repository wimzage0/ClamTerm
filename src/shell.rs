//! Building command lines to type into the shell.

use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// Ctrl+E Ctrl+U: move to end of line, then kill back to its start, so a
/// half-typed command doesn't get glued onto ours.
const CLEAR_LINE: &[u8] = b"\x05\x15";
/// Ctrl+V: readline's (and zsh's) quoted-insert, so the next byte is inserted
/// literally instead of acting as a key (Tab completing, newline running...).
const QUOTED_INSERT: u8 = 0x16;
/// Expanded by the shell itself, so editor settings from .bashrc apply and
/// values with arguments like `code -w` work.
const EDITOR: &[u8] = b"${VISUAL:-${EDITOR:-nano}}";

/// Quote bytes for a POSIX shell: wrap in single quotes, with each embedded
/// `'` written as `'\''`. Works for any bytes, including non-UTF-8 names.
pub fn quote(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    out.push(b'\'');
    for &b in s {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

/// Make bytes safe to *type* at a line editor: control characters (a Tab or
/// newline in a file name) are each preceded by Ctrl+V.
fn typed(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for &b in s {
        if b < 0x20 || b == 0x7f {
            out.push(QUOTED_INSERT);
        }
        out.push(b);
    }
    out
}

/// A path as one quoted shell word, ready to type.
fn word(path: &Path) -> Vec<u8> {
    typed(&quote(path.as_os_str().as_bytes()))
}

/// `path` relative to `base` (both absolute), using `..` where needed. When
/// the two only share `/`, the absolute path is clearer and is returned as is.
pub fn relative(path: &Path, base: &Path) -> PathBuf {
    let p: Vec<Component> = path.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common <= 1 && b.len() > 1 {
        return path.to_path_buf();
    }
    let mut rel = PathBuf::new();
    for _ in common..b.len() {
        rel.push("..");
    }
    for c in &p[common..] {
        rel.push(c);
    }
    if rel.as_os_str().is_empty() {
        rel.push(".");
    }
    // A leading '-' would be read as an option.
    if rel.as_os_str().as_bytes().starts_with(b"-") {
        rel = Path::new(".").join(rel);
    }
    rel
}

/// Keystrokes that make the shell cd to `dir`. The leading space keeps the
/// command out of bash history (with HISTCONTROL=ignorespace/ignoreboth).
pub fn cd_command(dir: &Path) -> Vec<u8> {
    let mut cmd = CLEAR_LINE.to_vec();
    cmd.extend_from_slice(b" cd -- ");
    cmd.extend_from_slice(&word(dir));
    cmd.push(b'\r');
    cmd
}

/// Keystrokes that open `file` in the user's editor. `file` should already
/// be relative to the shell's cwd (see `relative`).
pub fn edit_command(file: &Path) -> Vec<u8> {
    let mut cmd = CLEAR_LINE.to_vec();
    cmd.push(b' ');
    cmd.extend_from_slice(EDITOR);
    cmd.push(b' ');
    cmd.extend_from_slice(&word(file));
    cmd.push(b'\r');
    cmd
}

/// The paths as quoted words relative to `cwd`, each followed by a space,
/// to type at the cursor (no Enter).
pub fn insert_text(paths: &[PathBuf], cwd: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for p in paths {
        out.extend_from_slice(&word(&relative(p, cwd)));
        out.push(b' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::process::Command;

    fn q(s: &str) -> String {
        String::from_utf8(quote(s.as_bytes())).unwrap()
    }

    #[test]
    fn quoting() {
        assert_eq!(q("plain"), "'plain'");
        assert_eq!(q("with space"), "'with space'");
        assert_eq!(q("it's"), r"'it'\''s'");
        assert_eq!(q(""), "''");
        assert_eq!(q("$HOME `x` \"y\" \\ *"), "'$HOME `x` \"y\" \\ *'");
    }

    /// Round-trip awkward names through a real shell.
    #[test]
    fn quoting_survives_sh() {
        for name in ["a b", "it's", "'", "''x''", "$(echo no)", "tab\there", "new\nline", "日本 🦀", "-n"] {
            let script = [b"printf %s ".as_slice(), &quote(name.as_bytes())].concat();
            let out = Command::new("sh")
                .arg("-c")
                .arg(OsStr::from_bytes(&script))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap(), name);
        }
    }

    #[test]
    fn control_chars_are_quoted_insert() {
        assert_eq!(word(Path::new("a\tb")), b"'a\x16\tb'".to_vec());
        assert_eq!(word(Path::new("a\nb")), b"'a\x16\nb'".to_vec());
    }

    #[test]
    fn cd_command_shape() {
        assert_eq!(cd_command(Path::new("/tmp/it's here")), b"\x05\x15 cd -- '/tmp/it'\\''s here'\r".to_vec());
    }

    #[test]
    fn edit_command_shape() {
        assert_eq!(
            edit_command(Path::new("notes.txt")),
            b"\x05\x15 ${VISUAL:-${EDITOR:-nano}} 'notes.txt'\r".to_vec()
        );
    }

    #[test]
    fn relative_paths() {
        let r = |p: &str, b: &str| relative(Path::new(p), Path::new(b));
        assert_eq!(r("/a/b/c", "/a/b"), Path::new("c"));
        assert_eq!(r("/a/b/c/d", "/a/b"), Path::new("c/d"));
        assert_eq!(r("/a/x", "/a/b"), Path::new("../x"));
        assert_eq!(r("/a/x/y", "/a/b/c"), Path::new("../../x/y"));
        assert_eq!(r("/a/b", "/a/b"), Path::new("."));
        assert_eq!(r("/a", "/a/b"), Path::new(".."));
        assert_eq!(r("/etc/hosts", "/home/u"), Path::new("/etc/hosts"));
        assert_eq!(r("/etc/hosts", "/"), Path::new("etc/hosts"));
        assert_eq!(r("/a/b/-rf", "/a/b"), Path::new("./-rf"));
    }

    #[test]
    fn insert_text_shape() {
        let paths = [PathBuf::from("/home/u/a b.txt"), PathBuf::from("/home/u/d/it's"), PathBuf::from("/etc/x")];
        assert_eq!(
            String::from_utf8(insert_text(&paths, Path::new("/home/u"))).unwrap(),
            r"'a b.txt' 'd/it'\''s' '/etc/x' "
        );
    }
}
