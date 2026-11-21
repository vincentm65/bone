//! Switching models in the desktop app, against a real core: alt+m, the
//! wheel over the composer's model button, its picker (which adds a model
//! typed into it), and /model.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_gui::{GuiOptions, Headless, Key, Modifiers};
use bone_server::Server;

fn settings(config: &Path) -> serde_json::Value {
    std::fs::read_to_string(config.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Run the input queued for the next frame, then what it started.
async fn run(g: &mut Headless) {
    g.frame();
    g.settle(Duration::from_millis(300)).await;
}

/// The middle of the composer's model button.
fn button(g: &mut Headless) -> (f32, f32) {
    let at = g
        .gui
        .eval("(function() local r = bone.gui.rect('composer-model'); return r.x + r.w / 2 .. ',' .. r.y + r.h / 2 end)()")
        .unwrap();
    let (x, y) = at.split_once(',').unwrap();
    (x.parse().unwrap(), y.parse().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn models_switch_from_keys_wheel_picker_and_command() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let config = config.path();
    std::fs::write(config.join("gui-library"), "standard\n").unwrap();
    std::fs::write(
        config.join("core.lua"),
        r#"bone.config.providers.alpha = { base_url = "http://127.0.0.1:9/v1", model = "a1", models = { "a1", "a2", "a3" } }
bone.config.providers.beta = { base_url = "http://127.0.0.1:9/v1", model = "b1" }
bone.config.provider = "alpha""#,
    )
    .unwrap();
    let loaded = scripting::load_with(config, &|_| None).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let opts = GuiOptions {
        cwd: work.path().to_string_lossy().into_owned(),
        reconnect: None,
        config_dir: Some(config.to_owned()),
    };
    let mut g = Headless::start(server.connect_in_process(), opts, 1000.0, 700.0)
        .await
        .unwrap();
    let alpha = |config: &Path| settings(config)["providers"]["alpha"].clone();
    for _ in 0..100 {
        if g.gui
            .eval("bone.gui.rect('composer-model') ~= nil")
            .unwrap()
            == "true"
        {
            break;
        }
        g.settle(Duration::from_millis(50)).await;
    }

    // alt+m: core.lua's next model.
    g.key(
        Key::Char('m'),
        Modifiers {
            alt: true,
            ..Modifiers::NONE
        },
    );
    run(&mut g).await;
    assert_eq!(alpha(config)["model"], "a2", "alt+m");

    // The wheel over the button: down the next, up the one before.
    let (x, y) = button(&mut g);
    g.hover_at(x, y);
    g.wheel(0.0, -60.0);
    run(&mut g).await;
    assert_eq!(alpha(config)["model"], "a3", "wheel down");
    g.hover_at(x, y);
    g.wheel(0.0, 60.0);
    run(&mut g).await;
    assert_eq!(alpha(config)["model"], "a2", "wheel up");

    // A press opens the picker; a name that is not listed is added.
    let (x, y) = button(&mut g);
    g.click_at(x, y);
    run(&mut g).await;
    g.text("a9");
    run(&mut g).await;
    g.key(Key::Enter, Modifiers::NONE);
    run(&mut g).await;
    assert_eq!(
        alpha(config),
        serde_json::json!({ "model": "a9", "models": ["a1", "a2", "a3", "a9"] })
    );

    // /model rm takes one off (not the one in use); /model NAME adds.
    g.gui.run_lua("t", "bone.cmd('/model rm a2')").unwrap();
    run(&mut g).await;
    g.gui.run_lua("t", "bone.cmd('/model rm a9')").unwrap();
    run(&mut g).await;
    assert_eq!(
        alpha(config)["models"],
        serde_json::json!(["a1", "a3", "a9"])
    );
    g.gui.run_lua("t", "bone.cmd('/model a5')").unwrap();
    run(&mut g).await;
    assert_eq!(
        alpha(config),
        serde_json::json!({ "model": "a5", "models": ["a1", "a3", "a9", "a5"] })
    );

    // Another provider's model, from the picker: that provider is chosen.
    let (x, y) = button(&mut g);
    g.click_at(x, y);
    run(&mut g).await;
    g.text("b1");
    run(&mut g).await;
    g.key(Key::Enter, Modifiers::NONE);
    run(&mut g).await;
    assert_eq!(settings(config)["provider"], "beta");
}
