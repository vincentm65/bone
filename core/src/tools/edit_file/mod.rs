//! The `edit_file` tool: search/replace and rewrite edits with fuzzy anchor matching.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use strsim::normalized_levenshtein;
use tokio::fs;

use crate::tools::types::{Tool, ToolDefinition};
use crate::tools::write_atomic::write_atomic;

pub(crate) mod diff;

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
    replace_all: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawEditOperation {
    search: Option<String>,
    replace: Option<String>,
    delete: Option<String>,
    insert_before: Option<String>,
    insert_after: Option<String>,
    text: Option<String>,
    replace_all: Option<bool>,
    #[serde(rename = "match")]
    match_mode: Option<String>,
}

#[derive(Debug, Clone)]
enum EditOperation {
    Replace {
        search: String,
        replace: String,
        replace_all: bool,
    },
    Delete {
        search: String,
    },
    InsertBefore {
        anchor: String,
        text: String,
    },
    InsertAfter {
        anchor: String,
        text: String,
    },
}

pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Edit an existing UTF-8 file. Exactly one mode per call: (a) top-level search+replace for a single change, (b) edits[] for multiple search/replace changes, or (c) mode=\"rewrite\"+content for a full rewrite. Anchors must match exactly one location unless replace_all=true (exact global replace). On success a unified diff is returned; on failure the error names the valid shapes and closest candidates.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path to edit."
                    },
                    "search": {
                        "type": "string",
                        "description": "Single replacement: text to find."
                    },
                    "replace": {
                        "type": "string",
                        "description": "Replacement text for top-level search."
                    },
                    "edits": {
                        "type": "array",
                        "description": "List of edit operations. Each item: search+replace with optional replace_all.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search": { "type": "string", "description": "Text to find and replace." },
                                "replace": { "type": "string", "description": "Replacement text." },
                                "replace_all": { "type": "boolean", "description": "Replace every exact occurrence instead of requiring a unique match." }
                            },
                            "required": ["search", "replace"],
                            "additionalProperties": false
                        }
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["rewrite"],
                        "description": "Set to \"rewrite\" to replace entire file; requires content."
                    },
                    "content": {
                        "type": "string",
                        "description": "New file contents (only with mode=\"rewrite\")."
                    },
                    "expected_hash": {
                        "type": "string",
                        "description": "SHA-256 hash. Fails if file changed since preview."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "For the top-level search+replace: replace every exact occurrence instead of requiring a unique match. Use for renames."
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

pub async fn preview_edit_file(tool_name: &str, arguments: Value) -> Result<EditPreview, String> {
    let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
    let (original, next) = build_candidate_content(&args).await?;
    let before_hash = sha256_hex(&original);
    let diff = diff::build_unified_diff(tool_name, &args.path, &original, &next);
    Ok(EditPreview { before_hash, diff })
}

pub async fn execute_edit_file(arguments: Value) -> Result<String, String> {
    let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
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

    {
        let path = std::path::Path::new(&args.path);
        let metadata = fs::metadata(path).await.map_err(crate::util::errstr)?;
        let permissions = Some(metadata.permissions());
        write_atomic(path, &next, permissions).await?;
    }
    let summary = diff::summarize_change(&original, &next);
    let diff = truncate_diff(
        &diff::build_unified_diff("edit_file", &args.path, &original, &next),
        200,
    );
    Ok(format!("edited file ({summary}){diff}"))
}

fn truncate_diff(diff: &str, max_lines: usize) -> String {
    crate::tools::shell::truncate_output(diff, max_lines)
}

async fn build_candidate_content(args: &Args) -> Result<(String, String), String> {
    let metadata = fs::metadata(&args.path)
        .await
        .map_err(crate::util::errstr)?;
    if !metadata.is_file() {
        return Err("path is not a regular file".to_string());
    }

    let original = fs::read_to_string(&args.path)
        .await
        .map_err(crate::util::errstr)?;

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
    if args.search.is_some()
        || args.replace.is_some()
        || args.edits.is_some()
        || args.replace_all.is_some()
    {
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
            Ok(vec![EditOperation::Replace {
                search,
                replace,
                replace_all: args.replace_all.unwrap_or(false),
            }])
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
        return Ok(EditOperation::Replace {
            search,
            replace,
            replace_all: raw.replace_all.unwrap_or(false),
        });
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
    // Replace-all is exact global replacement; it deliberately bypasses the
    // unique-anchor rule used for every other operation.
    if let EditOperation::Replace {
        search,
        replace,
        replace_all: true,
    } = operation
    {
        return replace_all_spans(content, search, replace);
    }

    let (needle, replacement, label) = match operation {
        EditOperation::Replace {
            search, replace, ..
        } => (search.as_str(), replace.clone(), "search text"),
        EditOperation::Delete { search } => (search.as_str(), String::new(), "delete text"),
        EditOperation::InsertBefore { anchor, text } => (
            anchor.as_str(),
            format!("{text}{anchor}"),
            "insert_before anchor",
        ),
        EditOperation::InsertAfter { anchor, text } => (
            anchor.as_str(),
            format!("{anchor}{text}"),
            "insert_after anchor",
        ),
    };
    replace_matched_span(content, needle, &replacement, label)
}

/// Exact global replacement for rename-style edits (replace_all=true). Unlike
/// the single-match path, this does not require a unique anchor.
fn replace_all_spans(content: &str, needle: &str, replacement: &str) -> Result<String, String> {
    if needle.is_empty() {
        return Err("search text must not be empty".to_string());
    }
    if !content.contains(needle) {
        return Err("search text matched 0 times; expected at least 1".to_string());
    }
    Ok(content.replace(needle, replacement))
}

fn replace_matched_span(
    content: &str,
    needle: &str,
    replacement: &str,
    label: &str,
) -> Result<String, String> {
    let MatchSpan { start, end } = find_match_span(content, needle, label)?;
    let mut next = String::with_capacity(content.len() - (end - start) + replacement.len());
    next.push_str(&content[..start]);
    next.push_str(replacement);
    next.push_str(&content[end..]);
    Ok(next)
}

struct MatchSpan {
    start: usize,
    end: usize,
}

/// Line-window matches always end at a line boundary, but the needle may
/// omit the final newline. Keep the file's newline in place rather than
/// letting the replacement swallow it.
fn trim_span_newline(content: &str, needle: &str, span: MatchSpan) -> MatchSpan {
    if needle.ends_with('\n') {
        return span;
    }
    let matched = &content[span.start..span.end];
    let end = if matched.ends_with("\r\n") {
        span.end - 2
    } else if matched.ends_with('\n') {
        span.end - 1
    } else {
        span.end
    };
    MatchSpan {
        start: span.start,
        end,
    }
}

fn find_match_span(content: &str, needle: &str, label: &str) -> Result<MatchSpan, String> {
    if needle.is_empty() {
        return Err(format!("{label} must not be empty"));
    }

    let exact: Vec<_> = content
        .match_indices(needle)
        .map(|(start, text)| Candidate {
            start,
            end: start + text.len(),
            score: 1.0,
        })
        .collect();
    if exact.len() == 1 {
        return Ok(MatchSpan {
            start: exact[0].start,
            end: exact[0].end,
        });
    }
    if exact.len() > 1 {
        return Err(ambiguous_error(
            content,
            label,
            exact.len(),
            exact.iter().map(|m| m.start),
        ));
    }

    let normalized = normalized_candidates(content, needle);
    if normalized.len() == 1 {
        return Ok(trim_span_newline(
            content,
            needle,
            MatchSpan {
                start: normalized[0].start,
                end: normalized[0].end,
            },
        ));
    }
    if normalized.len() > 1 {
        return Err(ambiguous_error(
            content,
            label,
            normalized.len(),
            normalized.iter().map(|m| m.start),
        ));
    }

    match fuzzy_candidate(content, needle) {
        Some(best) if best.score >= 0.92 && needle.trim().len() >= 30 && best.margin >= 0.08 => {
            Ok(trim_span_newline(
                content,
                needle,
                MatchSpan {
                    start: best.start,
                    end: best.end,
                },
            ))
        }
        Some(best) => Err(no_match_error(
            content,
            needle,
            label,
            Some((best.start, best.end, best.score)),
        )),
        None => Err(no_match_error(content, needle, label, None)),
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    start: usize,
    end: usize,
    score: f64,
}

struct FuzzyCandidate {
    start: usize,
    end: usize,
    score: f64,
    margin: f64,
}

struct ClosestHint {
    line: usize,
    snippet: String,
}

fn normalized_candidates(content: &str, needle: &str) -> Vec<Candidate> {
    let needle_norm = normalize_for_compare(needle);
    line_window_candidates(content, needle)
        .into_iter()
        .filter(|candidate| {
            normalize_for_compare(&content[candidate.start..candidate.end]) == needle_norm
        })
        .collect()
}

fn fuzzy_candidate(content: &str, needle: &str) -> Option<FuzzyCandidate> {
    let needle_norm = normalize_for_compare(needle);
    let mut ranked: Vec<Candidate> = line_window_candidates(content, needle)
        .into_iter()
        .map(|mut candidate| {
            candidate.score = normalized_levenshtein(
                &normalize_for_compare(&content[candidate.start..candidate.end]),
                &needle_norm,
            );
            candidate
        })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    let best = *ranked.first()?;
    // Margin measures ambiguity between distinct locations; windows that
    // overlap the best span (e.g. the same block one line larger) are the
    // same location, not a competing candidate.
    let second = ranked
        .iter()
        .skip(1)
        .find(|c| c.end <= best.start || c.start >= best.end)
        .map(|c| c.score)
        .unwrap_or(0.0);
    Some(FuzzyCandidate {
        start: best.start,
        end: best.end,
        score: best.score,
        margin: best.score - second,
    })
}

fn line_window_candidates(content: &str, needle: &str) -> Vec<Candidate> {
    let spans = line_spans(content);
    let needle_lines = needle_line_count(needle);
    if spans.is_empty() || needle_lines == 0 {
        return Vec::new();
    }

    // Also try windows one line shorter and longer than the needle, so a
    // search text with a dropped or added blank line can still be recovered.
    let mut candidates = Vec::new();
    for window in needle_lines.saturating_sub(1).max(1)..=needle_lines + 1 {
        if window > spans.len() {
            continue;
        }
        for start_line in 0..=spans.len() - window {
            candidates.push(Candidate {
                start: spans[start_line].0,
                end: spans[start_line + window - 1].1,
                score: 0.0,
            });
        }
    }
    candidates.sort_by_key(|c| (c.start, c.end));
    candidates.dedup_by_key(|c| (c.start, c.end));
    candidates
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

fn needle_line_count(needle: &str) -> usize {
    if needle.is_empty() {
        0
    } else {
        needle.split_inclusive('\n').count()
    }
}

/// Normalized text with a single trailing newline removed. Line windows
/// always end in `\n` (except at EOF), so needles that omit the final
/// newline would otherwise never compare equal to any window.
fn normalize_for_compare(text: &str) -> String {
    let mut norm = normalize_for_match(text);
    if norm.ends_with('\n') {
        norm.pop();
    }
    norm
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
    // Models often "correct" typographic characters when reproducing file
    // content; fold the common ones so they don't defeat recovery.
    let mapped: String = line
        .chars()
        .filter_map(|ch| match ch {
            '\u{2018}' | '\u{2019}' => Some('\''),
            '\u{201C}' | '\u{201D}' => Some('"'),
            '\u{00A0}' => Some(' '),
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => None,
            _ => Some(ch),
        })
        .collect();
    let trimmed = mapped.trim_end_matches([' ', '\t']);
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

fn ambiguous_error(
    content: &str,
    label: &str,
    count: usize,
    starts: impl Iterator<Item = usize>,
) -> String {
    let mut msg = format!("{label} matched {count} times; expected exactly 1\n\nMatches:");
    for start in starts.take(10) {
        let snippet: String = content[start..]
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        msg.push_str(&format!(
            "\n- line {}: {}",
            line_number_for_byte_offset(content, start),
            snippet.trim_end()
        ));
    }
    if count > 10 {
        msg.push_str(&format!("\n- ... and {} more", count - 10));
    }
    msg.push_str("\n\nInclude more surrounding lines to make the match unique.");
    msg
}

fn no_match_error(
    content: &str,
    needle: &str,
    label: &str,
    best: Option<(usize, usize, f64)>,
) -> String {
    let mut msg = format!("{label} matched 0 times; expected exactly 1");
    if let Some((start, end, score)) = best {
        msg.push_str(&format!(
            "\nBest candidate line {} scored {:.2}; edit not applied because the match was not confident enough.\n\nActual candidate:\n{}\n\nSubmitted search:\n{}",
            line_number_for_byte_offset(content, start),
            score,
            &content[start..end],
            needle
        ));
    } else if let Some(hint) = find_closest_lines(content, needle) {
        msg.push_str(&format!(
            "\nClosest candidate line {}:\n{}\n\nSubmitted search:\n{}",
            hint.line, hint.snippet, needle
        ));
    } else {
        msg.push_str(&format!("\n\nSubmitted search:\n{needle}"));
    }
    msg
}

fn find_closest_lines(content: &str, needle: &str) -> Option<ClosestHint> {
    fuzzy_candidate(content, needle).map(|candidate| ClosestHint {
        line: line_number_for_byte_offset(content, candidate.start),
        snippet: content[candidate.start..candidate.end].to_string(),
    })
}

fn line_number_for_byte_offset(content: &str, offset: usize) -> usize {
    content[..offset.min(content.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

pub fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}
