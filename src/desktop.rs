//! Handing paths to the desktop.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The desktop's "open with the default app" command.
const OPENER: &str = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };

/// Open `path` with its default application (xdg-open, or `open` on
/// macOS) without blocking: a folder opens in the file manager (Finder), a
/// file in whatever handles its type.
pub fn open(path: PathBuf) {
    // Waiting happens on a thread so the panel stays responsive and no
    // zombie process is left behind.
    std::thread::spawn(move || {
        let mut cmd = Command::new(OPENER);
        cmd.arg(&path);
        // No stdio (it must not draw over the panel) and its own session (it
        // must not die with the terminal).
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let _ = cmd.status();
    });
}
