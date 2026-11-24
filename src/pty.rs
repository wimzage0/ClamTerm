//! Spawning the user's shell on a fresh pseudo-terminal.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::sys;

pub struct Pty {
    /// Our side of the PTY: shell output is read here, keystrokes written here.
    pub master: OwnedFd,
    pub child: Child,
}

impl Pty {
    pub fn fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn resize(&self, ws: &libc::winsize) -> io::Result<()> {
        sys::set_winsize(self.fd(), ws)
    }

    /// True when the shell itself (not a program it started) owns the
    /// terminal, i.e. it's sitting at a prompt and can take a `cd`.
    pub fn shell_in_foreground(&self) -> bool {
        let pgrp = unsafe { libc::tcgetpgrp(self.fd()) };
        pgrp == self.child.id() as libc::pid_t
    }

    /// The shell's current directory (Linux: `/proc/<pid>/cwd`).
    #[cfg(not(target_os = "macos"))]
    pub fn cwd(&self) -> io::Result<PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.child.id()))
    }

    /// The shell's current directory (macOS: `proc_pidinfo`, as `lsof` does).
    #[cfg(target_os = "macos")]
    pub fn cwd(&self) -> io::Result<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        unsafe {
            let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
            let n = libc::proc_pidinfo(
                self.child.id() as libc::c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut libc::proc_vnodepathinfo as *mut libc::c_void,
                size,
            );
            if n != size {
                return Err(io::Error::last_os_error());
            }
            let path = std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr() as *const libc::c_char);
            Ok(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
        }
    }
}

/// Start `shell` attached to a new PTY of size `ws`.
pub fn spawn(shell: &str, ws: &libc::winsize) -> io::Result<Pty> {
    let (mut m, mut s) = (-1, -1);
    let ret = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            // `*mut` coerces to the `*const` Linux takes; macOS wants `*mut`.
            std::ptr::null_mut(),
            ws as *const libc::winsize as *mut libc::winsize,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(m) };
    let slave = unsafe { File::from_raw_fd(s) };
    // Neither original fd should leak into the shell; it gets the slave via
    // dup2 onto 0/1/2 only.
    sys::set_cloexec(m)?;
    sys::set_cloexec(s)?;

    let mut cmd = Command::new(shell);
    cmd.stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave))
        .env("CLAMTERM", "1");
    unsafe {
        cmd.pre_exec(|| {
            // New session, with the PTY as its controlling terminal, so job
            // control and Ctrl+C reach the shell's foreground process group.
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(Pty { master, child })
}
