//! Tools the agent can call.
//!
//! A [`Tool`] is a spec the model sees plus an async `call`. The [`Registry`]
//! is dynamic so core-side Lua can add tools later.

mod edit_file;
mod read_file;
mod shell;
mod write_file;

use std::path::{Path, PathBuf};
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
    /// Cancelled when the turn is. Long-running tools should watch it; the
    /// agent also drops the call future on cancel.
    pub cancel: CancellationToken,
}

impl ToolContext {
    pub fn resolve(&self, path: &str) -> PathBuf {
        let path = Path::new(path);
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.cwd.join(path)
        }
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
    serde_json::from_str(raw).map_err(|e| format!("arguments are not valid JSON: {e}"))
}

fn typed_args<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))
}

/// Keep the head and tail of long output, cut on char boundaries.
pub(crate) fn truncate_middle(s: &str, head: usize, tail: usize) -> String {
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
