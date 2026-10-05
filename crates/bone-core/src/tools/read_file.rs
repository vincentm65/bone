use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::hashline::{Lines, render_line};
use super::{Tool, ToolContext, ToolResult, ToolSpec, typed_args};

const MAX_LINE_CHARS: usize = 2000;
const DEFAULT_LIMIT: usize = 2000;

pub struct ReadFile(ToolSpec);

impl ReadFile {
    pub fn new() -> Self {
        ReadFile(ToolSpec {
            name: "read_file".into(),
            description: "Read a text file. Each line is shown as LINE#HASH|content, e.g. \
                          `12#k3|    let x = 1;`; edit_file uses those lines as anchors. Search \
                          first, then use ranges for focused sections, or full=true to read the \
                          whole file."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path, absolute or relative to the working directory." },
                    "ranges": {
                        "type": "array",
                        "description": "Line ranges to read, in one call. Each range is inclusive.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "start": { "type": "integer", "minimum": 1 },
                                "end": { "type": "integer", "minimum": 1 }
                            },
                            "required": ["start", "end"],
                            "additionalProperties": false
                        }
                    },
                    "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based). Default 1." },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum lines to read; choose this after searching." },
                    "full": { "type": "boolean", "description": "Read all lines from offset onward instead of a focused range." }
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
    ranges: Option<Vec<Range>>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    full: bool,
}

#[derive(Deserialize)]
struct Range {
    start: usize,
    end: usize,
}

impl Tool for ReadFile {
    fn spec(&self) -> &ToolSpec {
        &self.0
    }

    fn parallel(&self) -> bool {
        true
    }

    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: Args = typed_args(args)?;
            let path = ctx.resolve(&args.path);
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            if bytes[..bytes.len().min(8192)].contains(&0) {
                return Err(format!("{} looks like a binary file", path.display()));
            }
            let text = String::from_utf8_lossy(&bytes);
            let lines = Lines::parse(&text).lines;
            if lines.is_empty() {
                ctx.views
                    .record(&ctx.session_id, &path, &lines, Default::default());
                return Ok("(empty file)".into());
            }

            let total = lines.len();
            let ranges = if let Some(ranges) = args.ranges {
                if ranges.is_empty() {
                    return Err("`ranges` must contain at least one range".into());
                }
                ranges
                    .into_iter()
                    .map(|r| {
                        if r.start == 0 || r.end == 0 || r.end < r.start {
                            return Err(format!(
                                "invalid range {}-{}; use positive inclusive start/end lines",
                                r.start, r.end
                            ));
                        }
                        if r.start > total {
                            return Err(format!(
                                "range start {} is past the end of the file ({total} lines)",
                                r.start
                            ));
                        }
                        Ok((r.start, r.end.min(total)))
                    })
                    .collect::<Result<Vec<_>, String>>()?
            } else {
                let start = args.offset.unwrap_or(1).max(1);
                let limit = if args.full {
                    usize::MAX
                } else {
                    args.limit.unwrap_or(DEFAULT_LIMIT).max(1)
                };
                if start > total {
                    return Err(format!(
                        "offset {start} is past the end of the file ({total} lines)"
                    ));
                }
                vec![(start, (start - 1).saturating_add(limit).min(total))]
            };
            let mut out = String::new();
            let mut seen = std::collections::BTreeSet::new();
            for (range_index, (start, end)) in ranges.iter().copied().enumerate() {
                if ranges.len() > 1 {
                    if range_index > 0 {
                        out.push('\n');
                    }
                    out.push_str(&format!("[lines {start}-{end} of {total}]\n"));
                }
                for (i, line) in lines.iter().enumerate().take(end).skip(start - 1) {
                    // The hash is of the whole line, even when it is cut short.
                    let mut shown = render_line(i + 1, line);
                    if let Some((cut, _)) = line.char_indices().nth(MAX_LINE_CHARS) {
                        shown.truncate(shown.len() - (line.len() - cut));
                        shown.push_str(" [line truncated]");
                    }
                    out.push_str(&shown);
                    out.push('\n');
                    seen.insert(i + 1);
                }
            }
            if ranges.len() == 1 {
                let (start, end) = ranges[0];
                if end < total {
                    out.push_str(&format!(
                        "\n[showing lines {start}-{end} of {total}; use offset to read more]\n"
                    ));
                }
            }
            ctx.views.record(&ctx.session_id, &path, &lines, seen);
            Ok(out)
        })
    }
}
