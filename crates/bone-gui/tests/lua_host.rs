//! The GUI's Lua host: what it loads and in which order, the bare library,
//! and reloading with the TUI's rule (a reload with a new error keeps the
//! configuration in use).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_gui::{GuiOptions, Headless};
use bone_server::Server;

async fn gui(config: &Path, work: &Path) -> Headless {
    std::fs::write(
        config.join("core.lua"),
        r#"bone.config.providers.fake = { base_url = "http://127.0.0.1:9", model = "fake" }"#,
    )
    .unwrap();
    let loaded = scripting::load_with(config, &|_| None).unwrap();
    let server = Server::new(Arc::new(Core::from_loaded(loaded)));
    let opts = GuiOptions {
        cwd: work.to_string_lossy().into_owned(),
        reconnect: None,
        config_dir: Some(config.to_owned()),
    };
    Headless::start(server.connect_in_process(), opts, 1000.0, 700.0)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_then_the_user_ui_lua_then_gui_lua() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let c = config.path();
    std::fs::create_dir_all(c.join("plugins/p")).unwrap();
    std::fs::write(
        c.join("plugins/p/ui.lua"),
        "order = (order or '') .. 'P-ui '",
    )
    .unwrap();
    std::fs::write(
        c.join("plugins/p/gui.lua"),
        "order = (order or '') .. 'P-gui '",
    )
    .unwrap();
    std::fs::write(c.join("plugins/p/tui.lua"), "error('the TUI half')").unwrap();
    std::fs::write(c.join("ui.lua"), "order = (order or '') .. 'ui '").unwrap();
    std::fs::write(c.join("gui.lua"), "order = (order or '') .. 'gui '").unwrap();
    std::fs::write(c.join("tui.lua"), "error('the TUI config')").unwrap();
    let mut g = gui(c, work.path()).await;
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    g.gui
        .run_lua("t", "assert(order == 'P-ui P-gui ui gui ', order)")
        .unwrap();
    g.gui
        .run_lua("t", "assert(bone.api_info().side == 'gui')")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bare_library_draws_plain_text() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(config.path().join("library"), "bare\n").unwrap();
    std::fs::write(
        config.path().join("gui.lua"),
        "error('not loaded with bare')",
    )
    .unwrap();
    let mut g = gui(config.path(), work.path()).await;
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    g.gui
        .run_lua("t", "assert(next(bone.ui.views) == nil, 'no views')")
        .unwrap();
    g.gui
        .run_lua("t", "bone.chat.add('note', { text = 'plain words' })")
        .unwrap();
    g.frame();
    assert_eq!(g.gui.transcript(), "plain words");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reload_with_a_new_error_keeps_the_config_in_use() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let file = config.path().join("gui.lua");
    std::fs::write(&file, "bone.o.define('conf_n', 1)\nmarker = 'first'").unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.gui.run_lua("t", "bone.o.conf_n = 5").unwrap();

    std::fs::write(&file, "bone.o.define('conf_n', 1)\nmarker = ").unwrap();
    assert!(!g.gui.reload_lua(), "a syntax error is not taken");
    g.gui
        .run_lua("t", "assert(marker == 'first' and bone.o.conf_n == 5)")
        .unwrap();

    std::fs::write(&file, "bone.o.define('conf_n', 1)\nmarker = 'second'").unwrap();
    assert!(g.gui.reload_lua());
    g.gui
        .run_lua("t", "assert(marker == 'second', marker)")
        .unwrap();
    g.gui
        .run_lua("t", "assert(bone.o.conf_n == 5, 'the value carries over')")
        .unwrap();

    // An edit is picked up by itself once it has settled.
    std::fs::write(&file, "bone.o.define('conf_n', 1)\nmarker = 'third'").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !g.gui.watch_config() {
        assert!(tokio::time::Instant::now() < deadline, "edit not noticed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    g.gui
        .run_lua("t", "assert(marker == 'third', marker)")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_reach_lua_and_typing_reaches_the_prompt() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(
        config.path().join("gui.lua"),
        "bone.keymap.set('ctrl+k', function() pressed = (pressed or 0) + 1 end)",
    )
    .unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();

    // A mapped key goes to Lua, not the text box.
    g.key(Key::K, Modifiers::CTRL);
    g.frame();
    g.gui
        .run_lua("t", "assert(pressed == 1, tostring(pressed))")
        .unwrap();
    assert_eq!(g.gui.draft, "");

    // Typing is the text box's.
    g.text("hello");
    g.frame();
    assert_eq!(g.gui.draft, "hello");
    // An editing key stays native: backspace deletes in the text box.
    g.key(Key::Backspace, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.draft, "hell");

    // Enter is the standard keymap's submit.
    g.key(Key::Enter, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.draft, "");
    assert!(g.gui.running(), "submitted");
}

/// The standard library's / menu: a region above the prompt that typing
/// "/" fills and esc hides, as in the TUI.
#[tokio::test(flavor = "multi_thread")]
async fn the_slash_menu_opens_and_esc_hides_it() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    let menu = |g: &Headless| {
        (g.gui.lua.geometry.leaves.iter())
            .find(|(n, _)| n == "above_prompt")
            .map_or(0, |(_, r)| r.height)
    };
    g.frame();
    assert_eq!(menu(&g), 0);
    g.text("/");
    g.frame();
    g.frame();
    assert!(menu(&g) > 0, "typing / opens the menu");
    g.key(Key::Escape, Modifiers::NONE);
    g.frame();
    g.frame();
    assert_eq!(menu(&g), 0, "esc hides it");
    assert_eq!(g.gui.draft, "/", "and keeps the text");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// ctrl+o opens the standard sessions panel, which takes the keyboard; esc
/// gives it back to the prompt.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_o_opens_the_sessions_panel() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.key(Key::O, Modifiers::CTRL);
    g.frame();
    g.frame();
    let p = g
        .gui
        .lua
        .panels
        .iter()
        .find(|p| p.id == "sessions")
        .expect("open");
    assert!(p.area.is_some_and(|a| a.width > 0), "drawn");
    assert_eq!(g.gui.lua.panel_focus.as_deref(), Some("sessions"));
    g.key(Key::Escape, Modifiers::NONE);
    g.frame();
    assert_ne!(
        g.gui.lua.panel_focus.as_deref(),
        Some("sessions"),
        "esc leaves it"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A click reaches Lua's mouse handlers with what is under it, and focuses
/// the panel it lands on.
#[tokio::test(flavor = "multi_thread")]
async fn clicks_reach_lua_with_what_is_under_them() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(
        config.path().join("gui.lua"),
        r#"
        clicks = {}
        bone.on("mouse", function(ev)
          if ev.action == "down" then
            clicks[#clicks + 1] = (ev.region or "-") .. ":" .. tostring(ev.panel)
          end
        end)
        bone.ui.panel.open({ id = "conf_click", dock = "right", size = 10, lines = { "x" } })
        "#,
    )
    .unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    let prompt = (g.gui.lua.geometry.leaves.iter())
        .find(|(n, _)| n == "prompt")
        .map(|(_, r)| *r)
        .unwrap();
    g.click(prompt.x + 2, prompt.y + 1);
    g.frame();
    let p = g.gui.lua.panels[0].area.unwrap();
    g.click(p.x + 1, p.y);
    g.frame();
    g.gui
        .run_lua("t", "assert(clicks[1] == 'prompt:nil', clicks[1])")
        .unwrap();
    g.gui
        .run_lua("t", "assert(clicks[2] == '-:conf_click', clicks[2])")
        .unwrap();
    assert_eq!(g.gui.lua.panel_focus.as_deref(), Some("conf_click"));
}

/// bone.o.font_size sets the size of the character cells.
#[tokio::test(flavor = "multi_thread")]
async fn the_font_size_option_sizes_the_cells() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.frame();
    let (_, _, col, row) = g.gui.lua.cells.unwrap();
    g.gui.run_lua("t", "bone.o.font_size = 28").unwrap();
    g.frame();
    g.frame();
    let (_, _, col2, row2) = g.gui.lua.cells.unwrap();
    assert!(
        col2 > col * 1.5 && row2 > row * 1.5,
        "{col}x{row} -> {col2}x{row2}"
    );
    g.gui
        .run_lua("t", "bone.o.font = '/no/such/font.ttf'")
        .unwrap();
    g.frame();
    assert!(
        g.gui.status.as_ref().is_some_and(|s| s.error),
        "a missing font is reported"
    );
}

/// /health opens its page with the GUI's own checks.
#[tokio::test(flavor = "multi_thread")]
async fn health_shows_the_guis_checks() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    let names = g
        .gui
        .eval("(function() local n = {} for _, it in ipairs(bone.api.health()) do n[#n + 1] = it.name end return table.concat(n, ',') end)()")
        .unwrap();
    assert_eq!(names, "renderer,connection,lua,plugins,font");
    g.gui.run_lua("t", "bone.cmd('/health')").unwrap();
    g.settle(std::time::Duration::from_millis(100)).await;
    let open = !g.gui.lua.popups.is_empty() || !g.gui.lua.panels.is_empty();
    assert!(open, "a page opened");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}
