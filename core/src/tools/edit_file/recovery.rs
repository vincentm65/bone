//! Stale-snapshot recovery: a line-level 3-way merge.
//!
//! When the read tag no longer matches the live file, the tool looks up the
//! snapshot the model actually read (`base`), applies the ops to it
//! (`edited`), and asks this module to merge that intended change onto the
//! current file (`current`). The merge succeeds only when the model's edited
//! region did not itself change in the live file (non-overlapping line
//! changes); otherwise it is rejected with a re-read instruction.
//!
//! This is the standard 3-way merge (common ancestor = `base`, two derived
//! versions = `edited` and `current`) restricted to non-overlapping changes —
//! no fuzzy or content-relocation matching, which OMP removed as unsafe.

use std::collections::BTreeSet;
use std::fmt;

use similar::{DiffTag, TextDiff};

/// A successful recovery: the merged text to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub text: String,
}

/// Recovery failure. The model must re-read the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryError {
    /// The model's edited region diverged in the live file (overlapping or
    /// adjacent changes); the merge cannot proceed safely.
    Conflict,
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict => write!(
                f,
                "the file changed since you last read it, and your edit touches \
                 lines that also changed; re-read the file and re-issue the edit \
                 against the current line numbers"
            ),
        }
    }
}

impl std::error::Error for RecoveryError {}

/// A single base→derived change: base lines `[start, end)` are replaced by
/// `repl`. For a pure insertion `start == end` (an empty base range) and the
/// change lives in the gap before base line `start`.
#[derive(Debug, Clone)]
struct Hunk {
    start: usize,
    end: usize,
    repl: Vec<String>,
}

impl Hunk {
    fn is_insert(&self) -> bool {
        self.start == self.end
    }
}

/// Merge the model's intended change (`edited`, derived from `base`) onto the
/// live file (`current`).
///
/// Returns the merged text, or [`RecoveryError::Conflict`] when the edited
/// region diverged in `current`. Trailing-newline status follows `current`
/// (the file being written).
pub fn merge_onto(base: &str, edited: &str, current: &str) -> Result<Recovered, RecoveryError> {
    // Already converged (or a no-op): nothing to merge.
    if edited == current {
        return Ok(Recovered {
            text: current.to_string(),
        });
    }

    let edit_hunks = hunks(base, edited);
    let cur_hunks = hunks(base, current);

    let edit_touch = touched(&edit_hunks);
    let cur_touch = touched(&cur_hunks);

    // Overlapping changed base lines → the edited region itself changed.
    if edit_touch.iter().any(|l| cur_touch.contains(l)) {
        return Err(RecoveryError::Conflict);
    }
    // An insertion's gap must not border a line the other side changed, or its
    // placement is ambiguous.
    if inserts_conflict(&edit_hunks, &cur_touch) || inserts_conflict(&cur_hunks, &edit_touch) {
        return Err(RecoveryError::Conflict);
    }

    let base_lines: Vec<&str> = base.lines().collect();
    let merged = apply_merge(&base_lines, &edit_hunks, &cur_hunks);

    let mut text = merged.join("\n");
    if current.ends_with('\n') && !merged.is_empty() {
        text.push('\n');
    }
    Ok(Recovered { text })
}

/// Non-`Equal` ops of `diff(base, target)`, as hunks keyed by base line index.
/// `from_lines(base, target)` makes `old_range` index `base` and `new_range`
/// index `target`.
fn hunks(base: &str, target: &str) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(base, target);
    let target_lines: Vec<&str> = target.lines().collect();
    let mut out = Vec::new();
    for op in diff.ops() {
        match op.tag() {
            DiffTag::Equal => {}
            DiffTag::Delete => {
                let r = op.old_range();
                out.push(Hunk {
                    start: r.start,
                    end: r.end,
                    repl: Vec::new(),
                });
            }
            DiffTag::Insert => {
                let or = op.old_range();
                let nr = op.new_range();
                out.push(Hunk {
                    start: or.start,
                    end: or.start,
                    repl: target_lines[nr].iter().map(|s| s.to_string()).collect(),
                });
            }
            DiffTag::Replace => {
                let or = op.old_range();
                let nr = op.new_range();
                out.push(Hunk {
                    start: or.start,
                    end: or.end,
                    repl: target_lines[nr].iter().map(|s| s.to_string()).collect(),
                });
            }
        }
    }
    out
}

/// Base line indices removed/replaced by any non-insert hunk.
fn touched(hunks: &[Hunk]) -> BTreeSet<usize> {
    hunks
        .iter()
        .filter(|h| !h.is_insert())
        .flat_map(|h| h.start..h.end)
        .collect()
}

/// True if any insertion in `hunks` borders a line in `other_touch`
/// (gap `g` borders base lines `g-1` and `g`).
fn inserts_conflict(hunks: &[Hunk], other_touch: &BTreeSet<usize>) -> bool {
    hunks.iter().any(|h| {
        if !h.is_insert() {
            return false;
        }
        let before = h.start.saturating_sub(1);
        let borders =
            (h.start > 0 && other_touch.contains(&before)) || other_touch.contains(&h.start);
        borders
    })
}

/// Two-pointer walk: at each base position at most one side has a change
/// (verified non-overlapping/non-adjacent), so emit unchanged base lines up to
/// the next hunk, then that hunk's replacement.
fn apply_merge(base: &[&str], edit_hunks: &[Hunk], cur_hunks: &[Hunk]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pos = 0usize;
    let (mut ei, mut ci) = (0usize, 0usize);

    loop {
        // Pick the next hunk by smallest start; ties favour the edit side.
        let chosen: Option<(&Hunk, bool)> = match (edit_hunks.get(ei), cur_hunks.get(ci)) {
            (None, None) => None,
            (Some(_), None) => Some((&edit_hunks[ei], true)),
            (None, Some(_)) => Some((&cur_hunks[ci], false)),
            (Some(eh), Some(ch)) => {
                if eh.start <= ch.start {
                    Some((eh, true))
                } else {
                    Some((ch, false))
                }
            }
        };

        match chosen {
            None => {
                out.extend(base[pos..].iter().map(|s| s.to_string()));
                break;
            }
            Some((h, is_edit)) => {
                out.extend(base[pos..h.start].iter().map(|s| s.to_string()));
                out.extend(h.repl.iter().cloned());
                pos = h.end;
                if is_edit {
                    ei += 1;
                } else {
                    ci += 1;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::edit_file::apply::apply_ops;
    use crate::tools::edit_file::parser::parse;

    /// Apply a patch to `base` to obtain `edited` (helper for recovery tests).
    fn edited_of(base: &str, patch: &str) -> String {
        let ops = &parse(patch).unwrap().sections[0].ops;
        apply_ops(base, ops, None).unwrap()
    }

    fn merge(base: &str, patch: &str, current: &str) -> Result<String, RecoveryError> {
        let edited = edited_of(base, patch);
        merge_onto(base, &edited, current).map(|r| r.text)
    }

    // ── no drift: base == current → edit applies cleanly ───────────────────

    #[test]
    fn no_drift_applies_edit() {
        let base = "a\nb\nc\nd\ne\n";
        // current is identical to base; recover == just applying the edit.
        assert_eq!(
            merge(base, "[f#T]\nSWAP 3:\n+C\n", base).unwrap(),
            "a\nb\nC\nd\ne\n"
        );
    }

    // ── disjoint drift: edit and current touch different regions ───────────

    #[test]
    fn disjoint_drift_merges_both_changes() {
        let base = "a\nb\nc\nd\ne\n";
        // Model swaps line 2; current independently changed line 5.
        let current = "a\nb\nc\nd\nZ\n";
        let out = merge(base, "[f#T]\nSWAP 2:\n+B\n", current).unwrap();
        assert_eq!(out, "a\nB\nc\nd\nZ\n");
    }

    #[test]
    fn disjoint_multiline_swap_merges() {
        let base = "1\n2\n3\n4\n5\n6\n";
        // Model swaps lines 2..=3; current changed line 6.
        let current = "1\n2\n3\n4\n5\nSIX\n";
        let out = merge(base, "[f#T]\nSWAP 2.=3:\n+TWO\n+THREE\n", current).unwrap();
        assert_eq!(out, "1\nTWO\nTHREE\n4\n5\nSIX\n");
    }

    #[test]
    fn current_inserts_elsewhere_model_deletes() {
        let base = "a\nb\nc\n";
        // Model deletes line 2; current appended a line at the end (disjoint).
        let current = "a\nb\nc\nTAIL\n";
        let out = merge(base, "[f#T]\nDEL 2\n", current).unwrap();
        // base line 1 kept, line 2 deleted, line 3 kept, current's tail insert kept.
        assert_eq!(out, "a\nc\nTAIL\n");
    }

    // ── conflicts ──────────────────────────────────────────────────────────

    #[test]
    fn overlapping_edit_region_is_conflict() {
        let base = "a\nb\nc\nd\ne\n";
        // Model swaps line 3; current also changed line 3.
        let current = "a\nb\nX\nd\ne\n";
        assert_eq!(
            merge(base, "[f#T]\nSWAP 3:\n+C\n", current),
            Err(RecoveryError::Conflict)
        );
    }

    #[test]
    fn conflict_message_tells_to_reread() {
        let base = "a\nb\nc\n";
        let err = merge(base, "[f#T]\nSWAP 2:\n+B\n", "a\nX\nc\n").unwrap_err();
        assert!(err.to_string().contains("re-read"), "{}", err);
    }

    #[test]
    fn insert_adjacent_to_current_change_is_conflict() {
        let base = "a\nb\nc\n";
        // Model inserts before line 2 (anchor base line 2); current replaced line 2.
        let current = "a\nB2\nc\n";
        assert_eq!(
            merge(base, "[f#T]\nINS.PRE 2:\n+X\n", current),
            Err(RecoveryError::Conflict)
        );
    }

    #[test]
    fn insert_at_unchanged_boundary_is_ok() {
        let base = "a\nb\nc\nd\n";
        // Model inserts after line 1; current changed line 4 (disjoint boundary).
        let current = "a\nb\nc\nD\n";
        let out = merge(base, "[f#T]\nINS.POST 1:\n+X\n", current).unwrap();
        assert_eq!(out, "a\nX\nb\nc\nD\n");
    }

    // ── converged & edge cases ─────────────────────────────────────────────

    #[test]
    fn edited_equals_current_returns_current() {
        // Both sides produced the same result independently.
        let base = "a\nb\nc\n";
        let current = "a\nB\nc\n";
        let out = merge(base, "[f#T]\nSWAP 2:\n+B\n", current).unwrap();
        assert_eq!(out, "a\nB\nc\n");
    }

    #[test]
    fn both_insert_at_same_gap_keeps_both() {
        // Model inserts at the very end (gap = base_len); current also appended.
        let base = "a\nb\n";
        let current = "a\nb\nCUR\n"; // current inserted "CUR" at the end
        let out = merge(base, "[f#T]\nINS.TAIL:\n+MOD\n", current).unwrap();
        // Both insertions at the unchanged tail boundary: edit then current.
        assert_eq!(out, "a\nb\nMOD\nCUR\n");
    }

    #[test]
    fn preserves_current_trailing_newline() {
        let base = "a\nb\nc\n";
        let current = "a\nb\nc\nd\n";
        let out = merge(base, "[f#T]\nSWAP 1:\n+A\n", current).unwrap();
        assert!(out.ends_with('\n'), "trailing newline preserved: {out:?}");
        assert_eq!(out, "A\nb\nc\nd\n");
    }

    #[test]
    fn preserves_current_missing_trailing_newline() {
        let base = "a\nb";
        let current = "a\nb\nc"; // no trailing newline
        let out = merge(base, "[f#T]\nSWAP 1:\n+A\n", current).unwrap();
        assert_eq!(out, "A\nb\nc");
    }
}
