use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use strsim::normalized_levenshtein;
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};
use crate::tools::write_atomic::write_atomic;

use super::edit_file::diff;

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
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
}

#[derive(Debug)]
enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

#[async_trait]
impl Tool for EditFileUnifiedTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Edit an existing UTF-8 file transactionally by applying a unified diff patch. Provide path and patch (or diff). Preview/display output uses the same numbered diff format. Hunks use context-aware matching with exact, whitespace-normalized, and fuzzy fallback.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the existing UTF-8 file to edit. Relative paths resolved from cwd."
                    },
                    "patch": {
                        "type": "string",
                        "description": "Unified diff patch for this file. May include ---/+++ file headers and one or more @@ hunks. Use space-prefixed context lines, - removed lines, and + added lines."
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
        diff: diff::build_unified_diff(&args.path, &original, &next),
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

    let summary = diff::summarize_change(&original, &next);
    let shown_diff = diff::build_unified_diff(&args.path, &original, &next);
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
    let hunks = parse_unified_diff(&args.patch)?;
    let mut next = original.clone();
    for (index, hunk) in hunks.iter().enumerate() {
        next = apply_hunk(&next, hunk).map_err(|e| format!("hunk {} failed: {e}", index + 1))?;
    }
    Ok((original, next))
}

fn parse_unified_diff(patch: &str) -> Result<Vec<Hunk>, String> {
    let mut hunks = Vec::new();
    let mut current: Option<Hunk> = None;

    for raw in patch.split_inclusive('\n') {
        let line = raw;
        if line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("diff --git ")
            || line.starts_with("index ")
        {
            continue;
        }
        if line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(Hunk {
                old_start: parse_old_start(line)
                    .ok_or_else(|| format!("invalid hunk header: {}", line.trim_end()))?,
                lines: Vec::new(),
            });
            continue;
        }

        let Some(hunk) = current.as_mut() else {
            if line.trim().is_empty() {
                continue;
            }
            return Err("patch must contain at least one @@ hunk".to_string());
        };

        if let Some(text) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine::Context(text.to_string()));
        } else if let Some(text) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine::Remove(text.to_string()));
        } else if let Some(text) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine::Add(text.to_string()));
        } else if line.starts_with("\\ No newline at end of file") {
            // Keep parser permissive. The previous line already carries the text.
        } else if line.trim().is_empty() {
            // A truly empty diff line is invalid in strict unified diff, but models
            // sometimes emit it. Treat it as an empty context line.
            hunk.lines.push(HunkLine::Context(line.to_string()));
        } else {
            return Err(format!("invalid hunk line: {}", line.trim_end()));
        }
    }
    if let Some(hunk) = current.take() {
        hunks.push(hunk);
    }
    if hunks.is_empty() {
        return Err("patch must contain at least one @@ hunk".to_string());
    }
    Ok(hunks)
}

fn parse_old_start(header: &str) -> Option<usize> {
    let mut parts = header.split_whitespace();
    parts.next()?; // @@
    let old = parts.next()?;
    let range = old.strip_prefix('-')?;
    range.split(',').next()?.parse().ok()
}

fn apply_hunk(content: &str, hunk: &Hunk) -> Result<String, String> {
    let old_lines: Vec<String> = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.clone()),
            HunkLine::Add(_) => None,
        })
        .collect();
    let new_text: String = hunk
        .lines
        .iter()
        .filter_map(|line| match line {
            HunkLine::Context(s) | HunkLine::Add(s) => Some(s.as_str()),
            HunkLine::Remove(_) => None,
        })
        .collect();

    if old_lines.is_empty() {
        return insert_at_line(content, hunk.old_start, &new_text);
    }

    let old_text: String = old_lines.iter().map(String::as_str).collect();
    let span = find_hunk_span(content, &old_text, old_lines.len(), hunk.old_start)?;
    let mut next = String::with_capacity(content.len() - (span.end - span.start) + new_text.len());
    next.push_str(&content[..span.start]);
    next.push_str(&new_text);
    next.push_str(&content[span.end..]);
    Ok(next)
}

struct Span {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
struct Candidate {
    start: usize,
    end: usize,
    line: usize,
    score: f64,
}

fn find_hunk_span(
    content: &str,
    old_text: &str,
    old_line_count: usize,
    expected_line: usize,
) -> Result<Span, String> {
    let exact: Vec<_> = line_window_candidates(content, old_line_count)
        .into_iter()
        .filter(|candidate| content[candidate.start..candidate.end] == *old_text)
        .collect();
    if let Some(candidate) = choose_exact_or_normalized(&exact, expected_line) {
        return Ok(Span {
            start: candidate.start,
            end: candidate.end,
        });
    }
    if exact.len() > 1 {
        return Err(ambiguous_error(
            "hunk context",
            exact.len(),
            exact.iter().map(|c| c.line),
        ));
    }

    let old_norm = normalize_for_match(old_text);
    let normalized: Vec<_> = line_window_candidates(content, old_line_count)
        .into_iter()
        .filter(|candidate| {
            normalize_for_match(&content[candidate.start..candidate.end]) == old_norm
        })
        .collect();
    if let Some(candidate) = choose_exact_or_normalized(&normalized, expected_line) {
        return Ok(Span {
            start: candidate.start,
            end: candidate.end,
        });
    }
    if normalized.len() > 1 {
        return Err(ambiguous_error(
            "hunk context",
            normalized.len(),
            normalized.iter().map(|c| c.line),
        ));
    }

    let mut ranked: Vec<Candidate> = line_window_candidates(content, old_line_count)
        .into_iter()
        .map(|mut candidate| {
            candidate.score = normalized_levenshtein(
                &normalize_for_match(&content[candidate.start..candidate.end]),
                &old_norm,
            );
            candidate
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| {
            line_distance(a.line, expected_line).cmp(&line_distance(b.line, expected_line))
        })
    });

    let Some(best) = ranked.first().copied() else {
        return Err("hunk context matched 0 times; expected exactly 1".to_string());
    };
    let second_score = ranked.get(1).map(|c| c.score).unwrap_or(0.0);
    let margin = best.score - second_score;
    if best.score >= 0.85 && (margin >= 0.04 || best.line == expected_line) {
        return Ok(Span {
            start: best.start,
            end: best.end,
        });
    }

    Err(format!(
        "hunk context matched 0 times; expected exactly 1\nBest candidate line {} scored {:.2}; patch not applied because the match was not confident enough.\n\nActual candidate:\n{}\n\nSubmitted hunk context:\n{}",
        best.line,
        best.score,
        &content[best.start..best.end],
        old_text
    ))
}

fn choose_exact_or_normalized(candidates: &[Candidate], expected_line: usize) -> Option<Candidate> {
    if candidates.len() == 1 {
        return Some(candidates[0]);
    }
    let at_expected: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.line == expected_line)
        .collect();
    if at_expected.len() == 1 {
        Some(at_expected[0])
    } else {
        None
    }
}

fn ambiguous_error(label: &str, count: usize, lines: impl Iterator<Item = usize>) -> String {
    let mut msg = format!("{label} matched {count} times; expected exactly 1\n\nMatches:");
    for line in lines.take(10) {
        msg.push_str(&format!("\n- line {line}"));
    }
    if count > 10 {
        msg.push_str(&format!("\n- ... and {} more", count - 10));
    }
    msg
}

fn insert_at_line(content: &str, line: usize, text: &str) -> Result<String, String> {
    let spans = line_spans(content);
    let start = if line == 0 {
        0
    } else if line > spans.len() {
        content.len()
    } else {
        spans[line - 1].1
    };
    let mut next = String::with_capacity(content.len() + text.len());
    next.push_str(&content[..start]);
    next.push_str(text);
    next.push_str(&content[start..]);
    Ok(next)
}

fn line_window_candidates(content: &str, line_count: usize) -> Vec<Candidate> {
    let spans = line_spans(content);
    if spans.is_empty() || line_count == 0 || line_count > spans.len() {
        return Vec::new();
    }
    (0..=spans.len() - line_count)
        .map(|idx| Candidate {
            start: spans[idx].0,
            end: spans[idx + line_count - 1].1,
            line: idx + 1,
            score: 0.0,
        })
        .collect()
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut start = 0;
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            spans.push((start, idx + 1));
            start = idx + 1;
        }
    }
    if start < content.len() {
        spans.push((start, content.len()));
    }
    spans
}

fn normalize_for_match(text: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map(|body| (body, true))
            .unwrap_or((line, false));
        push_normalized_line(&mut out, body);
        if newline {
            out.push('\n');
        }
    }
    out
}

fn push_normalized_line(out: &mut String, line: &str) {
    let trimmed = line.trim_end_matches([' ', '\t']);
    let mut in_space = false;
    for ch in trimmed.chars() {
        if ch == ' ' || ch == '\t' {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
        } else {
            out.push(ch);
            in_space = false;
        }
    }
}

fn line_distance(a: usize, b: usize) -> usize {
    a.abs_diff(b)
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}
