use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};
use crate::tools::write_atomic::write_atomic;

use super::edit_diff;

pub struct EditFileUnifiedTool;

#[derive(Debug, Deserialize)]
struct Args {
    path: String,
    #[serde(alias = "diff")]
    patch: String,
    expected_hash: Option<String>,
}

pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

#[derive(Debug)]
struct Patch {
    update_path: Option<String>,
    chunks: Vec<Chunk>,
}

#[derive(Debug)]
struct Chunk {
    change_context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    is_end_of_file: bool,
}

#[async_trait]
impl Tool for EditFileUnifiedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Edit an existing UTF-8 file transactionally using Codex-style apply_patch chunks. Provide path and patch (or diff). Patch may be a full *** Begin Patch / *** Update File envelope or just one or more @@ chunks. Use - old lines, + new lines, and optional space-prefixed context lines; keep anchors small and unique. Unified hunk range headers like @@ -10,4 +10,6 @@ are accepted but ignored for matching.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the existing UTF-8 file to edit. Relative paths resolved from cwd."
                    },
                    "patch": {
                        "type": "string",
                        "description": "Codex-style patch for this file. May be a full *** Begin Patch / *** Update File patch or just @@ chunks. Each changed line starts with -, +, or space for context. Unified hunk range headers are accepted."
                    },
                    "diff": {
                        "type": "string",
                        "description": "Alias for patch."
                    },
                    "expected_hash": {
                        "type": "string",
                        "description": "Optional SHA-256 hash from preview. When provided, execution fails if the file changed since preview."
                    }
                },
                "required": ["path"],
                "anyOf": [
                    { "required": ["patch"] },
                    { "required": ["diff"] }
                ],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        execute_edit_file_unified(arguments).await
    }
}

pub async fn preview_edit_file_unified(arguments: Value) -> Result<EditPreview, String> {
    let args: Args = serde_json::from_value(arguments).map_err(|e| e.to_string())?;
    let (original, next) = build_candidate_content(&args).await?;
    Ok(EditPreview {
        before_hash: sha256_hex(&original),
        diff: edit_diff::build_unified_diff(&args.path, &original, &next),
    })
}

pub async fn execute_edit_file_unified(arguments: Value) -> Result<String, String> {
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

    let path = std::path::Path::new(&args.path);
    let metadata = fs::metadata(path).await.map_err(|e| e.to_string())?;
    let permissions = Some(metadata.permissions());
    write_atomic(path, &next, permissions).await?;

    let summary = edit_diff::summarize_change(&original, &next);
    let shown_diff = edit_diff::build_unified_diff(&args.path, &original, &next);
    Ok(format!("edited file ({summary}){shown_diff}"))
}

async fn build_candidate_content(args: &Args) -> Result<(String, String), String> {
    let metadata = fs::metadata(&args.path).await.map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("path is not a regular file".to_string());
    }
    let original = fs::read_to_string(&args.path)
        .await
        .map_err(|e| e.to_string())?;
    let patch = parse_codex_patch(&args.patch)?;
    validate_patch_path(&args.path, patch.update_path.as_deref())?;
    let next = apply_chunks(&original, &args.path, &patch.chunks)?;
    Ok((original, next))
}

fn validate_patch_path(path: &str, update_path: Option<&str>) -> Result<(), String> {
    let Some(update_path) = update_path else {
        return Ok(());
    };
    let requested = std::path::Path::new(path);
    let update = std::path::Path::new(update_path);
    if requested == update || requested.ends_with(update) || update.ends_with(requested) {
        Ok(())
    } else {
        Err(format!(
            "patch updates {update_path}, but edit_file path is {path}"
        ))
    }
}

fn parse_codex_patch(patch: &str) -> Result<Patch, String> {
    let mut update_path = None;
    let mut chunks = Vec::new();
    let mut current: Option<Chunk> = None;
    let mut saw_update_header = false;
    let mut saw_begin = false;

    for raw in patch.lines() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line == "*** Begin Patch" {
            saw_begin = true;
            continue;
        }
        if line == "*** End Patch" {
            break;
        }
        if let Some(path) = line.strip_prefix("*** Update File: ") {
            if update_path.is_some() {
                return Err("patch must update exactly one file".to_string());
            }
            saw_update_header = true;
            update_path = Some(path.trim().to_string());
            continue;
        }
        if line.starts_with("*** Add File: ") || line.starts_with("*** Delete File: ") {
            return Err("edit_file only supports Update File patches".to_string());
        }
        if line.starts_with("*** Move to: ") {
            return Err("edit_file does not support file moves".to_string());
        }
        if line == "*** End of File" {
            let Some(chunk) = current.as_mut() else {
                return Err("*** End of File must appear inside a hunk".to_string());
            };
            chunk.is_end_of_file = true;
            continue;
        }
        if let Some(header) = line.strip_prefix("@@") {
            if let Some(chunk) = current.take() {
                push_chunk(&mut chunks, chunk)?;
            }
            let header = header.trim();
            current = Some(Chunk {
                change_context: if header.is_empty() || is_unified_hunk_range(header) {
                    None
                } else {
                    Some(header.to_string())
                },
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                is_end_of_file: false,
            });
            continue;
        }

        let Some(chunk) = current.as_mut() else {
            if line.trim().is_empty() {
                continue;
            }
            if !saw_begin && !saw_update_header {
                return Err("patch must contain at least one @@ hunk".to_string());
            }
            return Err(format!("invalid patch line before hunk: {line}"));
        };

        if let Some(text) = line.strip_prefix(' ') {
            chunk.old_lines.push(text.to_string());
            chunk.new_lines.push(text.to_string());
        } else if let Some(text) = line.strip_prefix('-') {
            chunk.old_lines.push(text.to_string());
        } else if let Some(text) = line.strip_prefix('+') {
            chunk.new_lines.push(text.to_string());
        } else if line.trim().is_empty() {
            chunk.old_lines.push(String::new());
            chunk.new_lines.push(String::new());
        } else {
            return Err(format!("invalid hunk line: {line}"));
        }
    }

    if let Some(chunk) = current.take() {
        push_chunk(&mut chunks, chunk)?;
    }
    if chunks.is_empty() {
        return Err("patch must contain at least one @@ hunk".to_string());
    }
    Ok(Patch {
        update_path,
        chunks,
    })
}

fn is_unified_hunk_range(header: &str) -> bool {
    let header = header.strip_suffix("@@").unwrap_or(header).trim();
    let mut parts = header.split_whitespace();
    let Some(old_part) = parts.next() else {
        return false;
    };
    if !old_part.starts_with('-') || !is_hunk_range(&old_part[1..]) {
        return false;
    }
    match parts.next() {
        Some(new_part) if new_part.starts_with('+') && is_hunk_range(&new_part[1..]) => true,
        None => true,
        _ => false,
    }
}

fn is_hunk_range(range: &str) -> bool {
    let mut parts = range.split(',');
    let Some(start) = parts.next() else {
        return false;
    };
    if start.is_empty() || !start.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        Some(len) if !len.is_empty() && len.chars().all(|ch| ch.is_ascii_digit()) => {
            parts.next().is_none()
        }
        None => true,
        _ => false,
    }
}

fn push_chunk(chunks: &mut Vec<Chunk>, chunk: Chunk) -> Result<(), String> {
    if chunk.old_lines.is_empty() && chunk.new_lines.is_empty() {
        return Err("empty hunk".to_string());
    }
    chunks.push(chunk);
    Ok(())
}

fn apply_chunks(content: &str, path: &str, chunks: &[Chunk]) -> Result<String, String> {
    let mut lines: Vec<String> = content.split('\n').map(String::from).collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }

    let mut replacements = Vec::new();
    let mut line_index = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        if let Some(context) = &chunk.change_context {
            let pattern = vec![context.clone()];
            let Some(found) = seek_sequence(&lines, &pattern, line_index, false) else {
                return Err(format!(
                    "hunk {} failed: failed to find context '{}' in {path}",
                    index + 1,
                    context
                ));
            };
            line_index = found + 1;
        }

        if chunk.old_lines.is_empty() {
            let insertion_idx = if lines.last().is_some_and(String::is_empty) {
                lines.len() - 1
            } else {
                lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern = chunk.old_lines.as_slice();
        let mut new_slice = chunk.new_lines.as_slice();
        let mut found = seek_sequence(&lines, pattern, line_index, chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(&lines, pattern, line_index, chunk.is_end_of_file);
        }

        let Some(start_idx) = found else {
            return Err(format!(
                "hunk {} failed: failed to find expected lines in {path}:\n{}",
                index + 1,
                chunk.old_lines.join("\n")
            ));
        };
        replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
        line_index = start_idx + pattern.len();
    }

    replacements.sort_by_key(|(index, _, _)| *index);
    for (start_idx, old_len, new_segment) in replacements.into_iter().rev() {
        for _ in 0..old_len {
            if start_idx < lines.len() {
                lines.remove(start_idx);
            }
        }
        for (offset, new_line) in new_segment.into_iter().enumerate() {
            lines.insert(start_idx + offset, new_line);
        }
    }

    if !lines.last().is_some_and(String::is_empty) {
        lines.push(String::new());
    }
    Ok(lines.join("\n"))
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| lines[i + offset].trim_end() == pat.trim_end())
        {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| lines[i + offset].trim() == pat.trim())
        {
            return Some(i);
        }
    }
    for i in search_start..=lines.len().saturating_sub(pattern.len()) {
        if pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| normalize_line(&lines[i + offset]) == normalize_line(pat))
        {
            return Some(i);
        }
    }
    None
}

fn normalize_line(line: &str) -> String {
    line.trim()
        .chars()
        .map(|ch| match ch {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}
