//! The conformance runner, shared by every frontend's test crate
//! (`#[path]`-included): each `tests/ui/*.lua` script runs in a headless
//! frontend against a real core. A script is a chunk, or returns a list of
//! steps (functions) run in order with the frontend's events handled
//! between them; a step that sets `__click = { x, y }` has that cell clicked
//! before the next, one that sets `__paste = text` has that pasted, and one
//! that sets `__wait = function` waits (up to five seconds) for it to
//! return true. A script fails by raising an error; `eq(got, want,
//! what)` is the one helper.

use std::path::{Path, PathBuf};

/// What the runner needs from a frontend's headless driver.
pub trait UiDriver {
    fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String>;
    /// A Lua expression's value, as a string.
    fn eval(&mut self, expr: &str) -> Result<String, String>;
    /// Press and release the left button over character cell `(x, y)`.
    fn click(&mut self, x: u16, y: u16);
    /// Paste `text`, as the terminal or window would.
    fn paste(&mut self, text: &str);
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
    // Counted now: a step that loads the UI again must not end the script
    // early by losing the rest.
    let steps: usize = ui
        .eval("type(__steps) == 'table' and #__steps or 0")?
        .parse()
        .map_err(|e| format!("{e}"))?;
    for step in 1..=steps {
        let there = format!(
            "if type(__steps) ~= 'table' or __steps[{step}] == nil then error('{DONE}', 0) end"
        );
        match ui.run_lua("=steps", &there) {
            Err(e) if e.contains(DONE) => {
                return Err(format!(
                    "{}: step {step} was lost (the Lua state went?)",
                    path.display()
                ));
            }
            r => r?,
        }
        ui.run_lua(
            &format!("={}: step {step}", path.display()),
            &format!("__steps[{step}]()"),
        )?;
        ui.settle().await;
        // A step that set `__click = { x, y }` gets that cell clicked.
        let at = ui.eval("__click and (__click[1] .. ' ' .. __click[2]) or ''")?;
        if let Some((x, y)) = at.split_once(' ') {
            let cell = |v: &str| v.parse::<u16>().map_err(|e| e.to_string());
            ui.click(cell(x)?, cell(y)?);
            ui.run_lua("=click", "__click = nil")?;
            ui.settle().await;
        }
        // `__wait`: settle until it holds (a job finishing, a reply).
        if ui.eval("type(__wait) == 'function'")? == "true" {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while ui.eval("__wait() == true")? != "true" {
                if std::time::Instant::now() > deadline {
                    return Err(format!("{}: step {step} waited too long", path.display()));
                }
                ui.settle().await;
            }
            ui.run_lua("=wait", "__wait = nil")?;
        }
        let pasted = ui.eval("__paste or ''")?;
        if !pasted.is_empty() {
            ui.paste(&pasted);
            ui.run_lua("=paste", "__paste = nil")?;
            ui.settle().await;
        }
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
