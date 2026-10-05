//! `edit_file`: edits placed by the `LINE#HASH|content` anchors `read_file`
//! shows, applied all together or not at all.
//!
//! An anchor is checked against what the model saw, not just the file as it
//! is: the latest read or edit of the file and a few earlier versions
//! ([`Views`](super::hashline::Views)), each mapped to the live file. So
//! anchors stay good after the model's own edits and after unrelated
//! changes, and lines changed since they were read are refused.
//!
//! Models copy a line's text far more reliably than its hash, so an anchor
//! may carry the text after `|`. The text finds a line when its number is off,
//! while a valid hash must still match. Everything else fails with the reason per edit
//! and the current lines, with fresh anchors, to retry from.

use std::collections::BTreeSet;
use std::path::Path;

use futures_util::future::BoxFuture;
use serde_json::{Map, Value, json};

use super::hashline::{Lines, is_hash, line_hash, line_map, render_line};
use super::{Tool, ToolContext, ToolResult, ToolSpec};

/// When an anchor's text appears more than once, a line this close to its
/// number is still taken if it is the only one.
const NEAR: usize = 20;
/// Lines shown on each side of an anchor that failed.
const CONTEXT: usize = 2;
/// Cap on lines echoed for one failed edit.
const MAX_SHOWN: usize = 80;
/// Cap on changed lines echoed after a successful edit.
const MAX_OUTPUT: usize = 120;

pub struct EditFile(ToolSpec);

impl EditFile {
    pub fn new() -> Self {
        let anchor = |what: &str| json!({ "type": "string", "description": what });
        EditFile(ToolSpec {
            name: "edit_file".into(),
            description: "Edit a file by the LINE#HASH anchors read_file shows, instead of \
                          retyping old text. Use {at, text} to replace, {at, end, text} for a \
                          range, or {after, text}/{before, text} to insert (after \"0\" = top). \
                          Include the whole anchor such as `12#k3|    let x = 1;` when possible; \
                          text after | can recover a copied line number, but a valid hash must \
                          still match. New content is \
                          separated by newlines; an empty string deletes. Edits are atomic."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "description": "Applied together, or not at all.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "at": anchor("Line to replace, from read_file."),
                                "end": anchor("Last line replaced, inclusive (with `at`)."),
                                "after": anchor("Insert after this line; \"0\" inserts at the top."),
                                "before": anchor("Insert before this line."),
                                "position": { "type": "string", "enum": ["replace", "after", "before"], "default": "replace" },
                                "text": { "type": "string", "description": "New lines, without anchors. \"\" deletes." }
                            },
                            "required": ["text"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["path", "edits"],
                "additionalProperties": false
            }),
        })
    }
}

impl Tool for EditFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let last = ctx.views.last_path(&ctx.session_id);
            match parse(args, last.as_deref())? {
                Request::Anchored { path, edits } => edit(ctx, &path, &edits).await,
                Request::Replace {
                    path,
                    old,
                    new,
                    all,
                } => replace(ctx, &path, &old, &new, all).await,
            }
        })
    }
}

// ── arguments ───────────────────────────────────────────────────────────────

#[derive(Debug)]
enum Request {
    Anchored {
        path: String,
        edits: Vec<Edit>,
    },
    /// `old_string`/`new_string`, as models used to other tools send.
    Replace {
        path: String,
        old: String,
        new: String,
        all: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Replace,
    After,
    Before,
}

#[derive(Debug, Clone)]
struct Anchor {
    /// 1-based; 0 only for `after: "0"`.
    line: usize,
    hash: Option<String>,
    text: Option<String>,
    raw: String,
}

#[derive(Debug)]
struct Edit {
    index: usize,
    kind: Kind,
    start: Anchor,
    end: Option<Anchor>,
    lines: Vec<String>,
}

impl Edit {
    fn anchors(&self) -> impl Iterator<Item = &Anchor> {
        std::iter::once(&self.start).chain(self.end.as_ref())
    }

    fn label(&self) -> String {
        match &self.end {
            Some(end) => format!("`{}`..`{}`", self.start.raw, end.raw),
            None => format!("`{}`", self.start.raw),
        }
    }
}

const SHAPE: &str = "edit_file expects {path, edits: [{at, position?, end?, text}]}";
const EDIT_KEYS: [&str; 6] = ["at", "end", "position", "after", "before", "text"];

fn parse(args: Value, last: Option<&Path>) -> Result<Request, String> {
    let Value::Object(mut args) = args else {
        return Err(format!("{SHAPE}; got {}", kind_of(&args)));
    };
    let path = match ["path", "file_path", "file"]
        .iter()
        .find_map(|k| args.remove(*k))
    {
        Some(Value::String(p)) if !p.trim().is_empty() => p,
        Some(_) => return Err("`path` must be a non-empty string".into()),
        None => {
            let hint = last
                .map(|p| format!(" (the last file you read or edited is {})", p.display()))
                .unwrap_or_default();
            return Err(format!(
                "edit_file is missing `path`{hint}; send it again with `path` before `edits`"
            ));
        }
    };

    if let Some(old) = args.remove("old_string") {
        let text = |v: Option<Value>, name: &str| match v {
            Some(Value::String(s)) => Ok(s),
            _ => Err(format!("`{name}` must be a string")),
        };
        return Ok(Request::Replace {
            path,
            old: text(Some(old), "old_string")?,
            new: text(args.remove("new_string"), "new_string")?,
            all: args
                .remove("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }

    let top: Map<String, Value> = EDIT_KEYS
        .iter()
        .filter_map(|k| args.remove(*k).map(|v| (k.to_string(), v)))
        .collect();
    let edits = args.remove("edits");
    if !args.is_empty() {
        let keys: Vec<_> = args.keys().map(|k| format!("`{k}`")).collect();
        return Err(format!("{SHAPE}; unknown {}", keys.join(", ")));
    }
    let specs = match edits {
        Some(_) if !top.is_empty() => {
            return Err("give the edits either in `edits` or at the top level, not both".into());
        }
        // A list sent as a JSON string.
        Some(Value::String(s)) => match serde_json::from_str(&s) {
            Ok(Value::Array(a)) if !a.is_empty() => a,
            Ok(Value::Object(o)) => vec![Value::Object(o)],
            _ => return Err("`edits` must be a list of edits, not a string".into()),
        },
        Some(Value::Array(a)) if a.is_empty() => return Err("`edits` is empty".into()),
        Some(Value::Array(a)) => a,
        Some(Value::Object(o)) => vec![Value::Object(o)],
        Some(other) => return Err(format!("`edits` must be a list, not {}", kind_of(&other))),
        None if top.is_empty() => return Err(format!("{SHAPE}; `edits` is missing")),
        None => vec![Value::Object(top)],
    };
    let edits = specs
        .into_iter()
        .enumerate()
        .map(|(i, spec)| parse_edit(i + 1, spec).map_err(|e| format!("edit {}: {e}", i + 1)))
        .collect::<Result<_, _>>()?;
    Ok(Request::Anchored { path, edits })
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

fn parse_edit(index: usize, spec: Value) -> Result<Edit, String> {
    let Value::Object(mut spec) = spec else {
        return Err(format!("must be an object, not {}", kind_of(&spec)));
    };
    if let Some(k) = spec.keys().find(|k| !EDIT_KEYS.contains(&k.as_str())) {
        return Err(format!("unknown field `{k}`; use at, position, end, text"));
    }
    let mut take = |k: &str| spec.remove(k).filter(|v| !v.is_null());
    let (at, end, position, after, before) = (
        take("at"),
        take("end"),
        take("position"),
        take("after"),
        take("before"),
    );
    let text = take("text");
    let position = position
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("`position` must be a string")
        })
        .transpose()?;
    let (kind, start) = match (at, position.as_deref(), after, before) {
        (Some(a), None | Some("replace"), None, None) => (Kind::Replace, a),
        (Some(a), Some("after"), None, None) => (Kind::After, a),
        (Some(a), Some("before"), None, None) => (Kind::Before, a),
        (None, None, Some(a), None) => (Kind::After, a),
        (None, None, None, Some(a)) => (Kind::Before, a),
        _ => {
            return Err(
                "set exactly one position: `at` with optional position=replace|after|before".into(),
            );
        }
    };
    if end.is_some() && kind != Kind::Replace {
        return Err("`end` only goes with `at`".into());
    }
    let start = parse_anchor(&start, kind == Kind::After)?;
    let end = end.map(|e| parse_anchor(&e, false)).transpose()?;
    if let Some(e) = &end
        && e.line < start.line
    {
        return Err(format!("`end` {} is before `at` {}", e.raw, start.raw));
    }
    let lines = match text {
        Some(Value::String(s)) => split_text(&s)?,
        Some(Value::Array(a)) => a
            .iter()
            .map(|l| l.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or("`text` must be a string")?,
        Some(_) => return Err("`text` must be a string".into()),
        None => return Err("`text` is missing (\"\" deletes)".into()),
    };
    if kind != Kind::Replace && lines.is_empty() {
        return Err("an insert needs non-empty `text`".into());
    }
    Ok(Edit {
        index,
        kind,
        start,
        end,
        lines,
    })
}

fn parse_anchor(v: &Value, allow_top: bool) -> Result<Anchor, String> {
    let raw = match v {
        Value::String(s) => s.trim().to_owned(),
        Value::Number(n) => n.to_string(),
        other => return Err(format!("an anchor is a string, not {}", kind_of(other))),
    };
    if allow_top && raw == "0" {
        return Ok(Anchor {
            line: 0,
            hash: None,
            text: None,
            raw,
        });
    }
    let (head, text) = match raw.split_once('|') {
        Some((h, t)) => (
            h.trim(),
            Some(t.trim_end_matches(" [line truncated]").to_owned()),
        ),
        None => (raw.as_str(), None),
    };
    let head = head.strip_prefix('+').unwrap_or(head);
    let (number, hash) = match head.split_once('#') {
        Some((n, h)) => (n.trim(), Some(h.trim().to_ascii_lowercase())),
        None => (head, None),
    };
    let line = number.parse::<usize>().ok().filter(|n| *n > 0);
    // A malformed hash is no help when the text is there to go by.
    let hash = hash.filter(|h| is_hash(h) || text.is_none());
    let (Some(line), true) = (line, hash.as_deref().is_none_or(is_hash)) else {
        return Err(format!(
            "anchor `{raw}` must be a line as read_file shows it, e.g. `12#k3|    let x = 1;`"
        ));
    };
    if hash.is_none() && text.is_none() {
        return Err(format!(
            "anchor `{raw}` needs the #HASH or the |text read_file showed for line {line}"
        ));
    }
    let shown = match &text {
        Some(t) if t.chars().count() > 40 => {
            let cut: String = t.chars().take(40).collect();
            format!("{head}|{cut}…")
        }
        Some(t) => format!("{head}|{t}"),
        None => head.to_owned(),
    };
    Ok(Anchor {
        line,
        hash,
        text,
        raw: shown,
    })
}

/// New lines from `text`: one trailing newline is dropped, and anchors
/// copied in front of every line are removed.
fn split_text(text: &str) -> Result<Vec<String>, String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let body = text.strip_suffix('\n').unwrap_or(&text);
    let lines: Vec<&str> = body.split('\n').collect();
    let anchors: Vec<_> = lines.iter().map(|l| strip_anchor(l)).collect();
    match (
        anchors.iter().any(Option::is_some),
        anchors.into_iter().collect::<Option<Vec<_>>>(),
    ) {
        (false, None) => Ok(lines.into_iter().map(str::to_owned).collect()),
        (true, Some(stripped)) => Ok(stripped.into_iter().map(str::to_owned).collect()),
        (true, None) => Err(
            "text mixes read_file anchors with unanchored lines; send only the new file content"
                .into(),
        ),
        (false, Some(_)) => unreachable!(),
    }
}

fn strip_anchor(line: &str) -> Option<&str> {
    let line = line.strip_prefix('+').unwrap_or(line);
    let (number, rest) = line.split_once('#')?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hash, rest) = rest.split_at_checked(2)?;
    is_hash(hash).then_some(())?;
    rest.strip_prefix('|')
}

// ── placing ─────────────────────────────────────────────────────────────────

/// Whitespace-insensitive form of a line, for comparing copied text.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether copied `text` is the line `line`. A long copy may stop short,
/// as read_file cuts very long lines.
fn same(text: &str, line: &str) -> bool {
    let (t, l) = (norm(text), norm(line));
    t == l || (t.chars().count() >= 40 && l.starts_with(&t))
}

fn same_exact(text: &str, line: &str) -> bool {
    text == line || (text.chars().count() >= 40 && line.starts_with(text))
}

fn similarity(a: &str, b: &str) -> f32 {
    similar::TextDiff::from_chars(&norm(a), &norm(b)).ratio()
}

/// Where an anchor is in one version's lines (0-based).
#[derive(Debug, Clone)]
enum Found {
    At { index: usize, moved: bool },
    Ambiguous(Vec<usize>),
    Missing,
}

fn find(lines: &[String], a: &Anchor) -> Found {
    let at = a.line.checked_sub(1).and_then(|i| lines.get(i));
    let hash_ok = |l: &String| a.hash.as_deref() == Some(line_hash(l).as_str());
    let here = Found::At {
        index: a.line.saturating_sub(1),
        moved: false,
    };
    let Some(text) = &a.text else {
        return if at.is_some_and(hash_ok) {
            here
        } else {
            Found::Missing
        };
    };
    let matches = |l: &String| {
        (a.hash.is_some() && same(text, l) || a.hash.is_none() && same_exact(text, l))
            && (a.hash.is_none() || hash_ok(l))
    };
    if at.is_some_and(matches) {
        return here;
    }
    let all: Vec<usize> = (0..lines.len())
        .filter(|&i| {
            matches(&lines[i])
                || (a.hash.is_none() && i != a.line.saturating_sub(1) && same(text, &lines[i]))
        })
        .collect();
    let near: Vec<usize> = all
        .iter()
        .copied()
        .filter(|&i| i.abs_diff(a.line - 1) <= NEAR)
        .collect();
    match (all.as_slice(), near.as_slice()) {
        ([i], _) | (_, [i]) => Found::At {
            index: *i,
            moved: true,
        },
        // The right line, with its text copied a little wrong.
        ([], _) if at.is_some_and(|l| hash_ok(l) && similarity(text, l) >= 0.7) => here,
        ([], _) => Found::Missing,
        (_, []) => Found::Ambiguous(all),
        _ => Found::Ambiguous(near),
    }
}

/// A version of the file anchors may come from, mapped onto the live file.
struct Ver {
    lines: Vec<String>,
    /// What was shown of it; `None` means all of it.
    seen: Option<BTreeSet<usize>>,
    /// Version line → live line, where unchanged; `None` for the live file.
    map: Option<Vec<Option<usize>>>,
}

impl Ver {
    fn live_index(&self, i: usize) -> Option<usize> {
        match &self.map {
            None => Some(i),
            Some(m) => m.get(i).copied().flatten(),
        }
    }
}

/// An edit placed on the live file: replace `delete` lines at `start`.
#[derive(Debug)]
struct Op {
    index: usize,
    start: usize,
    delete: usize,
    lines: Vec<String>,
}

enum Miss {
    Missing,
    Changed,
    Ambiguous(Vec<usize>),
    /// Live lines inside the range the model never saw.
    Unseen(usize, usize),
    /// The edit's text is already there.
    Applied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hit {
    start: usize,
    count: usize,
}

/// What each version says about one edit.
#[derive(Default)]
struct Tally {
    /// Placed by number and hash or text, with whether lines were unseen.
    exact: Vec<(Hit, bool)>,
    /// Placed by text alone, at another number.
    moved: Vec<(Hit, bool)>,
    ambiguous: BTreeSet<usize>,
    changed: bool,
}

impl Tally {
    fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.moved.is_empty() && self.ambiguous.is_empty() && !self.changed
    }

    fn scan(&mut self, v: &Ver, e: &Edit) {
        let s = find(&v.lines, &e.start);
        let t = match &e.end {
            Some(end) => find(&v.lines, end),
            None => s.clone(),
        };
        let (s, t, moved) = match (s, t) {
            (
                Found::At {
                    index: s,
                    moved: m1,
                },
                Found::At {
                    index: t,
                    moved: m2,
                },
            ) if t >= s => (s, t, m1 || m2),
            (Found::Ambiguous(c), _) | (_, Found::Ambiguous(c)) => {
                self.ambiguous
                    .extend(c.iter().filter_map(|&i| v.live_index(i)));
                return;
            }
            _ => return,
        };
        let Some(start) = v.live_index(s) else {
            self.changed = true;
            return;
        };
        if (s..=t).any(|i| v.live_index(i) != Some(start + i - s)) {
            self.changed = true;
            return;
        }
        // Inner lines of a range must have been shown; its ends were copied.
        let unseen = v
            .seen
            .as_ref()
            .is_some_and(|seen| (s + 2..=t).any(|n| !seen.contains(&n)));
        let hit = Hit {
            start,
            count: t - s + 1,
        };
        let list = if moved {
            &mut self.moved
        } else {
            &mut self.exact
        };
        match list.iter_mut().find(|(h, _)| *h == hit) {
            // Seen in any version that places it there is enough.
            Some((_, u)) => *u &= unseen,
            None => list.push((hit, unseen)),
        }
    }
}

/// Place one edit on the live file, and say whether its text had to find
/// a line its number missed.
fn place(vers: &[Ver], fallback: Option<&Ver>, e: &Edit) -> Result<(Hit, bool), Miss> {
    if e.start.line == 0 {
        return Ok((Hit { start: 0, count: 0 }, false));
    }
    let mut t = Tally::default();
    for v in vers {
        t.scan(v, e);
    }
    if t.is_empty()
        && let Some(f) = fallback
    {
        t.scan(f, e);
    }
    let (hits, moved) = if t.exact.is_empty() {
        (t.moved, true)
    } else {
        (t.exact, false)
    };
    match hits.as_slice() {
        [(hit, false)] => Ok((*hit, moved)),
        [(hit, true)] => Err(Miss::Unseen(hit.start, hit.count)),
        [] if !t.ambiguous.is_empty() => Err(Miss::Ambiguous(t.ambiguous.into_iter().collect())),
        [] if t.changed => Err(Miss::Changed),
        [] => Err(Miss::Missing),
        many => Err(Miss::Ambiguous(many.iter().map(|(h, _)| h.start).collect())),
    }
}

/// Whether an edit that could not be placed is already in the file: its
/// new text sits near where it was aimed. Answers a retried edit whose
/// first try went through.
fn applied(live: &[String], e: &Edit) -> bool {
    let n = e.lines.len();
    let weight: usize = e.lines.iter().map(|l| l.trim().len()).sum();
    if n == 0 || weight < 12 || n > live.len() {
        return false;
    }
    let aim = e.start.line.saturating_sub(1);
    let lo = aim.saturating_sub(NEAR + n);
    let hi = (aim + NEAR + n).min(live.len() - n);
    (lo..=hi).any(|i| {
        live[i..i + n]
            .iter()
            .zip(&e.lines)
            .all(|(a, b)| a.trim_end() == b.trim_end())
    })
}

fn to_op(e: &Edit, hit: Hit) -> Op {
    let (start, delete) = match e.kind {
        Kind::Replace => (hit.start, hit.count),
        Kind::After if e.start.line == 0 => (0, 0),
        Kind::After => (hit.start + 1, 0),
        Kind::Before => (hit.start, 0),
    };
    Op {
        index: e.index,
        start,
        delete,
        lines: e.lines.clone(),
    }
}

fn check_overlaps(ops: &[Op]) -> Result<(), String> {
    let mut covered: Option<(usize, usize)> = None; // (end, edit)
    let mut last_insert: Option<&Op> = None;
    for op in ops {
        if let Some((end, index)) = covered
            && op.start < end
        {
            return Err(format!(
                "edits {index} and {} overlap; merge them into one edit",
                op.index
            ));
        }
        if op.delete > 0 {
            covered = Some((op.start + op.delete, op.index));
        } else {
            if let Some(prev) = last_insert
                && prev.start == op.start
                && prev.lines == op.lines
            {
                return Err(format!(
                    "edits {} and {} insert the same text at the same place",
                    prev.index, op.index
                ));
            }
            last_insert = Some(op);
        }
    }
    Ok(())
}

// ── editing ─────────────────────────────────────────────────────────────────

async fn read_text(path: &Path) -> Result<String, String> {
    match tokio::fs::read(path).await {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| format!("{} is not UTF-8 text; edit it another way", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "{} does not exist; create it with write_file",
            path.display()
        )),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Write `text` over `path` if it still holds `before`, through a temporary
/// file so a crash never leaves half a file.
async fn commit(path: &Path, before: &str, text: &str) -> Result<(), String> {
    let target = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_owned());
    if tokio::fs::read(&target).await.ok().as_deref() != Some(before.as_bytes()) {
        return Err(format!(
            "{} changed while the edit was being made; nothing was written. Try again.",
            path.display()
        ));
    }
    let perms = tokio::fs::metadata(&target)
        .await
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?
        .permissions();
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.{}.bone-edit", uuid::Uuid::now_v7()));
    let write = async {
        tokio::fs::write(&tmp, text).await?;
        tokio::fs::set_permissions(&tmp, perms).await?;
        tokio::fs::rename(&tmp, &target).await
    };
    if let Err(e) = write.await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

fn validate_output(path: &Path, text: &str) -> Result<(), String> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => syn::parse_file(text)
            .map(|_| ())
            .map_err(|e| format!("{} would be invalid Rust: {e}", path.display())),
        Some("json") => serde_json::from_str::<Value>(text)
            .map(|_| ())
            .map_err(|e| format!("{} would be invalid JSON: {e}", path.display())),
        _ => Ok(()),
    }
}

/// Add the lines around `center` (1-based) of a `len`-line file.
fn window(len: usize, center: usize, shown: &mut BTreeSet<usize>) {
    if len == 0 {
        return;
    }
    let c = center.clamp(1, len);
    shown.extend(c.saturating_sub(CONTEXT).max(1)..=(c + CONTEXT).min(len));
}

/// Lines by number with anchors, `...` between gaps.
fn show(lines: &[String], numbers: &BTreeSet<usize>) -> String {
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for &n in numbers {
        if prev.is_some_and(|p| n > p + 1) {
            out.push_str("...\n");
        }
        out.push_str(&render_line(n, &lines[n - 1]));
        out.push('\n');
        prev = Some(n);
    }
    out
}

/// Why an edit could not be placed, adding the live lines worth showing.
fn explain(e: &Edit, miss: &Miss, live: &[String], shown: &mut BTreeSet<usize>) -> String {
    let n = live.len();
    let what = format!("edit {} {}", e.index, e.label());
    match miss {
        Miss::Applied => format!("{what}: already in the file"),
        Miss::Unseen(start, count) => {
            let hi = (start + count).min(start + MAX_SHOWN);
            shown.extend(start + 1..=hi);
            format!(
                "{what}: lines {}-{} were never shown to you; they are below. Check them, then send the same edit again.",
                start + 1,
                start + count
            )
        }
        Miss::Ambiguous(spots) => {
            for &s in spots.iter().take(5) {
                window(n, s + 1, shown);
            }
            let at: Vec<String> = spots.iter().take(5).map(|s| (s + 1).to_string()).collect();
            format!(
                "{what}: could be line {}; anchor on one of the lines below instead",
                at.join(" or ")
            )
        }
        Miss::Changed => {
            for a in e.anchors() {
                window(n, a.line, shown);
            }
            format!("{what}: those lines changed since you read them; use the current lines below")
        }
        Miss::Missing => {
            let mut why = Vec::new();
            for a in e.anchors() {
                if !matches!(find(live, a), Found::Missing) {
                    continue;
                }
                window(n, a.line, shown);
                let current = a.line.checked_sub(1).and_then(|i| live.get(i));
                match (&a.text, current) {
                    (Some(text), _) => {
                        let lo = a.line.saturating_sub(NEAR * 5).max(1);
                        let hi = (a.line + NEAR * 5).min(n);
                        let best = (lo..=hi)
                            .map(|i| (i, similarity(text, &live[i - 1])))
                            .filter(|(_, r)| *r >= 0.6)
                            .max_by(|a, b| a.1.total_cmp(&b.1));
                        why.push(match best {
                            Some((i, _)) => {
                                window(n, i, shown);
                                format!(
                                    "the text of `{}` is not in the file; the closest line is {}",
                                    a.raw,
                                    render_line(i, &live[i - 1])
                                )
                            }
                            None => format!(
                                "the text of `{}` is not in the file near line {}",
                                a.raw, a.line
                            ),
                        });
                    }
                    (None, None) => why.push(format!("the file has only {n} lines")),
                    (None, Some(line)) => {
                        let hash = a.hash.as_deref().unwrap_or_default();
                        let lo = a.line.saturating_sub(NEAR).max(1);
                        let hi = (a.line + NEAR).min(n);
                        let elsewhere: Vec<String> = (lo..=hi)
                            .filter(|&i| line_hash(&live[i - 1]) == hash)
                            .map(|i| i.to_string())
                            .collect();
                        let also = if elsewhere.is_empty() {
                            String::new()
                        } else {
                            format!(" (#{hash} is on line {})", elsewhere.join(", "))
                        };
                        why.push(format!(
                            "line {} is now {}, not #{hash}{also}",
                            a.line,
                            render_line(a.line, line)
                        ));
                    }
                }
            }
            if why.is_empty() {
                why.push("does not match the lines you read".into());
            }
            format!("{what}: {}", why.join("; "))
        }
    }
}

async fn edit(ctx: &ToolContext, path_arg: &str, edits: &[Edit]) -> ToolResult {
    let path = ctx.resolve(path_arg);
    let before = read_text(&path).await?;
    let live = Lines::parse(&before);
    let (head, history) = ctx.views.get(&ctx.session_id, &path);

    // Anchors refer to what the model saw: the live file when that is what
    // it last saw; when the file changed since, the live file is only a
    // last resort.
    let stale = head.as_ref().is_some_and(|h| h.lines != live.lines);
    let mut vers: Vec<Ver> = Vec::new();
    let live_seen: BTreeSet<usize> = match &head {
        None => (1..=live.lines.len()).collect(),
        Some(h) if !stale => h.seen.clone(),
        Some(h) => {
            let map = line_map(&h.lines, &live.lines);
            let seen = h
                .seen
                .iter()
                .filter_map(|&n| map.get(n - 1).copied().flatten().map(|i| i + 1))
                .collect();
            vers.push(Ver {
                lines: h.lines.clone(),
                seen: Some(h.seen.clone()),
                map: Some(map),
            });
            seen
        }
    };
    let live_ver = Ver {
        lines: live.lines.clone(),
        seen: head.is_some().then(|| live_seen.clone()),
        map: None,
    };
    let fallback = if stale {
        Some(live_ver)
    } else {
        vers.push(live_ver);
        None
    };
    for v in history {
        let could = edits
            .iter()
            .flat_map(Edit::anchors)
            .any(|a| a.line > 0 && !matches!(find(&v.lines, a), Found::Missing));
        if could {
            vers.push(Ver {
                map: Some(line_map(&v.lines, &live.lines)),
                lines: v.lines,
                seen: Some(v.seen),
            });
        }
    }

    let mut ops = Vec::new();
    let mut misses: Vec<(&Edit, Miss)> = Vec::new();
    let mut notes = Vec::new();
    for e in edits {
        match place(&vers, fallback.as_ref(), e) {
            Ok((hit, moved)) => {
                if moved {
                    notes.push(format!(
                        "edit {}: found {} by its text at line {}",
                        e.index,
                        e.label(),
                        hit.start + 1
                    ));
                }
                ops.push(to_op(e, hit));
            }
            Err(Miss::Missing | Miss::Changed) if applied(&live.lines, e) => {
                misses.push((e, Miss::Applied));
            }
            Err(miss) => misses.push((e, miss)),
        }
    }

    let display = path.display();
    let failed = misses
        .iter()
        .filter(|(_, m)| !matches!(m, Miss::Applied))
        .count();
    if failed > 0 {
        let mut shown = BTreeSet::new();
        let reasons: Vec<String> = misses
            .iter()
            .map(|(e, m)| explain(e, m, &live.lines, &mut shown))
            .collect();
        let mut msg = format!(
            "No changes written to {display}: {failed} of {} edits could not be placed.\n{}",
            edits.len(),
            reasons.join("\n")
        );
        if !ops.is_empty() {
            msg.push_str(&format!(
                "\nThe other {} placed fine; send them again with the fixed ones.",
                ops.len()
            ));
        }
        let hashes_only = misses
            .iter()
            .any(|(e, m)| matches!(m, Miss::Missing) && e.anchors().any(|a| a.text.is_none()));
        if hashes_only {
            msg.push_str(
                "\nTip: write anchors as the whole line, e.g. `12#k3|    let x = 1;`, so the text can find the line.",
            );
        }
        if !shown.is_empty() {
            msg.push_str("\nCurrent lines:\n");
            msg.push_str(&show(&live.lines, &shown));
            // What was shown now counts as seen, so the retry can use it.
            let mut seen = live_seen;
            seen.extend(&shown);
            ctx.views.record(&ctx.session_id, &path, &live.lines, seen);
        }
        return Err(msg.trim_end().to_owned());
    }
    for (e, _) in &misses {
        notes.push(format!(
            "edit {}: {} is already in the file; skipped",
            e.index,
            e.label()
        ));
    }

    ops.sort_by_key(|op| (op.start, usize::from(op.delete > 0), op.index));
    check_overlaps(&ops).map_err(|e| format!("No changes written to {display}: {e}"))?;

    // Apply, keeping track of what the model has seen and where the
    // changes landed.
    let newline = live.newline();
    let mut out = Lines {
        bom: live.bom,
        lines: Vec::new(),
        ends: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut regions = Vec::new();
    let mut cursor = 0;
    let keep = |i: usize, out: &mut Lines, seen: &mut BTreeSet<usize>| {
        out.lines.push(live.lines[i].clone());
        out.ends.push(live.ends[i]);
        if live_seen.contains(&(i + 1)) {
            seen.insert(out.lines.len());
        }
    };
    for op in &ops {
        while cursor < op.start {
            keep(cursor, &mut out, &mut seen);
            cursor += 1;
        }
        regions.push(Region {
            start: out.lines.len(),
            added: op.lines.len(),
            removed: &live.lines[op.start..op.start + op.delete],
        });
        for l in &op.lines {
            out.lines.push(l.clone());
            out.ends.push(newline);
            seen.insert(out.lines.len());
        }
        cursor += op.delete;
    }
    while cursor < live.lines.len() {
        keep(cursor, &mut out, &mut seen);
        cursor += 1;
    }
    // Every line but the last ends with a newline; the last one does if
    // the file's did.
    let final_newline = live.ends.last().is_none_or(|e| !e.is_empty());
    let last = out.lines.len().saturating_sub(1);
    for (i, end) in out.ends.iter_mut().enumerate() {
        if i < last && end.is_empty() {
            *end = newline;
        } else if i == last {
            *end = match (final_newline, *end) {
                (false, _) => "",
                (true, "") => newline,
                (true, e) => e,
            };
        }
    }

    let text = out.render();
    if text == before {
        let mut msg = format!("No change: {display} already has this content.");
        for note in &notes {
            msg.push('\n');
            msg.push_str(note);
        }
        return Ok(msg);
    }
    validate_output(&path, &text).map_err(|e| format!("No changes written to {display}: {e}"))?;
    commit(&path, &before, &text).await?;

    let rows = diff_rows(&out.lines, &regions);
    seen.extend(rows.iter().filter_map(|r| r.line));
    ctx.views.record(&ctx.session_id, &path, &out.lines, seen);

    let deleted: usize = ops.iter().map(|o| o.delete).sum();
    let inserted: usize = ops.iter().map(|o| o.lines.len()).sum();
    let mut msg = format!("Edited {display} (-{deleted} +{inserted})\n");
    for note in &notes {
        msg.push_str(note);
        msg.push('\n');
    }
    for row in rows.iter().take(MAX_OUTPUT) {
        msg.push_str(&row.text);
        msg.push('\n');
    }
    if rows.len() > MAX_OUTPUT {
        msg.push_str(&format!(
            "[{} more lines of changes; read_file to see them]\n",
            rows.len() - MAX_OUTPUT
        ));
    }
    Ok(msg.trim_end().to_owned())
}

/// Where one edit landed in the new file.
struct Region<'a> {
    start: usize,
    added: usize,
    removed: &'a [String],
}

struct Row {
    text: String,
    /// The new file's line it shows, if it is one.
    line: Option<usize>,
}

/// The changes as a diff: `-text` for removed lines, `+LINE#HASH|text` for
/// added ones, and `LINE#HASH|text` around them, `...` between.
fn diff_rows(lines: &[String], regions: &[Region]) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut next = 0; // first new-file line not shown yet
    let keep = |i: usize, rows: &mut Vec<Row>, next: &mut usize, sign: &str| {
        if i < *next || i >= lines.len() {
            return;
        }
        if i > *next && !rows.is_empty() {
            rows.push(Row {
                text: "...".into(),
                line: None,
            });
        }
        rows.push(Row {
            text: format!("{sign}{}", render_line(i + 1, &lines[i])),
            line: Some(i + 1),
        });
        *next = i + 1;
    };
    for r in regions {
        if r.start > 0 {
            keep(r.start - 1, &mut rows, &mut next, "");
        }
        for old in r.removed {
            rows.push(Row {
                text: format!("-{old}"),
                line: None,
            });
        }
        for i in r.start..r.start + r.added {
            keep(i, &mut rows, &mut next, "+");
        }
        keep(r.start + r.added, &mut rows, &mut next, "");
    }
    rows
}

/// `old_string` → `new_string`: exactly, or line by line ignoring
/// indentation when that is unique.
async fn replace(ctx: &ToolContext, path_arg: &str, old: &str, new: &str, all: bool) -> ToolResult {
    let path = ctx.resolve(path_arg);
    let display = path.display();
    if old.is_empty() {
        return Err("old_string is empty; use write_file to create files".into());
    }
    if old == new {
        return Err("old_string and new_string are the same".into());
    }
    let before = read_text(&path).await?;
    let count = before.matches(old).count();
    let text = match count {
        0 => fuzzy_replace(&before, old, new).map_err(|e| format!("{e} in {display}"))?,
        n if n > 1 && !all => {
            return Err(format!(
                "old_string matches {n} places in {display}; include more context to make it unique, or set replace_all"
            ));
        }
        _ => before.replace(old, new),
    };
    validate_output(&path, &text).map_err(|e| format!("No changes written to {display}: {e}"))?;
    commit(&path, &before, &text).await?;
    let lines = Lines::parse(&text).lines;
    let seen = (1..=lines.len()).collect();
    ctx.views.record(&ctx.session_id, &path, &lines, seen);
    let count = count.max(1);
    let s = if count == 1 { "" } else { "s" };
    Ok(format!("Replaced {count} occurrence{s} in {display}"))
}

fn fuzzy_replace(text: &str, old: &str, new: &str) -> Result<String, String> {
    let file = Lines::parse(text);
    let want: Vec<&str> = old
        .trim_end_matches('\n')
        .split('\n')
        .map(str::trim)
        .collect();
    let n = want.len();
    let hits: Vec<usize> = if n > file.lines.len() {
        Vec::new()
    } else {
        (0..=file.lines.len() - n)
            .filter(|&i| {
                file.lines[i..i + n]
                    .iter()
                    .zip(&want)
                    .all(|(l, w)| l.trim() == *w)
            })
            .collect()
    };
    match hits.as_slice() {
        &[i] => {
            let mut out = file.clone();
            let new_lines = split_text(new)?;
            let mut ends = vec![file.newline(); new_lines.len()];
            if let Some(last) = ends.last_mut() {
                *last = file.ends[i + n - 1];
            }
            out.lines.splice(i..i + n, new_lines);
            out.ends.splice(i..i + n, ends);
            Ok(out.render())
        }
        [] => {
            let first = want.iter().copied().find(|w| !w.is_empty()).unwrap_or("");
            let best = (0..file.lines.len())
                .map(|i| (i, similarity(first, &file.lines[i])))
                .filter(|(_, r)| *r >= 0.6)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            Err(match best {
                Some((i, _)) => format!(
                    "old_string was not found; its first line is closest to {}",
                    render_line(i + 1, &file.lines[i])
                ),
                None => "old_string was not found; read the file and copy the text exactly".into(),
            })
        }
        many => Err(format!(
            "old_string matches {} places (ignoring indentation); include more context",
            many.len()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_parse_with_or_without_text() {
        let a = parse_anchor(&json!(" 12#K3|    let x = 1;"), false).unwrap();
        assert_eq!(a.line, 12);
        assert_eq!(a.hash.as_deref(), Some("k3"));
        assert_eq!(a.text.as_deref(), Some("    let x = 1;"));
        assert!(parse_anchor(&json!("12|x"), false).unwrap().hash.is_none());
        // A bad hash is dropped when the text can stand in for it.
        let a = parse_anchor(&json!("12#zzz|x"), false).unwrap();
        assert!(a.hash.is_none());
        assert!(parse_anchor(&json!("12#zzz"), false).is_err());
        assert!(parse_anchor(&json!("12"), false).is_err());
        assert!(parse_anchor(&json!(12), false).is_err());
        assert!(parse_anchor(&json!("0"), false).is_err());
        assert_eq!(parse_anchor(&json!("0"), true).unwrap().line, 0);
        assert!(parse_anchor(&json!("12#il"), false).is_err());
    }

    #[test]
    fn copied_anchors_are_stripped_from_text() {
        assert_eq!(split_text("").unwrap(), Vec::<String>::new());
        assert_eq!(split_text("\n").unwrap(), vec![String::new()]);
        assert_eq!(split_text("a\r\nb\n").unwrap(), vec!["a", "b"]);
        assert_eq!(split_text("1#aa|x\n2#bb|y").unwrap(), vec!["x", "y"]);
        let err = split_text("1#aa|x\ny").unwrap_err();
        assert!(err.contains("mixes read_file anchors"), "{err}");
    }

    #[test]
    fn arguments_are_forgiving_but_clear() {
        let err = parse(json!({"edits": []}), Some(Path::new("/x/a.rs"))).unwrap_err();
        assert!(
            err.contains("missing `path`") && err.contains("/x/a.rs"),
            "{err}"
        );
        let edits = json!([{"at": "1#aa", "text": "x"}]).to_string();
        let Ok(Request::Anchored { edits, .. }) = parse(json!({"path": "a", "edits": edits}), None)
        else {
            panic!("a list sent as a string")
        };
        assert_eq!(edits.len(), 1);
        let Ok(Request::Anchored { edits, .. }) =
            parse(json!({"path": "a", "after": "0", "text": "x"}), None)
        else {
            panic!("one edit at the top level")
        };
        assert_eq!(edits[0].kind, Kind::After);
        let err = parse(
            json!({"path": "a", "edits": [{"at": "1#aa", "txt": ""}]}),
            None,
        )
        .unwrap_err();
        assert!(err.starts_with("edit 1: unknown field `txt`"), "{err}");
        let err = parse(json!({"path": "a", "edits": [{"text": ""}]}), None).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        assert!(matches!(
            parse(
                json!({"path": "a", "old_string": "x", "new_string": "y"}),
                None
            ),
            Ok(Request::Replace { .. })
        ));
    }

    fn anchor(line: usize, hash: Option<&str>, text: Option<&str>) -> Anchor {
        Anchor {
            line,
            hash: hash.map(str::to_owned),
            text: text.map(str::to_owned),
            raw: String::new(),
        }
    }

    #[test]
    fn text_overrides_a_wrong_number_or_hash_only_when_unambiguous() {
        let f: Vec<String> = ["fn a() {", "    x();", "}", "fn b() {", "    y();", "}"]
            .map(String::from)
            .to_vec();
        let at = |found| match found {
            Found::At { index, moved } => Some((index, moved)),
            _ => None,
        };
        // A valid but wrong hash cannot be bypassed by copied text.
        assert_eq!(at(find(&f, &anchor(2, Some("zz"), Some("x();")))), None);
        // A text-only anchor cannot silently accept a whitespace-only change.
        let changed = vec![r#"let value = "a  b";"#.to_owned()];
        assert_eq!(
            at(find(
                &changed,
                &anchor(1, None, Some(r#"let value = "a b";"#))
            )),
            None
        );
        // Number off by one: the text finds the line.
        assert_eq!(
            at(find(&f, &anchor(4, None, Some("y();")))),
            Some((4, true))
        );
        // A wrong hash alone is refused.
        assert!(at(find(&f, &anchor(2, Some("zz"), None))).is_none());
        // `}` twice, neither at the number: ambiguous.
        assert!(matches!(
            find(&f, &anchor(2, None, Some("}"))),
            Found::Ambiguous(_)
        ));
        // Right hash, text a little off: still that line.
        let h = line_hash("    y();");
        assert_eq!(
            at(find(&f, &anchor(5, Some(&h), Some("  y()")))),
            Some((4, false))
        );
    }
}
