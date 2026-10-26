//! alterm: runs $SHELL in a PTY and passes everything through, except for
//! the hotkeys that open the file panel.

mod desktop;
mod input;
mod keymap;
mod media;
mod panel;
mod pty;
mod screen;
mod shell;
mod sys;
mod tinytoml;
mod view;

use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::time::SystemTime;

use input::ShellInput;
use media::Gfx;
use keymap::{Keymap, ShellCmd};
use panel::Panel;
use screen::AltScreen;

/// Most shell output we hold while the panel is open; beyond this the oldest
/// bytes are dropped (a runaway command shouldn't eat all memory).
const MAX_HELD_OUTPUT: usize = 8 << 20;

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(e) => {
            sys::restore_terminal();
            eprintln!("alterm: {e}");
            1
        }
    };
    std::process::exit(code);
}

const USAGE: &str = "\
usage: alterm [--print-keys | --keys-path | --help | --version]

Runs your $SHELL with a type-to-filter file panel on Alt+m.

  --print-keys   print a keys.toml with every binding at its default
  --keys-path    print where keys.toml is read from
";

fn run() -> io::Result<i32> {
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "--print-keys" => print!("{}", keymap::default_toml()),
            "--keys-path" => match keymap::config_path() {
                Some(p) => println!("{}", p.display()),
                None => return Err(io::Error::new(io::ErrorKind::NotFound, "no $HOME or $XDG_CONFIG_HOME")),
            },
            "-h" | "--help" => print!("{USAGE}"),
            "-V" | "--version" => println!("alterm {}", env!("CARGO_PKG_VERSION")),
            _ => {
                eprint!("alterm: unknown argument {arg:?}\n{USAGE}");
                return Ok(2);
            }
        }
        return Ok(0);
    }
    if unsafe { libc::isatty(sys::STDIN) } == 0 {
        return Err(io::Error::new(io::ErrorKind::Other, "stdin is not a terminal"));
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let ws = sys::get_winsize(sys::STDIN)?;
    // Warnings go out before raw mode, so they read as normal lines above
    // the shell's first prompt.
    let stamp = keys_stamp();
    let (keymap, warnings) = Keymap::load();
    for w in &warnings {
        eprintln!("alterm: {w}");
    }

    let pty = pty::spawn(&shell, &ws)?;
    sys::set_nonblocking(pty.fd())?;
    let winch = sys::watch_resize()?;

    sys::install_panic_hook();
    sys::enable_raw_mode()?;
    let (gfx, gfx_warning) = Gfx::new(&ws);
    if let Some(w) = gfx_warning {
        eprintln!("alterm: {w}");
    }
    let mut app = App::new(pty, ws, keymap, stamp, gfx);
    let result = app.event_loop(winch);
    let _ = sys::write_all(sys::STDOUT, app.gfx.clear().as_bytes());
    sys::restore_terminal();
    app.gfx.cleanup();
    if app.alt.active() {
        // The shell died under a full-screen program; don't strand the user
        // on its alternate screen.
        let _ = sys::write_all(sys::STDOUT, b"\x1b[?1049l");
    }
    result?;

    let status = app.pty.child.wait()?;
    Ok(status.code().unwrap_or(1))
}

struct App {
    pty: pty::Pty,
    ws: libc::winsize,
    /// Keystrokes not yet accepted by the PTY. The master is non-blocking so
    /// a large paste can't deadlock us against a shell busy echoing it back.
    to_shell: Vec<u8>,
    shell_input: ShellInput,
    /// Whether the shell's program is on the alternate screen.
    alt: AltScreen,
    panel: Option<Panel>,
    /// Shell output that arrived while the panel was open.
    held: Vec<u8>,
    keymap: Keymap,
    /// keys.toml's mtime when `keymap` was loaded, to notice edits.
    keymap_stamp: Option<SystemTime>,
    /// Image/PDF/video previews.
    gfx: Gfx,
}

/// The event loop should stop: the shell exited or our terminal went away.
struct Quit;

impl App {
    fn new(pty: pty::Pty, ws: libc::winsize, keymap: Keymap, keymap_stamp: Option<SystemTime>, gfx: Gfx) -> App {
        App {
            pty,
            ws,
            to_shell: Vec::new(),
            shell_input: ShellInput::default(),
            alt: AltScreen::default(),
            panel: None,
            held: Vec::new(),
            keymap,
            keymap_stamp,
            gfx,
        }
    }

    fn event_loop(&mut self, winch: RawFd) -> io::Result<()> {
        let master = self.pty.fd();
        // Thumbnail-ready notifications; -1 makes poll ignore the slot.
        let thumbs = self.gfx.wake_fd().unwrap_or(-1);
        let mut buf = [0u8; 8192];

        loop {
            let master_events = if self.to_shell.is_empty() {
                libc::POLLIN
            } else {
                libc::POLLIN | libc::POLLOUT
            };
            let mut fds = [
                libc::pollfd { fd: sys::STDIN, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: master, events: master_events, revents: 0 },
                libc::pollfd { fd: winch, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: thumbs, events: libc::POLLIN, revents: 0 },
            ];
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }

            // Shell output. EIO / EOF means the shell has exited.
            if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                match sys::read(master, &mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => self.shell_output(&buf[..n])?,
                    Err(e) if is_retry(&e) => {}
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(()),
                    Err(e) => return Err(e),
                }
            }

            // Keyboard.
            if fds[0].revents & libc::POLLIN != 0 {
                match sys::read(sys::STDIN, &mut buf) {
                    Ok(0) => return Ok(()), // our terminal went away
                    Ok(n) => self.keyboard(&buf[..n])?,
                    Err(e) if is_retry(&e) => {}
                    Err(e) => return Err(e),
                }
            } else if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }

            if self.flush_to_shell().is_err() {
                return Ok(());
            }

            if fds[2].revents & libc::POLLIN != 0 {
                sys::drain(winch);
                self.resized()?;
            }

            if fds[3].revents & libc::POLLIN != 0 {
                sys::drain(thumbs);
                self.draw()?;
            }
        }
    }

    fn shell_output(&mut self, data: &[u8]) -> io::Result<()> {
        self.alt.feed(data);
        if self.panel.is_some() {
            self.held.extend_from_slice(data);
            if self.held.len() > MAX_HELD_OUTPUT {
                let excess = self.held.len() - MAX_HELD_OUTPUT;
                self.held.drain(..excess);
            }
            Ok(())
        } else {
            sys::write_all(sys::STDOUT, data)
        }
    }

    fn keyboard(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            if self.panel.is_some() {
                data = self.panel_keys(data)?;
            } else if self.alt.active() {
                // A full-screen program owns the keyboard.
                self.to_shell.extend_from_slice(data);
                return Ok(());
            } else {
                let scan = self.shell_input.scan(data, &self.keymap);
                self.to_shell.extend_from_slice(&data[..scan.forward]);
                match scan.hotkey {
                    Some((ShellCmd::OpenPanel, end)) => {
                        self.open_panel()?;
                        data = &data[end..];
                    }
                    None => return Ok(()),
                }
            }
        }
        Ok(())
    }

    /// Feed keys to the panel until it closes; return the unused input, which
    /// belongs to the shell again.
    fn panel_keys<'a>(&mut self, data: &'a [u8]) -> io::Result<&'a [u8]> {
        let mut i = 0;
        while i < data.len() {
            let (key, n) = input::parse_key(&data[i..]);
            i += n;
            let panel = self.panel.as_mut().expect("panel is open");
            match panel.handle_key(key, &self.keymap) {
                panel::Action::None => {}
                panel::Action::Close => {
                    self.close_panel()?;
                    return Ok(&data[i..]);
                }
                panel::Action::Cd(_) | panel::Action::Edit(_) if !self.pty.shell_in_foreground() => {
                    // Typing a command into a running program would be wrong.
                    panel.set_message("shell is busy running a program");
                }
                panel::Action::Cd(dir) => {
                    self.close_panel()?;
                    self.to_shell.extend_from_slice(&shell::cd_command(&dir));
                    return Ok(&data[i..]);
                }
                panel::Action::Edit(file) => {
                    let rel = shell::relative(&file, &self.shell_cwd());
                    self.close_panel()?;
                    self.to_shell.extend_from_slice(&shell::edit_command(&rel));
                    return Ok(&data[i..]);
                }
                panel::Action::OpenExternal(path) => {
                    let msg = if path.is_dir() { "opened in file manager" } else { "opened in default app" };
                    desktop::open(path);
                    panel.set_message(msg);
                }
                panel::Action::Insert(paths) => {
                    // No busy check: typing paths into a REPL or prompt is fine.
                    let text = shell::insert_text(&paths, &self.shell_cwd());
                    self.close_panel()?;
                    self.to_shell.extend_from_slice(&text);
                    return Ok(&data[i..]);
                }
            }
        }
        self.draw()?;
        Ok(&[])
    }

    fn shell_cwd(&self) -> PathBuf {
        self.pty
            .cwd()
            .or_else(|_| std::env::current_dir())
            .unwrap_or_else(|_| "/".into())
    }

    fn open_panel(&mut self) -> io::Result<()> {
        let mut panel = Panel::open(self.shell_cwd());
        // Pick up keys.toml edits without a restart.
        let stamp = keys_stamp();
        if stamp != self.keymap_stamp {
            let (keymap, warnings) = Keymap::load();
            self.keymap = keymap;
            self.keymap_stamp = stamp;
            panel.set_message(match warnings.len() {
                0 => "reloaded keys.toml".to_string(),
                1 => warnings[0].clone(),
                n => format!("{} (+{} more; run alterm --print-keys for the format)", warnings[0], n - 1),
            });
        }
        self.panel = Some(panel);
        sys::enter_panel_screen()?;
        self.draw()
    }

    fn close_panel(&mut self) -> io::Result<()> {
        self.panel = None;
        sys::write_all(sys::STDOUT, self.gfx.clear().as_bytes())?;
        sys::leave_panel_screen()?;
        let held = std::mem::take(&mut self.held);
        sys::write_all(sys::STDOUT, &held)
    }

    fn draw(&mut self) -> io::Result<()> {
        if let Some(panel) = &mut self.panel {
            sys::write_all(sys::STDOUT, &view::render(panel, &self.keymap, &mut self.gfx, self.ws.ws_row, self.ws.ws_col))?;
        }
        Ok(())
    }

    fn resized(&mut self) -> io::Result<()> {
        if let Ok(ws) = sys::get_winsize(sys::STDIN) {
            self.ws = ws;
            self.gfx.set_winsize(&ws);
            self.pty.resize(&ws)?;
            self.draw()?;
        }
        Ok(())
    }

    /// Write pending keystrokes; `Err` means the shell is gone.
    fn flush_to_shell(&mut self) -> Result<(), Quit> {
        if self.to_shell.is_empty() {
            return Ok(());
        }
        match sys::write(self.pty.fd(), &self.to_shell) {
            Ok(n) => {
                self.to_shell.drain(..n);
                Ok(())
            }
            Err(e) if is_retry(&e) => Ok(()),
            Err(_) => Err(Quit),
        }
    }
}

fn is_retry(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted)
}

/// Modification time of keys.toml, or `None` if it doesn't exist.
fn keys_stamp() -> Option<SystemTime> {
    std::fs::metadata(keymap::config_path()?).and_then(|m| m.modified()).ok()
}
