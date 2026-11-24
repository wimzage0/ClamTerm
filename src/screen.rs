//! Tracking whether the program in the shell is using the alternate screen
//! (vim, less, htop...). While it is, ClamTerm passes every key through and
//! never opens the panel.

/// Private modes that switch to the alternate screen.
const ALT_MODES: [u32; 3] = [47, 1047, 1049];
/// Cap on buffered CSI parameter bytes; real sequences are far shorter.
const MAX_PARAMS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    CsiStart,
    /// Inside `ESC [ ?` collecting parameters.
    Private,
    /// Inside some other CSI sequence; waiting for its final byte.
    OtherCsi,
}

/// Incremental scanner over shell output. Sequences may be split across
/// reads, so state carries over between `feed` calls.
#[derive(Debug)]
pub struct AltScreen {
    active: bool,
    state: State,
    params: Vec<u8>,
}

impl Default for AltScreen {
    fn default() -> Self {
        AltScreen { active: false, state: State::Ground, params: Vec::new() }
    }
}

impl AltScreen {
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn feed(&mut self, data: &[u8]) {
        for &b in data {
            self.state = self.step(b);
        }
    }

    fn step(&mut self, b: u8) -> State {
        use State::*;
        let is_final = (0x40..=0x7e).contains(&b);
        match self.state {
            _ if b == 0x1b => Esc,
            Ground => Ground,
            Esc => match b {
                b'[' => CsiStart,
                b'c' => {
                    // RIS (full reset) drops back to the normal screen.
                    self.active = false;
                    Ground
                }
                _ => Ground,
            },
            CsiStart if b == b'?' => {
                self.params.clear();
                Private
            }
            CsiStart | OtherCsi => {
                if is_final {
                    Ground
                } else {
                    OtherCsi
                }
            }
            Private => match b {
                b'0'..=b'9' | b';' => {
                    if self.params.len() < MAX_PARAMS {
                        self.params.push(b);
                    }
                    Private
                }
                b'h' | b'l' => {
                    self.apply(b == b'h');
                    Ground
                }
                _ if is_final => Ground,
                _ => OtherCsi,
            },
        }
    }

    fn apply(&mut self, set: bool) {
        let touches_alt = self
            .params
            .split(|&c| c == b';')
            .filter_map(|p| std::str::from_utf8(p).ok()?.parse::<u32>().ok())
            .any(|m| ALT_MODES.contains(&m));
        if touches_alt {
            self.active = set;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(chunks: &[&[u8]]) -> bool {
        let mut a = AltScreen::default();
        for c in chunks {
            a.feed(c);
        }
        a.active()
    }

    #[test]
    fn enter_and_leave_1049() {
        assert!(fed(&[b"hello\x1b[?1049hworld"]));
        assert!(!fed(&[b"\x1b[?1049h", b"bye\x1b[?1049l"]));
    }

    #[test]
    fn modes_47_and_1047() {
        assert!(fed(&[b"\x1b[?47h"]));
        assert!(fed(&[b"\x1b[?1047h"]));
        assert!(!fed(&[b"\x1b[?47h\x1b[?47l"]));
        assert!(!fed(&[b"\x1b[?1047h\x1b[?1047l"]));
    }

    #[test]
    fn combined_params() {
        // less and others set several modes at once.
        assert!(fed(&[b"\x1b[?1;1049h"]));
        assert!(fed(&[b"\x1b[?1049;25h"]));
    }

    #[test]
    fn split_across_reads() {
        assert!(fed(&[b"\x1b", b"[", b"?10", b"49", b"h"]));
        assert!(!fed(&[b"\x1b[?1049h", b"\x1b[?10", b"49l"]));
    }

    #[test]
    fn unrelated_sequences_ignored() {
        assert!(!fed(&[b"\x1b[?25l\x1b[?2004h\x1b[1049h\x1b[0m"]));
        assert!(!fed(&[b"\x1b[?10490h"]));
        assert!(fed(&[b"\x1b[?1049h\x1b[?25h\x1b[31mred\x1b[0m\x1b[?7l"]));
    }

    #[test]
    fn reset_leaves_alt_screen() {
        assert!(!fed(&[b"\x1b[?1049h\x1bc"]));
    }

    #[test]
    fn escape_restarts_sequence() {
        // A stray ESC mid-sequence starts a new one.
        assert!(fed(&[b"\x1b[?12\x1b[?1049h"]));
    }
}
