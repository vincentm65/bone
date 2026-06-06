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
            name: "edit_file".to_string(),
            description: "Edit an existing UTF-8 file. Use one of: search+replace (single change), edits[] (multiple changes), or mode=\"rewrite\"+content (full rewrite). Anchors must match one location.".to_string(),
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
                        "description": "List of edit operations. Each item: search+replace, delete, insert_before+text, or insert_after+text.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search": { "type": "string", "description": "Text to find and replace." },
                                "replace": { "type": "string", "description": "Replacement text." },
                                "delete": { "type": "string", "description": "Text to delete." },
                                "insert_before": { "type": "string", "description": "Insert text before this anchor." },
                                "insert_after": { "type": "string", "description": "Insert text after this anchor." },
                                "text": { "type": "string", "description": "Text to insert." },
                                "match": { "type": "string", "enum": ["exact"], "description": "Match mode: \"exact\"." }
                            },
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

    {
        let path = std::path::Path::new(&args.path);
        let metadata = fs::metadata(path).await.map_err(|e| e.to_string())?;
        let permissions = Some(metadata.permissions());
        write_atomic(path, &next, permissions).await?;
    }
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
    let (needle, replacement, label) = match operation {
        EditOperation::Replace { search, replace } => {
            (search.as_str(), replace.clone(), "search text")
        }
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
        return Ok(MatchSpan {
            start: normalized[0].start,
            end: normalized[0].end,
        });
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
            Ok(MatchSpan {
                start: best.start,
                end: best.end,
            })
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
    let needle_norm = normalize_for_match(needle);
    line_window_candidates(content, needle)
        .into_iter()
        .filter(|candidate| {
            normalize_for_match(&content[candidate.start..candidate.end]) == needle_norm
        })
        .collect()
}

fn fuzzy_candidate(content: &str, needle: &str) -> Option<FuzzyCandidate> {
    let needle_norm = normalize_for_match(needle);
    let mut ranked: Vec<Candidate> = line_window_candidates(content, needle)
        .into_iter()
        .map(|mut candidate| {
            candidate.score = normalized_levenshtein(
                &normalize_for_match(&content[candidate.start..candidate.end]),
                &needle_norm,
            );
            candidate
        })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    let best = *ranked.first()?;
    let second = ranked.get(1).map(|c| c.score).unwrap_or(0.0);
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

    let mut candidates = Vec::new();
    if needle_lines > spans.len() {
        return Vec::new();
    }
    for start_line in 0..=spans.len() - needle_lines {
        candidates.push(Candidate {
            start: spans[start_line].0,
            end: spans[start_line + needle_lines - 1].1,
            score: 0.0,
        });
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

fn ambiguous_error(
    content: &str,
    label: &str,
    count: usize,
    starts: impl Iterator<Item = usize>,
) -> String {
    let mut msg = format!("{label} matched {count} times; expected exactly 1\n\nMatches:");
    for start in starts.take(10) {
        msg.push_str(&format!(
            "\n- line {}",
            line_number_for_byte_offset(content, start)
        ));
    }
    if count > 10 {
        msg.push_str(&format!("\n- ... and {} more", count - 10));
    }
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
