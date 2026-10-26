//! The file panel ("highlight mode"): directory model, cursor and key
//! handling. No terminal I/O here; `view.rs` draws it and `main.rs` carries
//! out the returned `Action`s, so this could drive another frontend later.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::input::{Key, KeyCode};
use crate::keymap::{Cmd, FilterCmd, Keymap};

/// How many lines of a text file the preview reads.
const PREVIEW_LINES: usize = 200;
/// Bytes read to decide text vs binary and fill the preview.
const PREVIEW_BYTES: usize = 32 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Close,
    /// cd the shell to this folder and close.
    Cd(PathBuf),
    /// Close and type these paths into the shell's command line.
    Insert(Vec<PathBuf>),
    /// Close and open this file in the user's editor.
    Edit(PathBuf),
    /// Open this path in its default desktop app (a folder: the file
    /// manager); the panel stays open.
    OpenExternal(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    /// `to_dir`: the link points at a directory (so it sorts and opens like one).
    Symlink { to_dir: bool },
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: OsString,
    /// Lossy UTF-8 for display and sorting.
    pub display: String,
    pub kind: Kind,
    pub executable: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, Kind::Dir | Kind::Symlink { to_dir: true })
    }
}

pub enum Preview {
    Dir(Vec<Entry>),
    Text(Vec<String>),
    Binary(u64),
    Empty,
    Error(String),
}

/// List `dir`: folders first, then case-insensitive by name.
pub fn list_dir(dir: &Path, show_hidden: bool) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            // Control characters in names show as '?', like ls does.
            let display: String =
                name.to_string_lossy().chars().map(|c| if c.is_control() { '?' } else { c }).collect();
            if !show_hidden && display.starts_with('.') {
                return None;
            }
            let path = e.path();
            let lmeta = fs::symlink_metadata(&path).ok()?;
            // Follow links for dir-ness and exec bits; broken links stay links.
            let meta = fs::metadata(&path).ok();
            let target_dir = meta.as_ref().map_or(false, |m| m.is_dir());
            let kind = if lmeta.file_type().is_symlink() {
                Kind::Symlink { to_dir: target_dir }
            } else if target_dir {
                Kind::Dir
            } else {
                Kind::File
            };
            let executable =
                !target_dir && meta.map_or(false, |m| m.is_file() && m.permissions().mode() & 0o111 != 0);
            Some(Entry { name, display, kind, executable })
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.is_dir(), e.display.to_lowercase(), e.display.clone()));
    Ok(entries)
}

pub fn read_preview(path: &Path, is_dir: bool, show_hidden: bool) -> Preview {
    if is_dir {
        return match list_dir(path, show_hidden) {
            Ok(entries) if entries.is_empty() => Preview::Empty,
            Ok(entries) => Preview::Dir(entries),
            Err(e) => Preview::Error(e.to_string()),
        };
    }
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Preview::Error(e.to_string()),
    };
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(PREVIEW_BYTES.min(size as usize));
    if let Err(e) = (&mut file).take(PREVIEW_BYTES as u64).read_to_end(&mut buf) {
        return Preview::Error(e.to_string());
    }
    if buf.is_empty() {
        return Preview::Empty;
    }
    if buf.contains(&0) {
        return Preview::Binary(size);
    }
    let text = String::from_utf8_lossy(&buf);
    // Mostly undecodable: call it binary rather than show replacement chars.
    if text.matches('\u{fffd}').count() > buf.len() / 20 {
        return Preview::Binary(size);
    }
    Preview::Text(text.lines().take(PREVIEW_LINES).map(str::to_owned).collect())
}

/// Show `path` with the home directory abbreviated to `~`.
pub fn tilde_path(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

/// "512 B", "1.2 K", "3.4 M".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["K", "M", "G", "T", "P"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

pub struct Panel {
    pub dir: PathBuf,
    /// Everything in `dir` (subject to `show_hidden`).
    entries: Vec<Entry>,
    /// Indices into `entries` that pass the filter, in display order.
    view: Vec<usize>,
    /// Index into `view`.
    pub cursor: usize,
    /// First visible list row; kept by the view so scrolling is stable.
    pub scroll: usize,
    /// List height from the last render, for PgUp/PgDn.
    pub page: usize,
    pub show_hidden: bool,
    /// `Some` while the filter prompt is active.
    pub filter: Option<String>,
    /// Marked paths, kept across folder changes.
    pub marks: BTreeSet<PathBuf>,
    /// One-shot message for the footer, cleared by the next key.
    pub message: Option<String>,
    preview: Option<(PathBuf, Preview)>,
}

impl Panel {
    pub fn open(dir: PathBuf, filtering: bool) -> Panel {
        let mut p = Panel {
            dir: PathBuf::new(),
            entries: Vec::new(),
            view: Vec::new(),
            cursor: 0,
            scroll: 0,
            page: 10,
            show_hidden: false,
            filter: filtering.then(String::new),
            marks: BTreeSet::new(),
            message: None,
            preview: None,
        };
        p.change_dir(dir, None);
        p
    }

    /// Number of entries shown (after filtering).
    pub fn len(&self) -> usize {
        self.view.len()
    }

    /// The `i`th shown entry.
    pub fn get(&self, i: usize) -> Option<&Entry> {
        self.view.get(i).map(|&j| &self.entries[j])
    }

    /// Entries in `dir` before filtering.
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.get(self.cursor)
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected().map(|e| self.dir.join(&e.name))
    }

    pub fn is_marked(&self, e: &Entry) -> bool {
        !self.marks.is_empty() && self.marks.contains(&self.dir.join(&e.name))
    }

    pub fn set_message(&mut self, msg: impl Into<String>) {
        self.message = Some(msg.into());
    }

    /// Preview of the highlighted entry, cached until the cursor moves.
    pub fn preview(&mut self) -> Option<&Preview> {
        let path = self.selected_path()?;
        if self.preview.as_ref().map_or(true, |(p, _)| *p != path) {
            let is_dir = self.selected().map_or(false, Entry::is_dir);
            self.preview = Some((path.clone(), read_preview(&path, is_dir, self.show_hidden)));
        }
        self.preview.as_ref().map(|(_, p)| p)
    }

    /// Show `dir`, putting the cursor on `select` if given. On error, stay
    /// where we are and say why.
    fn change_dir(&mut self, dir: PathBuf, select: Option<&OsString>) -> bool {
        match list_dir(&dir, self.show_hidden) {
            Ok(entries) => {
                self.entries = entries;
                self.dir = dir;
                self.scroll = 0;
                self.preview = None;
                self.refilter(select);
                true
            }
            Err(e) => {
                if self.dir.as_os_str().is_empty() {
                    // First open failed: show the folder anyway, empty.
                    self.dir = dir;
                }
                self.set_message(format!("can't open: {e}"));
                false
            }
        }
    }

    /// Rebuild `view` from the filter, keeping the cursor on `keep` if it is
    /// still shown, else on the first match.
    fn refilter(&mut self, keep: Option<&OsString>) {
        let needle = self.filter.as_deref().unwrap_or("").to_lowercase();
        self.view = (0..self.entries.len())
            .filter(|&i| needle.is_empty() || self.entries[i].display.to_lowercase().contains(&needle))
            .collect();
        self.cursor = keep
            .and_then(|name| self.view.iter().position(|&i| &self.entries[i].name == name))
            .unwrap_or(0);
    }

    fn selected_name(&self) -> Option<OsString> {
        self.selected().map(|e| e.name.clone())
    }

    fn go_parent(&mut self) {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else { return };
        let came_from = self.dir.file_name().map(OsString::from);
        self.change_dir(parent, came_from.as_ref());
    }

    /// l / →: enter a folder, or open a file.
    fn enter(&mut self) -> Action {
        match (self.selected().map(Entry::is_dir), self.selected_path()) {
            (Some(true), Some(path)) => {
                self.change_dir(path, None);
                Action::None
            }
            (Some(false), Some(path)) => Action::Edit(path),
            _ => Action::None,
        }
    }

    /// Enter: cd to a folder, or open a file.
    fn activate(&mut self) -> Action {
        match (self.selected().map(Entry::is_dir), self.selected_path()) {
            (Some(true), Some(path)) => Action::Cd(path),
            (Some(false), Some(path)) => Action::Edit(path),
            // Nothing to pick (empty folder or no matches): cd to the folder itself.
            _ => Action::Cd(self.dir.clone()),
        }
    }

    fn go_home(&mut self) {
        if let Some(home) = std::env::var_os("HOME") {
            self.change_dir(PathBuf::from(home), None);
        }
    }

    fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        let keep = self.selected_name();
        let dir = self.dir.clone();
        self.change_dir(dir, keep.as_ref());
        self.set_message(if self.show_hidden { "showing hidden files" } else { "hiding hidden files" });
    }

    fn toggle_mark(&mut self) {
        if let Some(path) = self.selected_path() {
            if !self.marks.remove(&path) {
                self.marks.insert(path);
            }
            self.move_by(1);
        }
    }

    /// i: marked paths, or the highlighted one when nothing is marked.
    fn insert(&self) -> Action {
        if self.marks.is_empty() {
            match self.selected_path() {
                Some(p) => Action::Insert(vec![p]),
                None => Action::Close,
            }
        } else {
            Action::Insert(self.marks.iter().cloned().collect())
        }
    }

    fn move_by(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let last = self.view.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    pub fn handle_key(&mut self, key: Key, keymap: &Keymap) -> Action {
        self.message = None;
        if self.filter.is_some() {
            return self.filter_key(key, keymap);
        }
        match keymap.panel(key) {
            Some(cmd) => self.run(cmd),
            None => Action::None,
        }
    }

    fn run(&mut self, cmd: Cmd) -> Action {
        match cmd {
            Cmd::Down => self.move_by(1),
            Cmd::Up => self.move_by(-1),
            Cmd::Parent => self.go_parent(),
            Cmd::Enter => return self.enter(),
            Cmd::Activate => return self.activate(),
            Cmd::CdHere => return Action::Cd(self.dir.clone()),
            Cmd::Mark => self.toggle_mark(),
            Cmd::Insert => return self.insert(),
            Cmd::Filter => self.filter = Some(String::new()),
            Cmd::Top => self.cursor = 0,
            Cmd::Bottom => self.cursor = self.view.len().saturating_sub(1),
            Cmd::PageUp => self.move_by(-(self.page.max(1) as isize)),
            Cmd::PageDown => self.move_by(self.page.max(1) as isize),
            Cmd::ToggleHidden => self.toggle_hidden(),
            Cmd::Home => self.go_home(),
            // Nothing highlighted (empty folder): open the folder itself.
            Cmd::OpenExternal => return Action::OpenExternal(self.selected_path().unwrap_or_else(|| self.dir.clone())),
            Cmd::Close => return Action::Close,
        }
        Action::None
    }

    /// Keys while the filter prompt is active: filter bindings first, then
    /// plain characters type, then other panel bindings (arrows, Alt+j...).
    fn filter_key(&mut self, key: Key, keymap: &Keymap) -> Action {
        match keymap.filter(key) {
            Some(FilterCmd::Accept) => return self.activate(),
            Some(FilterCmd::Into) => self.into_folder_filtering(),
            Some(FilterCmd::Clear) => {
                let keep = self.selected_name();
                self.filter = None;
                self.refilter(keep.as_ref());
            }
            Some(FilterCmd::ClearText) => self.set_filter_text(String::new()),
            None => match key.code {
                KeyCode::Backspace if !key.alt => {
                    let mut text = self.filter.clone().unwrap_or_default();
                    if text.pop().is_none() {
                        self.go_parent();
                    } else {
                        self.set_filter_text(text);
                    }
                }
                KeyCode::Char(c) if !key.alt => {
                    let mut text = self.filter.clone().unwrap_or_default();
                    text.push(c);
                    self.set_filter_text(text);
                }
                _ => match keymap.panel(key) {
                    Some(Cmd::Enter) => self.into_folder_filtering(),
                    Some(Cmd::Filter) | None => {}
                    Some(cmd) => return self.run(cmd),
                },
            },
        }
        Action::None
    }

    fn set_filter_text(&mut self, text: String) {
        self.filter = Some(text);
        let keep = self.selected_name();
        self.refilter(keep.as_ref());
    }

    /// Tab while filtering: enter the highlighted folder with a fresh filter.
    fn into_folder_filtering(&mut self) {
        if self.selected().map_or(false, Entry::is_dir) {
            let path = self.selected_path().unwrap();
            self.filter = Some(String::new());
            self.change_dir(path, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    /// A scratch directory removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let p = std::env::temp_dir().join(format!("alterm-test-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.display.as_str()).collect()
    }

    impl Panel {
        fn handle_key_default(&mut self, key: Key) -> Action {
            self.handle_key(key, &Keymap::default())
        }
    }

    fn key(c: char) -> Key {
        Key::new(KeyCode::Char(c))
    }

    fn fixture(tag: &str) -> TempDir {
        let t = TempDir::new(tag);
        for d in ["beta", "Alpha", "zeta", ".hidden_dir"] {
            fs::create_dir(t.0.join(d)).unwrap();
        }
        for f in ["b.txt", "A.txt", ".dotfile"] {
            fs::write(t.0.join(f), "hello\nworld\n").unwrap();
        }
        fs::write(t.0.join("beta/inner.txt"), "x").unwrap();
        symlink(t.0.join("zeta"), t.0.join("link_to_dir")).unwrap();
        t
    }

    #[test]
    fn listing_sorts_dirs_first_case_insensitive() {
        let t = fixture("sort");
        let e = list_dir(&t.0, false).unwrap();
        assert_eq!(names(&e), ["Alpha", "beta", "link_to_dir", "zeta", "A.txt", "b.txt"]);
        assert_eq!(e[2].kind, Kind::Symlink { to_dir: true });
        let all = list_dir(&t.0, true).unwrap();
        assert_eq!(all.len(), 8);
        assert_eq!(all[0].display, ".hidden_dir");
    }

    #[test]
    fn parent_keeps_cursor_on_folder_we_came_from() {
        let t = fixture("parent");
        let mut p = Panel::open(t.0.clone(), false);
        p.handle_key_default(key('j')); // beta
        p.handle_key_default(key('l'));
        assert_eq!(p.dir, t.0.join("beta"));
        assert_eq!(names(&p.entries), ["inner.txt"]);
        p.handle_key_default(Key::new(KeyCode::Backspace));
        assert_eq!(p.dir, t.0);
        assert_eq!(p.selected().unwrap().display, "beta");
    }

    #[test]
    fn alt_keys_act_like_plain_keys() {
        let t = fixture("alt");
        let mut p = Panel::open(t.0.clone(), false);
        p.handle_key_default(Key::alt(KeyCode::Char('j')));
        p.handle_key_default(Key::alt(KeyCode::Char('l')));
        assert_eq!(p.dir, t.0.join("beta"));
        p.handle_key_default(Key::alt(KeyCode::Char('h')));
        assert_eq!(p.dir, t.0);
    }

    #[test]
    fn movement_is_clamped() {
        let t = fixture("move");
        let mut p = Panel::open(t.0.clone(), false);
        p.handle_key_default(key('k'));
        assert_eq!(p.cursor, 0);
        p.handle_key_default(key('G'));
        assert_eq!(p.selected().unwrap().display, "b.txt");
        p.handle_key_default(key('j'));
        assert_eq!(p.selected().unwrap().display, "b.txt");
        p.handle_key_default(key('g'));
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn enter_and_c_return_cd() {
        let t = fixture("cd");
        let mut p = Panel::open(t.0.clone(), false);
        assert_eq!(p.handle_key_default(Key::new(KeyCode::Enter)), Action::Cd(t.0.join("Alpha")));
        assert_eq!(p.handle_key_default(key('c')), Action::Cd(t.0.clone()));
        p.handle_key_default(key('G'));
        assert_eq!(p.handle_key_default(Key::new(KeyCode::Enter)), Action::Edit(t.0.join("b.txt")));
        assert_eq!(p.handle_key_default(key('l')), Action::Edit(t.0.join("b.txt")));
        assert_eq!(p.handle_key_default(key('q')), Action::Close);
        assert_eq!(p.handle_key_default(Key::alt(KeyCode::Char('m'))), Action::Close);
    }

    fn typing(p: &mut Panel, s: &str) {
        for c in s.chars() {
            p.handle_key_default(key(c));
        }
    }

    fn shown(p: &Panel) -> Vec<&str> {
        (0..p.len()).map(|i| p.get(i).unwrap().display.as_str()).collect()
    }

    #[test]
    fn marks_and_insert() {
        let t = fixture("marks");
        let mut p = Panel::open(t.0.clone(), false);
        // Nothing marked: insert the highlighted entry.
        assert_eq!(p.handle_key_default(key('i')), Action::Insert(vec![t.0.join("Alpha")]));
        // Order: Alpha beta link_to_dir zeta A.txt b.txt
        p.handle_key_default(key(' ')); // mark Alpha, move down
        assert_eq!(p.selected().unwrap().display, "beta");
        p.handle_key_default(key('G'));
        p.handle_key_default(key(' ')); // mark b.txt (last row: stays)
        p.handle_key_default(key('k'));
        p.handle_key_default(key(' ')); // mark A.txt
        p.handle_key_default(key('k'));
        p.handle_key_default(key('k'));
        p.handle_key_default(key(' ')); // mark zeta
        p.handle_key_default(key('k'));
        p.handle_key_default(key(' ')); // unmark zeta
        assert!(!p.is_marked(p.get(3).unwrap()));
        assert!(p.is_marked(p.get(0).unwrap()));
        // Marks survive entering a folder.
        p.handle_key_default(key('g'));
        p.handle_key_default(key('j'));
        p.handle_key_default(key('l'));
        assert_eq!(p.dir, t.0.join("beta"));
        assert_eq!(
            p.handle_key_default(key('i')),
            Action::Insert(vec![t.0.join("A.txt"), t.0.join("Alpha"), t.0.join("b.txt")])
        );
    }

    #[test]
    fn filter_narrows_case_insensitively() {
        let t = fixture("filter");
        let mut p = Panel::open(t.0.clone(), true);
        typing(&mut p, "TXT");
        assert_eq!(shown(&p), ["A.txt", "b.txt"]);
        p.handle_key_default(Key::new(KeyCode::Down));
        assert_eq!(p.handle_key_default(Key::new(KeyCode::Enter)), Action::Edit(t.0.join("b.txt")));
        // Esc clears the filter and keeps the selection.
        p.handle_key_default(Key::new(KeyCode::Esc));
        assert!(p.filter.is_none());
        assert_eq!(p.len(), 6);
        assert_eq!(p.selected().unwrap().display, "b.txt");
        // In normal mode 'j' moves; while filtering it types.
        p.handle_key_default(key('/'));
        typing(&mut p, "j");
        assert_eq!(p.filter.as_deref(), Some("j"));
        assert_eq!(p.len(), 0);
        assert_eq!(p.handle_key_default(Key::new(KeyCode::Enter)), Action::Cd(t.0.clone()));
    }

    #[test]
    fn filter_tab_enters_and_backspace_goes_up() {
        let t = fixture("tab");
        let mut p = Panel::open(t.0.clone(), true);
        typing(&mut p, "bet");
        assert_eq!(shown(&p), ["beta"]);
        p.handle_key_default(Key::new(KeyCode::Tab));
        assert_eq!(p.dir, t.0.join("beta"));
        assert_eq!(p.filter.as_deref(), Some(""));
        typing(&mut p, "in");
        assert_eq!(shown(&p), ["inner.txt"]);
        p.handle_key_default(Key::new(KeyCode::Backspace));
        p.handle_key_default(Key::new(KeyCode::Backspace));
        assert_eq!(p.dir, t.0.join("beta"));
        p.handle_key_default(Key::new(KeyCode::Backspace)); // empty filter: go up
        assert_eq!(p.dir, t.0);
        assert_eq!(p.selected().unwrap().display, "beta");
        assert!(p.filter.is_some());
        // Alt+j navigates instead of typing.
        p.handle_key_default(Key::alt(KeyCode::Char('j')));
        assert_eq!(p.selected().unwrap().display, "link_to_dir");
        assert_eq!(p.filter.as_deref(), Some(""));
    }

    #[test]
    fn p_opens_externally() {
        let t = fixture("fm");
        let mut p = Panel::open(t.0.clone(), false);
        assert_eq!(p.handle_key_default(key('p')), Action::OpenExternal(t.0.join("Alpha")));
        p.handle_key_default(key('G'));
        assert_eq!(
            p.handle_key_default(Key::alt(KeyCode::Char('p'))),
            Action::OpenExternal(t.0.join("b.txt"))
        );
        // Empty folder: the folder itself.
        let empty = t.0.join("zeta");
        let mut p = Panel::open(empty.clone(), false);
        assert_eq!(p.handle_key_default(key('p')), Action::OpenExternal(empty));
    }

    #[test]
    fn hidden_toggle_keeps_selection() {
        let t = fixture("hidden");
        let mut p = Panel::open(t.0.clone(), false);
        p.handle_key_default(key('j'));
        p.handle_key_default(key('.'));
        assert_eq!(p.len(), 8);
        assert_eq!(p.selected().unwrap().display, "beta");
        p.handle_key_default(key('.'));
        assert_eq!(p.len(), 6);
        assert_eq!(p.selected().unwrap().display, "beta");
    }

    #[test]
    fn previews() {
        let t = fixture("preview");
        fs::write(t.0.join("bin"), b"\x7fELF\0\0\x01").unwrap();
        fs::write(t.0.join("empty"), b"").unwrap();
        assert!(matches!(read_preview(&t.0.join("A.txt"), false, false),
            Preview::Text(l) if l == ["hello", "world"]));
        assert!(matches!(read_preview(&t.0.join("bin"), false, false), Preview::Binary(7)));
        assert!(matches!(read_preview(&t.0.join("empty"), false, false), Preview::Empty));
        assert!(matches!(read_preview(&t.0.join("beta"), true, false),
            Preview::Dir(e) if names(&e) == ["inner.txt"]));
        assert!(matches!(read_preview(&t.0.join("zeta"), true, false), Preview::Empty));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 K");
        assert_eq!(human_size(1_258_291), "1.2 M");
    }
}
