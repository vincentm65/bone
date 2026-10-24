//! The shared Lua UI API in the GUI: every `tests/ui/*.lua` script (see
//! `tests/ui/runner.rs`), in the headless GUI against a real core. The same
//! scripts pass in the TUI (`crates/bone/tests/conformance.rs`).

use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_gui::{GuiOptions, Headless};
use bone_server::Server;

// `run_all` is for the frontends that run every script.
#[allow(dead_code)]
#[path = "../../../tests/ui/runner.rs"]
mod runner;

use runner::UiDriver;

/// The headless GUI and the directories it runs in.
struct Fixture {
    ui: Headless,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl UiDriver for Fixture {
    fn eval(&mut self, expr: &str) -> Result<String, String> {
        self.ui.gui.eval(expr)
    }

    fn click(&mut self, x: u16, y: u16) {
        self.ui.click(x, y);
    }

    fn paste(&mut self, text: &str) {
        self.ui.paste(text);
    }

    fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String> {
        self.ui.gui.run_lua(name, code)
    }

    async fn settle(&mut self) {
        self.ui.settle(Duration::from_millis(20)).await;
    }
}

/// A core with an unreachable provider, and a headless GUI on it with
/// `library`.
async fn gui(library: &str) -> Fixture {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("core.lua"),
        r#"bone.config.providers.fake = { base_url = "http://127.0.0.1:9", model = "fake" }"#,
    )
    .unwrap();
    std::fs::write(config.path().join("gui-library"), format!("{library}\n")).unwrap();
    let work = tempfile::tempdir().unwrap();
    let loaded = scripting::load_with(config.path(), &|_| None).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let opts = GuiOptions {
        cwd: work.path().to_string_lossy().into_owned(),
        reconnect: None,
        config_dir: Some(config.path().to_owned()),
    };
    let ui = Headless::start(server.connect_in_process(), opts, 1000.0, 700.0)
        .await
        .unwrap();
    assert!(ui.gui.lua_errors().is_empty(), "{:?}", ui.gui.lua_errors());
    Fixture {
        ui,
        _dirs: (config, work),
    }
}

/// The scripts that are about the TUI's cell layout: regions, docked cell
/// panels, the chat's row scroll. The GUI draws a node tree, so what they
/// check (where a cell went) has no meaning there; the TUI runs them.
const CELL_LAYOUT: [&str; 4] = ["frontend.lua", "layout.lua", "panels.lua", "scrolling.lua"];

/// The shared behaviour (events, commands, keymaps, options, prompt,
/// popups, plugins, jobs...) under the library the GUI draws by default.
#[tokio::test(flavor = "multi_thread")]
async fn shared_api_in_the_gui() {
    let mut failures = Vec::new();
    for path in runner::scripts() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // It asserts which library is drawn.
        if name == "libraries.lua" || CELL_LAYOUT.contains(&name) {
            continue;
        }
        let mut ui = gui("desktop").await;
        if let Err(e) = runner::run_script(&mut ui, &path).await {
            failures.push(format!("{}: {e}", path.display()));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
