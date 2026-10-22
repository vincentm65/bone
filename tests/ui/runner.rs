//! The conformance runner, shared by every frontend's test crate
//! (`#[path]`-included): each `tests/ui/*.lua` script runs in a headless
//! frontend against a real core. A script is a chunk, or returns a list of
//! steps (functions) run in order with the frontend's events handled
//! between them. It fails by raising an error; `eq(got, want, what)` is the
//! one helper.

use std::path::{Path, PathBuf};

/// What the runner needs from a frontend's headless driver.
pub trait UiDriver {
    fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String>;
    async fn settle(&mut self);
}

const PRELUDE: &str = r#"
function eq(got, want, what)
  if got ~= want then
    error(string.format("%s: got %s, want %s", what or "value",
      bone.inspect(got), bone.inspect(want)), 2)
  end
end
"#;

/// Run one script: its chunk, then each step it returned.
pub async fn run_script(ui: &mut impl UiDriver, path: &Path) -> Result<(), String> {
    let name = format!("@{}", path.display());
    let source = std::fs::read_to_string(root().join(path)).map_err(|e| e.to_string())?;
    ui.run_lua("=prelude", PRELUDE)?;
    // On the chunk's first line, so error line numbers match the file.
    ui.run_lua(&name, &format!("__steps = (function(...) {source}\nend)()"))?;
    ui.settle().await;
    for step in 1.. {
        let last = format!(
            "if type(__steps) ~= 'table' or __steps[{step}] == nil then error('{DONE}', 0) end"
        );
        match ui.run_lua("=steps", &last) {
            Err(e) if e.contains(DONE) => break,
            r => r?,
        }
        ui.run_lua(
            &format!("={}: step {step}", path.display()),
            &format!("__steps[{step}]()"),
        )?;
        ui.settle().await;
    }
    Ok(())
}

/// Raised when a script has no more steps.
const DONE: &str = "__conformance_done";

/// The repository root, which script paths are reported from.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// `tests/ui/*.lua`, relative to the root.
pub fn scripts() -> Vec<PathBuf> {
    let dir = root().join("tests/ui");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path().strip_prefix(root()).unwrap().to_owned())
        .filter(|p| p.extension().is_some_and(|x| x == "lua"))
        .collect();
    out.sort();
    out
}

/// Run every script in `ui`s from `start`; the failures, one per line.
pub async fn run_all<D: UiDriver, F: std::future::Future<Output = D>>(
    start: impl Fn() -> F,
) -> Vec<String> {
    let mut failures = Vec::new();
    for path in scripts() {
        let mut ui = start().await;
        if let Err(e) = run_script(&mut ui, &path).await {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    failures
}
