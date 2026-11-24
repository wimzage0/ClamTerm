//! Low-level terminal plumbing: raw mode, window size, SIGWINCH, fd I/O.

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;

pub const STDIN: RawFd = libc::STDIN_FILENO;
pub const STDOUT: RawFd = libc::STDOUT_FILENO;

fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

// ---------------------------------------------------------------------------
// Raw mode

/// Terminal attributes saved before entering raw mode, so `restore_terminal`
/// can be called from anywhere (including the panic hook).
static ORIG_TERMIOS: Mutex<Option<libc::termios>> = Mutex::new(None);

pub fn enable_raw_mode() -> io::Result<()> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        cvt(libc::tcgetattr(STDIN, &mut t))?;
        *ORIG_TERMIOS.lock().unwrap() = Some(t);
        let mut raw = t;
        libc::cfmakeraw(&mut raw);
        cvt(libc::tcsetattr(STDIN, libc::TCSANOW, &raw))?;
    }
    Ok(())
}

/// Enter the alternate screen, hide the cursor, disable autowrap.
pub const ENTER_PANEL_SCREEN: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l";
/// Undo `ENTER_PANEL_SCREEN`, bringing back the shell's screen and scrollback.
pub const LEAVE_PANEL_SCREEN: &[u8] = b"\x1b[?7h\x1b[?25h\x1b[?1049l";

/// Whether the panel has the alternate screen, so `restore_terminal` can
/// leave it even from the panic hook.
static PANEL_SCREEN: AtomicBool = AtomicBool::new(false);

pub fn enter_panel_screen() -> io::Result<()> {
    PANEL_SCREEN.store(true, Ordering::SeqCst);
    write_all(STDOUT, ENTER_PANEL_SCREEN)
}

pub fn leave_panel_screen() -> io::Result<()> {
    if PANEL_SCREEN.swap(false, Ordering::SeqCst) {
        write_all(STDOUT, LEAVE_PANEL_SCREEN)?;
    }
    Ok(())
}

/// Leave the panel's alternate screen (if open) and undo raw mode. Safe to
/// call more than once.
pub fn restore_terminal() {
    let _ = leave_panel_screen();
    let saved = match ORIG_TERMIOS.lock() {
        Ok(mut g) => g.take(),
        Err(p) => p.into_inner().take(),
    };
    if let Some(t) = saved {
        unsafe {
            libc::tcsetattr(STDIN, libc::TCSANOW, &t);
        }
    }
}

/// Restore the terminal before the default panic message is printed, so the
/// message is readable and the user's shell isn't left in raw mode.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default(info);
    }));
}

// ---------------------------------------------------------------------------
// Window size

pub fn get_winsize(fd: RawFd) -> io::Result<libc::winsize> {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        // `as _`: the request's integer type differs between Linux and macOS.
        cvt(libc::ioctl(fd, libc::TIOCGWINSZ as _, &mut ws))?;
        Ok(ws)
    }
}

pub fn set_winsize(fd: RawFd, ws: &libc::winsize) -> io::Result<()> {
    unsafe {
        cvt(libc::ioctl(fd, libc::TIOCSWINSZ as _, ws))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SIGWINCH -> self-pipe, so resizes show up in the poll loop

static WINCH_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_sigwinch(_: libc::c_int) {
    let fd = WINCH_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let b = 1u8;
        unsafe {
            libc::write(fd, &b as *const u8 as *const libc::c_void, 1);
        }
    }
}

/// Install a SIGWINCH handler and return the read end of its self-pipe.
pub fn watch_resize() -> io::Result<RawFd> {
    let (rd, wr) = pipe()?;
    unsafe {
        WINCH_WRITE_FD.store(wr, Ordering::Relaxed);
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_sigwinch as extern "C" fn(libc::c_int) as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        cvt(libc::sigaction(libc::SIGWINCH, &sa, std::ptr::null_mut()))?;
    }
    Ok(rd)
}

/// A non-blocking, close-on-exec pipe: (read, write). Used for wakeups, so
/// a full pipe just drops the byte instead of blocking the writer. (Plain
/// `pipe` + `fcntl`, since macOS has no `pipe2`.)
pub fn pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0; 2];
    unsafe {
        cvt(libc::pipe(fds.as_mut_ptr()))?;
    }
    for fd in fds {
        set_cloexec(fd)?;
        set_nonblocking(fd)?;
    }
    Ok((fds[0], fds[1]))
}

/// Empty a non-blocking pipe.
pub fn drain(fd: RawFd) {
    let mut buf = [0u8; 64];
    while read(fd, &mut buf).map_or(false, |n| n > 0) {}
}

// ---------------------------------------------------------------------------
// fd helpers

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    unsafe {
        let flags = cvt(libc::fcntl(fd, libc::F_GETFL))?;
        cvt(libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
    }
    Ok(())
}

pub fn set_cloexec(fd: RawFd) -> io::Result<()> {
    unsafe {
        cvt(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC))?;
    }
    Ok(())
}

pub fn read(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

pub fn write(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// Write everything to a blocking fd (used for stdout).
pub fn write_all(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match write(fd, buf) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
