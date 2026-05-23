use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::{fs, io::AsyncWriteExt};

use crate::tools::types::{Tool, ToolDefinition};

mod diff;

pub struct EditFileTool;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    search: Option<String>,
    replace: Option<String>,
    edits: Option<Vec<RawEditOperation>>,
    mode: Option<String>,
    content: Option<String>,
    expected_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawEditOperation {
    search: Option<String>,
    replace: Option<String>,
    delete: Option<String>,
    insert_before: Option<String>,
    insert_after: Option<String>,
    text: Option<String>,
    #[serde(rename = "match")]
    match_mode: Option<String>,
}

#[derive(Debug, Clone)]
enum EditOperation {
    Replace { search: String, replace: String },
    Delete { search: String },
    InsertBefore { anchor: String, text: String },
    InsertAfter { anchor: String, text: String },
}

pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file",
            description: "Apply precise transactional text edits to an existing UTF-8 file. Supports search/replace, edits, delete, insert_before, insert_after, and rewrite mode. Search must match exactly once — include enough context. Preview shown before applying.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the existing UTF-8 file to edit. Relative paths resolved from cwd."
                    },
                    "search": {
                        "type": "string",
                        "description": "Exact text to replace. Must appear exactly once — include enough surrounding context. Use with replace."
                    },
                    "replace": {
                        "type": "string",
                        "description": "Replacement text for search compatibility mode."
                    },
                    "edits": {
                        "type": "array",
                        "description": "Transactional edits applied in order. If any edit fails, nothing is written.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search": { "type": "string", "description": "Exact text to replace. Must appear exactly once. Use with replace." },
                                "replace": { "type": "string", "description": "Replacement text (also accepts text as fallback)." },
                                "delete": { "type": "string", "description": "Exact text to remove. Must appear exactly once." },
                                "insert_before": { "type": "string", "description": "Exact anchor to insert before. Must appear exactly once." },
                                "insert_after": { "type": "string", "description": "Exact anchor to insert after. Must appear exactly once." },
                                "text": { "type": "string", "description": "Text to insert for insert_before or insert_after." },
                                "match": { "type": "string", "enum": ["exact"], "description": "Match mode (only exact supported)." }
                            },
                            "additionalProperties": false
                        }
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["rewrite"],
                        "description": "Set to rewrite to replace the entire file."
                    },
                    "content": {
                        "type": "string",
                        "description": "New file contents for rewrite mode."
                    },
                    "expected_hash": {
                        "type": "string",
                        "description": "Optional SHA-256 hash to detect stale edits."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        execute_edit_file(arguments).await
    }
}

pub async fn preview_edit_file(arguments: Value) -> Result<EditPreview, String> {
    let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
    let (original, next) = build_candidate_content(&args).await?;
    let before_hash = sha256_hex(&original);
    let diff = diff::build_unified_diff(&args.path, &original, &next);
    Ok(EditPreview { before_hash, diff })
}

pub async fn execute_edit_file(arguments: Value) -> Result<String, String> {
    let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
    let (original, next) = build_candidate_content(&args).await?;

    if let Some(expected_hash) = args.expected_hash.as_deref() {
        let actual_hash = sha256_hex(&original);
        if actual_hash != expected_hash {
            return Err("file changed since preview; edit not applied".to_string());
        }
    }

    if original == next {
        return Ok("no changes".to_string());
    }

    write_atomic_preserving_permissions(&args.path, &next).await?;
    let summary = diff::summarize_change(&original, &next);
    let diff = diff::build_unified_diff(&args.path, &original, &next);
    Ok(format!("edited file ({summary}){diff}"))
}

async fn build_candidate_content(args: &Args) -> Result<(String, String), String> {
    let metadata = fs::metadata(&args.path).await.map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("path is not a regular file".to_string());
    }

    let original = fs::read_to_string(&args.path)
        .await
        .map_err(|e| e.to_string())?;

    if args.mode.as_deref() == Some("rewrite") {
        ensure_no_edit_fields_for_rewrite(args)?;
        let content = args
            .content
            .clone()
            .ok_or_else(|| "mode=rewrite requires content".to_string())?;
        return Ok((original, content));
    }

    if args.mode.is_some() {
        return Err("unsupported mode; expected \"rewrite\"".to_string());
    }
    if args.content.is_some() {
        return Err("content is only valid with mode=rewrite".to_string());
    }

    let operations = parse_operations(args)?;
    let mut next = original.clone();
    for (index, operation) in operations.iter().enumerate() {
        next = apply_one_operation(&next, operation)
            .map_err(|e| format!("edit {} failed: {e}", index + 1))?;
    }
    Ok((original, next))
}

fn ensure_no_edit_fields_for_rewrite(args: &Args) -> Result<(), String> {
    if args.search.is_some() || args.replace.is_some() || args.edits.is_some() {
        return Err("mode=rewrite cannot be combined with search/replace or edits".to_string());
    }
    Ok(())
}

fn parse_operations(args: &Args) -> Result<Vec<EditOperation>, String> {
    let has_single = args.search.is_some() || args.replace.is_some();
    let has_multi = args.edits.is_some();

    match (has_single, has_multi) {
        (true, true) => Err("use either search/replace or edits, not both".to_string()),
        (false, false) => Err("provide search/replace, edits, or mode=rewrite".to_string()),
        (true, false) => {
            let search = args
                .search
                .clone()
                .ok_or_else(|| "search is required when replace is provided".to_string())?;
            let replace = args
                .replace
                .clone()
                .ok_or_else(|| "replace is required when search is provided".to_string())?;
            if search.is_empty() {
                return Err("search must not be empty".to_string());
            }
            Ok(vec![EditOperation::Replace { search, replace }])
        }
        (false, true) => {
            let raw = args.edits.as_ref().expect("checked above");
            if raw.is_empty() {
                return Err("edits must not be empty".to_string());
            }
            raw.iter()
                .enumerate()
                .map(|(i, edit)| {
                    parse_operation(edit).map_err(|e| format!("edit {} invalid: {e}", i + 1))
                })
                .collect()
        }
    }
}

fn parse_operation(raw: &RawEditOperation) -> Result<EditOperation, String> {
    if raw.match_mode.as_deref().unwrap_or("exact") != "exact" {
        return Err("only match=\"exact\" is currently supported".to_string());
    }

    let mut kinds = 0;
    kinds += (raw.search.is_some() || raw.replace.is_some()) as usize;
    kinds += raw.delete.is_some() as usize;
    kinds += raw.insert_before.is_some() as usize;
    kinds += raw.insert_after.is_some() as usize;
    if kinds != 1 {
        return Err(
            "specify exactly one of search/replace, delete, insert_before, or insert_after"
                .to_string(),
        );
    }

    if raw.search.is_some() || raw.replace.is_some() {
        if raw.delete.is_some() || raw.insert_before.is_some() || raw.insert_after.is_some() {
            return Err("search/replace cannot be combined with delete or insert".to_string());
        }
        // Be intentionally tolerant of a stray `text` field here. Some models
        // include it after seeing the insert operation schema. If `replace` is
        // missing, treat `text` as the replacement so the edit can still work.
        let search = raw
            .search
            .clone()
            .ok_or_else(|| "search is required with replace".to_string())?;
        let replace = raw
            .replace
            .clone()
            .or_else(|| raw.text.clone())
            .ok_or_else(|| "replace is required with search".to_string())?;
        if search.is_empty() {
            return Err("search must not be empty".to_string());
        }
        return Ok(EditOperation::Replace { search, replace });
    }

    if let Some(search) = raw.delete.clone() {
        if raw.text.is_some() {
            return Err("delete cannot include text".to_string());
        }
        if search.is_empty() {
            return Err("delete text must not be empty".to_string());
        }
        return Ok(EditOperation::Delete { search });
    }

    if let Some(anchor) = raw.insert_before.clone() {
        if anchor.is_empty() {
            return Err("insert_before anchor must not be empty".to_string());
        }
        let text = raw
            .text
            .clone()
            .ok_or_else(|| "insert_before requires text".to_string())?;
        return Ok(EditOperation::InsertBefore { anchor, text });
    }

    if let Some(anchor) = raw.insert_after.clone() {
        if anchor.is_empty() {
            return Err("insert_after anchor must not be empty".to_string());
        }
        let text = raw
            .text
            .clone()
            .ok_or_else(|| "insert_after requires text".to_string())?;
        return Ok(EditOperation::InsertAfter { anchor, text });
    }

    Err("invalid edit operation".to_string())
}

fn apply_one_operation(content: &str, operation: &EditOperation) -> Result<String, String> {
    match operation {
        EditOperation::Replace { search, replace } => {
            replace_unique(content, search, replace, "search text")
        }
        EditOperation::Delete { search } => replace_unique(content, search, "", "delete text"),
        EditOperation::InsertBefore { anchor, text } => replace_unique(
            content,
            anchor,
            &format!("{text}{anchor}"),
            "insert_before anchor",
        ),
        EditOperation::InsertAfter { anchor, text } => replace_unique(
            content,
            anchor,
            &format!("{anchor}{text}"),
            "insert_after anchor",
        ),
    }
}

fn replace_unique(
    content: &str,
    needle: &str,
    replacement: &str,
    label: &str,
) -> Result<String, String> {
    if needle.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    let offsets = match_offsets(content, needle);
    if offsets.len() != 1 {
        return Err(match_error(content, needle, label, &offsets));
    }
    Ok(content.replacen(needle, replacement, 1))
}

fn match_offsets(content: &str, needle: &str) -> Vec<usize> {
    content
        .match_indices(needle)
        .map(|(offset, _)| offset)
        .collect()
}

fn match_error(content: &str, needle: &str, label: &str, offsets: &[usize]) -> String {
    if offsets.is_empty() {
        let mut msg = format!("{label} matched 0 times; expected exactly 1");
        if let Some(hint) = find_closest_lines(content, needle) {
            msg.push_str(&format!(
                "\n\nClosest region in file (around line {}):\n{}",
                hint.line, hint.snippet
            ));
            // Diagnose common causes: whitespace mismatch, trailing newline, etc.
            let diag = diagnose_mismatch(&hint.snippet, needle);
            if !diag.is_empty() {
                msg.push_str(&format!("\n\nLikely cause: {diag}"));
            }
        }
        msg.push_str(
            "\n\nTip: read_file the target region first, then copy the exact text verbatim into search.",
        );
        return msg;
    }

    let mut msg = format!(
        "{label} matched {} times; expected exactly 1\n\nMatches:",
        offsets.len()
    );
    for offset in offsets.iter().take(10) {
        msg.push_str(&format!(
            "\n- line {}",
            line_number_for_byte_offset(content, *offset)
        ));
    }
    if offsets.len() > 10 {
        msg.push_str(&format!("\n- ... and {} more", offsets.len() - 10));
    }
    msg.push_str(
        "\n\nTip: include enough nearby unique context so the search text matches exactly one location. Do not retry a short repeated fragment; use a larger enclosing block or separate edits with distinct anchors.",
    );
    msg
}

struct ClosestHint {
    line: usize,
    snippet: String,
}

/// Compare the closest file region with the needle to identify likely causes of mismatch.
fn diagnose_mismatch(file_region: &str, needle: &str) -> String {
    let file_trimmed: Vec<&str> = file_region.lines().map(|l| l.trim_end()).collect();
    let needle_trimmed: Vec<&str> = needle.lines().map(|l| l.trim_end()).collect();
    let mut causes: Vec<String> = Vec::new();

    // Check trailing whitespace difference
    if file_trimmed == needle_trimmed {
        causes
            .push("trailing whitespace differs (tabs vs spaces, or extra trailing spaces)".into());
    }

    // Check indentation mismatch
    let file_prefixes: Vec<&str> = file_region
        .lines()
        .map(|l| {
            l.split_once(|c: char| !c.is_whitespace())
                .map(|(p, _)| p)
                .unwrap_or(l)
        })
        .collect();
    let needle_prefixes: Vec<&str> = needle
        .lines()
        .map(|l| {
            l.split_once(|c: char| !c.is_whitespace())
                .map(|(p, _)| p)
                .unwrap_or(l)
        })
        .collect();
    if file_prefixes.len() == needle_prefixes.len() && file_prefixes != needle_prefixes {
        causes.push("indentation differs (tabs vs spaces, or wrong indent level)".into());
    }

    // Check line count mismatch
    let fl = file_region.lines().count();
    let nl = needle.lines().count();
    if fl != nl {
        causes.push(format!(
            "line count differs (file has {fl}, search text has {nl})"
        ));
    }

    // Check trailing newline
    if file_region.ends_with('\n') != needle.ends_with('\n') && fl == nl {
        causes.push("trailing newline mismatch".into());
    }

    causes.join("; ")
}

/// Find the region in `content` that best matches `needle` using the first
/// non-empty line of the needle as a probe. Returns the 1-based line number
/// and a small snippet of the surrounding file content.
fn find_closest_lines(content: &str, needle: &str) -> Option<ClosestHint> {
    let probe = needle.lines().find(|l| !l.trim().is_empty())?;
    let probe_stripped = probe.trim();

    let file_lines: Vec<&str> = content.lines().collect();
    if file_lines.is_empty() {
        return None;
    }

    // Score each line by how much of the probe it shares.
    let mut best_idx = 0;
    let mut best_score = 0usize;
    for (i, line) in file_lines.iter().enumerate() {
        let line_stripped = line.trim();
        let score = common_prefix_len(probe_stripped, line_stripped)
            + common_substring_len(probe_stripped, line_stripped);
        if score > best_score {
            best_score = score;
            best_idx = i;
        }
    }

    if best_score == 0 {
        return None;
    }

    // Show 3 lines of context around the best match.
    let start = best_idx.saturating_sub(1);
    let end = (best_idx + 3).min(file_lines.len());
    let snippet = file_lines[start..end].join("\n");

    Some(ClosestHint {
        line: best_idx + 1,
        snippet,
    })
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .count()
}

/// Simple shared-char count via a sliding window on the shorter string.
fn common_substring_len(a: &str, b: &str) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (short, long) = if a.len() < b.len() { (a, b) } else { (b, a) };
    let short_bytes = short.as_bytes();
    let long_bytes = long.as_bytes();
    let window = short_bytes.len().min(16);
    let mut best = 0;
    for i in 0..=short_bytes.len().saturating_sub(window) {
        let chunk = &short_bytes[i..i + window];
        let matches = long_bytes
            .windows(window)
            .map(|w| chunk.iter().zip(w.iter()).filter(|(a, b)| a == b).count())
            .max()
            .unwrap_or(0);
        if matches > best {
            best = matches;
        }
    }
    best
}

fn line_number_for_byte_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

async fn write_atomic_preserving_permissions(path: &str, content: &str) -> Result<(), String> {
    let path = std::path::Path::new(path);
    let metadata = fs::metadata(path).await.map_err(|e| e.to_string())?;
    let permissions = metadata.permissions();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128);
    let pid = std::process::id();
    let temp_path = path.with_extension(format!("bone-tmp-{pid}-{nanos}"));

    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .map_err(|e| e.to_string())?;
        f.write_all(content.as_bytes()).await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
        f.flush().await.map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
    }

    fs::set_permissions(&temp_path, permissions)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp_path);
            e.to_string()
        })?;
    fs::rename(&temp_path, path).await.map_err(|e| {
        let _ = std::fs::remove_file(&temp_path);
        e.to_string()
    })?;
    Ok(())
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}


