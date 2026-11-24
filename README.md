# ClamTerm

A terminal that doubles as a keyboard-driven file manager.

ClamTerm runs your normal shell and gets out of the way: every keystroke goes
straight to bash, vim, htop or whatever is running. Press **Alt+m** and a
full-screen file panel opens at the shell's current folder. Type to filter,
preview files (including images and PDFs), then cd the shell there, open a file
in your editor, or drop paths onto the command line. Press **Alt+r** to jump
to any folder you've been to before, ranked by how much you use it. Your
shell's screen and scrollback come back untouched when the panel closes.

```
 ~/Documents/dev/clamterm                                               2 marked
 src/                          │ PNG image · 1280×720 · 34.1 K
 target/                       │
*Cargo.lock                    │   ┌──────────────────────┐
 Cargo.toml                    │   │      (the image)     │
*README.md                     │   └──────────────────────┘
 / rea█                                          1/5  ⏎ open  tab into  esc clear
```

```
 search folders you've visited (42 indexed)
 ~/Documents/dev/clamterm/                 12.0 │ src/
 ~/Documents/dev/clamterm/src/              4.0 │ Cargo.lock
 ~/Documents/dev/image-parser/              2.0 │ Cargo.toml
 search / clam█                       3/42  ⏎ cd  tab browse  esc clear
```

It's a small Rust program (two dependencies: `libc` and `unicode-width`) that
runs inside your existing terminal emulator. It doesn't replace it.

## Features

- **Feels like a normal terminal.** The shell runs on its own pseudo-terminal;
  ClamTerm only intercepts its two hotkeys, Alt+m and Alt+r. Full-screen
  programs (vim, less, htop) get every key, those two included.
- **Type-to-filter file panel.** Folders first, case-insensitive sorting,
  colors for folders, symlinks and executables, hidden files on demand.
- **Jump to folders you use.** Alt+r searches every folder you've been to,
  ranked by how often and how recently you visit (like zoxide), with each
  folder's weight shown next to it. Nothing crawls the disk: folders are
  indexed as you visit them.
- **Previews.** Folder contents, the start of text files (JSON, code, logs…),
  and images, SVGs, PDFs and videos drawn in the terminal on Ghostty, kitty
  and WezTerm.
- **Acts on your shell.** cd the shell to a folder, open a file in
  `$VISUAL`/`$EDITOR`, type quoted paths into the command line, or open
  anything in its default desktop app.
- **Configurable keys** in `~/.config/clamterm/keys.toml`, picked up without
  restarting.

## Requirements

- **Linux or macOS.**
- **Rust 1.70 or newer** to build ([rustup](https://rustup.rs)). On macOS also
  the Xcode command line tools, for the linker: `xcode-select --install`.
- **Optional, for image previews:** a terminal with the kitty graphics protocol
  (Ghostty, kitty or WezTerm). Thumbnails come from tools that are usually
  already there:
  - **Linux:** `gdk-pixbuf-thumbnailer` (images, SVG), `pdftoppm`/`pdfinfo`/
    `pdftotext` from poppler-utils (PDF), `ffmpeg`/`ffprobe` (video). All
    present on a stock Pop!_OS desktop.
  - **macOS:** Quick Look (`qlmanage`), `sips` and Spotlight (`mdls`), all
    built in. `brew install poppler` adds PDF text in terminals without
    graphics.

  Anything missing just means a simpler preview for that file type.
- **Optional:** `xdg-open` (from xdg-utils) for Alt+p on Linux; macOS uses
  `open`.

## Install

```bash
git clone <this repo> ~/Documents/dev/clamterm
cd ~/Documents/dev/clamterm
cargo install --path . --locked
```

This puts the binary at `~/.cargo/bin/clamterm` (make sure `~/.cargo/bin` is on
your `PATH`). Run `clamterm` in any terminal to try it, and `exit` to leave.

To update after pulling or editing the code, run the same `cargo install`
command again. Already-open terminals keep the old version until reopened.

### Coming from alterm

ClamTerm used to be called alterm. Your folder index comes along on its own:
the first time ClamTerm starts without an index, it copies
`~/.local/share/alterm/dirs` (it doesn't move it, so open alterm windows keep
working). Everything else uses the new name, so:

- Install ClamTerm as above, then `cargo uninstall alterm`.
- Replace `alterm-session` with `clamterm-session` (below) and point Ghostty's
  `command` at it.
- In `.bashrc`, check `$CLAMTERM` instead of `$ALTERM`; rename `ALTERM_IMAGES` /
  `ALTERM_SKIP` to `CLAMTERM_IMAGES` / `CLAMTERM_SKIP` wherever you set them.
- Move `~/.config/alterm/keys.toml` to `~/.config/clamterm/keys.toml` if you
  have one.
- Once no alterm windows are left, `~/.local/share/alterm/` can be deleted.

## Make it your default terminal session

### Ghostty

1. Create a small launcher, `~/.local/bin/clamterm-session`, that falls back to
   plain bash if ClamTerm is ever missing, so a broken install can't leave you
   without a terminal:

   ```sh
   #!/bin/sh
   # Ghostty's `command`: start ClamTerm, or a plain shell if it isn't
   # installed. Set CLAMTERM_SKIP=1 (e.g. `CLAMTERM_SKIP=1 ghostty`) to skip it.
   clamterm="$HOME/.cargo/bin/clamterm"
   if [ -z "$CLAMTERM_SKIP" ] && [ -x "$clamterm" ]; then
       exec "$clamterm"
   fi
   exec "${SHELL:-/bin/bash}"
   ```

   ```bash
   chmod +x ~/.local/bin/clamterm-session
   ```

2. Point Ghostty at it in `~/.config/ghostty/config.ghostty` (or
   `~/.config/ghostty/config`), using your own home path:

   ```
   command = direct:/home/YOU/.local/bin/clamterm-session
   ```

   Check the file with `ghostty +validate-config`, then reload Ghostty's
   config (**Ctrl+Shift+,**) or restart Ghostty. Every new window and tab now
   starts in ClamTerm. `echo $CLAMTERM` prints `1` inside it.

3. Restore Ghostty's shell integration (new tabs opening in the current
   folder, jumping between prompts, cursor shape, window titles). Ghostty only
   injects it when it starts bash itself, so add this near the top of
   `~/.bashrc`, after the "not interactive, return" check:

   ```bash
   # Ghostty shell integration inside ClamTerm. Plain bash in Ghostty skips this.
   if [ -n "$CLAMTERM" ] && [ -n "$GHOSTTY_RESOURCES_DIR" ]; then
       : "${GHOSTTY_SHELL_FEATURES:=cursor,title,path}"
       export GHOSTTY_SHELL_FEATURES
       builtin source "$GHOSTTY_RESOURCES_DIR/shell-integration/bash/ghostty.bash"
   fi
   ```

### Other terminals

Any terminal that can run a custom command works: point it at
`~/.local/bin/clamterm-session`. In GNOME Terminal that's Preferences → your
profile → Command → "Run a custom command instead of my shell". Image previews
need a kitty-graphics terminal; elsewhere you get the info line (and PDF text).

### macOS

macOS keyboards have no Alt key: **ClamTerm's Alt is ⌥ Option**, and the
footer shows ⌥ instead of `alt+`. (⌘ can't be used: terminals keep ⌘ keys for
themselves and macOS, so they never reach programs.) The terminal has to send
Option as Alt:

- **Ghostty:** add `macos-option-as-alt = left` to the config. The left ⌥
  then acts as Alt for ClamTerm while the right ⌥ still types special
  characters (å, ø, @, …). Use `true` for both.
- **Terminal.app:** Settings → Profiles → Keyboard → "Use Option as Meta key".
- **iTerm2:** Settings → Profiles → Keys → Left Option key: "Esc+".

The setup steps above work the same, with two differences:

- Home is `/Users/YOU`, so Ghostty's line is
  `command = direct:/Users/YOU/.local/bin/clamterm-session`.
- The default shell is **zsh**, so the shell-integration snippet goes in
  `~/.zshrc` instead:

  ```zsh
  # Ghostty shell integration inside ClamTerm. Plain zsh in Ghostty skips this.
  if [[ -n "$CLAMTERM" && -n "$GHOSTTY_RESOURCES_DIR" ]]; then
      source "$GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration"
  fi
  # Keep ClamTerm's cd / editor commands (they start with a space) out of history.
  setopt HIST_IGNORE_SPACE
  ```

Files live where they do on Linux: `~/.config/clamterm/keys.toml` and
`~/.local/share/clamterm/dirs`.

**If Alt+m opens a menu instead of the panel** (GNOME Terminal), turn off
Preferences → General → "Enable mnemonics" and "Enable the menu accelerator
key". To check what your terminal sends, run `cat -v` and press Alt+m: you
should see `^[m`.

## Usage

### In the shell

| Key | Action |
|---|---|
| Alt+m | Open the file panel at the shell's current folder |
| Alt+r | Open the panel searching folders you've visited (see [Folder search](#folder-search)) |

Everything else goes to the shell unchanged. While a full-screen program is
running (vim, less, htop…) even Alt+m and Alt+r go to that program. (Alt+r
replaces bash's rarely used `revert-line`.)

### In the panel

The panel is always filtering: **plain characters narrow the list**
(case-insensitive substring), so commands use Alt or keys that don't type.
On macOS, read Alt as ⌥ Option throughout.

| Key | Action |
|---|---|
| letters, digits, space… | Type into the filter |
| Backspace | Delete a filter letter. With an empty filter, go to the parent folder |
| Esc | Clear the filter. With an empty filter, close the panel |
| Ctrl+U | Clear the filter without closing |
| Alt+j / Alt+k, ↓ / ↑ | Move down / up |
| Alt+h, ← | Parent folder, with the cursor on the folder you came from |
| Alt+l, →, Tab | Enter the folder (fresh filter), or open the file in your editor |
| Enter | Folder: cd the shell there and close. File: open in `$VISUAL` / `$EDITOR` (nano if unset). Nothing selected: cd to the folder being viewed |
| Alt+c | cd the shell to the folder being viewed |
| Alt+Space, Ctrl+Space | Toggle mark and move down. Marks survive folder changes |
| Alt+i | Close and type the marked paths (or the highlighted one) into the command line: quoted, relative to the shell's folder, trailing space, no Enter |
| Alt+p | Open in the default app: files in whatever handles their type, folders in your file manager. The panel stays open |
| Alt+g / Alt+G, Home / End | First / last entry |
| PgUp / PgDn | Page up / down |
| Alt+. | Show / hide hidden files |
| Alt+~ | Go to your home folder |
| Alt+r | Search visited folders; press again to go back to browsing |
| Alt+m, Alt+q | Close without changing anything |

The footer lists the current bindings, and switches to the filter text with a
match count while you type.

How commands reach the shell:

- **cd and the editor** are typed into the shell as ` cd -- '…'` /
  ` ${VISUAL:-${EDITOR:-nano}} '…'`, after Ctrl+E Ctrl+U to clear the
  command line. Note that this discards anything half-typed at the prompt.
  The leading space keeps them out of bash history when `HISTCONTROL` contains
  `ignorespace` or `ignoreboth` (the Ubuntu/Pop!_OS default). Because your
  shell expands the editor variable, editors set in `.bashrc` and values like
  `code -w` work.
- **Paths are quoted** for any name: spaces, quotes, `$`, newlines, non-UTF-8
  bytes. Names starting with `-` get a `./` prefix.
- **Busy shell:** if a program is running in the foreground, cd and the editor
  are refused ("shell is busy running a program") instead of typing into that
  program. Alt+i still works, so you can type paths into a REPL prompt.
- **Shell output while the panel is open** is held and shown when it closes.

### Folder search

ClamTerm keeps an index of the folders you visit, so finding one never means
walking your whole disk:

- Every time the shell's folder changes (a `cd`, `pushd`, a script that cds…)
  the new folder gets a visit. Browsing into a folder in the panel counts as
  half a visit. `/` and your home folder aren't indexed.
- Results are ranked by **frecency**: visit count, weighted by recency (×4 if
  you were there in the last hour, ×2 today, ×½ this week, ×¼ older).
- Type part of the path to narrow the results. Every word must appear in the
  path, in that order (`dev clam` finds `~/Documents/dev/clamterm`). Folders
  whose own name matches the last word rank higher, more so when the name
  starts with it, so `dev` puts `~/Documents/dev` ahead of folders inside it.

Each result shows its weight (its frecency) on the right, shortened to fit:
`0.5`, `12.3`, `840`, `1.2k`. With a query, the order also counts how well the
name matches, so a folder can rank above a heavier one whose name doesn't match
the last word. In a narrow window the path is cut from the left so the
folder's own name and the weight stay visible.

In search mode:

| Key | Action |
|---|---|
| typing | Narrow the results; the best match is highlighted |
| Enter, Alt+c | cd the shell there and close |
| Tab, Alt+l, → | Browse into that folder in the panel |
| Esc | Clear the query; with an empty query, back to browsing |
| Backspace on an empty query, Alt+r, Alt+h, ← | Back to browsing |
| Alt+p, Alt+i, Alt+Space | Open in file manager, insert the path, mark: as usual |

The index is a plain text file, `~/.local/share/clamterm/dirs` (or
`$XDG_DATA_HOME/clamterm/dirs`): one line per folder with its rank, last visit
time and path. Every ClamTerm window shares it: saving merges with what's on
disk, so tabs don't overwrite each other. Folders that no longer exist drop out
the next time you search, and once the ranks add up to more than 10 000 they
are all scaled down and the faintest dropped, so old habits fade. Delete the
file to start over. It starts empty (or with your old alterm index) and fills
as you move around.

### Previews

| File | Preview |
|---|---|
| Folder | Its contents |
| Text (JSON, code, logs, …) | The first lines, with escape codes stripped |
| PNG, JPEG, GIF, WebP, TIFF, BMP, ICO | `PNG image · 1920×1080 · 1.2 M`, then the image |
| SVG | Info line, then the rendered image (the source without graphics) |
| PDF | `PDF document · 288.3 K · 93 pages`, then page 1 (its text without graphics) |
| Video | `video · 58.0 K · 0:12`, then a frame from 5 s in |
| Other binaries | `binary file, 1.2 M` |

Thumbnails are made in the background (only for the file you stop on, 10 s
limit per file) and kept in `$XDG_RUNTIME_DIR/clamterm-<pid>/`, deleted on exit.
Images are fitted to the pane and never drawn larger than their real size.

## Configuration

Keybindings live in `~/.config/clamterm/keys.toml` (or
`$XDG_CONFIG_HOME/clamterm/keys.toml`; `clamterm --keys-path` prints it). Start
from the defaults, which come with a comment on every action:

```bash
mkdir -p ~/.config/clamterm
clamterm --print-keys > ~/.config/clamterm/keys.toml
```

```toml
[shell]
open_panel = ["alt+m"]
search = ["alt+r"]

[panel]
down = ["alt+n", "down"]   # replaces the default ["alt+j", "down"]
insert = []                # unbinds it
```

- Each action takes a list of keys. Setting an action replaces its defaults,
  `[]` unbinds it, and actions you leave out keep their defaults.
- A key you set wins over another action's default: binding `alt+q` to `home`
  takes it away from `close`.
- Key names: a character with a modifier (`"alt+j"`, `"alt+G"`, `"ctrl+u"`) or
  a named key (`enter tab backspace delete insert esc space up down left right
  home end pageup pagedown`), with optional `alt+`, `ctrl+`, `shift+`.
  `opt+` / `option+` are the same as `alt+`.
- Plain characters can't be bound in either section, since they'd stop you
  typing them (in the shell, or into the panel's filter).
- Changes apply the next time the panel opens ("reloaded keys.toml" in the
  footer). Mistakes are skipped with a warning: in the footer, or above the
  first prompt when ClamTerm starts.

### Environment variables

| Variable | Effect |
|---|---|
| `CLAMTERM` | Set to `1` inside ClamTerm (useful in `.bashrc`) |
| `CLAMTERM_IMAGES` | `1` / `0` forces image previews on / off. Default: on for Ghostty, kitty and WezTerm, off inside tmux/screen |
| `CLAMTERM_SKIP` | Read by the launcher script: start a plain shell instead |
| `SHELL` | The shell ClamTerm runs (default `/bin/bash`) |
| `VISUAL`, `EDITOR` | The editor for Enter / Alt+l on files (read by your shell) |
| `XDG_CONFIG_HOME` | Where `clamterm/keys.toml` is looked up |
| `XDG_DATA_HOME` | Where the folder index (`clamterm/dirs`) is kept |

## How it works

ClamTerm puts your terminal in raw mode, starts `$SHELL` on a fresh
pseudo-terminal, and copies bytes both ways. The shell's current folder comes
from `/proc/<pid>/cwd` on Linux and `proc_pidinfo` on macOS. Input is scanned for the hotkey
without being re-encoded, so the shell sees exactly what the terminal sent.
Alt+x arrives as `ESC x` within one read; a lone `ESC` at the end of a read is
the Escape key. Output is scanned for alternate-screen switches (`ESC[?1049h`,
modes 47/1047) to know when a full-screen program is running. The panel draws
on the terminal's own alternate screen with autowrap off, overwriting every
row instead of clearing, so it doesn't flicker and the shell's screen comes
back intact. Window resizes are passed on to the shell, and the terminal is
restored on exit and on panic.

| File | Role |
|---|---|
| `src/main.rs` | Event loop: shell output, keyboard, resize, thumbnail wakeups. Carries out panel actions |
| `src/sys.rs` | Raw mode, window size, SIGWINCH, fd helpers, panel screen enter/leave |
| `src/pty.rs` | Spawning the shell on a PTY, its cwd (Linux and macOS) and whether it's busy |
| `src/input.rs` | Bytes to keys; hotkey scanning (ignores bracketed paste) |
| `src/screen.rs` | Tracking the shell program's alternate screen |
| `src/keymap.rs` | Actions, default keys, keys.toml loading |
| `src/tinytoml.rs` | The small TOML subset keys.toml uses |
| `src/panel.rs` | File panel model: listing, filter, marks, keys to actions (no terminal I/O) |
| `src/view.rs` | Drawing the panel (wide characters, truncation, footer hints) |
| `src/media.rs` | Media detection, image sizes, background thumbnails (Linux tools or Quick Look), kitty graphics |
| `src/dirindex.rs` | The visited-folder index: frecency, matching, saving and merging |
| `src/shell.rs` | Quoting and the cd / editor / insert keystrokes |
| `src/desktop.rs` | `xdg-open` / `open` for Alt+p |

The panel's model (`panel.rs`, `keymap.rs`) is kept apart from rendering and
I/O so it could later move into a standalone terminal emulator.

## Development

```bash
cargo build --release
cargo test
```

To check the macOS build from Linux (compiles everything, no linking):

```bash
rustup target add aarch64-apple-darwin
cargo check --target aarch64-apple-darwin --all-targets
```

The unit tests cover key parsing, hotkey detection, alt-screen tracking, shell
quoting (round-tripped through a real `sh`), the panel model, folder search
ranking and index merging, the TOML parser, image header parsing (checked
against files made by ffmpeg), and layout.

For end-to-end checks, run the binary inside tmux and drive it with
`tmux send-keys`:

```bash
tmux new-session -d -s t -x 120 -y 30 "SHELL=/bin/bash ./target/release/clamterm"
tmux send-keys -t t M-m          # open the panel
tmux send-keys -t t src Enter    # filter to src/, cd there and close
tmux send-keys -t t pwd Enter
tmux capture-pane -t t -p        # check the screen
tmux kill-session -t t
```

tmux doesn't pass images through, so previews fall back to the text version
there. `CLAMTERM_IMAGES=1` forces the image path, to check thumbnails get made.

## Limitations

- Linux and macOS only (it needs a way to read another process's folder).
- cd, the editor and Alt+i type into the shell's line editor; they're tested
  with bash. Other shells using emacs-style keys (zsh's default) should work;
  vi-mode line editing doesn't.
- On macOS, Alt is ⌥ Option and the terminal must send it as Alt (see
  [macOS](#macos)); ⌘ shortcuts can't reach ClamTerm.
- Inside the panel, Alt+Space may be taken by your desktop (window menu);
  Ctrl+Space also marks.
- Left and Right Alt can't be told apart: terminals send the same bytes.
- Image previews need the kitty graphics protocol; other terminals get the
  text fallback.
