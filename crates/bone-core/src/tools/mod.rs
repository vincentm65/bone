//! Tools the agent can call.
//!
//! A [`Tool`] is a spec the model sees plus an async `call`. The [`Registry`]
//! is dynamic so core-side Lua can add tools later.

mod edit_file;
pub mod hashline;
mod read_file;
mod shell;
mod write_file;

pub(crate) use shell::{ProcessRegistry, ProcessState, ProcessView};

use std::path::{Component, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
}

pub struct ToolContext {
    pub cwd: PathBuf,
    pub session_id: String,
    /// The model's id for this call (empty outside a turn).
    pub call_id: String,
    /// Cancelled when the turn is. Long-running tools should watch it; the
    /// agent also drops the call future on cancel.
    pub cancel: CancellationToken,
    /// What the session's model has seen of each file (see `hashline`).
    pub views: Arc<hashline::Views>,
    /// Session-scoped shell jobs shared by foreground and background runs.
    pub(crate) jobs: Arc<ProcessRegistry>,
    /// Where a tool can send output while it runs (`tool/output`), if anyone
    /// listens. The result still carries all of it.
    pub output: Option<OutputSink>,
    /// Where managed-process snapshots are published while a command runs.
    pub(crate) processes: Option<ProcessUpdateSink>,
}

/// Takes a running tool's output as it comes.
pub type OutputSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Receives an authoritative update for a managed shell process.
pub(crate) type ProcessUpdateSink = Arc<dyn Fn(ProcessView, u64) + Send + Sync>;

impl ToolContext {
    /// An absolute path, with `.` and `..` worked out so one file has one
    /// name.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let mut out = PathBuf::new();
        for part in self.cwd.join(path).components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                part => out.push(part),
            }
        }
        out
    }
}

/// Text for the model. `Err` marks the result as an error.
pub type ToolResult = Result<String, String>;

pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;
    fn call<'a>(&'a self, args: Value, ctx: &'a ToolContext) -> BoxFuture<'a, ToolResult>;
    /// Safe to run at the same time as other such calls (it only reads).
    fn parallel(&self) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct Registry {
    tools: Vec<Arc<dyn Tool>>,
    specs: Vec<ToolSpec>,
}

impl Registry {
    pub fn empty() -> Self {
        Registry {
            tools: Vec::new(),
            specs: Vec::new(),
        }
    }

    /// read_file, write_file, edit_file, shell.
    pub fn builtin() -> Self {
        let mut r = Self::empty();
        r.register(Arc::new(read_file::ReadFile::new()));
        r.register(Arc::new(write_file::WriteFile::new()));
        r.register(Arc::new(edit_file::EditFile::new()));
        r.register(Arc::new(shell::Shell::new()));
        r
    }

    /// Add a tool, replacing any existing tool with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name.clone();
        if let Some(i) = self.tools.iter().position(|t| t.spec().name == name) {
            self.specs[i] = tool.spec().clone();
            self.tools[i] = tool;
        } else {
            self.specs.push(tool.spec().clone());
            self.tools.push(tool);
        }
    }

    pub fn specs(&self) -> &[ToolSpec] {
        &self.specs
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.spec().name == name)
    }
}

/// Parse raw model arguments. Empty means `{}`.
pub fn parse_args(raw: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(raw).map_err(|e| {
        if e.is_eof() {
            format!(
                "the arguments were cut off after {} bytes, likely at the output token limit; \
                 send less at once, e.g. fewer or smaller edits",
                raw.len()
            )
        } else {
            format!(
                "the arguments are not valid JSON ({e}); inside strings, write newlines as \\n \
                 and escape quotes"
            )
        }
    })
}

fn typed_args<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

/// Keep the head and tail of long output, cut on char boundaries.
fn truncate_middle(s: &str, head: usize, tail: usize) -> String {
    if s.len() <= head + tail {
        return s.to_owned();
    }
    let mut h = head;
    while !s.is_char_boundary(h) {
        h -= 1;
    }
    let mut t = s.len() - tail;
    while !s.is_char_boundary(t) {
        t += 1;
    }
    format!(
        "{}\n\n[... {} bytes omitted ...]\n\n{}",
        &s[..h],
        t - h,
        &s[t..]
    )
}

#[cfg(test)]
mod tests;
