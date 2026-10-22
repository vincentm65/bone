//! The shared Lua UI API in the TUI: every `tests/ui/*.lua` script (see
//! `tests/ui/runner.rs`), in the headless TUI against a real core.

use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_server::Server;
use bone_tui::{Headless, RunOptions};

#[path = "../../../tests/ui/runner.rs"]
mod runner;

use runner::UiDriver;

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
    let failures = runner::run_all(|| async {
        let (ui, config, work) = tui().await;
        Fixture {
            ui,
            _dirs: (config, work),
        }
    })
    .await;
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// The headless TUI and the directories it runs in.
struct Fixture {
    ui: Headless,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl UiDriver for Fixture {
    fn run_lua(&mut self, name: &str, code: &str) -> Result<(), String> {
        self.ui.run_lua(name, code)
    }

    async fn settle(&mut self) {
        self.ui.settle(Duration::from_millis(20)).await;
    }
}
