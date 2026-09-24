//! Simple exact-text editing for existing files.
//!
//! The agent supplies a path plus one or more exact `old_text` → `new_text`
//! replacements. Context-aware calls require a preceding `read_file`; the
//! snapshot stays internal and is used for visibility and stale-file checks.

use std::collections::BTreeSet;
use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::tools::snapshot::{self, Snapshots};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::write_atomic::write_atomic_if_unchanged;

pub(crate) mod diff;

pub struct EditFileTool;

pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditHunk {
    old_text: String,
    new_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    #[serde(default)]
    old_text: Option<String>,
    #[serde(default)]
    new_text: Option<String>,
    #[serde(default)]
    edits: Option<Vec<EditHunk>>,
}

/// One normalized replacement: exact `old` → `new`.
struct Hunk {
    old: String,
    new: String,
}

/// A hunk matched against the original normalized file text.
struct MatchedHunk {
    index: usize,
    offset: usize,
    old: String,
    new: String,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Preferred tool for modifying existing file contents; use this instead of shell commands such as sed -i, tee, heredocs, scripts, or redirection. Read the file first, then pass the same path and copy exact text from displayed `read_file` lines without line-number prefixes, including enough unchanged surrounding context to make each hunk unique, not merely the changed line. Replaces one exact, unique block in an existing UTF-8 file, or several disjoint blocks in one call via `edits`. For disjoint edits, provide one complete `old_text`/`new_text` pair per `edits` entry; each hunk is matched against the original file. Use an empty new_text to delete. To insert, include a small unchanged surrounding block in both old_text and new_text. Returns a unified diff.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path. Relative paths resolve from the working directory."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact unique text copied from displayed `read_file` lines, without line-number prefixes, for a single replacement. Include enough unchanged surrounding context to make it unique, not just the changed text. May be empty only when the file is empty. Provide either old_text/new_text or edits, not both."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text for old_text. May be empty to delete old_text."
                    },
                    "edits": {
                        "type": "array",
                        "description": "Several disjoint replacements applied in one call. Provide one complete `old_text`/`new_text` pair per `edits` entry; each hunk is matched against the original file content, not the result of earlier hunks.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_text": {
                                    "type": "string",
                                    "description": "Exact unique text copied from displayed `read_file` lines, without line-number prefixes. Include enough unchanged surrounding context to make this hunk unique, not just the changed text. May be empty only when the file is empty."
                                },
                                "new_text": {
                                    "type": "string",
                                    "description": "Replacement text. May be empty to delete old_text."
                                }
                            },
                            "required": ["old_text", "new_text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        run_edit(arguments, None, None).await
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        run_edit(
            arguments,
            Some(&context.snapshots),
            context.working_dir.as_deref(),
        )
        .await
        .map(ToolOutput::text)
    }
}

pub async fn preview_edit_file(
    _tool_name: &str,
    arguments: Value,
    working_dir: Option<&Path>,
) -> Result<EditPreview, String> {
    let (path_arg, hunks) = parse_args(arguments)?;
    let resolved = snapshot::resolve_existing_path(&path_arg, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    let (_, live) = read_live(&resolved).await?;
    let (_, edited) = match_hunks(&live, &hunks, &path, None)?;
    Ok(EditPreview {
        before_hash: snapshot::compute_tag(&live),
        diff: diff::build_unified_diff("edit_file", &path, &live, &edited),
    })
}

async fn run_edit(
    arguments: Value,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<String, String> {
    let (path_arg, hunks) = parse_args(arguments)?;
    let resolved = snapshot::resolve_existing_path(&path_arg, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    let (live_raw, live) = read_live(&resolved).await?;

    let live_format = snapshot::TextFormat::detect(&live_raw);
    let (base, seen_lines, format) = if let Some(store) = snapshots {
        let guard = store.read().map_err(|e| e.to_string())?;
        let snap = guard
            .head(&path)
            .ok_or_else(|| format!("read `{path}` with read_file before editing it"))?;
        (snap.text.clone(), snap.seen_lines.clone(), snap.format)
    } else {
        (live.clone(), BTreeSet::new(), live_format)
    };

    if base != live || format != live_format {
        return Err(format!(
            "`{path}` changed after it was read; re-read it and retry"
        ));
    }
    let (matched, edited) = match_hunks(&live, &hunks, &path, snapshots.map(|_| &seen_lines))?;

    let permissions = fs::metadata(&resolved)
        .await
        .map_err(|e| format!("could not re-check `{path}` before writing: {e}"))?
        .permissions();
    let rendered = splice_raw(&live_raw, &matched, format);
    write_atomic_if_unchanged(&resolved, &rendered, Some(permissions), live_raw.as_bytes()).await?;
    let edited_format = snapshot::TextFormat::detect(&rendered);

    if let Some(store) = snapshots {
        let seen = remap_seen_lines(&live, &edited, &matched, &seen_lines);
        let mut guard = store.write().map_err(|e| e.to_string())?;
        guard.record_with_format(&path, &edited, edited_format, Some(&seen));
    }

    let rendered = truncate_output(&diff::build_unified_diff(
        "edit_file",
        &path,
        &live,
        &edited,
    ));
    Ok(format!("Edited: {path}\n{rendered}").trim_end().to_string())
}

fn parse_args(arguments: Value) -> Result<(String, Vec<Hunk>), String> {
    if !arguments.is_object() {
        return Err(
            "edit_file arguments must be an object containing `path` and an edit".to_string(),
        );
    }
    if arguments.get("path").is_none() {
        return Err("edit_file is missing `path`; provide the file path with the edit".to_string());
    }
    let args: Args = serde_json::from_value(arguments).map_err(|e| {
        format!(
            "edit_file requires path plus either old_text/new_text or a non-empty edits array: {e}"
        )
    })?;
    if args.path.trim().is_empty() {
        return Err("`path` must not be empty".to_string());
    }
    let edits = match (args.old_text, args.new_text, args.edits) {
        (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => {
            return Err("provide either old_text/new_text or edits, not both".to_string());
        }
        (None, None, Some(edits)) if edits.is_empty() => {
            return Err("`edits` must not be empty".to_string());
        }
        (None, None, Some(edits)) => edits,
        (Some(old_text), Some(new_text), None) => {
            if old_text.is_empty() && new_text.is_empty() {
                return Err("`old_text` and `new_text` cannot both be empty".to_string());
            }
            vec![EditHunk { old_text, new_text }]
        }
        (None, None, None) => {
            return Err("provide either old_text/new_text or a non-empty edits array".to_string());
        }
        _ => return Err("old_text and new_text must be provided together".to_string()),
    };
    let hunks = edits
        .into_iter()
        .map(|edit| Hunk {
            old: snapshot::normalize_text(&edit.old_text),
            new: snapshot::normalize_text(&edit.new_text),
        })
        .collect();
    Ok((args.path, hunks))
}

fn unique_match_offset(text: &str, needle: &str, path: &str) -> Result<usize, String> {
    if needle.is_empty() {
        return if text.is_empty() {
            Ok(0)
        } else {
            Err(format!(
                "old_text may be empty only when `{path}` is empty; copy a unique block from read_file"
            ))
        };
    }
    let mut matches = text.match_indices(needle);
    let Some((offset, _)) = matches.next() else {
        return Err(format!(
            "old_text was not found in `{path}`; copy it exactly from read_file"
        ));
    };
    if matches.next().is_some() {
        return Err(format!(
            "old_text occurs more than once in `{path}`; include more surrounding text so it is unique"
        ));
    }
    Ok(offset)
}

/// Match every hunk against the original normalized `text`, reject duplicates
/// and overlaps, and return the sorted matches plus the edited text. Preview
/// and execution share this path; execution also checks visibility using the
/// same offsets. Each hunk anchors on the original content.
fn match_hunks(
    text: &str,
    hunks: &[Hunk],
    path: &str,
    seen_lines: Option<&BTreeSet<usize>>,
) -> Result<(Vec<MatchedHunk>, String), String> {
    let mut matched = Vec::with_capacity(hunks.len());
    for (index, hunk) in hunks.iter().enumerate() {
        let offset = unique_match_offset(text, &hunk.old, path).map_err(|error| {
            format!(
                "hunk {} of {}: {error}; {index} earlier hunks matched; no changes written",
                index + 1,
                hunks.len()
            )
        })?;
        if let Some(seen) = seen_lines {
            ensure_visible(text, offset, &hunk.old, seen, path)
                .map_err(|error| format!("hunk {} of {}: {error}", index + 1, hunks.len()))?;
        }
        matched.push(MatchedHunk {
            index: index + 1,
            offset,
            old: hunk.old.clone(),
            new: hunk.new.clone(),
        });
    }
    matched.sort_by_key(|hunk| hunk.offset);
    for pair in matched.windows(2) {
        let (earlier, later) = (&pair[0], &pair[1]);
        if earlier.old == later.old {
            return Err(format!(
                "edits contains the same replacement twice (hunks {} and {}); remove the duplicate hunk",
                earlier.index, later.index
            ));
        }
        let overlaps = earlier.offset + earlier.old.len() > later.offset
            || (earlier.offset + earlier.old.len() == later.offset
                && (earlier.old.is_empty() || later.old.is_empty()));
        if overlaps {
            return Err(format!(
                "edits hunks {} and {} overlap in `{path}`; combine them into one replacement",
                earlier.index, later.index
            ));
        }
    }
    let edited = splice(text, &matched);
    if edited == text {
        return Err(format!(
            "no change to `{path}`; the edits produce identical content"
        ));
    }
    Ok((matched, edited))
}

fn splice(text: &str, hunks: &[MatchedHunk]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for hunk in hunks {
        debug_assert!(cursor <= hunk.offset && hunk.offset + hunk.old.len() <= text.len());
        result.push_str(&text[cursor..hunk.offset]);
        result.push_str(&hunk.new);
        cursor = hunk.offset + hunk.old.len();
    }
    result.push_str(&text[cursor..]);
    result
}

fn raw_offset(raw: &str, normalized_offset: usize) -> usize {
    let mut raw_offset = usize::from(raw.starts_with('\u{feff}')) * '\u{feff}'.len_utf8();
    let mut normalized = 0usize;
    while normalized < normalized_offset {
        let remaining = &raw[raw_offset..];
        let ch = remaining
            .chars()
            .next()
            .expect("normalized offset within raw text");
        raw_offset += ch.len_utf8();
        if ch == '\r' {
            if raw[raw_offset..].starts_with('\n') {
                raw_offset += 1;
            }
            normalized += 1;
        } else {
            normalized += ch.len_utf8();
        }
    }
    debug_assert_eq!(normalized, normalized_offset);
    raw_offset
}

/// Splice the sorted, non-overlapping matches into the raw (BOM / CRLF) text,
/// mapping every normalized offset back to raw bytes and restoring the file's
/// line-ending convention per hunk.
fn splice_raw(raw: &str, hunks: &[MatchedHunk], format: snapshot::TextFormat) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut cursor = 0usize;
    for hunk in hunks {
        let start = raw_offset(raw, hunk.offset);
        let end = raw_offset(raw, hunk.offset + hunk.old.len());
        debug_assert!(cursor <= start && start <= end);
        result.push_str(&raw[cursor..start]);
        result.push_str(&format.restore_newlines(&hunk.new));
        cursor = end;
    }
    result.push_str(&raw[cursor..]);
    result
}

fn ensure_visible(
    snapshot_text: &str,
    offset: usize,
    old_text: &str,
    seen_lines: &BTreeSet<usize>,
    path: &str,
) -> Result<(), String> {
    if old_text.is_empty() && snapshot_text.is_empty() {
        return Ok(());
    }
    let start_line = 1 + snapshot_text[..offset]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let end = offset + old_text.len();
    let end_line = 1 + snapshot_text[..end].bytes().filter(|b| *b == b'\n').count()
        - usize::from(old_text.ends_with('\n'));
    if (start_line..=end_line).any(|line| !seen_lines.contains(&line)) {
        return Err(format!(
            "old_text includes lines that were not shown from `{path}`; read lines {start_line}-{end_line} with read_file before editing"
        ));
    }
    Ok(())
}

fn remap_seen_lines(
    live: &str,
    edited: &str,
    hunks: &[MatchedHunk],
    seen_lines: &BTreeSet<usize>,
) -> Vec<usize> {
    if hunks.iter().any(|hunk| hunk.old.is_empty()) {
        // Insertion into an empty file: every line is new.
        return (1..=snapshot::numbered_lines(edited).len()).collect();
    }

    // Per-hunk line geometry in the original text: (start_line, old_end_line,
    // new_span, line_delta). `line_delta` is the change in line numbering for
    // the unchanged suffix, while `new_span` is the replacement range to mark
    // visible; these are different when editing within a line.
    let geometry: Vec<(usize, usize, usize, isize)> = hunks
        .iter()
        .map(|hunk| {
            let start_line = 1 + live[..hunk.offset].bytes().filter(|b| *b == b'\n').count();
            let old_newlines = hunk.old.bytes().filter(|b| *b == b'\n').count();
            let new_newlines = hunk.new.bytes().filter(|b| *b == b'\n').count();
            let old_end_line = start_line + old_newlines - usize::from(hunk.old.ends_with('\n'));
            let unchanged_suffix_continues = hunk.new.ends_with('\n')
                && !hunk.old.ends_with('\n')
                && hunk.offset + hunk.old.len() < live.len();
            let new_span = if hunk.new.is_empty() {
                0
            } else {
                snapshot::numbered_lines(&hunk.new).len().max(1)
                    + usize::from(unchanged_suffix_continues)
            };
            let line_delta = new_newlines as isize - old_newlines as isize;
            (start_line, old_end_line, new_span, line_delta)
        })
        .collect();

    let mut remapped = BTreeSet::new();
    for &line in seen_lines {
        let mut shift = 0isize;
        let mut replaced = false;
        for (start_line, old_end_line, _, delta) in &geometry {
            if *start_line <= line && line <= *old_end_line {
                replaced = true;
                break;
            }
            if line > *old_end_line {
                shift += *delta;
            }
        }
        if !replaced {
            remapped.insert((line as isize + shift) as usize);
        }
    }
    let mut shift = 0isize;
    for (start_line, _, new_span, delta) in &geometry {
        let remapped_start = (*start_line as isize + shift) as usize;
        remapped.extend(remapped_start..remapped_start + new_span);
        shift += *delta;
    }
    remapped.into_iter().collect()
}

async fn read_live(path: &Path) -> Result<(String, String), String> {
    let display = path.to_string_lossy();
    snapshot::ensure_readable_regular_file(&display, u64::MAX).await?;
    let raw = fs::read_to_string(path)
        .await
        .map_err(crate::util::errstr)?;
    let normalized = snapshot::normalize_text(&raw);
    Ok((raw, normalized))
}

fn truncate_output(text: &str) -> String {
    crate::tools::shell::truncate_output(text, 200)
}
