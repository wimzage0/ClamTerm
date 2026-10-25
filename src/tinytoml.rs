//! A parser for the small slice of TOML that keys.toml needs, so we don't
//! pull in serde: `[section]` headers and `key = "string"` or
//! `key = ["string", ...]` (arrays may span lines), plus `#` comments.
//! Errors are collected per line and parsing carries on.

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub section: String,
    pub key: String,
    pub values: Vec<String>,
    pub line: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub msg: String,
}

pub fn parse(src: &str) -> (Vec<Entry>, Vec<Error>) {
    let mut p = Parser { chars: src.chars().collect(), i: 0, line: 1 };
    let mut section = String::new();
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    loop {
        p.skip_blank_lines();
        if p.peek().is_none() {
            break;
        }
        let line = p.line;
        let result = if p.peek() == Some('[') {
            p.section().map(Err)
        } else {
            p.assignment().map(Ok)
        };
        // Only commit the statement once the rest of its line is clean.
        match result.and_then(|r| p.end_of_line().map(|_| r)) {
            Ok(Ok((key, values))) => entries.push(Entry { section: section.clone(), key, values, line }),
            Ok(Err(new_section)) => section = new_section,
            Err(msg) => {
                errors.push(Error { line, msg });
                p.skip_line();
            }
        }
    }
    (entries, errors)
}

struct Parser {
    chars: Vec<char>,
    i: usize,
    line: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\r')) {
            self.bump();
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.bump();
            }
        }
    }

    /// Whitespace, newlines and comments.
    fn skip_blank_lines(&mut self) {
        loop {
            self.skip_spaces();
            self.skip_comment();
            if self.peek() == Some('\n') {
                self.bump();
            } else {
                return;
            }
        }
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.bump() {
            if c == '\n' {
                return;
            }
        }
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_spaces();
        self.skip_comment();
        match self.peek() {
            None => Ok(()),
            Some('\n') => {
                self.bump();
                Ok(())
            }
            Some(c) => Err(format!("unexpected '{c}' after value")),
        }
    }

    fn bare_key(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')) {
            s.push(c);
            self.bump();
        }
        s
    }

    fn section(&mut self) -> Result<String, String> {
        self.bump(); // '['
        self.skip_spaces();
        let name = self.bare_key();
        self.skip_spaces();
        if name.is_empty() || self.peek() != Some(']') {
            return Err("bad section header, expected [name]".into());
        }
        self.bump();
        Ok(name)
    }

    fn assignment(&mut self) -> Result<(String, Vec<String>), String> {
        let key = match self.peek() {
            Some('"' | '\'') => self.string()?,
            _ => self.bare_key(),
        };
        if key.is_empty() {
            return Err(format!("expected a key, found '{}'", self.peek().unwrap_or(' ')));
        }
        self.skip_spaces();
        if self.peek() != Some('=') {
            return Err(format!("expected '=' after {key}"));
        }
        self.bump();
        self.skip_spaces();
        let values = match self.peek() {
            Some('[') => self.array()?,
            Some('"' | '\'') => vec![self.string()?],
            _ => return Err(format!("{key}: expected a string or an array of strings")),
        };
        Ok((key, values))
    }

    fn array(&mut self) -> Result<Vec<String>, String> {
        self.bump(); // '['
        let mut values = Vec::new();
        loop {
            self.skip_blank_lines();
            match self.peek() {
                Some(']') => {
                    self.bump();
                    return Ok(values);
                }
                Some('"' | '\'') => values.push(self.string()?),
                None => return Err("unterminated array".into()),
                Some(c) => return Err(format!("expected a string in array, found '{c}'")),
            }
            self.skip_blank_lines();
            match self.peek() {
                Some(',') => {
                    self.bump();
                }
                Some(']') => {}
                _ => return Err("expected ',' or ']' in array".into()),
            }
        }
    }

    /// `"basic"` (with \" \\ \t \n \e \uXXXX escapes) or `'literal'`.
    fn string(&mut self) -> Result<String, String> {
        let quote = self.bump().unwrap();
        let mut s = String::new();
        loop {
            // Leave a newline unconsumed so error recovery resumes on the next line.
            if matches!(self.peek(), None | Some('\n')) {
                return Err("unterminated string".into());
            }
            match self.bump() {
                Some(c) if c == quote => return Ok(s),
                Some('\\') if quote == '"' => {
                    let c = match self.bump() {
                        Some('"') => '"',
                        Some('\\') => '\\',
                        Some('t') => '\t',
                        Some('n') => '\n',
                        Some('e') => '\x1b',
                        Some('u') => {
                            let hex: String = (0..4).filter_map(|_| self.bump()).collect();
                            u32::from_str_radix(&hex, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or_else(|| format!("bad \\u escape '{hex}'"))?
                        }
                        Some(c) => return Err(format!("unknown escape '\\{c}'")),
                        None => return Err("unterminated string".into()),
                    };
                    s.push(c);
                }
                Some(c) => s.push(c),
                None => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(section: &str, key: &str, values: &[&str], line: usize) -> Entry {
        Entry {
            section: section.into(),
            key: key.into(),
            values: values.iter().map(|s| s.to_string()).collect(),
            line,
        }
    }

    #[test]
    fn sections_strings_and_arrays() {
        let src = r##"
# comment
top = "x"
[panel]
down = ["j", "down"]   # trailing comment
hash = "#"
quote = '"'
mark = 'space'
multi = [
  "a", # first
  "b",
]
[ shell ]
"quoted-key" = []
"##;
        let (entries, errors) = parse(src);
        assert_eq!(errors, []);
        assert_eq!(
            entries,
            [
                entry("", "top", &["x"], 3),
                entry("panel", "down", &["j", "down"], 5),
                entry("panel", "hash", &["#"], 6),
                entry("panel", "quote", &["\""], 7),
                entry("panel", "mark", &["space"], 8),
                entry("panel", "multi", &["a", "b"], 9),
                entry("shell", "quoted-key", &[], 14),
            ]
        );
    }

    #[test]
    fn escapes() {
        let (e, errors) = parse(r#"k = ["\"", "\\", "é"]"#);
        assert_eq!(errors, []);
        assert_eq!(e[0].values, ["\"", "\\", "é"]);
    }

    #[test]
    fn errors_are_reported_and_skipped() {
        let src = "a = \"ok\"\nb = 5\nnoequals\nc = \"unterminated\nd = [\"x\" \"y\"]\n[bad\ne = \"fine\" extra\nf = 'last'\n";
        let (entries, errors) = parse(src);
        assert_eq!(entries.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(), ["a", "f"]);
        assert_eq!(errors.iter().map(|e| e.line).collect::<Vec<_>>(), [2, 3, 4, 5, 6, 7]);
    }
}
