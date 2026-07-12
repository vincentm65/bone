//! The `edit_file` tool: OMP-style hashline edits.
//!
//! The model emits a hashline patch in the `input` field: one or more
//! `[path#TAG]` sections, each a list of line ops (SWAP / DEL / INS.PRE /
//! INS.POST / INS.HEAD / INS.TAIL) or a single file op (`MV dest` / `REM`).
//! Line numbers refer to the snapshot the model last read (identified by TAG);
//! the tool resolves that snapshot, applies the ops, and writes the result
//! atomically. When the live file has drifted from the read snapshot it
//! attempts a line-level 3-way merge ([`recovery`]); a conflicting overlap is
//! rejected with a re-read instruction.
//!
//! All sections are preflight-validated (parse + resolve + apply, no writes)
//! before any file is touched, so a failure in section 3 leaves sections 1–2
//! unmodified. Mid-batch write failures report which files were written and
//! which were not (no rollback).

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::fs;

use crate::tools::snapshot::{self, SnapshotStore};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::write_atomic::write_atomic;

pub(crate) mod apply;
pub(crate) mod diff;
pub(crate) mod parser;
pub(crate) mod recovery;

pub struct EditFileTool;

/// A preview of a pending edit, for TUI rendering. `before_hash` is the 4-hex
/// content tag of the live file at preview time; `diff` is a unified diff.
pub struct EditPreview {
    pub before_hash: String,
    pub diff: String,
}

type Snapshots = Arc<RwLock<SnapshotStore>>;

#[async_trait]
impl Tool for EditFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Edit existing files with a hashline patch. Pass `input`: one or more `[path#TAG]` sections, where TAG is the 4-hex tag from your last read_file/write_file/edit_file result and line numbers refer to that snapshot. Line ops: SWAP start.=end (replace lines), DEL start.=end (delete lines), INS.PRE n / INS.POST n (insert before/after line n), INS.HEAD / INS.TAIL (insert at top/end of file). File ops, alone in their section: MV dest (rename), REM (delete file). Every body line must start with `+` at column 0 — write an empty body line as a bare `+`; a line without `+` ends the op's body. Returns the new tag and a diff.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Hashline patch: `[path#TAG]` header, then ops. Every body line starts with `+`. Example:\n[src/main.rs#A1B2]\nSWAP 3.=4:\n+let x = 1;\n+let y = 2;\nINS.POST 10:\n+// inserted after line 10\nDEL 20.=22"
                    }
                },
                "required": ["input"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        run_edit(arguments, None).await
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        run_edit(arguments, Some(&context.snapshots))
            .await
            .map(ToolOutput::text)
    }
}

/// Preview a pending hashline edit for TUI rendering. Degrades to live-file
/// resolution (no snapshot store, no visible-line guard): it shows what the
/// patch would do against the current file. Stale-tag mismatches are only
/// caught at apply time.
pub async fn preview_edit_file(_tool_name: &str, arguments: Value) -> Result<EditPreview, String> {
    let input = extract_input(arguments)?;
    let patch = parser::parse(&input)?;
    let mut combined = String::new();
    let mut before_hash = String::new();
    for section in &patch.sections {
        let content_ops: Vec<_> = section
            .ops
            .iter()
            .filter(|o| !is_lifecycle(o))
            .cloned()
            .collect();
        if content_ops.is_empty() {
            continue;
        }
        let live = read_live_normalized(&section.path).await?;
        if before_hash.is_empty() {
            before_hash = snapshot::compute_tag(&live);
        }
        let edited = apply::apply_ops(&live, &content_ops, None).map_err(|e| e.to_string())?;
        combined.push_str(&diff::build_unified_diff(
            "edit_file",
            &section.path,
            &live,
            &edited,
        ));
    }
    Ok(EditPreview {
        before_hash,
        diff: combined,
    })
}

/// Core entry point shared by the context-aware and degraded paths.
async fn run_edit(arguments: Value, snapshots: Option<&Snapshots>) -> Result<String, String> {
    let input = extract_input(arguments)?;
    let patch = parser::parse(&input)?;

    // ── Preflight: validate every section and build a write plan. ──────────
    // No writes happen here, so a failure leaves the filesystem untouched.
    let mut plans = Vec::with_capacity(patch.sections.len());
    let mut paths = HashSet::with_capacity(patch.sections.len());
    for section in &patch.sections {
        if !paths.insert(section.path.as_str()) {
            return Err(format!(
                "duplicate section for `{}`; combine its operations into one section",
                section.path
            ));
        }
        plans.push(build_plan(section, snapshots).await?);
    }

    // ── Execute: apply writes in order. Track successes for a mid-batch ────
    // ── failure report (no rollback). ──────────────────────────────────────
    let mut outcomes = Vec::with_capacity(plans.len());
    for (idx, plan) in plans.into_iter().enumerate() {
        match execute_plan(&plan).await {
            Ok(outcome) => outcomes.push((plan, outcome)),
            Err(e) => {
                let written: Vec<&str> =
                    outcomes.iter().map(|(p, _)| p.path_for_report()).collect();
                let mut msg = String::new();
                if !written.is_empty() {
                    msg.push_str(&format!("wrote: {} — ", written.join(", ")));
                }
                msg.push_str(&format!(
                    "section {} (`{}`) failed: {e}",
                    idx + 1,
                    plan.path_for_report()
                ));
                return Err(msg);
            }
        }
    }

    // ── Record: update the snapshot store for the next round. ──────────────
    if let Some(store) = snapshots {
        let mut guard = store.write().map_err(|e| e.to_string())?;
        for (plan, _) in &outcomes {
            match plan {
                Plan::Content { path, new_text, .. } => {
                    let n = snapshot::numbered_lines(new_text).len();
                    guard.record(path, new_text, Some(&(1..=n).collect::<Vec<_>>()));
                }
                Plan::Rename { from, to } => guard.relocate(from, to),
                Plan::Remove { path } => guard.invalidate(path),
            }
        }
    }

    Ok(build_summary(&outcomes))
}

/// One section's resolved write plan (post-preflight, pre-write).
enum Plan {
    /// Replace a file's contents. `live` is the pre-edit normalized text (diff
    /// base); `new_text` is what gets written (already merge-recovered if the
    /// read was stale).
    Content {
        path: String,
        live: String,
        new_text: String,
    },
    /// Rename `from` → `to`.
    Rename { from: String, to: String },
    /// Delete `path`.
    Remove { path: String },
}

impl Plan {
    /// Path used in progress/error reports.
    fn path_for_report(&self) -> &str {
        match self {
            Plan::Content { path, .. } | Plan::Remove { path } => path,
            Plan::Rename { from, .. } => from,
        }
    }
}

/// The result of executing a plan (for summary construction).
enum Outcome {
    Content {
        path: String,
        new_tag: String,
        diff: String,
    },
    Renamed {
        from: String,
        to: String,
    },
    Removed {
        path: String,
    },
}

/// True for file-lifecycle ops (MV/REM) handled outside the apply engine.
fn is_lifecycle(op: &parser::Op) -> bool {
    matches!(op, parser::Op::Move { .. } | parser::Op::Remove)
}

/// Resolve a section into a write plan: partition ops, fetch the snapshot,
/// apply content ops (preflight), and recover from drift if needed.
async fn build_plan(
    section: &parser::Section,
    snapshots: Option<&Snapshots>,
) -> Result<Plan, String> {
    let mut content_ops = Vec::new();
    let mut lifecycle = None;
    for op in &section.ops {
        if is_lifecycle(op) {
            if lifecycle.is_some() || !content_ops.is_empty() {
                return Err(format!(
                    "a file op ({}) must be the only op in its `[{}#{}`] section; \
                     move content edits to a separate call",
                    op.label(),
                    section.path,
                    section.tag
                ));
            }
            lifecycle = Some(op);
        } else {
            if lifecycle.is_some() {
                return Err(format!(
                    "content op ({}) cannot follow a file op in `[{}#{}`]; \
                     split into separate sections",
                    op.label(),
                    section.path,
                    section.tag
                ));
            }
            content_ops.push(op.clone());
        }
    }

    if let Some(op) = lifecycle {
        return Ok(match op {
            parser::Op::Move { dest } => Plan::Rename {
                from: section.path.clone(),
                to: dest.clone(),
            },
            parser::Op::Remove => Plan::Remove {
                path: section.path.clone(),
            },
            // Unreachable: is_lifecycle guards the variant.
            _ => unreachable!("lifecycle op was validated"),
        });
    }

    if content_ops.is_empty() {
        return Err(format!(
            "section `[{}#{}`] has no operations",
            section.path, section.tag
        ));
    }

    // ── Resolve the snapshot the model referenced. ─────────────────────────
    // Read the live file first so a content match can recover seen-lines even
    // when the tag itself drifted (e.g. the model re-read but quoted a stale
    // tag). Locks are held only for the brief lookup, never across awaits.
    let live = read_live_normalized(&section.path).await?;
    let (base_text, seen_lines) = resolve_base(snapshots, &section.path, &section.tag, &live)?;

    // ── Apply ops against the read snapshot (original-line semantics). ──────
    let edited =
        apply::apply_ops(&base_text, &content_ops, Some(&seen_lines)).map_err(|e| e.to_string())?;

    // ── Reconcile with the live file (fresh vs stale). ─────────────────────
    let new_text = if base_text == live {
        edited
    } else {
        // The file changed since the read: 3-way merge the intended change
        // onto the current contents.
        let recovered =
            recovery::merge_onto(&base_text, &edited, &live).map_err(|e| e.to_string())?;
        recovered.text
    };

    // ── No-op / loop guard: a content edit that changes nothing almost ──────
    // ── always means the model is re-applying an edit that already took. ────
    if new_text == live {
        return Err(format!(
            "no change to `{}` — the file already matches this edit, so it was likely \
             already applied; re-read the file to see current line numbers and tags",
            section.path
        ));
    }

    Ok(Plan::Content {
        path: section.path.clone(),
        live,
        new_text,
    })
}

/// Look up the snapshot the model referenced. Context-free callers operate on
/// the live file, while context-aware callers must supply a recorded tag.
fn resolve_base(
    snapshots: Option<&Snapshots>,
    path: &str,
    tag: &str,
    live: &str,
) -> Result<(String, BTreeSet<usize>), String> {
    let Some(store) = snapshots else {
        return Ok((live.to_string(), BTreeSet::new()));
    };
    let guard = store.read().map_err(|e| e.to_string())?;
    if let Some(snap) = guard.by_hash(path, tag) {
        return Ok((snap.text.clone(), snap.seen_lines.clone()));
    }

    // The transcript can outlive the in-memory snapshot store: for example,
    // the daemon may have resumed a conversation from SQLite, or a provider
    // may replay a cached read result without re-running the local tool. If the
    // live content itself has the requested tag, it is the exact snapshot the
    // model referenced, so applying against it is safe. We no longer have the
    // read's visibility metadata in this case; treat the whole exact-content
    // snapshot as visible rather than failing a valid cached/replayed read.
    if snapshot::compute_tag(live) == tag {
        let seen_lines = (1..=snapshot::numbered_lines(live).len()).collect();
        return Ok((live.to_string(), seen_lines));
    }

    Err(format!(
        "snapshot tag `{tag}` for `{path}` was not found; re-read the file and use the new tag"
    ))
}

/// Read a file as normalized text (BOM stripped, CRLF/CR → LF).
async fn read_live_normalized(path: &str) -> Result<String, String> {
    let meta = fs::metadata(path).await.map_err(crate::util::errstr)?;
    if !meta.is_file() {
        return Err(format!("`{path}` is not a regular file"));
    }
    let raw = fs::read_to_string(path)
        .await
        .map_err(crate::util::errstr)?;
    Ok(snapshot::normalize_text(&raw))
}

/// Execute one plan (filesystem write). Returns its outcome for the summary.
async fn execute_plan(plan: &Plan) -> Result<Outcome, String> {
    match plan {
        Plan::Content {
            path,
            live,
            new_text,
        } => {
            let p = Path::new(path);
            let permissions = fs::metadata(p).await.ok().map(|m| m.permissions());
            write_atomic(p, new_text, permissions).await?;
            let new_tag = snapshot::compute_tag(new_text);
            let diff =
                truncate_output(&diff::build_unified_diff("edit_file", path, live, new_text));
            Ok(Outcome::Content {
                path: path.clone(),
                new_tag,
                diff,
            })
        }
        Plan::Rename { from, to } => {
            fs::rename(from, to).await.map_err(crate::util::errstr)?;
            Ok(Outcome::Renamed {
                from: from.clone(),
                to: to.clone(),
            })
        }
        Plan::Remove { path } => {
            fs::remove_file(path).await.map_err(crate::util::errstr)?;
            Ok(Outcome::Removed { path: path.clone() })
        }
    }
}

/// Build the success message: one line per file plus a unified diff for
/// content edits.
fn build_summary(outcomes: &[(Plan, Outcome)]) -> String {
    let mut out = String::new();
    for (_, outcome) in outcomes {
        match outcome {
            Outcome::Content {
                path,
                new_tag,
                diff,
            } => {
                out.push_str(&format!("edited `{path}` [`{path}#{new_tag}`]\n"));
                if !diff.trim().is_empty() {
                    out.push_str(diff);
                    if !diff.ends_with('\n') {
                        out.push('\n');
                    }
                }
            }
            Outcome::Renamed { from, to } => {
                out.push_str(&format!("renamed `{from}` → `{to}`\n"));
            }
            Outcome::Removed { path } => {
                out.push_str(&format!("deleted `{path}`\n"));
            }
        }
    }
    out.trim_end().to_string()
}

/// Extract and validate the `input` field from the tool arguments.
fn extract_input(arguments: Value) -> Result<String, String> {
    let input = arguments
        .get("input")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            "edit_file requires an `input` string field holding a hashline patch".to_string()
        })?;
    if input.trim().is_empty() {
        return Err("`input` is empty; provide a `[path#TAG]` hashline patch".to_string());
    }
    Ok(input.to_string())
}

/// Cap a diff to a readable length so tool results stay bounded.
fn truncate_output(text: &str) -> String {
    crate::tools::shell::truncate_output(text, 200)
}
