//! The shared Lua UI API, checked in every frontend. Each `tests/ui/*.lua`
//! script runs against a real core in a headless frontend. A script is a
//! chunk, or returns a list of steps (functions) run in order with the
//! frontend's events handled between them. It fails by raising an error;
//! `eq(got, want, what)` is the one helper.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_server::Server;
use bone_tui::{Headless, RunOptions};

/// What the runner needs from a frontend's headless driver.
trait UiDriver {
    fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String>;
    async fn settle(&mut self);
}

impl UiDriver for Headless {
    fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String> {
        Headless::run_lua(self, name, code)
    }

    async fn settle(&mut self) {
        Headless::settle(self, Duration::from_millis(20)).await;
    }
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
async fn run_script(ui: &mut impl UiDriver, path: &Path) -> Result<(), String> {
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
fn scripts() -> Vec<PathBuf> {
    let dir = root().join("tests/ui");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path().strip_prefix(root()).unwrap().to_owned())
        .filter(|p| p.extension().is_some_and(|x| x == "lua"))
        .collect();
    out.sort();
    out
}

/// A core with an unreachable provider and the standard library, and a
/// headless TUI on it.
async fn tui() -> (Headless, tempfile::TempDir, tempfile::TempDir) {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("core.lua"),
        r#"bone.config.providers.fake = { base_url = "http://127.0.0.1:9", model = "fake" }"#,
    )
    .unwrap();
    let work = tempfile::tempdir().unwrap();
    let loaded = scripting::load_with(config.path(), &|_| None).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let opts = RunOptions {
        clipboard_executable: None,
        cwd: work.path().to_string_lossy().into_owned(),
        config_dir: Some(config.path().to_owned()),
        resume: None,
        reload_core: true,
        reconnect: None,
    };
    let ui = Headless::start(server.connect_in_process(), opts, 80, 24)
        .await
        .unwrap();
    (ui, config, work)
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_api_in_the_tui() {
    let mut failures = Vec::new();
    for path in scripts() {
        let (mut ui, _config, _work) = tui().await;
        if let Err(e) = run_script(&mut ui, &path).await {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
