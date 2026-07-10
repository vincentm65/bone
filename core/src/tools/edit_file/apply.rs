//! Hashline apply engine.
//!
//! Pure transformation: parsed content ops + original snapshot text → new text.
//! Line numbers are 1-indexed and always refer to the *original* snapshot the
//! model read — they are never shifted as hunks apply. Overlapping SWAP/DEL
//! ranges are rejected; insertions may only anchor on lines that survive (are
//! not themselves swapped or deleted). When a seen-lines set is supplied, every
//! anchor line must have been shown to the model (the visible-line guard).
//!
//! `Op::Move` / `Op::Remove` are file-lifecycle ops handled by the tool layer
//! and have no effect here; they are simply skipped.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use super::parser::Op;
use crate::tools::snapshot::numbered_lines;

/// Error from applying ops to a snapshot. Messages are teaching-oriented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// A line number (SWAP/DEL range end, or INS anchor) is past EOF.
    OutOfBounds { line: usize, total: usize },
    /// Two SWAP/DEL ranges share at least one original line.
    Overlap {
        a: (usize, usize),
        b: (usize, usize),
    },
    /// An INS.PRE/INS.POST anchor falls inside a SWAP/DEL range.
    AnchorInsideMutation {
        anchor: usize,
        range: (usize, usize),
    },
    /// An anchor line was not shown to the model (visible-line guard).
    UnseenLine { line: usize },
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { line, total } => write!(
                f,
                "line {line} is out of bounds (file has {total} line{s}); \
                 re-read the file to see current line numbers",
                s = if *total == 1 { "" } else { "s" }
            ),
            Self::Overlap { a, b } => write!(
                f,
                "ranges {a0}.={a1} and {b0}.={b1} overlap; \
                 merge them into one op or use separate non-overlapping ranges",
                a0 = a.0,
                a1 = a.1,
                b0 = b.0,
                b1 = b.1
            ),
            Self::AnchorInsideMutation { anchor, range } => write!(
                f,
                "INS anchor line {anchor} is inside SWAP/DEL range {r0}.={r1}; \
                 anchor on a line that is not being swapped or deleted",
                r0 = range.0,
                r1 = range.1
            ),
            Self::UnseenLine { line } => write!(
                f,
                "line {line} was not shown in your last read (elided); \
                 you can only edit lines you saw — re-read the file or target a visible line"
            ),
        }
    }
}

impl std::error::Error for ApplyError {}

/// Internal: what to do when the walk reaches the start of a SWAP/DEL range.
enum RangeAction {
    Replace { end: usize, body: Vec<String> },
    Delete { end: usize },
}

/// Apply parsed content ops to `original` snapshot text, returning the new text.
///
/// `seen_lines`, when `Some` and non-empty, enforces the visible-line guard:
/// every anchor line must appear in the set. Pass `None` to disable (e.g. for a
/// snapshot recorded without seen-line info).
///
/// The caller detects a no-op by comparing the result with `original`.
pub fn apply_ops(
    original: &str,
    ops: &[Op],
    seen_lines: Option<&BTreeSet<usize>>,
) -> Result<String, ApplyError> {
    let lines = numbered_lines(original);
    let n = lines.len();
    let guard = seen_lines.filter(|s| !s.is_empty());

    // ── Pass 1: collect SWAP/DEL ranges, validate bounds. ──────────────────
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut action: HashMap<usize, RangeAction> = HashMap::new();
    for op in ops {
        match op {
            Op::Swap { start, end, body } => {
                check_bounds(*start, *end, n)?;
                ranges.push((*start, *end));
                action.insert(
                    *start,
                    RangeAction::Replace {
                        end: *end,
                        body: body.clone(),
                    },
                );
            }
            Op::Del { start, end } => {
                check_bounds(*start, *end, n)?;
                ranges.push((*start, *end));
                action.insert(*start, RangeAction::Delete { end: *end });
            }
            _ => {}
        }
    }

    // ── Overlap check (sorted by start; inclusive ranges overlap if a.1>=b.0).
    ranges.sort_unstable_by_key(|(s, _)| *s);
    for w in ranges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.1 >= b.0 {
            return Err(ApplyError::Overlap { a, b });
        }
    }
    let mutated: BTreeSet<usize> = ranges.iter().flat_map(|(s, e)| *s..=*e).collect();

    // ── Pass 2: visible-line guard for SWAP/DEL; collect all insertions. ────
    let mut head_bodies: Vec<Vec<String>> = Vec::new();
    let mut tail_bodies: Vec<Vec<String>> = Vec::new();
    let mut pre_inserts: HashMap<usize, Vec<Vec<String>>> = HashMap::new();
    let mut post_inserts: HashMap<usize, Vec<Vec<String>>> = HashMap::new();

    for op in ops {
        match op {
            Op::Swap { start, end, .. } | Op::Del { start, end } => {
                if let Some(g) = guard {
                    for l in *start..=*end {
                        if !g.contains(&l) {
                            return Err(ApplyError::UnseenLine { line: l });
                        }
                    }
                }
            }
            Op::InsPre { line, body } => {
                check_anchor(*line, n, &ranges, &mutated, guard)?;
                pre_inserts.entry(*line).or_default().push(body.clone());
            }
            Op::InsPost { line, body } => {
                check_anchor(*line, n, &ranges, &mutated, guard)?;
                post_inserts.entry(*line).or_default().push(body.clone());
            }
            Op::InsHead { body } => head_bodies.push(body.clone()),
            Op::InsTail { body } => tail_bodies.push(body.clone()),
            // File-lifecycle ops: handled by the tool layer, no content effect.
            Op::Move { .. } | Op::Remove => {}
        }
    }

    // ── Walk original lines in order, emitting insertions around survivors. ─
    let mut out: Vec<String> = Vec::new();
    for body in &head_bodies {
        out.extend(body.iter().cloned());
    }
    let mut i = 1usize;
    while i <= n {
        if let Some(bodies) = pre_inserts.get(&i) {
            for body in bodies {
                out.extend(body.iter().cloned());
            }
        }
        if let Some(act) = action.get(&i) {
            match act {
                RangeAction::Replace { end, body } => {
                    out.extend(body.iter().cloned());
                    i = end + 1;
                }
                RangeAction::Delete { end } => i = end + 1,
            }
        } else {
            out.push(lines[i - 1].to_string());
            if let Some(bodies) = post_inserts.get(&i) {
                for body in bodies {
                    out.extend(body.iter().cloned());
                }
            }
            i += 1;
        }
    }
    for body in &tail_bodies {
        out.extend(body.iter().cloned());
    }

    // Reconstruct the trailing newline: a file ending in '\n' keeps it as long
    // as it still has ≥1 line. `out.join("\n")` of a single empty line [""] is
    // "" so we key off `!out.is_empty()` (line count), not the joined string.
    let mut result = out.join("\n");
    if original.ends_with('\n') && !out.is_empty() {
        result.push('\n');
    }
    Ok(result)
}

fn check_bounds(start: usize, end: usize, n: usize) -> Result<(), ApplyError> {
    if start > n {
        Err(ApplyError::OutOfBounds {
            line: start,
            total: n,
        })
    } else if end > n {
        Err(ApplyError::OutOfBounds {
            line: end,
            total: n,
        })
    } else {
        Ok(())
    }
}

fn range_containing(ranges: &[(usize, usize)], line: usize) -> Option<(usize, usize)> {
    ranges
        .iter()
        .copied()
        .find(|(s, e)| *s <= line && line <= *e)
}

fn check_anchor(
    line: usize,
    n: usize,
    ranges: &[(usize, usize)],
    mutated: &BTreeSet<usize>,
    guard: Option<&BTreeSet<usize>>,
) -> Result<(), ApplyError> {
    if line > n {
        return Err(ApplyError::OutOfBounds { line, total: n });
    }
    if let Some(range) = range_containing(ranges, line) {
        return Err(ApplyError::AnchorInsideMutation {
            anchor: line,
            range,
        });
    }
    if mutated.contains(&line) {
        // Defensive: range_containing should have caught this, but keep the
        // invariant explicit.
        let range = range_containing(ranges, line).unwrap_or((line, line));
        return Err(ApplyError::AnchorInsideMutation {
            anchor: line,
            range,
        });
    }
    if let Some(g) = guard {
        if !g.contains(&line) {
            return Err(ApplyError::UnseenLine { line });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::edit_file::parser::{Op, parse};

    fn ops(input: &str) -> Vec<Op> {
        parse(input).unwrap().sections[0].ops.clone()
    }

    fn seen(lines: &[usize]) -> BTreeSet<usize> {
        lines.iter().copied().collect()
    }

    // ── basic ops ──────────────────────────────────────────────────────────

    #[test]
    fn swap_single_line() {
        let orig = "a\nb\nc\n";
        let out = apply_ops(orig, &ops("[f#T]\nSWAP 2:\n+B\n"), None).unwrap();
        assert_eq!(out, "a\nB\nc\n");
    }

    #[test]
    fn swap_range() {
        let orig = "a\nb\nc\nd\ne\n";
        let out = apply_ops(orig, &ops("[f#T]\nSWAP 2.=3:\n+X\n+Y\n"), None).unwrap();
        assert_eq!(out, "a\nX\nY\nd\ne\n");
    }

    #[test]
    fn del_single_and_range() {
        let orig = "a\nb\nc\nd\ne\n";
        let out = apply_ops(orig, &ops("[f#T]\nDEL 2\nDEL 4.=5\n"), None).unwrap();
        assert_eq!(out, "a\nc\n");
    }

    #[test]
    fn ins_pre_and_post() {
        let orig = "a\nb\nc\n";
        let out = apply_ops(orig, &ops("[f#T]\nINS.PRE 2:\n+P\nINS.POST 2:\n+Q\n"), None).unwrap();
        assert_eq!(out, "a\nP\nb\nQ\nc\n");
    }

    #[test]
    fn ins_head_and_tail() {
        let orig = "m\n";
        let out = apply_ops(
            orig,
            &ops("[f#T]\nINS.HEAD:\n+H1\n+H2\nINS.TAIL:\n+T\n"),
            None,
        )
        .unwrap();
        assert_eq!(out, "H1\nH2\nm\nT\n");
    }

    // ── original-line (not shifted) semantics ──────────────────────────────

    #[test]
    fn multi_hunk_uses_original_line_numbers() {
        // Edit line 2 and line 5 in the *original* numbering; line 2's edit
        // must not shift line 5's anchor.
        let orig = "1\n2\n3\n4\n5\n6\n";
        let out = apply_ops(orig, &ops("[f#T]\nSWAP 2:\n+TWO\nSWAP 5:\n+FIVE\n"), None).unwrap();
        assert_eq!(out, "1\nTWO\n3\n4\nFIVE\n6\n");
    }

    #[test]
    fn insert_after_line_grows_file_but_anchors_stay_original() {
        let orig = "1\n2\n3\n";
        let out = apply_ops(
            orig,
            &ops("[f#T]\nINS.POST 1:\n+X\nSWAP 3:\n+THREE\n"),
            None,
        )
        .unwrap();
        // INS.POST 1 inserts after line 1; SWAP 3 still hits original line 3.
        assert_eq!(out, "1\nX\n2\nTHREE\n");
    }

    // ── ordering / stacking ────────────────────────────────────────────────

    #[test]
    fn multiple_ins_pre_at_same_line_stack_in_order() {
        let orig = "a\nb\n";
        let out = apply_ops(
            orig,
            &ops("[f#T]\nINS.PRE 1:\n+P1\nINS.PRE 1:\n+P2\n"),
            None,
        )
        .unwrap();
        assert_eq!(out, "P1\nP2\na\nb\n");
    }

    #[test]
    fn ins_post_last_line_equiv_tail_adjacent() {
        let orig = "a\nb\n";
        let out = apply_ops(orig, &ops("[f#T]\nINS.POST 2:\n+Z\n"), None).unwrap();
        assert_eq!(out, "a\nb\nZ\n");
    }

    // ── newline reconstruction ─────────────────────────────────────────────

    #[test]
    fn preserves_trailing_newline() {
        assert_eq!(
            apply_ops("a\nb\n", &ops("[f#T]\nSWAP 1:\n+A\n"), None).unwrap(),
            "A\nb\n"
        );
    }

    #[test]
    fn preserves_missing_trailing_newline() {
        let orig = "a\nb"; // no trailing newline
        let out = apply_ops(orig, &ops("[f#T]\nSWAP 1:\n+A\n"), None).unwrap();
        assert_eq!(out, "A\nb");
    }

    #[test]
    fn single_blank_line_preserved_on_noop() {
        let orig = "\n"; // one empty line, newline-terminated
        let out = apply_ops(orig, &[], None).unwrap();
        assert_eq!(out, "\n");
    }

    #[test]
    fn delete_all_lines_yields_empty() {
        let orig = "a\nb\n";
        let out = apply_ops(orig, &ops("[f#T]\nDEL 1.=2\n"), None).unwrap();
        assert_eq!(out, "");
    }

    // ── rejections ─────────────────────────────────────────────────────────

    #[test]
    fn rejects_overlap() {
        let orig = "a\nb\nc\nd\ne\n";
        let err = apply_ops(orig, &ops("[f#T]\nSWAP 2.=4:\n+X\nDEL 3:\n"), None).unwrap_err();
        assert!(matches!(err, ApplyError::Overlap { .. }), "{err}");
        assert!(err.to_string().contains("overlap"), "{}", err);
    }

    #[test]
    fn rejects_out_of_bounds() {
        let orig = "a\nb\n";
        let err = apply_ops(orig, &ops("[f#T]\nSWAP 5:\n+X\n"), None).unwrap_err();
        assert!(
            matches!(err, ApplyError::OutOfBounds { line: 5, total: 2 }),
            "{err}"
        );
        assert!(err.to_string().contains("out of bounds"), "{err}");
    }

    #[test]
    fn rejects_ins_anchor_inside_swap_range() {
        let orig = "a\nb\nc\nd\n";
        let err = apply_ops(
            orig,
            &ops("[f#T]\nSWAP 2.=3:\n+X\n+Y\nINS.PRE 3:\n+P\n"),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApplyError::AnchorInsideMutation { anchor: 3, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("inside SWAP/DEL"), "{err}");
    }

    #[test]
    fn ins_anchor_on_surviving_neighbor_is_ok() {
        let orig = "a\nb\nc\nd\n";
        // SWAP 2.=3, then INS.POST 1 (survives) and INS.PRE 4 (survives).
        let out = apply_ops(
            orig,
            &ops("[f#T]\nSWAP 2.=3:\n+X\n+Y\nINS.POST 1:\n+P\nINS.PRE 4:\n+Q\n"),
            None,
        )
        .unwrap();
        assert_eq!(out, "a\nP\nX\nY\nQ\nd\n");
    }

    // ── visible-line guard ─────────────────────────────────────────────────

    #[test]
    fn guard_rejects_unseen_swap_line() {
        let orig = "a\nb\nc\nd\n";
        // Only lines 1,3,4 were shown; line 2 was elided.
        let g = seen(&[1, 3, 4]);
        let err = apply_ops(orig, &ops("[f#T]\nSWAP 2:\n+X\n"), Some(&g)).unwrap_err();
        assert!(matches!(err, ApplyError::UnseenLine { line: 2 }), "{err}");
    }

    #[test]
    fn guard_rejects_unseen_ins_anchor() {
        let orig = "a\nb\nc\n";
        let g = seen(&[1, 3]); // line 2 elided
        let err = apply_ops(orig, &ops("[f#T]\nINS.POST 2:\n+X\n"), Some(&g)).unwrap_err();
        assert!(matches!(err, ApplyError::UnseenLine { line: 2 }), "{err}");
    }

    #[test]
    fn guard_allows_seen_lines() {
        let orig = "a\nb\nc\n";
        let g = seen(&[1, 2, 3]);
        let out = apply_ops(orig, &ops("[f#T]\nSWAP 2:\n+B\n"), Some(&g)).unwrap();
        assert_eq!(out, "a\nB\nc\n");
    }

    #[test]
    fn guard_disabled_when_none_or_empty() {
        let orig = "a\nb\n";
        // None: no guard.
        assert_eq!(
            apply_ops(orig, &ops("[f#T]\nSWAP 1:\n+A\n"), None).unwrap(),
            "A\nb\n"
        );
        // Empty set: disabled (snapshot recorded without seen info).
        let empty = seen(&[]);
        assert_eq!(
            apply_ops(orig, &ops("[f#T]\nSWAP 1:\n+A\n"), Some(&empty)).unwrap(),
            "A\nb\n"
        );
    }

    // ── no-op & lifecycle ops ───────────────────────────────────────────────

    #[test]
    fn identical_swap_body_is_noop_text() {
        let orig = "a\nb\n";
        let out = apply_ops(orig, &ops("[f#T]\nSWAP 1:\n+a\n"), None).unwrap();
        assert_eq!(out, orig, "result equals original → caller treats as no-op");
    }

    #[test]
    fn move_and_remove_are_ignored_by_apply() {
        let orig = "a\nb\n";
        // A section with only MV: apply returns the original unchanged.
        let mv = parse("[f#T]\nMV g\n").unwrap();
        let out = apply_ops(orig, &mv.sections[0].ops, None).unwrap();
        assert_eq!(out, orig);
        let rem = parse("[f#T]\nREM\n").unwrap();
        let out = apply_ops(orig, &rem.sections[0].ops, None).unwrap();
        assert_eq!(out, orig);
    }

    // ── empty file ─────────────────────────────────────────────────────────

    #[test]
    fn empty_file_accepts_head_insert() {
        let out = apply_ops("", &ops("[f#T]\nINS.HEAD:\n+first\n"), None).unwrap();
        assert_eq!(out, "first");
    }

    #[test]
    fn empty_file_rejects_line_ops() {
        let err = apply_ops("", &ops("[f#T]\nSWAP 1:\n+X\n"), None).unwrap_err();
        assert!(
            matches!(err, ApplyError::OutOfBounds { total: 0, .. }),
            "{err}"
        );
    }
}
