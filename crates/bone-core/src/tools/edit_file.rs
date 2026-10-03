use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolResult, ToolSpec, typed_args};

pub struct EditFile(ToolSpec);

impl EditFile {
    pub fn new() -> Self {
        EditFile(ToolSpec {
            name: "edit_file".into(),
            description: "Replace an exact string in a file. old_string must match the file \
                          exactly (including whitespace) and be unique unless replace_all is \
                          true. Read the file first."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                    "old_string": { "type": "string", "description": "Exact text to replace." },
                    "new_string": { "type": "string", "description": "Replacement text." },
                    "replace_all": { "type": "boolean", "description": "Replace every occurrence. Default false." }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        })
    }
}

#[derive(Deserialize)]
struct Args {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl Tool for EditFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            if args.old_string.is_empty() {
                return Err("old_string is empty; use write_file to create files".into());
            }
            if args.old_string == args.new_string {
                return Err("old_string and new_string are identical".into());
            }
            let path = ctx.resolve(&args.path);
            let text = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let count = text.matches(&args.old_string).count();
            match count {
                0 => {
                    return Err(format!(
                        "old_string not found in {}. Re-read the file and copy the text exactly.",
                        path.display()
                    ));
                }
                n if n > 1 && !args.replace_all => {
                    return Err(format!(
                        "old_string matches {n} places in {}. Include more surrounding context \
                         to make it unique, or set replace_all.",
                        path.display()
                    ));
                }
                _ => {}
            }
            let updated = text.replace(&args.old_string, &args.new_string);
            tokio::fs::write(&path, updated)
                .await
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            let s = if count == 1 { "" } else { "s" };
            Ok(format!(
                "Replaced {count} occurrence{s} in {}",
                path.display()
            ))
        })
    }
}
