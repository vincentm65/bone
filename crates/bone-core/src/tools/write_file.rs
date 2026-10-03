use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolResult, ToolSpec, typed_args};

pub struct WriteFile(ToolSpec);

impl WriteFile {
    pub fn new() -> Self {
        WriteFile(ToolSpec {
            name: "write_file".into(),
            description: "Create a file or overwrite it entirely. Creates parent directories. \
                          Prefer edit_file for changes to existing files."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                    "content": { "type": "string", "description": "The complete file content." }
                },
                "required": ["path", "content"]
            }),
        })
    }
}

#[derive(Deserialize)]
struct Args {
    path: String,
    content: String,
}

impl Tool for WriteFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            let path = ctx.resolve(&args.path);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            let existed = tokio::fs::try_exists(&path).await.unwrap_or(false);
            tokio::fs::write(&path, &args.content)
                .await
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            let lines = super::hashline::Lines::parse(&args.content).lines;
            let seen = (1..=lines.len()).collect();
            ctx.views.record(&ctx.session_id, &path, &lines, seen);
            let verb = if existed { "Overwrote" } else { "Created" };
            Ok(format!(
                "{verb} {} ({} bytes)",
                path.display(),
                args.content.len()
            ))
        })
    }
}
