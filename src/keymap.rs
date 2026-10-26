//! Keybindings: the actions alterm knows, their default keys, and loading
//! overrides from `~/.config/alterm/keys.toml`.

use std::path::PathBuf;

use crate::input::{Key, KeyCode};
use crate::tinytoml;

/// Hotkeys recognised while the shell has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCmd {
    OpenPanel,
}

/// File panel commands. Plain characters always type into the panel's
/// filter, so these are bound to Alt+key or non-character keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Down,
    Up,
    Parent,
    Enter,
    Activate,
    CdHere,
    Mark,
    Insert,
    Top,
    Bottom,
    PageUp,
    PageDown,
    ToggleHidden,
    Home,
    OpenExternal,
    /// Clear the filter, or close the panel when it's already empty.
    Clear,
    ClearText,
    Close,
}

/// (config name, action, default keys, description for the generated file)
type Table<A> = &'static [(&'static str, A, &'static [&'static str], &'static str)];

const SHELL: Table<ShellCmd> = &[
    ("open_panel", ShellCmd::OpenPanel, &["alt+m"], "open the file panel at the shell's folder"),
];

const PANEL: Table<Cmd> = &[
    ("down", Cmd::Down, &["alt+j", "down"], "move down"),
    ("up", Cmd::Up, &["alt+k", "up"], "move up"),
    ("parent", Cmd::Parent, &["alt+h", "left"], "go to the parent folder"),
    ("enter", Cmd::Enter, &["alt+l", "right", "tab"], "enter folder / open file"),
    ("activate", Cmd::Activate, &["enter"], "folder: cd the shell there and close; file: open in editor"),
    ("cd_here", Cmd::CdHere, &["alt+c"], "cd the shell to the folder being viewed"),
    ("mark", Cmd::Mark, &["alt+space", "ctrl+space"], "toggle mark and move down"),
    ("insert", Cmd::Insert, &["alt+i"], "close and type marked (or highlighted) paths into the shell"),
    ("top", Cmd::Top, &["alt+g", "home"], "jump to the first entry"),
    ("bottom", Cmd::Bottom, &["alt+G", "end"], "jump to the last entry"),
    ("page_up", Cmd::PageUp, &["pageup"], "one page up"),
    ("page_down", Cmd::PageDown, &["pagedown"], "one page down"),
    ("toggle_hidden", Cmd::ToggleHidden, &["alt+."], "show / hide dotfiles"),
    ("home", Cmd::Home, &["alt+~"], "go to your home folder"),
    ("open_external", Cmd::OpenExternal, &["alt+p"], "open in the default app (folders: file manager)"),
    ("clear", Cmd::Clear, &["esc"], "clear the filter, or close if it's empty"),
    ("clear_text", Cmd::ClearText, &["ctrl+u"], "clear the filter"),
    ("close", Cmd::Close, &["alt+m", "alt+q"], "close without changing anything"),
];

#[derive(Clone, Debug)]
pub struct Keymap {
    shell: Vec<(Key, ShellCmd)>,
    panel: Vec<(Key, Cmd)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::from_toml("").0
    }
}

/// A key that types a character (no Alt), so it can't be a command where
/// typing is expected.
fn types_char(k: &Key) -> bool {
    !k.alt && matches!(k.code, KeyCode::Char(_))
}

impl Keymap {
    pub fn shell(&self, key: Key) -> Option<ShellCmd> {
        lookup(&self.shell, key)
    }

    pub fn panel(&self, key: Key) -> Option<Cmd> {
        lookup(&self.panel, key)
    }

    /// First key bound to `cmd`, for footer hints.
    pub fn first_key(&self, cmd: Cmd) -> Option<Key> {
        self.panel.iter().find(|(_, c)| *c == cmd).map(|(k, _)| *k)
    }

    /// Build from keys.toml text. Unset actions keep their defaults; problems
    /// are returned as warnings and the offending entry is skipped.
    pub fn from_toml(src: &str) -> (Keymap, Vec<String>) {
        let (entries, errors) = tinytoml::parse(src);
        let mut warnings: Vec<String> = errors.iter().map(|e| format!("line {}: {}", e.line, e.msg)).collect();
        let shell = build(SHELL, "shell", &entries, &mut warnings);
        let panel = build(PANEL, "panel", &entries, &mut warnings);
        for e in &entries {
            if !matches!(e.section.as_str(), "shell" | "panel") {
                let section = if e.section.is_empty() { "(top level)" } else { e.section.as_str() };
                warnings.push(format!("line {}: unknown section [{section}]", e.line));
            }
        }
        // Plain characters would block typing them: in the shell that's
        // normal typing, in the panel it's the filter.
        let shell = drop_typing_keys(shell, "shell", &mut warnings);
        let panel = drop_typing_keys(panel, "panel", &mut warnings);
        (Keymap { shell, panel }, warnings)
    }

    /// Load the user's keys.toml, or the defaults if there is none.
    pub fn load() -> (Keymap, Vec<String>) {
        let Some(path) = config_path() else { return (Keymap::default(), Vec::new()) };
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                let (keymap, warnings) = Keymap::from_toml(&src);
                let shown = path.display();
                (keymap, warnings.into_iter().map(|w| format!("{shown}: {w}")).collect())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Keymap::default(), Vec::new()),
            Err(e) => (Keymap::default(), vec![format!("{}: {e}", path.display())]),
        }
    }
}

fn lookup<A: Copy>(table: &[(Key, A)], key: Key) -> Option<A> {
    table.iter().find(|(k, _)| *k == key).map(|(_, a)| *a)
}

fn drop_typing_keys<A>(mut list: Vec<(Key, A)>, section: &str, warnings: &mut Vec<String>) -> Vec<(Key, A)> {
    list.retain(|(k, _)| {
        if types_char(k) {
            warnings.push(format!("[{section}] \"{}\" types a character; use alt+ instead (ignored)", format_key(*k)));
        }
        !types_char(k)
    });
    list
}

/// Bindings for one section. Actions set in the file are bound first (a
/// conflict between two of them keeps the earlier one), then defaults for
/// the rest, skipping keys the user has already claimed.
fn build<A: Copy + PartialEq>(
    table: Table<A>,
    section: &str,
    entries: &[tinytoml::Entry],
    warnings: &mut Vec<String>,
) -> Vec<(Key, A)> {
    let mut bound: Vec<(Key, A)> = Vec::new();
    let mut user_set: Vec<A> = Vec::new();
    for e in entries.iter().filter(|e| e.section == section) {
        let Some(&(name, action, _, _)) = table.iter().find(|(n, ..)| *n == e.key) else {
            warnings.push(format!("line {}: unknown action \"{}\" in [{section}]", e.line, e.key));
            continue;
        };
        if user_set.contains(&action) {
            warnings.push(format!("line {}: \"{name}\" is set twice; using the first", e.line));
            continue;
        }
        user_set.push(action);
        for spec in &e.values {
            match parse_key_spec(spec) {
                Ok(key) => match bound.iter().find(|(k, _)| *k == key) {
                    Some(&(_, other)) => {
                        let other = table.iter().find(|t| t.1 == other).map_or("?", |t| t.0);
                        warnings.push(format!(
                            "line {}: \"{spec}\" is already bound to \"{other}\"; ignored for \"{name}\"",
                            e.line
                        ));
                    }
                    None => bound.push((key, action)),
                },
                Err(msg) => warnings.push(format!("line {}: {msg}", e.line)),
            }
        }
    }
    for &(_, action, defaults, _) in table {
        if user_set.contains(&action) {
            continue;
        }
        for spec in defaults {
            let key = parse_key_spec(spec).expect("default key specs are valid");
            if !bound.iter().any(|(k, _)| *k == key) {
                bound.push((key, action));
            }
        }
    }
    bound
}

pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("alterm").join("keys.toml"))
}

/// Parse `"j"`, `"G"`, `"alt+m"`, `"ctrl+u"`, `"pagedown"`, `"shift+tab"`...
/// Named keys and modifiers ignore case; single characters don't.
pub fn parse_key_spec(spec: &str) -> Result<Key, String> {
    let mut rest = spec;
    let (mut alt, mut ctrl, mut shift) = (false, false, false);
    loop {
        let lower = rest.to_ascii_lowercase();
        let flag = if lower.starts_with("alt+") || lower.starts_with("meta+") {
            &mut alt
        } else if lower.starts_with("ctrl+") {
            &mut ctrl
        } else if lower.starts_with("shift+") {
            &mut shift
        } else {
            break;
        };
        *flag = true;
        rest = &rest[rest.find('+').unwrap() + 1..];
        if rest.is_empty() {
            // "alt++": the key is '+'.
            rest = "+";
            break;
        }
    }
    let bad = || format!("unknown key \"{spec}\"");
    let mut chars = rest.chars();
    let code = match (chars.next(), chars.next()) {
        (Some(c), None) => {
            if ctrl {
                match c.to_ascii_lowercase() {
                    'i' => KeyCode::Tab,
                    'm' | 'j' => KeyCode::Enter,
                    'h' => return Err(format!("\"{spec}\" can't be told apart from backspace")),
                    '[' => KeyCode::Esc,
                    c @ 'a'..='z' => KeyCode::Ctrl(c),
                    ' ' => KeyCode::Ctrl(' '),
                    _ => return Err(bad()),
                }
            } else if shift {
                KeyCode::Char(c.to_uppercase().next().unwrap_or(c))
            } else {
                KeyCode::Char(c)
            }
        }
        _ => {
            let code = match rest.to_ascii_lowercase().as_str() {
                "enter" | "return" => KeyCode::Enter,
                "tab" if shift => KeyCode::BackTab,
                "tab" => KeyCode::Tab,
                "backtab" => KeyCode::BackTab,
                "backspace" | "bs" => KeyCode::Backspace,
                "delete" | "del" => KeyCode::Delete,
                "insert" | "ins" => KeyCode::Insert,
                "esc" | "escape" => KeyCode::Esc,
                "space" if ctrl => KeyCode::Ctrl(' '),
                "space" => KeyCode::Char(' '),
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                "left" => KeyCode::Left,
                "right" => KeyCode::Right,
                "home" => KeyCode::Home,
                "end" => KeyCode::End,
                "pageup" | "pgup" => KeyCode::PageUp,
                "pagedown" | "pgdn" => KeyCode::PageDown,
                _ => return Err(bad()),
            };
            if ctrl && code != KeyCode::Ctrl(' ') {
                return Err(format!("\"{spec}\": ctrl only works with letters and space"));
            }
            code
        }
    };
    Ok(Key { code, alt })
}

/// Inverse of `parse_key_spec`, used for hints and the generated file.
pub fn format_key(key: Key) -> String {
    let name = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Ctrl(' ') => "ctrl+space".to_string(),
        KeyCode::Ctrl(c) => format!("ctrl+{c}"),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "shift+tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::PasteStart | KeyCode::PasteEnd | KeyCode::Unknown => "?".into(),
    };
    if key.alt {
        format!("alt+{name}")
    } else {
        name
    }
}

/// A complete keys.toml with every action at its default, for
/// `alterm --print-keys`.
pub fn default_toml() -> String {
    fn section<A>(out: &mut String, name: &str, intro: &str, table: Table<A>) {
        out.push_str(&format!("\n{intro}[{name}]\n"));
        let width = table.iter().map(|t| t.0.len()).max().unwrap_or(0);
        for (key, _, defaults, about) in table {
            let keys: Vec<String> = defaults.iter().map(|k| format!("{k:?}")).collect();
            let value = format!("[{}]", keys.join(", "));
            out.push_str(&format!("{key:<width$} = {value:<28} # {about}\n"));
        }
    }
    let mut out = String::from(
        "# alterm keybindings: copy to ~/.config/alterm/keys.toml and edit.\n\
         # Every action takes a list of keys; [] unbinds it, and actions you leave\n\
         # out keep their defaults. Keys: a character (\"alt+j\", \"alt+G\"), a name\n\
         # (enter tab backspace delete insert esc space up down left right home end\n\
         # pageup pagedown), with optional alt+ / ctrl+ / shift+ prefixes.\n\
         # Changes apply the next time the panel opens.\n",
    );
    section(&mut out, "shell", "# Hotkeys in the shell. Use alt+ or ctrl+ so normal typing is unaffected.\n", SHELL);
    section(
        &mut out,
        "panel",
        "# File panel. Typing plain characters filters the list (Backspace deletes,\n\
         # and goes to the parent folder when the filter is empty), so commands use\n\
         # alt+ or keys that don't type (arrows, enter, tab, esc...).\n",
        PANEL,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyCode::*;

    #[test]
    fn key_specs() {
        assert_eq!(parse_key_spec("j"), Ok(Key::new(Char('j'))));
        assert_eq!(parse_key_spec("G"), Ok(Key::new(Char('G'))));
        assert_eq!(parse_key_spec("alt+m"), Ok(Key::alt(Char('m'))));
        assert_eq!(parse_key_spec("Alt+M"), Ok(Key::alt(Char('M'))));
        assert_eq!(parse_key_spec("ctrl+u"), Ok(Key::new(Ctrl('u'))));
        assert_eq!(parse_key_spec("alt+ctrl+x"), Ok(Key::alt(Ctrl('x'))));
        assert_eq!(parse_key_spec("ctrl+i"), Ok(Key::new(Tab)));
        assert_eq!(parse_key_spec("shift+tab"), Ok(Key::new(BackTab)));
        assert_eq!(parse_key_spec("shift+g"), Ok(Key::new(Char('G'))));
        assert_eq!(parse_key_spec("PageDown"), Ok(Key::new(PageDown)));
        assert_eq!(parse_key_spec("space"), Ok(Key::new(Char(' '))));
        assert_eq!(parse_key_spec("alt++"), Ok(Key::alt(Char('+'))));
        assert_eq!(parse_key_spec("+"), Ok(Key::new(Char('+'))));
        assert_eq!(parse_key_spec("é"), Ok(Key::new(Char('é'))));
        assert!(parse_key_spec("ctrl+h").is_err());
        assert!(parse_key_spec("ctrl+up").is_err());
        assert!(parse_key_spec("hyper+x").is_err());
        assert!(parse_key_spec("").is_err());
    }

    #[test]
    fn format_round_trips() {
        let specs = PANEL.iter().flat_map(|t| t.2.iter());
        for spec in specs.chain(["alt+ctrl+x", "shift+tab", "ctrl+space", "alt+space"].iter()) {
            let key = parse_key_spec(spec).unwrap();
            assert_eq!(parse_key_spec(&format_key(key)), Ok(key), "{spec}");
        }
        assert_eq!(format_key(Key::alt(Char('m'))), "alt+m");
    }

    #[test]
    fn defaults() {
        let k = Keymap::default();
        assert_eq!(k.shell(Key::alt(Char('m'))), Some(ShellCmd::OpenPanel));
        assert_eq!(k.shell(Key::alt(Char('o'))), None); // back to bash
        assert_eq!(k.shell(Key::new(Char('m'))), None);
        assert_eq!(k.panel(Key::alt(Char('j'))), Some(Cmd::Down));
        assert_eq!(k.panel(Key::new(Down)), Some(Cmd::Down));
        assert_eq!(k.panel(Key::alt(Char('p'))), Some(Cmd::OpenExternal));
        assert_eq!(k.panel(Key::alt(Char('G'))), Some(Cmd::Bottom));
        assert_eq!(k.panel(Key::new(Tab)), Some(Cmd::Enter));
        assert_eq!(k.panel(Key::new(Esc)), Some(Cmd::Clear));
        assert_eq!(k.panel(Key::alt(Char('m'))), Some(Cmd::Close));
        // Plain characters are never panel commands: they type into the filter.
        for c in "jkhlpcigG.~mq /".chars() {
            assert_eq!(k.panel(Key::new(Char(c))), None, "{c:?}");
        }
        assert_eq!(k.first_key(Cmd::Insert), Some(Key::alt(Char('i'))));
    }

    #[test]
    fn overrides_replace_defaults_and_claim_keys() {
        let (k, warnings) = Keymap::from_toml(
            "[shell]\nopen_panel = \"alt+f\"\n[panel]\nmark = [\"alt+c\", \"insert\"]\ncd_here = \"alt+C\"\ninsert = []\n",
        );
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(k.shell(Key::alt(Char('f'))), Some(ShellCmd::OpenPanel));
        assert_eq!(k.shell(Key::alt(Char('m'))), None);
        assert_eq!(k.panel(Key::alt(Char('c'))), Some(Cmd::Mark));
        assert_eq!(k.panel(Key::new(Insert)), Some(Cmd::Mark));
        assert_eq!(k.panel(Key::alt(Char('C'))), Some(Cmd::CdHere));
        assert_eq!(k.panel(Key::alt(Char('i'))), None);
        assert_eq!(k.panel(Key::alt(Char('j'))), Some(Cmd::Down)); // untouched default
    }

    #[test]
    fn user_keys_win_over_defaults_of_other_actions() {
        // alt+q is close's default; giving it to `home` takes it away from close.
        let (k, warnings) = Keymap::from_toml("[panel]\nhome = \"alt+q\"\n");
        assert!(warnings.is_empty());
        assert_eq!(k.panel(Key::alt(Char('q'))), Some(Cmd::Home));
        assert_eq!(k.panel(Key::alt(Char('m'))), Some(Cmd::Close));
    }

    #[test]
    fn problems_become_warnings() {
        let (k, warnings) = Keymap::from_toml(
            "[panel]\nfly = \"alt+f\"\ndown = [\"hyper+j\", \"alt+n\"]\nup = \"alt+n\"\ndown = \"alt+x\"\n\
             top = \"g\"\n[shell]\nopen_panel = \"p\"\n[filter]\nclear = \"esc\"\n",
        );
        // unknown action, bad key, alt+n taken, down set twice, plain "g" in
        // the panel, plain "p" in the shell, the old [filter] section.
        assert_eq!(warnings.len(), 7, "{warnings:#?}");
        assert_eq!(k.panel(Key::alt(Char('n'))), Some(Cmd::Down));
        assert_eq!(k.panel(Key::alt(Char('x'))), None);
        assert_eq!(k.panel(Key::new(Char('g'))), None);
        assert_eq!(k.shell(Key::new(Char('p'))), None);
        // A broken open_panel binding leaves no default either (the user set it).
        assert_eq!(k.shell(Key::alt(Char('m'))), None);
    }

    #[test]
    fn generated_default_file_parses_cleanly_to_the_defaults() {
        let (k, warnings) = Keymap::from_toml(&default_toml());
        assert_eq!(warnings, Vec::<String>::new());
        let d = Keymap::default();
        assert_eq!(k.shell, d.shell);
        assert_eq!(k.panel, d.panel);
    }
}
