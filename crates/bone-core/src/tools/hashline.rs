//! Hashline: `read_file` shows each line as `LINE#HASH|content`, and
//! `edit_file` places edits by those anchors instead of retyped old text.
//!
//! [`Views`] remembers, per session and file, what the model has seen: the
//! text of the file when it was read or edited (plus a few earlier
//! versions) and which lines were shown. `edit_file` uses it to place
//! anchors copied from an older read and to refuse edits over lines the
//! model never saw.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// Earlier versions kept per file, besides the latest.
const HISTORY: usize = 8;
/// Files remembered per session; the least recently used go first.
const FILES: usize = 128;

/// 2-character hash of one line's content (FNV-1a folded to 10 bits).
pub fn line_hash(line: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for byte in line.as_bytes() {
        h ^= u32::from(*byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    let v = ((h >> 16) ^ h) & 0x3ff;
    let mut out = String::with_capacity(2);
    out.push(ALPHABET[(v >> 5) as usize] as char);
    out.push(ALPHABET[(v & 31) as usize] as char);
    out
}

pub fn is_hash(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| ALPHABET.contains(&b))
}

/// `LINE#HASH|content`.
pub fn render_line(number: usize, content: &str) -> String {
    format!("{number}#{}|{content}", line_hash(content))
}

/// A text file split into lines, keeping what is needed to write it back
/// byte for byte: each line's own terminator, a byte order mark, and
/// whether the last line ends with a newline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lines {
    pub bom: bool,
    pub lines: Vec<String>,
    /// Terminator after each line ("" for a last line without one).
    pub ends: Vec<&'static str>,
}

impl Lines {
    pub fn parse(text: &str) -> Self {
        let (bom, body) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let mut lines = Vec::new();
        let mut ends = Vec::new();
        let mut rest = body;
        while !rest.is_empty() {
            match rest.find(['\n', '\r']) {
                Some(i) => {
                    let end: &'static str = if rest[i..].starts_with("\r\n") {
                        "\r\n"
                    } else if rest.as_bytes()[i] == b'\r' {
                        "\r"
                    } else {
                        "\n"
                    };
                    lines.push(rest[..i].to_owned());
                    ends.push(end);
                    rest = &rest[i + end.len()..];
                }
                None => {
                    lines.push(rest.to_owned());
                    ends.push("");
                    rest = "";
                }
            }
        }
        Lines { bom, lines, ends }
    }

    /// The newline new lines get: the file's first one, else `\n`.
    pub fn newline(&self) -> &'static str {
        self.ends
            .iter()
            .copied()
            .find(|e| !e.is_empty())
            .unwrap_or("\n")
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        for (line, end) in self.lines.iter().zip(&self.ends) {
            out.push_str(line);
            out.push_str(end);
        }
        out
    }
}

/// One state of a file the model has seen, and which lines of it.
#[derive(Debug, Clone)]
pub struct Version {
    pub lines: Vec<String>,
    /// 1-based line numbers shown to the model.
    pub seen: BTreeSet<usize>,
}

impl Version {
    pub fn all_seen(lines: Vec<String>) -> Self {
        let seen = (1..=lines.len()).collect();
        Version { lines, seen }
    }
}

#[derive(Default)]
struct FileViews {
    head: Option<Version>,
    history: VecDeque<Version>,
    used: u64,
}

#[derive(Default)]
struct SessionViews {
    files: HashMap<PathBuf, FileViews>,
    last: Option<PathBuf>,
    tick: u64,
}

/// What each session's model has seen of each file. In memory only: after a
/// restart anchors are checked against the file as it is.
#[derive(Default)]
pub struct Views(Mutex<HashMap<String, SessionViews>>);

impl Views {
    /// The model saw `seen` lines of `path` as `lines`. Seeing the same text
    /// again adds to what was seen; new text becomes the latest version.
    pub fn record(&self, session: &str, path: &Path, lines: &[String], seen: BTreeSet<usize>) {
        let mut all = self.0.lock().unwrap();
        let s = all.entry(session.to_owned()).or_default();
        s.tick += 1;
        let tick = s.tick;
        s.last = Some(path.to_owned());
        if !s.files.contains_key(path) && s.files.len() >= FILES {
            let oldest = s
                .files
                .iter()
                .min_by_key(|(_, f)| f.used)
                .map(|(p, _)| p.clone());
            if let Some(p) = oldest {
                s.files.remove(&p);
            }
        }
        let f = s.files.entry(path.to_owned()).or_default();
        f.used = tick;
        match &mut f.head {
            Some(head) if head.lines == lines => head.seen.extend(seen),
            head => {
                let new = Version {
                    lines: lines.to_vec(),
                    seen,
                };
                if let Some(old) = head.replace(new) {
                    f.history.retain(|v| v.lines != old.lines);
                    f.history.push_front(old);
                    f.history.truncate(HISTORY);
                }
            }
        }
    }

    /// The latest version of `path` the model saw, then earlier ones,
    /// newest first.
    pub fn get(&self, session: &str, path: &Path) -> (Option<Version>, Vec<Version>) {
        let all = self.0.lock().unwrap();
        match all.get(session).and_then(|s| s.files.get(path)) {
            Some(f) => (f.head.clone(), f.history.iter().cloned().collect()),
            None => (None, Vec::new()),
        }
    }

    /// The file this session last read or edited.
    pub fn last_path(&self, session: &str) -> Option<PathBuf> {
        self.0.lock().unwrap().get(session)?.last.clone()
    }

    pub fn forget(&self, session: &str) {
        self.0.lock().unwrap().remove(session);
    }
}

/// For each line of `old`, where it is in `new` if unchanged.
pub fn line_map(old: &[String], new: &[String]) -> Vec<Option<usize>> {
    let mut map = vec![None; old.len()];
    if old == new {
        return (0..old.len()).map(Some).collect();
    }
    let deadline = Some(Instant::now() + Duration::from_millis(500));
    for op in similar::capture_diff_slices_deadline(similar::Algorithm::Myers, old, new, deadline) {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for k in 0..len {
                map[old_index + k] = Some(new_index + k);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_two_alphabet_chars() {
        let h = line_hash("fn main() {");
        assert!(is_hash(&h));
        assert_eq!(h, line_hash("fn main() {"));
        assert_eq!(render_line(3, "x"), format!("3#{}|x", line_hash("x")));
    }

    #[test]
    fn lines_round_trip() {
        for text in [
            "",
            "a",
            "a\n",
            "a\r\nb",
            "a\rb\r",
            "\n\n",
            "\u{feff}x\r\n\r\ny\n",
        ] {
            let l = Lines::parse(text);
            assert_eq!(l.render(), text, "{text:?}");
        }
        assert_eq!(Lines::parse("a\n\nb").lines, ["a", "", "b"]);
        assert_eq!(Lines::parse("a\r\nb\n").newline(), "\r\n");
    }

    #[test]
    fn views_keep_history_and_merge_seen() {
        let v = Views::default();
        let p = Path::new("/f");
        let a = vec!["a".to_owned(), "b".to_owned()];
        let b = vec!["a".to_owned(), "c".to_owned()];
        v.record("s", p, &a, [1].into());
        v.record("s", p, &a, [2].into());
        let (head, hist) = v.get("s", p);
        assert_eq!(head.unwrap().seen, [1, 2].into());
        assert!(hist.is_empty());
        v.record("s", p, &b, [2].into());
        let (head, hist) = v.get("s", p);
        assert_eq!(head.unwrap().lines, b);
        assert_eq!(hist[0].lines, a);
        assert_eq!(v.last_path("s").unwrap(), p);
        assert!(v.get("other", p).0.is_none());
    }

    #[test]
    fn line_map_follows_moved_lines() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let map = line_map(&s(&["a", "b", "c"]), &s(&["x", "a", "c"]));
        assert_eq!(map, [Some(1), None, Some(2)]);
    }
}
