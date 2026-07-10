//! Hashline patch parser.
//!
//! Grammar (strict, with a few tolerated model-friendly variants):
//! ```text
//! Patch    := Section+
//! Section  := Header Op+
//! Header   := "[" Path "#" Tag "]"
//! Op       := Swap | Del | InsPre | InsPost | InsHead | InsTail | Move | Remove
//! Swap     := "SWAP" Range [":"] Body
//! Del      := "DEL" Range                 (no body)
//! InsPre   := "INS.PRE" Int [":"] Body
//! InsPost  := "INS.POST" Int [":"] Body
//! InsHead  := "INS.HEAD" [":"] Body
//! InsTail  := "INS.TAIL" [":"] Body
//! Move     := "MV" Path                    (no body)
//! Remove   := "REM"                        (no body)
//! Range    := Int (".=" | ".." | "…" | "—" | "–" | "-") Int | Int
//! Body     := ("+" Line?)*                 // a bare "+" is one empty line
//! ```
//! The parser deliberately rejects `apply_patch` sentinels, unified-diff `@@`
//! hunks, and `-old` rows with teaching errors that point the model at the
//! hashline syntax. Line numbers are 1-indexed and refer to the original
//! snapshot the model read (the apply engine never shifts them per hunk).

/// One parsed `[path#tag] ... ops ...` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub path: String,
    pub tag: String,
    pub ops: Vec<Op>,
}

/// The full patch: one or more sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub sections: Vec<Section>,
}

/// A single edit operation. Line numbers are 1-indexed, original-snapshot lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Replace original lines `start..=end` (inclusive) with `body`.
    Swap {
        start: usize,
        end: usize,
        body: Vec<String>,
    },
    /// Delete original lines `start..=end` (inclusive).
    Del { start: usize, end: usize },
    /// Insert `body` immediately before original line `line`.
    InsPre { line: usize, body: Vec<String> },
    /// Insert `body` immediately after original line `line`.
    InsPost { line: usize, body: Vec<String> },
    /// Insert `body` at the very top (before line 1).
    InsHead { body: Vec<String> },
    /// Insert `body` at the very end (after the last line).
    InsTail { body: Vec<String> },
    /// Rename this section's file to `dest`.
    Move { dest: String },
    /// Delete this section's file entirely.
    Remove,
}

impl Op {
    /// The body lines, for body-bearing ops; empty otherwise.
    pub fn body(&self) -> &[String] {
        match self {
            Op::Swap { body, .. }
            | Op::InsPre { body, .. }
            | Op::InsPost { body, .. }
            | Op::InsHead { body }
            | Op::InsTail { body } => body,
            Op::Del { .. } | Op::Move { .. } | Op::Remove => &[],
        }
    }

    /// True if this op carries (and requires) a `+`-prefixed body.
    pub fn takes_body(&self) -> bool {
        matches!(
            self,
            Op::Swap { .. }
                | Op::InsPre { .. }
                | Op::InsPost { .. }
                | Op::InsHead { .. }
                | Op::InsTail { .. }
        )
    }

    fn push_body(&mut self, line: String) {
        match self {
            Op::Swap { body, .. }
            | Op::InsPre { body, .. }
            | Op::InsPost { body, .. }
            | Op::InsHead { body }
            | Op::InsTail { body } => body.push(line),
            // Body lines are only collected when body_sink is set, which only
            // happens for body-bearing ops; reaching here is a logic bug.
            _ => {}
        }
    }

    /// Short human label for error messages.
    pub fn label(&self) -> &'static str {
        match self {
            Op::Swap { .. } => "SWAP",
            Op::Del { .. } => "DEL",
            Op::InsPre { .. } => "INS.PRE",
            Op::InsPost { .. } => "INS.POST",
            Op::InsHead { .. } => "INS.HEAD",
            Op::InsTail { .. } => "INS.TAIL",
            Op::Move { .. } => "MV",
            Op::Remove => "REM",
        }
    }

    /// Original-snapshot line range this op touches, if any (for overlap checks).
    /// Insert ops occupy the single anchor line (they don't remove it).
    #[allow(dead_code)]
    pub fn anchor_range(&self) -> Option<(usize, usize)> {
        match self {
            Op::Swap { start, end, .. } | Op::Del { start, end } => Some((*start, *end)),
            Op::InsPre { line, .. } | Op::InsPost { line, .. } => Some((*line, *line)),
            Op::InsHead { .. } | Op::InsTail { .. } | Op::Move { .. } | Op::Remove => None,
        }
    }
}

/// Parse a hashline patch string into a [`Patch`].
pub fn parse(input: &str) -> Result<Patch, String> {
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<Section> = None;
    // index into `current.ops` of the op currently collecting `+` body lines.
    let mut body_sink: Option<usize> = None;
    // The label of the most recent non-body op, so an orphan `+` row after a
    // DEL/MV/REM gets a targeted error instead of a generic one.
    let mut prev_nonbody: Option<&'static str> = None;
    // The label of a body op whose body was ended by a blank line, so an
    // orphan `+` row after the gap explains the bare-`+` convention.
    let mut broken_body: Option<&'static str> = None;

    for (idx, raw) in input.split('\n').enumerate() {
        let lineno = idx + 1;
        // Tolerate a stray CR from CRLF input.
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim();

        if trimmed.is_empty() {
            if let (Some(i), Some(sec)) = (body_sink.take(), current.as_ref()) {
                broken_body = Some(sec.ops[i].label());
            }
            continue;
        }

        // apply_patch envelopes are tolerated (stripped); any other `*** `
        // sentinel is the apply_patch format and is rejected with a pointer.
        if trimmed == "*** Begin Patch" || trimmed == "*** End Patch" {
            body_sink = None;
            continue;
        }
        if trimmed.starts_with("*** ") {
            return Err(teach(
                format!(
                    "apply_patch sentinel `{trimmed}` is not supported; use [path#TAG] sections with SWAP/DEL/INS ops"
                ),
                lineno,
            ));
        }

        // Body row: a `+` at column 0. Content is everything after the `+`,
        // indentation preserved. A bare `+` is one empty inserted line.
        if let Some(rest) = line.strip_prefix('+') {
            let sink_idx = body_sink.ok_or_else(|| {
                let hint = prev_nonbody
                    .map(|n| format!("{n} takes no body"))
                    .or_else(|| {
                        broken_body.map(|n| {
                            format!(
                                "the blank line above ended the {n} body; \
                                 write blank body lines as a bare `+`"
                            )
                        })
                    })
                    .unwrap_or_else(|| "start a SWAP/INS op first".to_string());
                teach(
                    format!("body row `+...` has no op to attach to ({hint})"),
                    lineno,
                )
            })?;
            let sec = current
                .as_mut()
                .ok_or_else(|| teach("body row outside any section".into(), lineno))?;
            sec.ops[sink_idx].push_body(rest.to_string());
            continue;
        }

        // Reject unified-diff and apply_patch constructs explicitly so the
        // model gets a teachable error rather than "unknown op".
        if trimmed.starts_with("@@") {
            return Err(teach(
                "unified-diff `@@` hunks are not supported; use SWAP/DEL/INS ops".into(),
                lineno,
            ));
        }
        if line.starts_with('-') {
            return Err(teach(
                "`-` rows are not supported; body rows use a `+` prefix (delete lines with DEL)"
                    .into(),
                lineno,
            ));
        }

        // Header [path#TAG].
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() > 2 {
            if let Some(sec) = current.take() {
                sections.push(finalize_section(sec, lineno)?);
            }
            body_sink = None;
            prev_nonbody = None;
            broken_body = None;
            let inner = &trimmed[1..trimmed.len() - 1];
            let (path, tag) = split_header(inner).ok_or_else(|| {
                teach(
                    format!("bad header `{trimmed}`; expected [path#TAG]"),
                    lineno,
                )
            })?;
            current = Some(Section {
                path: path.to_string(),
                tag: tag.to_string(),
                ops: Vec::new(),
            });
            continue;
        }

        // Op line.
        let sec = current.as_mut().ok_or_else(|| {
            teach(
                "op outside any section; start with a [path#TAG] header".into(),
                lineno,
            )
        })?;
        let body_ctx = body_sink.map(|i| sec.ops[i].label());
        let (op, takes_body) = parse_op(trimmed, lineno, body_ctx)?;
        sec.ops.push(op);
        let new_idx = sec.ops.len() - 1;
        broken_body = None;
        if takes_body {
            body_sink = Some(new_idx);
            prev_nonbody = None;
        } else {
            body_sink = None;
            prev_nonbody = Some(sec.ops[new_idx].label());
        }
    }

    if let Some(sec) = current.take() {
        sections.push(finalize_section(sec, input.split('\n').count())?);
    }

    if sections.is_empty() {
        return Err("patch has no sections; start with a [path#TAG] header".to_string());
    }
    Ok(Patch { sections })
}

/// Validate a finished section: every body-bearing op has a non-empty body,
/// and the section has at least one op.
fn finalize_section(sec: Section, lineno: usize) -> Result<Section, String> {
    if sec.ops.is_empty() {
        return Err(teach(
            format!("section [{}#{}] has no ops", sec.path, sec.tag),
            lineno,
        ));
    }
    for op in &sec.ops {
        if op.takes_body() && op.body().is_empty() {
            return Err(teach(
                format!(
                    "{} has an empty body; add `+` lines, or use DEL to delete / REM to remove the file",
                    op.label()
                ),
                lineno,
            ));
        }
    }
    Ok(sec)
}

fn split_header(inner: &str) -> Option<(&str, &str)> {
    // Use the last '#' so a path containing '#' still splits correctly.
    let idx = inner.rfind('#')?;
    let path = inner[..idx].trim();
    let tag = inner[idx + 1..].trim();
    if path.is_empty() || tag.is_empty() {
        return None;
    }
    Some((path, tag))
}

/// Strip an op keyword only at a word boundary, so a body line missing its
/// `+` prefix (e.g. `DELTA_X = 3`, `MVP::new()`) falls through to the
/// unknown-op error instead of half-parsing as an op.
fn strip_op<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(kw)?;
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_whitespace() || c == ':' => Some(rest),
        Some(_) => None,
    }
}

/// Parse one op line (already trimmed) into the op and whether it takes a
/// body. `body_ctx` is the label of the op currently collecting body lines,
/// if any, so an unparseable line mid-body gets a missing-`+` hint.
fn parse_op(
    trimmed: &str,
    lineno: usize,
    body_ctx: Option<&'static str>,
) -> Result<(Op, bool), String> {
    if let Some(rest) = strip_op(trimmed, "SWAP") {
        let (range, leftover) = split_optional_colon(rest.trim());
        require_empty(leftover, "SWAP", lineno)?;
        let (start, end) = parse_range(range, lineno)?;
        return Ok((
            Op::Swap {
                start,
                end,
                body: Vec::new(),
            },
            true,
        ));
    }
    if let Some(rest) = strip_op(trimmed, "DEL") {
        let (range, leftover) = split_optional_colon(rest.trim());
        require_empty(leftover, "DEL", lineno)?;
        let (start, end) = parse_range(range, lineno)?;
        return Ok((Op::Del { start, end }, false));
    }
    if let Some(rest) = strip_op(trimmed, "INS.PRE") {
        let (num, leftover) = split_optional_colon(rest.trim());
        require_empty(leftover, "INS.PRE", lineno)?;
        let line = parse_line_num(num, "INS.PRE", lineno)?;
        return Ok((
            Op::InsPre {
                line,
                body: Vec::new(),
            },
            true,
        ));
    }
    if let Some(rest) = strip_op(trimmed, "INS.POST") {
        let (num, leftover) = split_optional_colon(rest.trim());
        require_empty(leftover, "INS.POST", lineno)?;
        let line = parse_line_num(num, "INS.POST", lineno)?;
        return Ok((
            Op::InsPost {
                line,
                body: Vec::new(),
            },
            true,
        ));
    }
    if let Some(rest) = strip_op(trimmed, "INS.HEAD") {
        let (_, leftover) = split_optional_colon(rest.trim());
        require_empty(leftover, "INS.HEAD", lineno)?;
        return Ok((Op::InsHead { body: Vec::new() }, true));
    }
    if let Some(rest) = strip_op(trimmed, "INS.TAIL") {
        let (_, leftover) = split_optional_colon(rest.trim());
        require_empty(leftover, "INS.TAIL", lineno)?;
        return Ok((Op::InsTail { body: Vec::new() }, true));
    }
    if let Some(rest) = strip_op(trimmed, "MV") {
        let dest = rest.trim();
        if dest.is_empty() {
            return Err(teach("MV needs a destination path".into(), lineno));
        }
        return Ok((
            Op::Move {
                dest: dest.to_string(),
            },
            false,
        ));
    }
    if trimmed == "REM" {
        return Ok((Op::Remove, false));
    }
    let mut msg = format!(
        "unknown op `{trimmed}`; expected one of SWAP/DEL/INS.PRE/INS.POST/INS.HEAD/INS.TAIL/MV/REM"
    );
    if let Some(label) = body_ctx {
        msg.push_str(&format!(
            " — if this is body text for the {label} above, every body line must start with `+` at column 0"
        ));
    }
    Err(teach(msg, lineno))
}

/// Split `rest` into the part before an optional trailing colon and the
/// (possibly empty) leftover after it. `"3.=5:"` -> `("3.=5", "")`;
/// `"3.=5"` -> `("3.=5", "")`; `"3.=5 : x"` -> `("3.=5 : x", "")` (caller
/// then rejects the spaces). A lone trailing colon is consumed.
fn split_optional_colon(rest: &str) -> (&str, &str) {
    if let Some(core) = rest.strip_suffix(':') {
        (core, "")
    } else {
        (rest, "")
    }
}

/// Reject anything other than the op keyword itself (modulo an optional colon,
/// already stripped) so typos like `INS.HEAD5` are caught.
fn require_empty(leftover: &str, op: &str, lineno: usize) -> Result<(), String> {
    let leftover = leftover.trim();
    if leftover.is_empty() {
        Ok(())
    } else {
        Err(teach(format!("unexpected `{leftover}` after {op}"), lineno))
    }
}

fn parse_line_num(num: &str, op: &str, lineno: usize) -> Result<usize, String> {
    let num = num.trim();
    let n = num
        .parse::<usize>()
        .map_err(|_| teach(format!("{op} needs a line number, got `{num}`"), lineno))?;
    if n == 0 {
        return Err(teach(format!("{op} line number must be >= 1"), lineno));
    }
    Ok(n)
}

/// Parse a range token (`N.=M`, `N..M`, `N-M`, `N…M`, …) or a bare `N`.
fn parse_range(s: &str, lineno: usize) -> Result<(usize, usize), String> {
    let s = s.trim();
    // Order matters: multi-char separators before single-char hyphen.
    for sep in [".=", "..", "…", "—", "–", "-"] {
        if let Some(idx) = s.find(sep) {
            let (a, b) = (&s[..idx], &s[idx + sep.len()..]);
            let start = a.trim().parse::<usize>().map_err(|_| {
                teach(
                    format!("range start `{}` is not a number", a.trim()),
                    lineno,
                )
            })?;
            let end = b
                .trim()
                .parse::<usize>()
                .map_err(|_| teach(format!("range end `{}` is not a number", b.trim()), lineno))?;
            if start == 0 || end == 0 {
                return Err(teach("range line numbers must be >= 1".into(), lineno));
            }
            if end < start {
                return Err(teach(
                    format!("range end {end} is before start {start}"),
                    lineno,
                ));
            }
            return Ok((start, end));
        }
    }
    let n = s
        .parse::<usize>()
        .map_err(|_| teach(format!("`{s}` is not a line number or range"), lineno))?;
    if n == 0 {
        return Err(teach("line numbers must be >= 1".into(), lineno));
    }
    Ok((n, n))
}

fn teach(msg: String, lineno: usize) -> String {
    format!("line {lineno}: {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swap(start: usize, end: usize, body: &[&str]) -> Op {
        Op::Swap {
            start,
            end,
            body: body.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parses_swap_with_body() {
        let p = parse("[src/a.rs#A1B2]\nSWAP 3.=4:\n+fn new() {}\n+// done\n").unwrap();
        assert_eq!(p.sections.len(), 1);
        let s = &p.sections[0];
        assert_eq!(s.path, "src/a.rs");
        assert_eq!(s.tag, "A1B2");
        assert_eq!(s.ops, vec![swap(3, 4, &["fn new() {}", "// done"])]);
    }

    #[test]
    fn bare_plus_is_empty_line_body() {
        let p = parse("[a#T]\nINS.HEAD:\n+\n+x\n").unwrap();
        assert_eq!(
            p.sections[0].ops,
            vec![Op::InsHead {
                body: vec![String::new(), "x".to_string()]
            }]
        );
    }

    #[test]
    fn parses_all_ins_variants_and_del() {
        let p = parse(
            "[a#T]\n\
             INS.HEAD:\n+h\n\
             INS.TAIL:\n+t\n\
             INS.PRE 2:\n+p\n\
             INS.POST 2:\n+q\n\
             DEL 5.=6\n",
        )
        .unwrap();
        let ops = &p.sections[0].ops;
        assert_eq!(
            ops[0],
            Op::InsHead {
                body: vec!["h".into()]
            }
        );
        assert_eq!(
            ops[1],
            Op::InsTail {
                body: vec!["t".into()]
            }
        );
        assert_eq!(
            ops[2],
            Op::InsPre {
                line: 2,
                body: vec!["p".into()]
            }
        );
        assert_eq!(
            ops[3],
            Op::InsPost {
                line: 2,
                body: vec!["q".into()]
            }
        );
        assert_eq!(ops[4], Op::Del { start: 5, end: 6 });
    }

    #[test]
    fn range_separator_variants() {
        for (tok, expect) in [
            ("3.=5", (3, 5)),
            ("3..5", (3, 5)),
            ("3-5", (3, 5)),
            ("3…5", (3, 5)),
            ("3", (3, 3)),
        ] {
            let p = parse(&format!("[a#T]\nDEL {tok}\n")).unwrap();
            assert_eq!(p.sections[0].ops[0].anchor_range(), Some(expect), "{tok}");
        }
    }

    #[test]
    fn missing_trailing_colon_tolerated() {
        let p = parse("[a#T]\nSWAP 1\n+x\nINS.PRE 2\n+y\n").unwrap();
        assert_eq!(p.sections[0].ops[0], swap(1, 1, &["x"]));
        assert_eq!(
            p.sections[0].ops[1],
            Op::InsPre {
                line: 2,
                body: vec!["y".into()]
            }
        );
    }

    #[test]
    fn mv_and_rem() {
        let p = parse("[a#T]\nMV b.txt\n").unwrap();
        assert_eq!(
            p.sections[0].ops,
            vec![Op::Move {
                dest: "b.txt".into()
            }]
        );
        let p = parse("[a#T]\nREM\n").unwrap();
        assert_eq!(p.sections[0].ops, vec![Op::Remove]);
    }

    #[test]
    fn multiple_sections() {
        let p = parse("[a#T1]\nSWAP 1:\n+x\n[b#T2]\nDEL 1\n").unwrap();
        assert_eq!(p.sections.len(), 2);
        assert_eq!(p.sections[0].path, "a");
        assert_eq!(p.sections[1].path, "b");
    }

    #[test]
    fn strips_apply_patch_envelope() {
        let p = parse("*** Begin Patch\n[a#T]\nSWAP 1:\n+x\n*** End Patch\n").unwrap();
        assert_eq!(p.sections[0].ops, vec![swap(1, 1, &["x"])]);
    }

    #[test]
    fn crlf_input_tolerated() {
        let p = parse("[a#T]\r\nSWAP 1.=1:\r\n+x\r\n").unwrap();
        assert_eq!(p.sections[0].ops, vec![swap(1, 1, &["x"])]);
    }

    #[test]
    fn path_with_hash_uses_last_separator() {
        let p = parse("[weird#path#TAG]\nDEL 1\n").unwrap();
        assert_eq!(p.sections[0].path, "weird#path");
        assert_eq!(p.sections[0].tag, "TAG");
    }

    // ── rejections ────────────────────────────────────────────────────────

    #[test]
    fn rejects_apply_patch_sentinel() {
        let err = parse("[a#T]\n*** Update File: a\n").unwrap_err();
        assert!(err.contains("apply_patch"), "{err}");
    }

    #[test]
    fn rejects_unified_diff_hunk() {
        let err = parse("[a#T]\n@@ -1,2 +1,2 @@\n").unwrap_err();
        assert!(err.contains("@@"), "{err}");
    }

    #[test]
    fn rejects_minus_row() {
        let err = parse("[a#T]\n-old line\n").unwrap_err();
        assert!(err.contains("`-` rows"), "{err}");
    }

    #[test]
    fn rejects_empty_body() {
        let err = parse("[a#T]\nSWAP 1.=2:\n\n").unwrap_err();
        assert!(err.contains("empty body"), "{err}");
    }

    #[test]
    fn rejects_body_under_del() {
        let err = parse("[a#T]\nDEL 1\n+surprise\n").unwrap_err();
        assert!(err.contains("DEL takes no body"), "{err}");
    }

    #[test]
    fn rejects_unknown_op() {
        let err = parse("[a#T]\nFUZZ 1.=2:\n+x\n").unwrap_err();
        assert!(err.contains("unknown op"), "{err}");
    }

    #[test]
    fn rejects_no_sections() {
        assert!(parse("").is_err());
        assert!(parse("\n\n").is_err());
    }

    #[test]
    fn rejects_op_outside_section() {
        let err = parse("SWAP 1.=2:\n+x\n").unwrap_err();
        assert!(err.contains("outside any section"), "{err}");
    }

    #[test]
    fn rejects_range_end_before_start() {
        let err = parse("[a#T]\nDEL 5.=3\n").unwrap_err();
        assert!(err.contains("before start"), "{err}");
    }

    #[test]
    fn rejects_zero_line() {
        let err = parse("[a#T]\nDEL 0\n").unwrap_err();
        assert!(err.contains(">= 1"), "{err}");
    }

    #[test]
    fn rejects_orphan_body_row() {
        let err = parse("[a#T]\n+lonely\n").unwrap_err();
        assert!(err.contains("no op to attach to"), "{err}");
    }

    #[test]
    fn missing_plus_mid_body_hints_at_prefix() {
        let err = parse("[a#T]\nSWAP 1.=2:\n+fn f() {\npub fn g() {}\n").unwrap_err();
        assert!(err.contains("body text for the SWAP"), "{err}");
        assert!(err.contains("start with `+`"), "{err}");
    }

    #[test]
    fn unknown_op_outside_body_has_no_plus_hint() {
        let err = parse("[a#T]\nFUZZ 1.=2:\n+x\n").unwrap_err();
        assert!(!err.contains("body text"), "{err}");
    }

    #[test]
    fn op_keywords_require_word_boundary() {
        for line in [
            "DELTA_X = 3;",
            "SWAPPED 1.=2:",
            "MVP::new()",
            "INS.PREMIUM 2:",
        ] {
            let err = parse(&format!("[a#T]\nSWAP 1:\n{line}\n")).unwrap_err();
            assert!(err.contains("unknown op"), "{line}: {err}");
        }
    }

    #[test]
    fn blank_line_in_body_hints_at_bare_plus() {
        let err = parse("[a#T]\nSWAP 1.=2:\n+x\n\n+y\n").unwrap_err();
        assert!(err.contains("bare `+`"), "{err}");
    }

    #[test]
    fn rejects_mv_without_dest() {
        let err = parse("[a#T]\nMV\n").unwrap_err();
        assert!(err.contains("destination path"), "{err}");
    }
}
