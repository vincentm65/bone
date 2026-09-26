//! Anchor-based file editing: edits anchored on the `LINE#HASH` tags
//! `read_file` renders when this tool is enabled.
//!
//! The model never re-types old text. Each anchor names a line number plus a
//! 2-character content hash; an anchor is accepted when it matches the live
//! file, or when it matches a recorded earlier version of the file (snapshot
//! head or bounded history) and that line is unchanged since. This keeps
//! anchors valid across the model's own earlier edits and unrelated external
//! changes, while rejecting edits to lines that changed after they were seen.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;

use crate::tools::snapshot::{self, Snapshot, Snapshots, TextFormat};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::write_atomic::write_atomic_if_unchanged;

mod diff;

pub const TOOL_NAME: &str = "edit_file";

const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
/// Live lines shown on each side of a failing anchor.
const ERROR_CONTEXT: usize = 2;
/// Distance within which a moved line with the same hash is suggested.
const SUGGEST_RADIUS: usize = 40;
/// Cap on unseen range lines echoed in one error.
const MAX_ECHO_LINES: usize = 80;

pub struct EditFileTool;

/// Preview payload for an anchor-based edit: hash of the file before the
/// change plus a unified diff of the planned result.
pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

/// 2-character content hash of one normalized line (FNV-1a, 10 bits).
pub fn line_hash(line: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for byte in line.as_bytes() {
        h ^= u32::from(*byte);
        h = h.wrapping_mul(0x0100_0193);
    }
    let v = ((h >> 16) ^ h) & 0x3ff;
    let mut out = String::with_capacity(2);
    out.push(ALPHABET[(v >> 5) as usize] as char);
    out.push(ALPHABET[(v & 31) as usize] as char);
    out
}

/// Render one line as `LINE#HASH|content`.
pub fn render_line(number: usize, content: &str) -> String {
    format!("{number}#{}|{content}", line_hash(content))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct EditSpec {
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    #[serde(default)]
    edits: Option<Vec<EditSpec>>,
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Clone, Debug)]
struct Anchor {
    line: usize,
    hash: String,
    raw: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Replace,
    After,
    Before,
}

#[derive(Debug)]
struct Edit {
    index: usize,
    kind: Kind,
    start: Anchor,
    end: Anchor,
    lines: Vec<String>,
}

/// A resolved edit in live-file coordinates (0-based line indices).
#[derive(Debug)]
struct Op {
    index: usize,
    start: usize,
    delete: usize,
    lines: Vec<String>,
}

/// One known state of the file that anchors may refer to.
struct Version {
    lines: Vec<String>,
    seen: BTreeSet<usize>,
    all_seen: bool,
    /// `None` for the live file; otherwise line index → live line index.
    map: Option<Vec<Option<usize>>>,
}

enum Miss {
    Mismatch,
    Changed,
    Unseen {
        start: usize,
        count: usize,
    },
    /// The anchor resolves to different live positions depending on which
    /// known version it was copied from (or one version says it changed).
    Ambiguous {
        spots: Vec<(usize, usize)>,
    },
}

struct Plan {
    rendered: String,
    edited: String,
    seen: Vec<usize>,
    output: String,
}

struct Failure {
    message: String,
    shown: Vec<usize>,
}

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        let anchor = |what: &str| json!({"type": "string", "description": what});
        ToolDefinition {
            name: TOOL_NAME.to_string(),
            description: "Preferred tool for modifying existing files; use it instead of shell commands such as sed -i or redirection. read_file shows every line as `LINE#HASH|content`. Reference lines by their `LINE#HASH` anchor (for example \"12#k3\") instead of copying old text. Each edit is one of: {at, text} replaces one line; {at, end, text} replaces the inclusive range; {after, text} or {before, text} inserts lines. `text` is the complete new content, lines separated by \\n, without anchors; \"\" deletes. All edits in one call use anchors from the same read, must not overlap, and apply together or not at all. Anchors stay valid when other lines move, including after your own earlier edits. The result shows changed lines with fresh anchors, so chain further edits without re-reading.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path. Relative paths resolve from the working directory."
                    },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Edits anchored on the same file state; applied atomically.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "at": anchor("Replace from this LINE#HASH anchor."),
                                "end": anchor("Optional inclusive last LINE#HASH of the replaced range."),
                                "after": anchor("Insert after this LINE#HASH anchor (\"0\" = start of file)."),
                                "before": anchor("Insert before this LINE#HASH anchor."),
                                "text": {
                                    "type": "string",
                                    "description": "New lines without anchors; \"\" deletes the replaced lines."
                                }
                            },
                            "required": ["text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
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

/// Compute the approval diff without writing. Snapshots are optional; without
/// them only anchors matching the live file resolve.
pub async fn preview_edit_file(
    arguments: Value,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<EditPreview, String> {
    let (path_arg, edits) = parse_args(arguments)?;
    let resolved = snapshot::resolve_existing_path(&path_arg, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    let (live_raw, live) = read_live(&resolved).await?;
    let (head, history) = store_view(snapshots, &path)?;
    let plan = plan(
        &path,
        &live_raw,
        &live,
        &edits,
        snapshots.is_some(),
        head,
        history,
    )
    .map_err(|failure| failure.message)?;
    Ok(EditPreview {
        before_hash: snapshot::compute_tag(&live),
        diff: diff::build_unified_diff(TOOL_NAME, &path, &live, &plan.edited),
    })
}

async fn run_edit(
    arguments: Value,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<String, String> {
    let (path_arg, edits) = parse_args(arguments)?;
    let resolved = snapshot::resolve_existing_path(&path_arg, working_dir).await?;
    let path = resolved.to_string_lossy().into_owned();
    let (live_raw, live) = read_live(&resolved).await?;
    let (head, history) = store_view(snapshots, &path)?;
    let live_format = TextFormat::detect(&live_raw);

    let plan = match plan(
        &path,
        &live_raw,
        &live,
        &edits,
        snapshots.is_some(),
        head,
        history,
    ) {
        Ok(plan) => plan,
        Err(failure) => {
            if let Some(store) = snapshots
                && !failure.shown.is_empty()
            {
                let mut guard = store.write().map_err(|e| e.to_string())?;
                guard.record_with_format(&path, &live, live_format, Some(&failure.shown));
            }
            return Err(failure.message);
        }
    };

    let permissions = fs::metadata(&resolved)
        .await
        .map_err(|e| format!("could not re-check `{path}` before writing: {e}"))?
        .permissions();
    write_atomic_if_unchanged(
        &resolved,
        &plan.rendered,
        Some(permissions),
        live_raw.as_bytes(),
    )
    .await?;

    if let Some(store) = snapshots {
        let mut guard = store.write().map_err(|e| e.to_string())?;
        guard.record_with_format(
            &path,
            &plan.edited,
            TextFormat::detect(&plan.rendered),
            Some(&plan.seen),
        );
    }
    Ok(plan.output)
}

fn store_view(
    snapshots: Option<&Snapshots>,
    path: &str,
) -> Result<(Option<Snapshot>, Vec<Snapshot>), String> {
    let Some(store) = snapshots else {
        return Ok((None, Vec::new()));
    };
    let guard = store
        .read()
        .map_err(|_| "snapshot store lock is poisoned".to_string())?;
    Ok((
        guard.head(path).cloned(),
        guard.history(path).cloned().collect(),
    ))
}

// ── argument parsing ────────────────────────────────────────────────────────

fn parse_args(arguments: Value) -> Result<(String, Vec<Edit>), String> {
    if !arguments.is_object() {
        return Err("edit_file arguments must be an object with `path` and `edits`".to_string());
    }
    if arguments.get("path").is_none() {
        return Err(
            "edit_file is missing `path`; provide the file path with the edits".to_string(),
        );
    }
    let args: Args = serde_json::from_value(arguments).map_err(|e| {
        format!("edit_file expects {{path, edits:[{{at|after|before, end?, text}}]}}: {e}")
    })?;
    if args.path.trim().is_empty() {
        return Err("`path` must not be empty".to_string());
    }
    let top_level = args.at.is_some()
        || args.end.is_some()
        || args.after.is_some()
        || args.before.is_some()
        || args.text.is_some();
    let specs = match (args.edits, top_level) {
        (Some(_), true) => {
            return Err("provide edits either in `edits` or at top level, not both".to_string());
        }
        (Some(edits), false) if edits.is_empty() => {
            return Err("`edits` must not be empty".to_string());
        }
        (Some(edits), false) => edits,
        (None, true) => vec![EditSpec {
            at: args.at,
            end: args.end,
            after: args.after,
            before: args.before,
            text: args.text,
        }],
        (None, false) => return Err("provide a non-empty `edits` array".to_string()),
    };
    let edits = specs
        .into_iter()
        .enumerate()
        .map(|(i, spec)| parse_edit(i + 1, spec))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((args.path, edits))
}

fn parse_edit(index: usize, spec: EditSpec) -> Result<Edit, String> {
    let fail = |msg: String| format!("edit {index}: {msg}");
    let chosen = [
        spec.at.is_some(),
        spec.after.is_some(),
        spec.before.is_some(),
    ]
    .iter()
    .filter(|set| **set)
    .count();
    if chosen != 1 {
        return Err(fail(
            "set exactly one of `at`, `after`, or `before`".to_string(),
        ));
    }
    if spec.end.is_some() && spec.at.is_none() {
        return Err(fail("`end` is only valid with `at`".to_string()));
    }
    let text = spec
        .text
        .ok_or_else(|| fail("missing `text` (use \"\" to delete)".to_string()))?;
    let lines = split_text(&text);
    let (kind, raw) = if let Some(at) = spec.at {
        (Kind::Replace, at)
    } else if let Some(after) = spec.after {
        (Kind::After, after)
    } else {
        (Kind::Before, spec.before.expect("one anchor is set"))
    };
    let start = parse_anchor(&raw, kind == Kind::After).map_err(&fail)?;
    let end = match spec.end {
        Some(end) => parse_anchor(&end, false).map_err(&fail)?,
        None => start.clone(),
    };
    if end.line < start.line {
        return Err(fail(format!(
            "`end` {} is before `at` {}",
            end.raw, start.raw
        )));
    }
    if kind != Kind::Replace && lines.is_empty() {
        return Err(fail("insertion `text` must not be empty".to_string()));
    }
    Ok(Edit {
        index,
        kind,
        start,
        end,
        lines,
    })
}

fn parse_anchor(raw: &str, allow_zero: bool) -> Result<Anchor, String> {
    let trimmed = raw.trim();
    let head = trimmed.split('|').next().unwrap_or_default().trim();
    let shown = head.to_string();
    if allow_zero && head == "0" {
        return Ok(Anchor {
            line: 0,
            hash: String::new(),
            raw: shown,
        });
    }
    let invalid = || {
        format!("anchor `{trimmed}` must be LINE#HASH as shown by read_file, for example `12#k3`")
    };
    let (number, hash) = head.split_once('#').ok_or_else(invalid)?;
    let line: usize = number.trim().parse().map_err(|_| invalid())?;
    let hash = hash.trim().to_ascii_lowercase();
    if line == 0 || hash.len() != 2 || !hash.bytes().all(|b| ALPHABET.contains(&b)) {
        return Err(invalid());
    }
    Ok(Anchor {
        line,
        hash,
        raw: shown,
    })
}

/// Split replacement text into lines. One trailing newline is ignored, and a
/// block whose every line carries a copied `LINE#HASH|` prefix is unwrapped.
fn split_text(text: &str) -> Vec<String> {
    let text = snapshot::normalize_text(text);
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<&str> = body.split('\n').collect();
    let stripped: Option<Vec<&str>> = lines.iter().map(|line| strip_prefix(line)).collect();
    match stripped {
        Some(stripped) => stripped.into_iter().map(str::to_string).collect(),
        None => lines.into_iter().map(str::to_string).collect(),
    }
}

fn strip_prefix(line: &str) -> Option<&str> {
    let (number, rest) = line.split_once('#')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let bytes = rest.as_bytes();
    if bytes.len() < 3 || bytes[2] != b'|' || !bytes[..2].iter().all(|b| ALPHABET.contains(b)) {
        return None;
    }
    Some(&rest[3..])
}

// ── planning ────────────────────────────────────────────────────────────────

fn plan(
    path: &str,
    live_raw: &str,
    live: &str,
    edits: &[Edit],
    tracked: bool,
    head: Option<Snapshot>,
    history: Vec<Snapshot>,
) -> Result<Plan, Failure> {
    let live_lines: Vec<String> = snapshot::numbered_lines(live)
        .into_iter()
        .map(str::to_string)
        .collect();
    // `stale`: the file changed since the model last saw it, so its anchors
    // refer to snapshots rather than live numbering (see `resolve_all`).
    let stale = head.as_ref().is_some_and(|head| head.text != live);
    let seen = live_seen(&live_lines, live, head.as_ref());
    let older = older_versions(&live_lines, live, head, history, edits);
    let mut versions = Vec::with_capacity(older.len() + 1);
    versions.push(Version {
        lines: live_lines,
        seen,
        all_seen: !tracked,
        map: None,
    });
    versions.extend(older);
    let live_lines = &versions[0].lines;

    let mut ops = Vec::with_capacity(edits.len());
    let mut errors: Vec<String> = Vec::new();
    let mut shown: BTreeSet<usize> = BTreeSet::new();
    for edit in edits {
        match resolve_all(&versions, edit, stale) {
            Ok((start, count)) => ops.push(to_op(edit, start, count)),
            Err(miss) => errors.push(describe_miss(edit, miss, live_lines, &mut shown)),
        }
    }
    if !errors.is_empty() {
        return Err(failure(path, errors, live_lines, shown));
    }

    ops.sort_by_key(|op| (op.start, usize::from(op.delete > 0), op.index));
    check_overlaps(path, &ops)?;

    let live_seen = &versions[0];
    let (entries, regions) =
        apply(live_raw, live_lines, live_seen, &ops).map_err(|message| Failure {
            message,
            shown: Vec::new(),
        })?;
    let rendered = render_raw(live_raw, live, &entries);
    let edited = snapshot::normalize_text(&rendered);
    if edited == live {
        return Err(Failure {
            message: format!("no change to `{path}`; the edits produce identical content"),
            shown: Vec::new(),
        });
    }
    let (output, output_lines) = render_output(path, &entries, &regions, &ops);
    let mut seen: BTreeSet<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.seen)
        .map(|(i, _)| i + 1)
        .collect();
    seen.extend(output_lines);
    Ok(Plan {
        rendered,
        edited,
        seen: seen.into_iter().collect(),
        output,
    })
}

/// Live line numbers the model has seen, carried over from the head snapshot.
fn live_seen(live_lines: &[String], live: &str, head: Option<&Snapshot>) -> BTreeSet<usize> {
    match head {
        Some(head) if head.text == live => head.seen_lines.clone(),
        Some(head) => {
            let head_lines: Vec<String> = snapshot::numbered_lines(&head.text)
                .into_iter()
                .map(str::to_string)
                .collect();
            let map = line_map(&head_lines, live_lines);
            head.seen_lines
                .iter()
                .filter_map(|line| map.get(line - 1).copied().flatten().map(|i| i + 1))
                .collect()
        }
        None => BTreeSet::new(),
    }
}

/// Snapshot versions that could recognise at least one edit's anchors. The
/// Myers line map is only computed for those, since every other snapshot
/// would resolve to `Miss::Mismatch` anyway.
fn older_versions(
    live_lines: &[String],
    live: &str,
    head: Option<Snapshot>,
    history: Vec<Snapshot>,
    edits: &[Edit],
) -> Vec<Version> {
    let mut texts: Vec<String> = vec![live.to_string()];
    let mut out = Vec::new();
    for snap in head.into_iter().chain(history) {
        if texts.contains(&snap.text) {
            continue;
        }
        texts.push(snap.text.clone());
        let lines: Vec<String> = snapshot::numbered_lines(&snap.text)
            .into_iter()
            .map(str::to_string)
            .collect();
        if !edits.iter().any(|edit| anchors_match(&lines, edit)) {
            continue;
        }
        let map = line_map(&lines, live_lines);
        out.push(Version {
            lines,
            seen: snap.seen_lines,
            all_seen: false,
            map: Some(map),
        });
    }
    out
}

/// Whether both of `edit`'s anchors hash-match `lines` (the cheap first check
/// in `resolve`).
fn anchors_match(lines: &[String], edit: &Edit) -> bool {
    let (s, e) = (&edit.start, &edit.end);
    s.line > 0
        && e.line <= lines.len()
        && line_hash(&lines[s.line - 1]) == s.hash
        && line_hash(&lines[e.line - 1]) == e.hash
}

/// Map each `old` line index to its live index where the line is unchanged.
fn line_map(old: &[String], new: &[String]) -> Vec<Option<usize>> {
    let mut map = vec![None; old.len()];
    let deadline = Some(Instant::now() + Duration::from_millis(500));
    for op in similar::capture_diff_slices_deadline(similar::Algorithm::Myers, old, new, deadline) {
        if let similar::DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for k in 0..len {
                map[old_index + k] = Some(new_index + k);
            }
        }
    }
    map
}

/// Resolve an edit against every known version of the file and refuse to
/// guess. Low-entropy lines (blank lines, closing braces) share hashes, so a
/// stale `LINE#HASH` can match a different live line; if the versions that
/// recognise the anchor disagree on its live position, or one reports that the
/// line changed, the edit fails with fresh anchors instead of landing on the
/// wrong line.
///
/// When the live file differs from the latest snapshot (`stale`), the model
/// has not seen the live numbering, so live is consulted only when no
/// snapshot recognises the anchor.
fn resolve_all(versions: &[Version], edit: &Edit, stale: bool) -> Result<(usize, usize), Miss> {
    let mut tally = Tally::default();
    if !stale {
        tally.note(resolve(&versions[0], edit));
    }
    for version in &versions[1..] {
        tally.note(resolve(version, edit));
    }
    if stale && tally.found.is_empty() && !tally.changed && tally.unseen.is_none() {
        tally.note(resolve(&versions[0], edit));
    }
    let Tally {
        found,
        changed,
        unseen,
    } = tally;
    match found.len() {
        0 => {}
        1 if !changed => return Ok(found[0]),
        _ => return Err(Miss::Ambiguous { spots: found }),
    }
    if let Some((start, count)) = unseen {
        return Err(Miss::Unseen { start, count });
    }
    if changed {
        return Err(Miss::Changed);
    }
    Err(Miss::Mismatch)
}

/// Outcomes of resolving one edit against several file versions.
#[derive(Default)]
struct Tally {
    found: Vec<(usize, usize)>,
    changed: bool,
    unseen: Option<(usize, usize)>,
}

impl Tally {
    fn note(&mut self, result: Result<(usize, usize), Miss>) {
        match result {
            Ok(hit) => {
                if !self.found.contains(&hit) {
                    self.found.push(hit);
                }
            }
            Err(Miss::Changed) => self.changed = true,
            Err(Miss::Unseen { start, count }) => {
                self.unseen.get_or_insert((start, count));
            }
            Err(_) => {}
        }
    }
}

/// Resolve an edit's anchor range in `version`, returning the live 0-based
/// start index and line count.
fn resolve(version: &Version, edit: &Edit) -> Result<(usize, usize), Miss> {
    let (s, e) = (&edit.start, &edit.end);
    if s.line == 0 {
        return if version.map.is_none() {
            Ok((0, 0))
        } else {
            Err(Miss::Mismatch)
        };
    }
    let n = version.lines.len();
    if e.line > n
        || line_hash(&version.lines[s.line - 1]) != s.hash
        || line_hash(&version.lines[e.line - 1]) != e.hash
    {
        return Err(Miss::Mismatch);
    }
    let start = match &version.map {
        None => s.line - 1,
        Some(map) => {
            let first = map[s.line - 1].ok_or(Miss::Changed)?;
            for line in s.line..=e.line {
                if map[line - 1] != Some(first + line - s.line) {
                    return Err(Miss::Changed);
                }
            }
            first
        }
    };
    let count = e.line - s.line + 1;
    let unseen =
        !version.all_seen && (s.line + 1..e.line).any(|line| !version.seen.contains(&line));
    if unseen {
        return Err(Miss::Unseen { start, count });
    }
    Ok((start, count))
}

fn to_op(edit: &Edit, start: usize, count: usize) -> Op {
    let (start, delete) = match edit.kind {
        Kind::Replace => (start, count),
        Kind::After if edit.start.line == 0 => (0, 0),
        Kind::After => (start + 1, 0),
        Kind::Before => (start, 0),
    };
    Op {
        index: edit.index,
        start,
        delete,
        lines: edit.lines.clone(),
    }
}

fn describe_miss(
    edit: &Edit,
    miss: Miss,
    live_lines: &[String],
    shown: &mut BTreeSet<usize>,
) -> String {
    let label = if edit.start.raw == edit.end.raw {
        format!("`{}`", edit.start.raw)
    } else {
        format!("`{}`..`{}`", edit.start.raw, edit.end.raw)
    };
    let n = live_lines.len();
    let window = |line: usize, shown: &mut BTreeSet<usize>| {
        if n == 0 {
            return;
        }
        let center = line.clamp(1, n);
        let lo = center.saturating_sub(ERROR_CONTEXT).max(1);
        let hi = (center + ERROR_CONTEXT).min(n);
        shown.extend(lo..=hi);
    };
    match miss {
        Miss::Ambiguous { spots } => {
            let mut at = Vec::new();
            for (start, count) in &spots {
                window(start + 1, shown);
                let hi = (start + count).min(start + MAX_ECHO_LINES).min(n);
                shown.extend(start + 1..=hi);
                at.push(format!("{}", start + 1));
            }
            format!(
                "edit {}: {label} is ambiguous: it could mean line {} depending on which earlier read it was copied from; re-anchor using the current lines below",
                edit.index,
                at.join(" or ")
            )
        }
        Miss::Unseen { start, count } => {
            let hi = (start + count).min(start + MAX_ECHO_LINES);
            shown.extend(start + 1..=hi);
            format!(
                "edit {}: range {label} includes lines that were not shown; they are shown below — check them and retry with the same anchors",
                edit.index
            )
        }
        Miss::Changed | Miss::Mismatch => {
            let mut hints = Vec::new();
            for anchor in [&edit.start, &edit.end] {
                if anchor.line == 0 {
                    continue;
                }
                window(anchor.line, shown);
                let lo = anchor.line.saturating_sub(SUGGEST_RADIUS).max(1);
                let hi = (anchor.line + SUGGEST_RADIUS).min(n);
                let moved: Vec<usize> = (lo..=hi)
                    .filter(|line| {
                        *line != anchor.line && line_hash(&live_lines[line - 1]) == anchor.hash
                    })
                    .take(3)
                    .collect();
                for line in &moved {
                    shown.insert(*line);
                }
                if !moved.is_empty() {
                    let list: Vec<String> = moved
                        .iter()
                        .map(|line| format!("{line}#{}", anchor.hash))
                        .collect();
                    hints.push(format!("`{}` may now be {}", anchor.raw, list.join(" or ")));
                }
            }
            let reason = if matches!(miss, Miss::Changed) {
                "lines changed after they were read"
            } else {
                "anchor does not match the file"
            };
            let hint = if hints.is_empty() {
                String::new()
            } else {
                format!(" ({})", hints.join("; "))
            };
            format!("edit {}: {label}: {reason}{hint}", edit.index)
        }
    }
}

fn failure(
    path: &str,
    errors: Vec<String>,
    live_lines: &[String],
    shown: BTreeSet<usize>,
) -> Failure {
    let mut message = format!("no changes written to `{path}`.\n{}", errors.join("\n"));
    if !shown.is_empty() {
        message.push_str("\nCurrent lines (use these anchors):\n");
        message.push_str(&render_numbers(live_lines, &shown));
    }
    Failure {
        message: message.trim_end().to_string(),
        shown: shown.into_iter().collect(),
    }
}

fn render_numbers<S: AsRef<str>>(lines: &[S], numbers: &BTreeSet<usize>) -> String {
    let mut out = String::new();
    let mut previous: Option<usize> = None;
    for &line in numbers {
        if previous.is_some_and(|p| line > p + 1) {
            out.push_str("...\n");
        }
        out.push_str(&render_line(line, lines[line - 1].as_ref()));
        out.push('\n');
        previous = Some(line);
    }
    out
}

fn check_overlaps(path: &str, ops: &[Op]) -> Result<(), Failure> {
    let mut covered: Option<(usize, usize)> = None; // (end exclusive, edit index)
    let mut last_insert: Option<&Op> = None;
    for op in ops {
        if let Some((end, index)) = covered
            && op.start < end
        {
            return Err(Failure {
                message: format!(
                    "no changes written to `{path}`: edits {index} and {} overlap; merge them into one edit",
                    op.index
                ),
                shown: Vec::new(),
            });
        }
        if op.delete > 0 {
            covered = Some((op.start + op.delete, op.index));
        } else {
            if let Some(prev) = last_insert
                && prev.start == op.start
                && prev.lines == op.lines
            {
                return Err(Failure {
                    message: format!(
                        "no changes written to `{path}`: edits {} and {} insert the same text twice; remove the duplicate",
                        prev.index, op.index
                    ),
                    shown: Vec::new(),
                });
            }
            last_insert = Some(op);
        }
    }
    Ok(())
}

// ── applying ────────────────────────────────────────────────────────────────

struct Entry<'a> {
    content: &'a str,
    terminator: Option<&'a str>,
    seen: bool,
}

/// (new 0-based start, inserted count, deleted count)
type Region = (usize, usize, usize);

fn split_raw_lines(body: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                let end = if bytes.get(i + 1) == Some(&b'\n') {
                    i + 2
                } else {
                    i + 1
                };
                out.push((&body[start..i], &body[i..end]));
                start = end;
                i = end;
            }
            b'\n' => {
                out.push((&body[start..i], &body[i..i + 1]));
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    if start < body.len() {
        out.push((&body[start..], ""));
    }
    out
}

fn apply<'a>(
    live_raw: &'a str,
    live_lines: &[String],
    live_version: &Version,
    ops: &'a [Op],
) -> Result<(Vec<Entry<'a>>, Vec<Region>), String> {
    let body = live_raw.strip_prefix('\u{feff}').unwrap_or(live_raw);
    let raw_lines = split_raw_lines(body);
    if raw_lines.len() != live_lines.len() {
        return Err("internal error: raw and normalized line counts differ".to_string());
    }
    let seen = |index: usize| live_version.all_seen || live_version.seen.contains(&(index + 1));
    let original = |index: usize| {
        let (content, terminator) = raw_lines[index];
        Entry {
            content,
            terminator: (!terminator.is_empty()).then_some(terminator),
            seen: seen(index),
        }
    };

    let mut entries = Vec::with_capacity(raw_lines.len());
    let mut regions = Vec::with_capacity(ops.len());
    let mut cursor = 0usize;
    for op in ops {
        while cursor < op.start {
            entries.push(original(cursor));
            cursor += 1;
        }
        let new_start = entries.len();
        for line in &op.lines {
            entries.push(Entry {
                content: line,
                terminator: None,
                seen: true,
            });
        }
        regions.push((new_start, op.lines.len(), op.delete));
        cursor += op.delete;
    }
    while cursor < raw_lines.len() {
        entries.push(original(cursor));
        cursor += 1;
    }
    Ok((entries, regions))
}

fn render_raw(live_raw: &str, live: &str, entries: &[Entry]) -> String {
    let format = TextFormat::detect(live_raw);
    let newline = format.restore_newlines("\n");
    let trailing_newline = live.is_empty() || live.ends_with('\n');
    let mut out = String::with_capacity(live_raw.len() + 64);
    if format.has_bom {
        out.push('\u{feff}');
    }
    let last = entries.len().saturating_sub(1);
    for (i, entry) in entries.iter().enumerate() {
        out.push_str(entry.content);
        if i < last || trailing_newline {
            out.push_str(entry.terminator.unwrap_or(&newline));
        }
    }
    out
}

fn render_output(
    path: &str,
    entries: &[Entry],
    regions: &[Region],
    ops: &[Op],
) -> (String, BTreeSet<usize>) {
    let deleted: usize = ops.iter().map(|op| op.delete).sum();
    let inserted: usize = ops.iter().map(|op| op.lines.len()).sum();
    let n = entries.len();
    let mut numbers = BTreeSet::new();
    for &(start, count, _) in regions {
        let lo = start.saturating_sub(1);
        let hi = (start + count + 1).min(n);
        numbers.extend((lo..hi).map(|i| i + 1));
    }
    let lines: Vec<&str> = entries.iter().map(|entry| entry.content).collect();
    let body = render_numbers(&lines, &numbers);
    let text = format!("Edited: {path} (-{deleted} +{inserted})\n{body}");
    let text = crate::tools::shell::truncate_output(text.trim_end(), 200);
    (text.trim_end().to_string(), numbers)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_two_alphabet_chars_and_stable() {
        let h = line_hash("fn main() {");
        assert_eq!(h.len(), 2);
        assert!(h.bytes().all(|b| ALPHABET.contains(&b)));
        assert_eq!(h, line_hash("fn main() {"));
        assert_eq!(render_line(3, "x"), format!("3#{}|x", line_hash("x")));
    }

    #[test]
    fn anchor_parsing_tolerates_copied_content() {
        let a = parse_anchor(" 12#K3|    let x = 1;", false).unwrap();
        assert_eq!((a.line, a.hash.as_str()), (12, "k3"));
        assert!(parse_anchor("12", false).is_err());
        assert!(parse_anchor("0", false).is_err());
        assert_eq!(parse_anchor("0", true).unwrap().line, 0);
        assert!(parse_anchor("12#il", false).is_err());
    }

    #[test]
    fn split_text_strips_copied_prefixes_only_when_all_lines_have_them() {
        assert_eq!(split_text(""), Vec::<String>::new());
        assert_eq!(split_text("\n"), vec![String::new()]);
        assert_eq!(split_text("a\nb\n"), vec!["a", "b"]);
        assert_eq!(split_text("1#aa|x\n2#bb|y"), vec!["x", "y"]);
        assert_eq!(split_text("1#aa|x\ny"), vec!["1#aa|x", "y"]);
    }

    #[test]
    fn raw_line_split_matches_normalized_lines() {
        for text in ["", "a", "a\n", "a\r\nb", "a\rb\r", "\n\n", "x\r\n\r\ny\n"] {
            let normalized = snapshot::normalize_text(text);
            assert_eq!(
                split_raw_lines(text).len(),
                snapshot::numbered_lines(&normalized).len(),
                "{text:?}"
            );
        }
    }
}
