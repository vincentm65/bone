use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::{Tool, ToolContext, ToolResponse, ToolResult, ToolSpec, typed_args};

const DEFAULT_LIMIT: usize = 2000;
const MAX_LINE_CHARS: usize = 2000;

pub struct ReadFile(ToolSpec);

impl ReadFile {
    pub fn new() -> Self {
        ReadFile(ToolSpec {
            name: "read_file".into(),
            description: "Read a text file or view a PNG, JPEG or WebP image. Images are returned \
                          as pixels; text is plain text without per-line prefixes. Line ranges apply \
                          only to text; use offset/limit for large files."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                    "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based). Default 1." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum lines to read. Default 2000." }
                },
                "required": ["path"]
            }),
        })
    }
}

#[derive(Deserialize)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

impl Tool for ReadFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn parallel(&self) -> bool {
        true
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move { self.call_with_images(args, ctx).await.map(|r| r.output) })
    }

    fn call_with_images<'a>(
        &'a self,
        args: Value,
        ctx: &'a ToolContext,
    ) -> BoxFuture<'a, Result<ToolResponse, String>> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            let path = ctx.resolve(&args.path);
            let read = async {
                let mut bytes = Vec::new();
                tokio::fs::File::open(&path)
                    .await?
                    .take(bone_media::MAX_IMAGE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .await?;
                let image = bone_media::is_image(&bytes);
                Ok::<_, std::io::Error>((bytes, image))
            };
            let (bytes, image) = read
                .await
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            if bytes.len() > bone_media::MAX_IMAGE_BYTES {
                return Err("file exceeds the 20 MiB read limit".into());
            }
            if image {
                return Ok(ToolResponse {
                    output: format!("Read image: {}", path.display()),
                    images: vec![(bytes, args.path)],
                });
            }
            if bytes[..bytes.len().min(8192)].contains(&0) {
                return Err(format!("{} looks like a binary file", path.display()));
            }
            let text = String::from_utf8_lossy(&bytes);
            if text.is_empty() {
                return Ok(String::from("(empty file)").into());
            }

            let total = text.lines().count();
            let start = args.offset.unwrap_or(1).max(1);
            let limit = args.limit.unwrap_or(DEFAULT_LIMIT).max(1);
            if start > total {
                return Err(format!(
                    "offset {start} is past the end of the file ({total} lines)"
                ));
            }
            let mut out = String::new();
            for line in text.lines().skip(start - 1).take(limit) {
                let line = match line.char_indices().nth(MAX_LINE_CHARS) {
                    Some((cut, _)) => format!("{} [line truncated]", &line[..cut]),
                    None => line.to_owned(),
                };
                out.push_str(&format!("{line}\n"));
            }
            let end = (start - 1).saturating_add(limit).min(total);
            if end < total {
                out.push_str(&format!(
                    "\n[showing lines {start}-{end} of {total}; use offset to read more]\n"
                ));
            }
            Ok(out.into())
        })
    }
}
