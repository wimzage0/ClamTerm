//! Handing paths to the desktop.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Open `path` with its default application via xdg-open, without blocking:
/// a folder opens in the file manager, a file in whatever handles its type.
pub fn open(path: PathBuf) {
    // Waiting happens on a thread so the panel stays responsive and no
    // zombie process is left behind.
    std::thread::spawn(move || {
        let mut cmd = Command::new("xdg-open");
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
