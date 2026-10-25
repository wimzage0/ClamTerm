//! Keybindings: the actions alterm knows, their default keys, and loading
//! overrides from `~/.config/alterm/keys.toml`.

use std::path::PathBuf;

use crate::input::{Key, KeyCode};
use crate::tinytoml;

/// Hotkeys recognised while the shell has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellCmd {
    OpenPanel,
    OpenFilter,
}

/// File panel commands.
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
    Filter,
    Top,
    Bottom,
    PageUp,
    PageDown,
    ToggleHidden,
    Home,
    Close,
}

/// Extra commands while the filter prompt is active. Panel bindings that
/// aren't plain characters (arrows, Alt+j...) keep working there too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterCmd {
    Accept,
    Into,
    Clear,
    ClearText,
}

/// (config name, action, default keys, description for the generated file)
type Table<A> = &'static [(&'static str, A, &'static [&'static str], &'static str)];

const SHELL: Table<ShellCmd> = &[
    ("open_panel", ShellCmd::OpenPanel, &["alt+m"], "open the file panel at the shell's folder"),
    ("open_filter", ShellCmd::OpenFilter, &["alt+o"], "open the panel with the filter active"),
];

const PANEL: Table<Cmd> = &[
    ("down", Cmd::Down, &["j", "down"], "move down"),
    ("up", Cmd::Up, &["k", "up"], "move up"),
    ("parent", Cmd::Parent, &["h", "left", "backspace"], "go to the parent folder"),
    ("enter", Cmd::Enter, &["l", "right"], "enter folder / open file"),
    ("activate", Cmd::Activate, &["enter"], "folder: cd the shell there and close; file: open in editor"),
    ("cd_here", Cmd::CdHere, &["c"], "cd the shell to the folder being viewed"),
    ("mark", Cmd::Mark, &["space"], "toggle mark and move down"),
    ("insert", Cmd::Insert, &["i"], "close and type marked (or highlighted) paths into the shell"),
    ("filter", Cmd::Filter, &["/", "o"], "start filtering"),
    ("top", Cmd::Top, &["g", "home"], "jump to the first entry"),
    ("bottom", Cmd::Bottom, &["G", "end"], "jump to the last entry"),
    ("page_up", Cmd::PageUp, &["pageup"], "one page up"),
    ("page_down", Cmd::PageDown, &["pagedown"], "one page down"),
    ("toggle_hidden", Cmd::ToggleHidden, &["."], "show / hide dotfiles"),
    ("home", Cmd::Home, &["~"], "go to your home folder"),
    ("close", Cmd::Close, &["q", "esc", "alt+m"], "close without changing anything"),
];

const FILTER: Table<FilterCmd> = &[
    ("accept", FilterCmd::Accept, &["enter"], "act on the highlighted entry (cd / open)"),
    ("into", FilterCmd::Into, &["tab"], "go into the highlighted folder and keep filtering"),
    ("clear", FilterCmd::Clear, &["esc"], "clear the filter and stop filtering"),
    ("clear_text", FilterCmd::ClearText, &["ctrl+u"], "clear the filter text"),
];

#[derive(Clone, Debug)]
pub struct Keymap {
    shell: Vec<(Key, ShellCmd)>,
    panel: Vec<(Key, Cmd)>,
    filter: Vec<(Key, FilterCmd)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::from_toml("").0
    }
}

impl Keymap {
    pub fn shell(&self, key: Key) -> Option<ShellCmd> {
        lookup(&self.shell, key)
    }

    /// Panel command for `key`. A plain binding also answers to Alt+key
    /// ("h and Alt+h do the same thing") unless Alt+key is bound itself.
    pub fn panel(&self, key: Key) -> Option<Cmd> {
        lookup(&self.panel, key).or_else(|| {
            if key.alt {
                lookup(&self.panel, Key::new(key.code))
            } else {
                None
            }
        })
    }

    pub fn filter(&self, key: Key) -> Option<FilterCmd> {
        lookup(&self.filter, key)
    }

    /// First key bound to `cmd`, for footer hints.
    pub fn key_for(&self, cmd: Cmd) -> Option<String> {
        self.panel.iter().find(|(_, c)| *c == cmd).map(|(k, _)| format_key(*k))
    }

    /// Build from keys.toml text. Unset actions keep their defaults; problems
    /// are returned as warnings and the offending entry is skipped.
    pub fn from_toml(src: &str) -> (Keymap, Vec<String>) {
        let (entries, errors) = tinytoml::parse(src);
        let mut warnings: Vec<String> = errors.iter().map(|e| format!("line {}: {}", e.line, e.msg)).collect();
        let shell = build(SHELL, "shell", &entries, &mut warnings);
        let panel = build(PANEL, "panel", &entries, &mut warnings);
        let filter = build(FILTER, "filter", &entries, &mut warnings);
        for e in &entries {
            if !matches!(e.section.as_str(), "shell" | "panel" | "filter") {
                let section = if e.section.is_empty() { "(top level)" } else { e.section.as_str() };
                warnings.push(format!("line {}: unknown section [{section}]", e.line));
            }
        }
        // Plain characters as shell hotkeys would make them impossible to type.
        let mut keymap = Keymap { shell, panel, filter };
        keymap.shell.retain(|(k, _)| {
            let typable = !k.alt && matches!(k.code, KeyCode::Char(_));
            if typable {
                warnings.push(format!("[shell] \"{}\" is a plain key and would block typing it; ignored", format_key(*k)));
            }
            !typable
        });
        (keymap, warnings)
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
         # out keep their defaults. Keys: a character (\"j\", \"G\", \"/\"), a name\n\
         # (enter tab backspace delete insert esc space up down left right home end\n\
         # pageup pagedown), with optional alt+ / ctrl+ / shift+ prefixes.\n\
         # Changes apply the next time the panel opens.\n",
    );
    section(&mut out, "shell", "# Hotkeys in the shell. Use alt+ or ctrl+ so normal typing is unaffected.\n", SHELL);
    section(
        &mut out,
        "panel",
        "# File panel. A plain key also works with Alt held (h and alt+h are the same).\n",
        PANEL,
    );
    section(
        &mut out,
        "filter",
        "# While filtering, plain characters type into the filter; these keys, plus\n\
         # panel keys that aren't plain characters (arrows, alt+j...), still act.\n",
        FILTER,
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
        let specs = PANEL.iter().flat_map(|t| t.2.iter()).chain(FILTER.iter().flat_map(|t| t.2.iter()));
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
        assert_eq!(k.shell(Key::alt(Char('o'))), Some(ShellCmd::OpenFilter));
        assert_eq!(k.shell(Key::new(Char('m'))), None);
        assert_eq!(k.panel(Key::new(Char('j'))), Some(Cmd::Down));
        assert_eq!(k.panel(Key::alt(Char('j'))), Some(Cmd::Down)); // alt fallback
        assert_eq!(k.panel(Key::alt(Char('m'))), Some(Cmd::Close)); // explicit alt binding
        assert_eq!(k.panel(Key::new(Char('m'))), None);
        assert_eq!(k.filter(Key::new(Tab)), Some(FilterCmd::Into));
        assert_eq!(k.key_for(Cmd::Insert).as_deref(), Some("i"));
    }

    #[test]
    fn overrides_replace_defaults_and_claim_keys() {
        let (k, warnings) = Keymap::from_toml(
            "[shell]\nopen_panel = \"alt+f\"\n[panel]\nmark = [\"c\", \"space\"]\ncd_here = \"C\"\ninsert = []\n",
        );
        assert_eq!(warnings, Vec::<String>::new());
        assert_eq!(k.shell(Key::alt(Char('f'))), Some(ShellCmd::OpenPanel));
        assert_eq!(k.shell(Key::alt(Char('m'))), None);
        assert_eq!(k.panel(Key::new(Char('c'))), Some(Cmd::Mark));
        assert_eq!(k.panel(Key::new(Char('C'))), Some(Cmd::CdHere));
        assert_eq!(k.panel(Key::new(Char('i'))), None);
        assert_eq!(k.panel(Key::new(Char('j'))), Some(Cmd::Down)); // untouched default
    }

    #[test]
    fn user_keys_win_over_defaults_of_other_actions() {
        // 'q' is close's default; giving it to `home` takes it away from close.
        let (k, warnings) = Keymap::from_toml("[panel]\nhome = \"q\"\n");
        assert!(warnings.is_empty());
        assert_eq!(k.panel(Key::new(Char('q'))), Some(Cmd::Home));
        assert_eq!(k.panel(Key::new(Esc)), Some(Cmd::Close));
    }

    #[test]
    fn problems_become_warnings() {
        let (k, warnings) = Keymap::from_toml(
            "[panel]\nfly = \"f\"\ndown = [\"hyper+j\", \"n\"]\nup = \"n\"\ndown = \"x\"\n[shell]\nopen_panel = \"p\"\n[nope]\na = \"b\"\n",
        );
        assert_eq!(warnings.len(), 6, "{warnings:#?}");
        assert_eq!(k.panel(Key::new(Char('n'))), Some(Cmd::Down));
        assert_eq!(k.panel(Key::new(Char('x'))), None);
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
        assert_eq!(k.filter, d.filter);
    }
}
