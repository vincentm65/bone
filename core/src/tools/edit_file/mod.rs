//! Simple exact-text editing for existing files.
//!
//! The agent supplies a path plus one exact `old_text` → `new_text`
//! replacement. Context-aware calls require a preceding `read_file`; the
//! snapshot stays internal and is used for visibility and stale-file checks.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::tools::snapshot::{self, SnapshotStore};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::write_atomic::write_atomic_if_unchanged;

pub(crate) mod diff;

pub struct EditFileTool;

pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

type Snapshots = Arc<RwLock<SnapshotStore>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    old_text: String,
    new_text: String,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Preferred tool for modifying existing file contents; use this instead of shell commands such as sed -i, tee, heredocs, scripts, or redirection. Replaces one exact, unique block in an existing UTF-8 file. Read the file first, then pass the same path, copy a unique block of shown text into old_text, and put the desired replacement in new_text. Use an empty new_text to delete. To insert, include a small unchanged surrounding block in both old_text and new_text. Returns a unified diff.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path. Relative paths resolve from the working directory."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact unique text copied from read_file output, without line-number prefixes. May be empty only when the file is empty."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text. May be empty to delete old_text."
                    }
                },
                "required": ["path", "old_text", "new_text"],
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
    let args = parse_args(arguments)?;
    let resolved = snapshot::resolve_existing_path(&args.path, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    let (_, live) = read_live(&resolved).await?;
    let old_text = snapshot::normalize_text(&args.old_text);
    let new_text = snapshot::normalize_text(&args.new_text);
    let edited = replace_unique(&live, &old_text, &new_text, &path)?;
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
    let args = parse_args(arguments)?;
    let resolved = snapshot::resolve_existing_path(&args.path, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    let (live_raw, live) = read_live(&resolved).await?;
    let old_text = snapshot::normalize_text(&args.old_text);
    let new_text = snapshot::normalize_text(&args.new_text);

    let (base, fully_seen) = if let Some(store) = snapshots {
        let guard = store.read().map_err(|e| e.to_string())?;
        let snap = guard
            .head(&path)
            .ok_or_else(|| format!("read `{path}` with read_file before editing it"))?;
        ensure_visible(&snap.text, &old_text, &snap.seen_lines, &path)?;
        let line_count = snapshot::numbered_lines(&snap.text).len();
        (
            snap.text.clone(),
            (1..=line_count).all(|line| snap.seen_lines.contains(&line)),
        )
    } else {
        (live.clone(), true)
    };

    // Validate against what the model saw even when the live file has drifted.
    unique_match_offset(&base, &old_text, &path)?;
    let live_offset = unique_match_offset(&live, &old_text, &path).map_err(|e| {
        if base != live {
            format!("{e}; `{path}` changed after it was read, so re-read it and retry")
        } else {
            e
        }
    })?;
    let edited = replace_unique(&live, &old_text, &new_text, &path)?;
    if edited == live {
        return Err(format!(
            "no change to `{path}`; old_text and new_text produce identical content"
        ));
    }

    let permissions = fs::metadata(&resolved)
        .await
        .map_err(|e| format!("could not re-check `{path}` before writing: {e}"))?
        .permissions();
    write_atomic_if_unchanged(&resolved, &edited, Some(permissions), live_raw.as_bytes()).await?;

    if let Some(store) = snapshots {
        let mut guard = store.write().map_err(|e| e.to_string())?;
        let seen = if fully_seen && base == live {
            (1..=snapshot::numbered_lines(&edited).len()).collect()
        } else {
            replacement_lines(&live, live_offset, &new_text)
        };
        guard.record(&path, &edited, Some(&seen));
    }

    let rendered = truncate_output(&diff::build_unified_diff(
        "edit_file",
        &path,
        &live,
        &edited,
    ));
    Ok(format!("Edited: {path}\n{rendered}").trim_end().to_string())
}

fn parse_args(arguments: Value) -> Result<Args, String> {
    let args: Args = serde_json::from_value(arguments).map_err(|e| {
        format!("edit_file requires path, old_text, and new_text string fields: {e}")
    })?;
    if args.path.trim().is_empty() {
        return Err("`path` must not be empty".to_string());
    }
    if args.old_text.is_empty() && args.new_text.is_empty() {
        return Err("`old_text` and `new_text` cannot both be empty".to_string());
    }
    Ok(args)
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

fn replace_unique(text: &str, old: &str, new: &str, path: &str) -> Result<String, String> {
    let offset = unique_match_offset(text, old, path)?;
    let mut result = String::with_capacity(text.len() - old.len() + new.len());
    result.push_str(&text[..offset]);
    result.push_str(new);
    result.push_str(&text[offset + old.len()..]);
    Ok(result)
}

fn ensure_visible(
    snapshot_text: &str,
    old_text: &str,
    seen_lines: &BTreeSet<usize>,
    path: &str,
) -> Result<(), String> {
    if old_text.is_empty() && snapshot_text.is_empty() {
        return Ok(());
    }
    let offset = unique_match_offset(snapshot_text, old_text, path)?;
    let start_line = 1 + snapshot_text[..offset]
        .bytes()
        .filter(|b| *b == b'\n')
        .count();
    let last_byte = offset + old_text.len() - 1;
    let end_line = 1 + snapshot_text[..=last_byte]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        - usize::from(old_text.ends_with('\n'));
    if (start_line..=end_line).any(|line| !seen_lines.contains(&line)) {
        return Err(format!(
            "old_text includes lines that were not shown from `{path}`; read that range before editing"
        ));
    }
    Ok(())
}

fn replacement_lines(live: &str, offset: usize, new_text: &str) -> Vec<usize> {
    if new_text.is_empty() {
        return Vec::new();
    }
    let start_line = 1 + live[..offset].bytes().filter(|b| *b == b'\n').count();
    let count = snapshot::numbered_lines(new_text).len().max(1);
    (start_line..start_line + count).collect()
}

async fn read_live(path: &Path) -> Result<(String, String), String> {
    let meta = fs::metadata(path).await.map_err(crate::util::errstr)?;
    if !meta.is_file() {
        return Err(format!("`{}` is not a regular file", path.display()));
    }
    let raw = fs::read_to_string(path)
        .await
        .map_err(crate::util::errstr)?;
    let normalized = snapshot::normalize_text(&raw);
    Ok((raw, normalized))
}

fn truncate_output(text: &str) -> String {
    crate::tools::shell::truncate_output(text, 200)
}
