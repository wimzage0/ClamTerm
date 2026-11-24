//! Previews for images, PDFs and videos: recognising them, making PNG
//! thumbnails on a background thread with the desktop's own tools
//! (gdk-pixbuf-thumbnailer, pdftoppm, ffmpeg), and drawing them with the
//! kitty graphics protocol (Ghostty, kitty, WezTerm).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use crate::sys;

/// Longest side of generated thumbnails, in pixels.
const THUMB_PX: u32 = 1024;
/// Converters get this long before we give up on a file.
const TIMEOUT_SECS: &str = "10";
/// Lines of PDF text shown when the terminal can't draw images.
const PDF_TEXT_LINES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    /// A raster image; the name is shown ("PNG", "JPEG"...).
    Image(&'static str),
    Svg,
    Pdf,
    Video,
}

impl MediaKind {
    pub fn label(self) -> String {
        match self {
            MediaKind::Image(name) => format!("{name} image"),
            MediaKind::Svg => "SVG image".into(),
            MediaKind::Pdf => "PDF document".into(),
            MediaKind::Video => "video".into(),
        }
    }
}

/// Recognise a media file from its first bytes, or its extension for
/// formats without a reliable signature. `None` means "treat as text or
/// binary as before".
pub fn detect(path: &Path, head: &[u8]) -> Option<MediaKind> {
    let starts = |sig: &[u8]| head.starts_with(sig);
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Some(MediaKind::Image("PNG"));
    }
    if starts(b"\xff\xd8\xff") {
        return Some(MediaKind::Image("JPEG"));
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Some(MediaKind::Image("GIF"));
    }
    if starts(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return Some(MediaKind::Image("WebP"));
    }
    if starts(b"II*\0") || starts(b"MM\0*") {
        return Some(MediaKind::Image("TIFF"));
    }
    if starts(b"%PDF-") {
        return Some(MediaKind::Pdf);
    }
    let ext = path.extension().and_then(OsStr::to_str).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "bmp" if starts(b"BM") => Some(MediaKind::Image("BMP")),
        "ico" => Some(MediaKind::Image("ICO")),
        "svg" | "svgz" => Some(MediaKind::Svg),
        "mp4" | "m4v" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "flv" | "mpg" | "mpeg" | "ogv" | "3gp" => {
            Some(MediaKind::Video)
        }
        _ => None,
    }
}

/// Pixel size from an image header (PNG, GIF, BMP, WebP, JPEG).
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le32 = |i: usize| Some(i32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?).unsigned_abs());
    let le24 = |i: usize| {
        let s = b.get(i..i + 3)?;
        Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
    };

    if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.get(12..16) == Some(b"IHDR") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        return Some((le32(18)?, le32(22)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let s = b.get(21..25)?;
                let w = 1 + (((s[1] as u32 & 0x3f) << 8) | s[0] as u32);
                let h = 1 + (((s[3] as u32 & 0xf) << 10) | (s[2] as u32) << 2 | (s[1] as u32 & 0xc0) >> 6);
                Some((w, h))
            }
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            _ => None,
        };
    }
    if b.starts_with(b"\xff\xd8") {
        // Walk the JPEG segments to the first start-of-frame marker.
        let mut i = 2;
        while i + 3 < b.len() {
            if b[i] != 0xff {
                return None;
            }
            let marker = b[i + 1];
            match marker {
                0xff => i += 1, // fill byte
                0xd0..=0xd9 | 0x01 => i += 2,
                0xc0..=0xcf if !matches!(marker, 0xc4 | 0xc8 | 0xcc) => {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                _ => i += 2 + be16(i + 2)? as usize,
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Terminal graphics

/// Whether the outer terminal speaks the kitty graphics protocol.
/// `CLAMTERM_IMAGES=0` / `=1` overrides the guess.
pub fn graphics_supported() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    match var("CLAMTERM_IMAGES").as_str() {
        "0" | "off" | "no" => return false,
        "1" | "on" | "yes" => return true,
        _ => {}
    }
    // Multiplexers swallow the graphics escapes.
    if std::env::var_os("TMUX").is_some() || var("TERM").starts_with("screen") {
        return false;
    }
    let term = var("TERM");
    matches!(var("TERM_PROGRAM").as_str(), "ghostty" | "WezTerm")
        || term.contains("kitty")
        || term.contains("ghostty")
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
}

/// Remove every image we've placed (and free its data).
pub const DELETE_ALL: &str = "\x1b_Ga=d,d=A,q=2\x1b\\";

/// Draw the PNG at `png` with its top-left at 0-based (`col`, `row`),
/// scaled to `cols` cells wide (height follows the aspect ratio). The
/// terminal reads the file itself; `q=2` keeps it from replying on stdin,
/// and `C=1` leaves the cursor alone.
pub fn place(png: &Path, col: usize, row: usize, cols: usize) -> String {
    format!(
        "\x1b[{};{}H\x1b_Ga=T,t=f,f=100,q=2,C=1,c={cols};{}\x1b\\",
        row + 1,
        col + 1,
        base64(png.as_os_str().as_bytes())
    )
}

/// Columns to give an image of `px` pixels so it fits in a box of
/// `box_cols` × `box_rows` cells of `cell` pixels, never enlarged.
pub fn fit_columns(px: (u32, u32), box_cols: usize, box_rows: usize, cell: (f64, f64)) -> usize {
    let (w, h) = (px.0.max(1) as f64, px.1.max(1) as f64);
    let scale = (box_cols as f64 * cell.0 / w).min(box_rows as f64 * cell.1 / h).min(1.0);
    ((w * scale / cell.0).floor() as usize).clamp(1, box_cols.max(1))
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Graphics state

/// What the view needs for media previews: whether images can be drawn,
/// the thumbnailer, the cell size in pixels, and which image is on screen
/// (so it's only re-sent when something changes).
pub struct Gfx {
    pub enabled: bool,
    thumbs: Option<Thumbnailer>,
    /// Pixel size of one terminal cell (from TIOCGWINSZ when it's reported).
    pub cell: (f64, f64),
    /// The placement escape currently shown, if any.
    shown: Option<String>,
    /// Positions changed (resize): re-place even if `shown` matches.
    dirty: bool,
}

impl Gfx {
    /// Set up previews; a warning comes back if thumbnails can't be made.
    pub fn new(ws: &libc::winsize) -> (Gfx, Option<String>) {
        let enabled = graphics_supported();
        let (thumbs, warning) = match Thumbnailer::new(enabled) {
            Ok(t) => (Some(t), None),
            Err(e) => (None, Some(format!("previews disabled: {e}"))),
        };
        let mut gfx = Gfx { enabled: enabled && thumbs.is_some(), thumbs, cell: (10.0, 20.0), shown: None, dirty: false };
        gfx.set_winsize(ws);
        (gfx, warning)
    }

    pub fn wake_fd(&self) -> Option<RawFd> {
        self.thumbs.as_ref().map(Thumbnailer::wake_fd)
    }

    /// Thumbnail/info for a media file: `None` while it's being made.
    pub fn thumb(&self, path: &Path, kind: MediaKind) -> Option<Result<Thumb, String>> {
        match &self.thumbs {
            Some(t) => t.get(path, kind),
            None => Some(Err("previews unavailable".into())),
        }
    }

    pub fn set_winsize(&mut self, ws: &libc::winsize) {
        if ws.ws_xpixel > 0 && ws.ws_ypixel > 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            self.cell = (ws.ws_xpixel as f64 / ws.ws_col as f64, ws.ws_ypixel as f64 / ws.ws_row as f64);
        }
        self.dirty = true;
    }

    /// Escapes that change the screen from what's shown to `want`.
    pub fn update(&mut self, want: Option<String>) -> String {
        if !self.enabled || (want == self.shown && !self.dirty) {
            return String::new();
        }
        let mut out = String::new();
        if self.shown.is_some() || self.dirty {
            out.push_str(DELETE_ALL);
        }
        out.push_str(want.as_deref().unwrap_or(""));
        self.shown = want;
        self.dirty = false;
        out
    }

    /// Escapes that remove any shown image (before leaving the panel).
    pub fn clear(&mut self) -> &'static str {
        match self.shown.take() {
            Some(_) if self.enabled => DELETE_ALL,
            _ => "",
        }
    }

    pub fn cleanup(&self) {
        if let Some(t) = &self.thumbs {
            t.cleanup();
        }
    }
}

// ---------------------------------------------------------------------------
// Background thumbnailer

#[derive(Clone, Debug, Default)]
pub struct Thumb {
    /// PNG to draw, when graphics are on and a converter succeeded.
    pub png: Option<PathBuf>,
    pub px: (u32, u32),
    /// Extra info for the header line ("12 pages", "3:41").
    pub info: Option<String>,
    /// Text to show instead of an image (PDFs without graphics).
    pub text: Vec<String>,
}

/// A file is identified by path, mtime and size, so edits get a new thumbnail.
type Key = (PathBuf, Option<SystemTime>, u64);

#[derive(Default)]
struct Shared {
    /// Only the latest request matters; scrolling past files replaces it.
    job: Option<(Key, MediaKind)>,
    done: HashMap<Key, Result<Thumb, String>>,
}

pub struct Thumbnailer {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    wake: RawFd,
    dir: PathBuf,
}

impl Thumbnailer {
    /// Start the worker. `graphics` says whether to make images (else only
    /// info and PDF text). Thumbnails live in a private per-process folder.
    pub fn new(graphics: bool) -> io::Result<Thumbnailer> {
        let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("clamterm-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let (wake, wake_tx) = sys::pipe()?;
        let shared: Arc<(Mutex<Shared>, Condvar)> = Arc::default();
        let worker = (Arc::clone(&shared), dir.clone());
        std::thread::spawn(move || work(worker.0, worker.1, wake_tx, graphics));
        Ok(Thumbnailer { shared, wake, dir })
    }

    /// Readable when a thumbnail finished; drain it and redraw.
    pub fn wake_fd(&self) -> RawFd {
        self.wake
    }

    /// The finished thumbnail for `path`, or `None` after queueing it.
    pub fn get(&self, path: &Path, kind: MediaKind) -> Option<Result<Thumb, String>> {
        let meta = std::fs::metadata(path).ok();
        let key = (path.to_path_buf(), meta.as_ref().and_then(|m| m.modified().ok()), meta.map_or(0, |m| m.len()));
        let (lock, cvar) = &*self.shared;
        let mut s = lock.lock().unwrap();
        if let Some(r) = s.done.get(&key) {
            return Some(r.clone());
        }
        if s.job.as_ref().map(|(k, _)| k) != Some(&key) {
            s.job = Some((key, kind));
            cvar.notify_one();
        }
        None
    }

    /// Delete the thumbnail folder (call before exiting).
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn work(shared: Arc<(Mutex<Shared>, Condvar)>, dir: PathBuf, wake: RawFd, graphics: bool) {
    let (lock, cvar) = &*shared;
    for n in 0u64.. {
        let (key, kind) = {
            let mut s = lock.lock().unwrap();
            loop {
                if let Some(job) = s.job.take() {
                    break job;
                }
                s = cvar.wait(s).unwrap();
            }
        };
        let result = make_thumb(&key.0, kind, &dir.join(format!("{n}.png")), graphics);
        lock.lock().unwrap().done.insert(key, result);
        let _ = sys::write(wake, b"x");
    }
}

fn make_thumb(src: &Path, kind: MediaKind, out: &Path, graphics: bool) -> Result<Thumb, String> {
    let mut thumb = Thumb::default();
    match kind {
        MediaKind::Pdf => {
            let info = run("pdfinfo", &[src.as_os_str()]).unwrap_or_default();
            let pages = info
                .lines()
                .find_map(|l| l.strip_prefix("Pages:"))
                .map(|n| n.trim().to_string())
                .or_else(|| spotlight(src, "kMDItemNumberOfPages"));
            thumb.info = pages.map(|n| if n == "1" { "1 page".into() } else { format!("{n} pages") });
            if !graphics {
                // Without poppler (stock macOS) there's just no text.
                let mut args = os(&["-l", "3", "-layout"]);
                args.extend([src.as_os_str(), OsStr::new("-")]);
                let text = run("pdftotext", &args).unwrap_or_default();
                thumb.text = text.lines().take(PDF_TEXT_LINES).map(str::to_owned).collect();
            }
        }
        MediaKind::Video => {
            let mut args = os(&["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"]);
            args.push(src.as_os_str());
            let secs = run("ffprobe", &args)
                .ok()
                .or_else(|| spotlight(src, "kMDItemDurationSeconds"))
                .and_then(|s| s.trim().parse::<f64>().ok());
            thumb.info = secs.map(|s| {
                let s = s as u64;
                if s >= 3600 {
                    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
                } else {
                    format!("{}:{:02}", s / 60, s % 60)
                }
            });
        }
        MediaKind::Image(_) | MediaKind::Svg => {}
    }
    if !graphics {
        return Ok(thumb);
    }

    let size = THUMB_PX.to_string();
    let o = out.as_os_str();
    // macOS: Quick Look renders every kind we preview; the tools below are
    // the fallback (and the only route on Linux).
    let quick_looked = cfg!(target_os = "macos") && quick_look(src, out, &size).is_ok();
    match kind {
        _ if quick_looked => {}
        MediaKind::Image(_) | MediaKind::Svg if cfg!(target_os = "macos") => {
            let mut args = os(&["-s", "format", "png", "-Z", &size]);
            args.push(src.as_os_str());
            args.extend([OsStr::new("--out"), o]);
            run("sips", &args)?;
        }
        MediaKind::Image(_) | MediaKind::Svg => {
            let mut args = os(&["-s", &size]);
            args.extend([src.as_os_str(), o]);
            run("gdk-pixbuf-thumbnailer", &args)?;
        }
        MediaKind::Pdf => {
            // -singlefile writes <stem>.png
            let stem = out.with_extension("");
            let mut args = os(&["-png", "-f", "1", "-l", "1", "-singlefile", "-scale-to", &size]);
            args.extend([src.as_os_str(), stem.as_os_str()]);
            run("pdftoppm", &args)?;
        }
        MediaKind::Video => {
            let scale = format!("scale='min({THUMB_PX},iw)':-2");
            // A frame a few seconds in is usually more telling than frame 0;
            // short clips fall back to the start.
            for at in ["5", "0"] {
                let mut args = os(&["-v", "error", "-y", "-ss", at, "-i"]);
                args.push(src.as_os_str());
                args.extend(os(&["-frames:v", "1", "-vf", &scale]));
                args.push(o);
                if run("ffmpeg", &args).is_ok() && std::fs::metadata(out).map_or(false, |m| m.len() > 0) {
                    break;
                }
            }
        }
    }
    let head = std::fs::read(out).map_err(|_| "no preview could be made".to_string())?;
    thumb.px = image_size(&head).ok_or("preview image is unreadable")?;
    thumb.png = Some(out.to_path_buf());
    Ok(thumb)
}

/// macOS Quick Look thumbnail of `src` at `out`. qlmanage names its output
/// after the input, so it writes into a scratch folder first.
fn quick_look(src: &Path, out: &Path, size: &str) -> Result<(), String> {
    let dir = out.with_extension("ql");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut args = os(&["-t", "-s", size, "-o"]);
    args.extend([dir.as_os_str(), src.as_os_str()]);
    let made = run("qlmanage", &args).ok().and_then(|_| {
        std::fs::read_dir(&dir).ok()?.filter_map(Result::ok).map(|e| e.path()).find(|p| p.extension() == Some(OsStr::new("png")))
    });
    let result = match made {
        Some(png) => std::fs::rename(png, out).map_err(|e| e.to_string()),
        None => Err("Quick Look made no thumbnail".into()),
    };
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// A Spotlight attribute (macOS `mdls`), e.g. a PDF's page count; `None`
/// elsewhere or when Spotlight doesn't know it.
fn spotlight(src: &Path, attr: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut args = os(&["-name", attr, "-raw"]);
    args.push(src.as_os_str());
    let value = run("mdls", &args).ok()?;
    let value = value.trim();
    (!value.is_empty() && value != "(null)").then(|| value.to_string())
}

fn os<'a>(args: &[&'a str]) -> Vec<&'a OsStr> {
    args.iter().map(|a| OsStr::new(*a)).collect()
}

/// Run a converter with a timeout and no terminal; stdout on success.
fn run<S: AsRef<OsStr>>(prog: &str, args: &[S]) -> Result<String, String> {
    let out = Command::new("timeout")
        .arg(TIMEOUT_SECS)
        .arg(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Some(124) => Err(format!("{prog} timed out")),
        Some(127) => Err(format!("{prog} is not installed")),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(format!("{prog}: {}", err.lines().next().unwrap_or("failed")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b
    }

    #[test]
    fn detects_by_magic_and_extension() {
        let p = Path::new;
        assert_eq!(detect(p("x"), &png_header(1, 1)), Some(MediaKind::Image("PNG")));
        assert_eq!(detect(p("x.dat"), b"\xff\xd8\xff\xe0"), Some(MediaKind::Image("JPEG")));
        assert_eq!(detect(p("x"), b"GIF89a.."), Some(MediaKind::Image("GIF")));
        assert_eq!(detect(p("x"), b"RIFF\0\0\0\0WEBPVP8 "), Some(MediaKind::Image("WebP")));
        assert_eq!(detect(p("doc"), b"%PDF-1.7\n"), Some(MediaKind::Pdf));
        assert_eq!(detect(p("a.SVG"), b"<svg"), Some(MediaKind::Svg));
        assert_eq!(detect(p("clip.mkv"), b"\x1a\x45\xdf\xa3"), Some(MediaKind::Video));
        // Text and JSON stay text, even with media-ish extensions misused.
        assert_eq!(detect(p("data.json"), b"{\"a\": 1}"), None);
        assert_eq!(detect(p("notes.txt"), b"BM is not a bitmap"), None);
        assert_eq!(detect(p("fake.bmp"), b"hello"), None);
    }

    #[test]
    fn sizes_from_headers() {
        assert_eq!(image_size(&png_header(1920, 1080)), Some((1920, 1080)));
        assert_eq!(image_size(b"GIF89a\x40\x01\xf0\x00"), Some((320, 240)));
        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[18..22].copy_from_slice(&640i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&(-480i32).to_le_bytes()); // top-down
        assert_eq!(image_size(&bmp), Some((640, 480)));
        // VP8X: 24-bit (w-1, h-1)
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend_from_slice(&[0x7f, 0x02, 0x00, 0xdf, 0x01, 0x00]);
        assert_eq!(image_size(&webp), Some((640, 480)));
        // JPEG: APP0 segment, then SOF0 with height 600, width 800.
        let jpeg = b"\xff\xd8\xff\xe0\x00\x04ab\xff\xc0\x00\x11\x08\x02\x58\x03\x20\x03";
        assert_eq!(image_size(jpeg), Some((800, 600)));
        assert_eq!(image_size(b"plain text"), None);
    }

    #[test]
    fn real_files_from_ffmpeg() {
        // Cross-check the header parsers against real encoder output.
        let dir = std::env::temp_dir().join(format!("clamterm-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ext in ["png", "jpg", "gif", "bmp", "webp"] {
            let f = dir.join(format!("t.{ext}"));
            let ok = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=321x123", "-frames:v", "1"])
                .arg(&f)
                .status()
                .map_or(false, |s| s.success());
            if !ok {
                continue; // ffmpeg missing or no encoder for this format
            }
            let data = std::fs::read(&f).unwrap();
            assert!(detect(&f, &data).is_some(), "{ext}");
            assert_eq!(image_size(&data), Some((321, 123)), "{ext}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fitting() {
        let cell = (10.0, 20.0);
        // Small image: native size (32 px = 3 cells), never enlarged.
        assert_eq!(fit_columns((32, 32), 40, 20, cell), 3);
        // Wide image limited by width.
        assert_eq!(fit_columns((4000, 1000), 40, 20, cell), 40);
        // Tall image limited by height: 400 px box / 4000 px → 0.1 → 100 px = 10 cells.
        assert_eq!(fit_columns((1000, 4000), 40, 20, cell), 10);
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    #[test]
    fn placement_escape() {
        let s = place(Path::new("/t/a.png"), 4, 2, 10);
        assert_eq!(s, "\x1b[3;5H\x1b_Ga=T,t=f,f=100,q=2,C=1,c=10;L3QvYS5wbmc=\x1b\\");
    }
}
