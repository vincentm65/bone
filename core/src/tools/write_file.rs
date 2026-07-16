//! The `write_file` tool: creates a new file atomically (fails if it exists).
//!
//! On success it records the file's normalized content as a fresh snapshot
//! (all lines visible — the model just authored them) so a following
//! `edit_file` can validate a simple exact-text replacement.

use std::path::Path;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::ErrorKind;
use tokio::fs;

use crate::tools::snapshot::{self, SnapshotStore};
use crate::tools::types::{Tool, ToolDefinition, ToolExecutionContext, ToolOutput};
use crate::tools::write_atomic::write_atomic;

/// Shared snapshot store type (mirrors the alias in `edit_file`/`read_file`).
type Snapshots = Arc<RwLock<SnapshotStore>>;

pub struct WriteFileTool;

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

#[async_trait]
impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Preferred tool for creating file contents; use this instead of shell commands such as tee, printf, heredocs, or redirection. Creates a NEW UTF-8 text file and errors if the path already exists. To change an existing file, read it and use edit_file with path, old_text, and new_text.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path. Relative paths resolve from the working directory. Parent directories created automatically."
                    },
                    "content": {
                        "type": "string",
                        "description": "File contents to write."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> Result<String, String> {
        write_file_inner(arguments, None, None).await
    }

    async fn execute_output_live(
        &self,
        arguments: Value,
        _events: Option<tokio::sync::mpsc::UnboundedSender<crate::pane_content::KeyRequest>>,
        context: ToolExecutionContext,
    ) -> Result<ToolOutput, String> {
        write_file_inner(
            arguments,
            Some(&context.snapshots),
            context.working_dir.as_deref(),
        )
        .await
        .map(ToolOutput::text)
    }
}

async fn write_file_inner(
    arguments: Value,
    snapshots: Option<&Snapshots>,
    working_dir: Option<&Path>,
) -> Result<String, String> {
    let args: Args = serde_json::from_value(arguments).map_err(crate::util::errstr)?;
    let path = snapshot::resolve_new_path(&args.path, working_dir).await?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .await
            .map_err(crate::util::errstr)?;
    }
    // Reject if the destination already exists. `rename` will silently
    // overwrite on Unix, and `exists()` misses dangling symlinks, so use
    // symlink_metadata. A create-between-check-and-rename race remains but
    // is acceptable for this tool's local convenience threat model.
    match fs::symlink_metadata(&path).await {
        Ok(_) => {
            return Err(format!(
                "file already exists — write_file only creates new files. Do NOT retry write_file for this path. \
                 To change it, read the file and call edit_file with path, old_text, and new_text.",
            ));
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(crate::util::errstr(e)),
    }
    write_atomic(&path, &args.content, None).await?;

    // Snapshot the normalized content with every line visible, so a follow-up
    // edit_file can anchor on any line. The tag matches what read_file would
    // emit for the same bytes (both normalize identically).
    let normalized = snapshot::normalize_text(&args.content);
    let n = snapshot::numbered_lines(&normalized).len();
    let snapshot_path = fs::canonicalize(&path)
        .await
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    if let Some(store) = snapshots
        && let Ok(mut guard) = store.write()
    {
        guard.record(
            &snapshot_path,
            &normalized,
            Some(&(1..=n).collect::<Vec<_>>()),
        );
    }

    Ok(format!(
        "wrote {} ({} bytes, {} line{})",
        snapshot_path,
        args.content.len(),
        n,
        if n == 1 { "" } else { "s" },
    ))
}
