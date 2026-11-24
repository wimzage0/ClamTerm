//! An index of folders you visit, ranked by "frecency" (how often and how
//! recently), so search can find folders without crawling the disk.
//!
//! Each visit adds a weight to the folder's rank; at query time the rank is
//! scaled by how long ago the last visit was. The file is plain text, one
//! folder per line: `rank<TAB>last-visit-unix-secs<TAB>path`. Several ClamTerm
//! instances share it: saving re-reads the file and adds only this
//! instance's new visits, so tabs don't overwrite each other.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// When the ranks add up to more than this, all are scaled down and the
/// faintest dropped, so old habits fade and the file stays small.
const MAX_TOTAL_RANK: f64 = 10_000.0;
/// Entries below this rank are dropped when ranks are scaled down.
const MIN_RANK: f64 = 1.0;
/// Bonus when the folder's own name contains the last search word.
const NAME_BONUS: f64 = 4.0;
/// Bonus when the folder's own name *starts with* the last search word.
const PREFIX_BONUS: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rec {
    pub rank: f64,
    /// Last visit, seconds since the Unix epoch.
    pub last: u64,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Rank weighted by recency: a folder from the last hour counts 4×, from
/// today 2×, from this week ½×, older ¼×.
pub fn frecency(rec: Rec, now: u64) -> f64 {
    let age = now.saturating_sub(rec.last);
    let factor = match age {
        0..=3_599 => 4.0,
        3_600..=86_399 => 2.0,
        86_400..=604_799 => 0.5,
        _ => 0.25,
    };
    rec.rank * factor
}

/// How well `path` matches the search words (lowercase), or `None` when it
/// doesn't. Every word must appear in the path, in order; a folder whose own
/// name contains the last word scores higher, more so if it starts with it.
pub fn match_bonus(path: &Path, words: &[String]) -> Option<f64> {
    let Some(last) = words.last() else { return Some(1.0) };
    let hay = path.to_string_lossy().to_lowercase();
    let mut from = 0;
    for w in words {
        from += hay[from..].find(w.as_str())? + w.len();
    }
    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().to_lowercase());
    Some(if name.starts_with(last.as_str()) {
        NAME_BONUS * PREFIX_BONUS
    } else if name.contains(last.as_str()) {
        NAME_BONUS
    } else {
        1.0
    })
}

/// Split a query into lowercase words.
pub fn query_words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// Search candidates: every indexed folder with its frecency, best first.
pub type Candidates = Vec<(PathBuf, f64)>;

/// Rank `candidates` against `query`; best first, at most `limit`. Each hit
/// comes with its folder's frecency (its weight, without the match bonus).
pub fn search(candidates: &[(PathBuf, f64)], query: &str, limit: usize) -> Vec<(PathBuf, f64)> {
    let words = query_words(query);
    let mut hits: Vec<(f64, &PathBuf, f64)> = candidates
        .iter()
        .filter_map(|(p, f)| match_bonus(p, &words).map(|b| (f * b, p, *f)))
        .collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    hits.into_iter().take(limit).map(|(_, p, f)| (p.clone(), f)).collect()
}

#[derive(Default)]
pub struct DirIndex {
    /// Where the index lives; `None` when there's no $HOME (nothing is saved).
    file: Option<PathBuf>,
    /// Everything known: the file as last read, plus this instance's visits.
    entries: HashMap<PathBuf, Rec>,
    /// Visits since the last save, to add to whatever the file holds then.
    pending: HashMap<PathBuf, Rec>,
    /// Folders found to be gone, removed from the file on the next save.
    removed: HashSet<PathBuf>,
    /// Folders never indexed: `/` and $HOME (one keystroke away anyway).
    skip: Vec<PathBuf>,
}

impl DirIndex {
    pub fn load() -> DirIndex {
        let mut skip = vec![PathBuf::from("/")];
        skip.extend(std::env::var_os("HOME").map(PathBuf::from));
        let file = data_path();
        if let Some(f) = &file {
            adopt_old_index(f);
        }
        let entries = file.as_deref().map(read_file).unwrap_or_default();
        DirIndex { file, entries, skip, ..Default::default() }
    }

    #[cfg(test)]
    fn at(file: PathBuf) -> DirIndex {
        DirIndex { entries: read_file(&file), file: Some(file), ..Default::default() }
    }

    /// Record a visit to `dir` worth `weight` (1 for a shell cd).
    pub fn visit(&mut self, dir: &Path, weight: f64, now: u64) {
        if !dir.is_absolute() || self.skip.iter().any(|s| s == dir) || dir.as_os_str().as_bytes().contains(&b'\n') {
            return;
        }
        for map in [&mut self.entries, &mut self.pending] {
            let rec = map.entry(dir.to_path_buf()).or_insert(Rec { rank: 0.0, last: 0 });
            rec.rank += weight;
            rec.last = rec.last.max(now);
        }
        self.removed.remove(dir);
    }

    /// Indexed folders that still exist, with their frecency, best first.
    /// Ones that have disappeared are dropped from the index.
    pub fn candidates(&mut self, now: u64) -> Candidates {
        let gone: Vec<PathBuf> = self.entries.keys().filter(|p| !p.is_dir()).cloned().collect();
        for p in gone {
            self.entries.remove(&p);
            self.pending.remove(&p);
            self.removed.insert(p);
        }
        let mut out: Candidates = self.entries.iter().map(|(p, r)| (p.clone(), frecency(*r, now))).collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Merge this instance's changes into the file (re-reading it first, so
    /// other ClamTerm windows' visits are kept) and pick up theirs.
    pub fn save(&mut self) -> io::Result<()> {
        let Some(file) = self.file.clone() else { return Ok(()) };
        let mut merged = read_file(&file);
        for (p, r) in self.pending.drain() {
            let rec = merged.entry(p).or_insert(Rec { rank: 0.0, last: 0 });
            rec.rank += r.rank;
            rec.last = rec.last.max(r.last);
        }
        for p in self.removed.drain() {
            merged.remove(&p);
        }
        age(&mut merged);
        write_file(&file, &merged)?;
        self.entries = merged;
        Ok(())
    }
}

/// Scale all ranks down once their total passes the cap, dropping the
/// faintest, so the index favours what you use now.
fn age(entries: &mut HashMap<PathBuf, Rec>) {
    let total: f64 = entries.values().map(|r| r.rank).sum();
    if total > MAX_TOTAL_RANK {
        let factor = 0.9 * MAX_TOTAL_RANK / total;
        entries.retain(|_, r| {
            r.rank *= factor;
            r.rank >= MIN_RANK
        });
    }
}

/// `$XDG_DATA_HOME/clamterm/dirs`, else `~/.local/share/clamterm/dirs`.
pub fn data_path() -> Option<PathBuf> {
    Some(data_home()?.join("clamterm").join("dirs"))
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// ClamTerm used to be called alterm: start from its index if there's none
/// yet under the new name. Copied, not moved, so alterm windows still open
/// keep theirs.
fn adopt_old_index(file: &Path) {
    let Some(old) = data_home().map(|d| d.join("alterm").join("dirs")) else { return };
    if !file.exists() && old.is_file() {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::copy(&old, file);
    }
}

fn read_file(file: &Path) -> HashMap<PathBuf, Rec> {
    std::fs::read(file).map(|data| parse(&data)).unwrap_or_default()
}

/// Parse the index text; malformed lines are skipped.
fn parse(data: &[u8]) -> HashMap<PathBuf, Rec> {
    let mut out = HashMap::new();
    for line in data.split(|&b| b == b'\n') {
        let mut parts = line.splitn(3, |&b| b == b'\t');
        let (Some(rank), Some(last), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let num = |b: &[u8]| std::str::from_utf8(b).ok().map(str::to_owned);
        let (Some(rank), Some(last)) = (
            num(rank).and_then(|s| s.parse::<f64>().ok()),
            num(last).and_then(|s| s.parse::<u64>().ok()),
        ) else {
            continue;
        };
        let path = PathBuf::from(OsStr::from_bytes(path));
        if path.is_absolute() && rank.is_finite() && rank > 0.0 {
            out.insert(path, Rec { rank, last });
        }
    }
    out
}

fn format(entries: &HashMap<PathBuf, Rec>) -> Vec<u8> {
    let mut rows: Vec<_> = entries.iter().collect();
    rows.sort_by(|a, b| b.1.rank.total_cmp(&a.1.rank).then_with(|| a.0.cmp(b.0)));
    let mut out = Vec::new();
    for (path, rec) in rows {
        out.extend_from_slice(format!("{:.3}\t{}\t", rec.rank, rec.last).as_bytes());
        out.extend_from_slice(path.as_os_str().as_bytes());
        out.push(b'\n');
    }
    out
}

/// Write via a temporary file and rename, so a crash or another instance
/// reading at the same moment never sees half a file.
fn write_file(file: &Path, entries: &HashMap<PathBuf, Rec>) -> io::Result<()> {
    let dir = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".dirs.{}.tmp", std::process::id()));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(&format(entries))?;
    f.sync_all()?;
    std::fs::rename(&tmp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn recency_weights() {
        let r = |age: u64| frecency(Rec { rank: 10.0, last: NOW - age }, NOW);
        assert_eq!(r(0), 40.0);
        assert_eq!(r(7_200), 20.0);
        assert_eq!(r(3 * 86_400), 5.0);
        assert_eq!(r(30 * 86_400), 2.5);
    }

    #[test]
    fn matching_needs_words_in_order() {
        let w = |q: &str| query_words(q);
        let path = p("/home/u/Documents/dev/clamterm");
        assert!(match_bonus(&path, &w("doc clam")).is_some());
        assert!(match_bonus(&path, &w("DEV")).is_some());
        assert!(match_bonus(&path, &w("clam doc")).is_none()); // wrong order
        assert!(match_bonus(&path, &w("nope")).is_none());
        assert_eq!(match_bonus(&path, &w("")), Some(1.0));
        // The folder's own name: prefix beats contains beats elsewhere.
        assert_eq!(match_bonus(&path, &w("clam")), Some(NAME_BONUS * PREFIX_BONUS));
        assert_eq!(match_bonus(&path, &w("term")), Some(NAME_BONUS));
        assert_eq!(match_bonus(&path, &w("docu")), Some(1.0));
    }

    #[test]
    fn search_ranks_name_matches_above_busier_parents() {
        let cands = vec![
            (p("/home/u/Documents/dev/clamterm"), 40.0), // busiest, but only its path has "dev"
            (p("/home/u/Documents/dev"), 8.0),
            (p("/home/u/Music"), 100.0),
        ];
        // Each hit carries its folder's own weight, not the boosted score.
        assert_eq!(search(&cands, "dev", 10), [(p("/home/u/Documents/dev"), 8.0), (p("/home/u/Documents/dev/clamterm"), 40.0)]);
        // Empty query: plain frecency order.
        let paths: Vec<PathBuf> = search(&cands, "", 2).into_iter().map(|h| h.0).collect();
        assert_eq!(paths, [p("/home/u/Music"), p("/home/u/Documents/dev/clamterm")]);
        assert!(search(&cands, "zzz", 10).is_empty());
    }

    #[test]
    fn file_round_trip_and_bad_lines() {
        let text = b"3.500\t1700000000\t/a/b c\n\
                     junk\n\
                     2\tnotanumber\t/x\n\
                     1\t5\trelative/path\n\
                     -1\t5\t/negative\n\
                     1.000\t1700000001\t/tab\tin name\n";
        let m = parse(text);
        assert_eq!(m.len(), 2);
        assert_eq!(m[&p("/a/b c")], Rec { rank: 3.5, last: 1_700_000_000 });
        assert!(m.contains_key(&p("/tab\tin name")));
        assert_eq!(parse(&format(&m)), m);
    }

    #[test]
    fn ageing_scales_down_and_drops_faint_entries() {
        let mut m = HashMap::new();
        m.insert(p("/big"), Rec { rank: 10_999.0, last: 1 });
        m.insert(p("/faint"), Rec { rank: 1.0, last: 1 });
        age(&mut m);
        assert!(m[&p("/big")].rank < MAX_TOTAL_RANK);
        assert!(!m.contains_key(&p("/faint")));
    }

    #[test]
    fn saving_merges_with_other_instances() {
        let dir = std::env::temp_dir().join(format!("clamterm-dirindex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("clamterm/dirs");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();

        // Two windows load the (empty) index, each visits something.
        let mut a = DirIndex::at(file.clone());
        let mut b = DirIndex::at(file.clone());
        a.visit(&real, 1.0, NOW);
        a.visit(&real, 1.0, NOW + 5);
        a.save().unwrap();
        b.visit(&real, 0.5, NOW + 10);
        b.visit(Path::new("/gone/away"), 1.0, NOW);
        b.save().unwrap();

        // b's save kept a's visits and added its own.
        let merged = read_file(&file);
        assert_eq!(merged[&real], Rec { rank: 2.5, last: NOW + 10 });
        // Folders that no longer exist drop out of the candidates and the file.
        let cands = b.candidates(NOW + 10);
        assert_eq!(cands.iter().map(|c| &c.0).collect::<Vec<_>>(), [&real]);
        b.save().unwrap();
        assert!(!read_file(&file).contains_key(Path::new("/gone/away")));
        // Saving with no new visits doesn't change anything.
        a.save().unwrap();
        assert_eq!(read_file(&file)[&real].rank, 2.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_root_home_and_relative() {
        let mut idx = DirIndex { skip: vec![p("/"), p("/home/u")], ..Default::default() };
        for d in ["/", "/home/u", "rel/dir", "/a\nb"] {
            idx.visit(Path::new(d), 1.0, NOW);
        }
        assert!(idx.entries.is_empty());
        idx.visit(Path::new("/home/u/x"), 1.0, NOW);
        assert_eq!(idx.entries.len(), 1);
    }
}
