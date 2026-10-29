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

/// A headless GUI with the shared defaults (unless a test chose a library).
async fn gui(config: &Path, work: &Path) -> Headless {
    std::fs::write(
        config.join("core.lua"),
        r#"bone.config.providers.fake = { base_url = "http://127.0.0.1:9", model = "fake" }"#,
    )
    .unwrap();
    if !config.join("gui-library").exists() && !config.join("library").exists() {
        std::fs::write(config.join("gui-library"), "standard\n").unwrap();
    }
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
    g.frame();
    assert!(g.painted.iter().any(|(t, _)| t.contains("plain words")));

    // The basic keys and the slash commands: enter runs a command, and
    // sends anything else.
    use eframe::egui::{Key, Modifiers};
    g.gui
        .run_lua("t", "bone.cmd.create('ping', function() pinged = true end)")
        .unwrap();
    g.text("/ping");
    g.frame();
    g.key(Key::Enter, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.eval("pinged").unwrap(), "true");
    assert_eq!(g.gui.draft, "");
    g.text("hello");
    g.frame();
    g.key(Key::Enter, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.draft, "");
    assert!(g.gui.running(), "submitted");
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

    // The first focus only records the sources. An edit is noticed
    // when the window comes back to the front.
    g.frame();
    g.settle(Duration::from_millis(100)).await;
    std::fs::write(&file, "bone.o.define('conf_n', 1)\nmarker = 'third'").unwrap();
    g.set_focused(false);
    g.frame();
    g.set_focused(true);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while g.gui.eval("marker").unwrap() != "third" {
        assert!(tokio::time::Instant::now() < deadline, "edit not noticed");
        g.settle(Duration::from_millis(50)).await;
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

/// "/" fills and esc hides, as in the TUI.
#[tokio::test(flavor = "multi_thread")]
async fn the_slash_menu_opens_and_esc_hides_it() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    let menu = |g: &Headless| {
        g.painted
            .iter()
            .any(|(t, _)| t.len() > 1 && t.len() < 20 && t.starts_with('/'))
    };
    g.frame();
    assert!(!menu(&g));
    g.text("/");
    g.frame();
    g.frame();
    assert!(menu(&g), "typing / opens the menu");
    g.key(Key::Escape, Modifiers::NONE);
    g.frame();
    g.frame();
    assert!(!menu(&g), "esc hides it");
    assert_eq!(g.gui.draft, "/", "and keeps the text");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// ctrl+o opens the sessions sidebar, which takes the keyboard; esc gives
/// it back to the prompt.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_o_opens_the_sessions_sidebar() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.key(Key::O, Modifiers::CTRL);
    g.frame();
    g.frame();
    assert!(g.painted.iter().any(|(t, _)| t == "Processes"), "drawn");
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.focused()").unwrap(),
        "sidebar-search",
        "its search field has the keyboard"
    );
    g.key(Key::Escape, Modifiers::NONE);
    g.frame();
    g.frame();
    assert_eq!(
        g.gui.eval("tostring(bone.gui.focused())").unwrap(),
        "nil",
        "esc leaves it"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A click reaches the handler of the node under it, and only that one;
/// the pointer over a node calls its hover handler.
#[tokio::test(flavor = "multi_thread")]
async fn clicks_reach_the_node_under_them() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(
        config.path().join("gui.lua"),
        r##"
        clicks = {}
        bone.gui.view(function()
          return { direction = "row",
            { width = 100, height = 100, bg = "#222", on_click = function(p) clicks[#clicks + 1] = "a" .. math.floor(p.x) end },
            { width = 100, height = 100, bg = "#333", on_click = function() clicks[#clicks + 1] = "b" end,
              on_hover = function(on) hovered = on end },
          }
        end)
        "##,
    )
    .unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.frame();
    g.click_at(30.0, 50.0);
    g.frame();
    g.click_at(150.0, 50.0);
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("table.concat(clicks, ',')").unwrap(), "a30,b");
    assert_eq!(g.gui.eval("hovered").unwrap(), "true");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// bone.o.font_size sets the size of the character cells.
#[tokio::test(flavor = "multi_thread")]
async fn the_font_size_option_sizes_the_cells() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.frame();
    let (col, row) = g.gui.view.cell.unwrap();
    g.gui.run_lua("t", "bone.o.font_size = 28").unwrap();
    g.frame();
    g.frame();
    let (col2, row2) = g.gui.view.cell.unwrap();
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

/// bone.gui: the window, only in the GUI.
#[tokio::test(flavor = "multi_thread")]
async fn lua_sets_the_window() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            "bone.gui.window({ title = 'conf title', maximized = true })",
        )
        .unwrap();
    g.frame();
    assert_eq!(g.gui.window_title.as_deref(), Some("conf title"));
    let w = g.gui.eval("bone.gui.window_info().width").unwrap();
    assert_eq!(w, "1000");
    g.frame();
}

/// pageup leaves the end of a long transcript; ctrl+end goes back.
#[tokio::test(flavor = "multi_thread")]
async fn page_keys_scroll_the_chat() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    let text = "row\n".repeat(120);
    g.gui
        .run_lua(
            "t",
            &format!("bone.chat.add('note', {{ text = [[{text}]] }})"),
        )
        .unwrap();
    for _ in 0..4 {
        g.frame();
    }
    let offset = |g: &mut Headless| {
        g.gui
            .eval("bone.gui.scroll_info('chat').offset")
            .unwrap()
            .parse::<f32>()
            .unwrap()
    };
    let max = g
        .gui
        .eval("bone.gui.scroll_info('chat').max")
        .unwrap()
        .parse::<f32>()
        .unwrap();
    assert!(
        max > 200.0 && offset(&mut g) == max,
        "at the end: {} of {max}",
        offset(&mut g)
    );
    g.key(Key::PageUp, Modifiers::NONE);
    g.frame();
    g.frame();
    assert!(offset(&mut g) < max, "pageup stayed at {max}");
    g.key(Key::End, Modifiers::CTRL);
    g.frame();
    g.frame();
    assert_eq!(offset(&mut g), max, "ctrl+end did not return");
}

/// A narrower window has fewer columns and fires `resize` (in columns and
/// rows, as in the TUI); a HiDPI display keeps the layout, which is in
/// logical units.
#[tokio::test(flavor = "multi_thread")]
async fn resizing_and_hidpi() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(
        config.path().join("gui.lua"),
        "bone.on('resize', function(ev) resized = ev.width .. 'x' .. ev.height end)",
    )
    .unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.frame();
    let cols = g.gui.view.screen_cols;

    g.resize(500.0, 700.0);
    g.frame();
    let narrow = g.gui.view.screen_cols;
    assert!(narrow < cols * 6 / 10, "{cols} -> {narrow} columns");
    let resized = g.gui.eval("resized").unwrap();
    assert_eq!(resized, format!("{narrow}x{}", g.gui.view.screen_size.1));

    g.set_scale(2.0);
    g.frame();
    g.frame();
    assert_eq!(
        g.gui.view.screen_cols, narrow,
        "the same columns on a 2x display"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// Images join the prompt from `/attach` and from a file dropped on the
/// window, Lua sees them, and they go out with the message; Lua's copy
/// reaches the clipboard.
#[tokio::test(flavor = "multi_thread")]
async fn images_in_the_prompt_and_the_clipboard() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let png = bone_media::from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
    std::fs::write(work.path().join("a.png"), &png.bytes).unwrap();
    std::fs::write(work.path().join("b.png"), &png.bytes).unwrap();
    std::fs::write(
        config.path().join("gui.lua"),
        "bone.on('prompt/changed', function(ev) seen = #ev.images end)",
    )
    .unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui.run_lua("t", "bone.cmd('attach a.png')").unwrap();
    g.drop_file(work.path().join("b.png"));
    let two = |gui: &bone_gui::Gui| gui.images.len() == 2;
    g.wait_for(Duration::from_secs(5), two).await.unwrap();
    assert_eq!(g.gui.eval("#bone.prompt.images()").unwrap(), "2");
    assert_eq!(g.gui.eval("seen").unwrap(), "2");
    // Both reads run at once, so either may come first.
    let mut names: Vec<_> = g.gui.images.iter().map(|i| i.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["a.png", "b.png"]);

    g.gui.run_lua("t", "bone.cmd('detach 1')").unwrap();
    assert_eq!(g.gui.images.len(), 1);
    let left = g.gui.images[0].name.clone();
    // An image alone is a message.
    g.gui.submit();
    assert!(g.gui.images.is_empty());
    let sent = |gui: &bone_gui::Gui| {
        let data = gui.chats[0]
            .items()
            .first()
            .map(|&i| gui.chats[0].item_data(i, 1, true));
        data.is_some_and(|d| d["images"][0]["name"] == left.as_str())
    };
    g.wait_for(Duration::from_secs(5), sent).await.unwrap();

    // Its row in the chat opens it when clicked.
    let id = g.gui.chats[0].item_data(g.gui.chats[0].items()[0], 1, true)["images"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for _ in 0..4 {
        g.frame();
    }
    let at = (g.painted.iter())
        .find(|(t, _)| t.contains(&format!("[Image: {left}")))
        .expect("the image's row")
        .1;
    g.click_at(at.center().x, at.center().y);
    let shown = |gui: &bone_gui::Gui| gui.image_shown(&id);
    g.wait_for(Duration::from_secs(5), shown).await.unwrap();

    // Not an image: an error, and nothing attached.
    std::fs::write(work.path().join("c.png"), "not a png").unwrap();
    g.gui.run_lua("t", "bone.prompt.attach('c.png')").unwrap();
    g.settle(Duration::from_millis(100)).await;
    assert!(g.gui.images.is_empty());

    g.gui
        .run_lua("t", "bone.clipboard.copy('copied text')")
        .unwrap();
    g.frame();
    assert_eq!(g.copied.as_deref(), Some("copied text"));
}

/// With no GUI library chosen, `/tui use` loads the UI again with the
/// library chosen, and one without GUI files (no `gui/` folder) leaves the
/// GUI its own (standard).
#[tokio::test(flavor = "multi_thread")]
async fn choosing_a_library_reloads_the_gui() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // No GUI library chosen: the GUI follows /tui where it can.
    std::fs::write(config.path().join("gui-library"), "").unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui.run_lua("t", "bone.library.use('bare')").unwrap();
    g.frame();
    assert_eq!(g.gui.eval("next(bone.ui.views) == nil").unwrap(), "true");
    assert_eq!(g.gui.eval("bone.library.info().drawn").unwrap(), "bare");

    // A library made for the terminal only (made before libraries had GUI
    // files, or with them deleted): chosen, but the GUI stays standard.
    g.gui.run_lua("t", "bone.library.create('mine')").unwrap();
    std::fs::remove_dir_all(config.path().join("libraries/mine/gui")).unwrap();
    g.gui.run_lua("t", "bone.library.use('mine')").unwrap();
    g.frame();
    assert_eq!(g.gui.eval("bone.library.info().active").unwrap(), "mine");
    assert_eq!(g.gui.eval("bone.library.info().drawn").unwrap(), "standard");
    assert_eq!(
        g.gui.eval("bone.ui.views.assistant ~= nil").unwrap(),
        "true"
    );

    // With a gui/ folder it is the GUI's too.
    let lib = config.path().join("libraries/mine/gui");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("defaults.lua"), "mine_loaded = true").unwrap();
    g.gui.run_lua("t", "bone.library.use('mine')").unwrap();
    g.frame();
    assert_eq!(g.gui.eval("mine_loaded").unwrap(), "true");
    assert_eq!(g.gui.eval("bone.library.info().drawn").unwrap(), "mine");
}

/// A view's `node` is rich text the GUI wraps itself; its `lines` are for
/// a frontend that does not draw nodes.
#[tokio::test(flavor = "multi_thread")]
async fn a_view_node_is_wrapped_rich_text() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    let long = "word ".repeat(200);
    g.gui
        .run_lua(
            "t",
            &format!(
                r#"bone.ui.views.conf_node = function(item)
                  return {{ node = {{ direction = "row", justify = "end",
                      {{ bg = "UserMessage", radius = 12, padding = {{ 8, 12 }}, max_width = "80%",
                        {{ spans = {{ {{ item.text, "Normal" }} }} }}, {{ text = "second", margin = {{ 6, 0, 0, 0 }} }} }},
                    }}, lines = {{ {{ {{ "fallback", "Normal" }} }} }} }}
                end
                bone.chat.add("conf_node", {{ text = "{long}" }})"#
            ),
        )
        .unwrap();
    for _ in 0..4 {
        g.frame();
    }
    let long = g
        .painted
        .iter()
        .find(|(t, _)| t.contains("word word"))
        .expect("the paragraph")
        .1;
    assert!(
        long.height() > 60.0,
        "a long paragraph wraps over several lines: {long:?}"
    );
    assert!(long.width() <= 0.8 * 1000.0, "within max_width: {long:?}");
    assert!(g.painted.iter().any(|(t, _)| t.contains("second")));
    assert!(
        !g.painted.iter().any(|(t, _)| t.contains("fallback")),
        "the GUI draws the node, not the lines"
    );
}

/// The GUI draws with the standard library unless another is chosen, and
/// `/gui use` chooses apart from the TUI's choice: its views draw nodes,
/// and it loads cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn the_standard_library() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // No choice at all: standard.
    std::fs::write(config.path().join("gui-library"), "").unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.library.info().active").unwrap(),
        "standard"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    std::fs::remove_file(config.path().join("gui-library")).unwrap();
    g.gui.run_lua("t", "bone.cmd('gui use standard')").unwrap();
    g.frame();
    g.gui.run_lua("t", "bone.cmd('gui use bare')").unwrap();
    g.frame();
    g.gui.run_lua("t", "bone.cmd('gui use standard')").unwrap();
    g.frame();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    assert_eq!(
        g.gui.eval("bone.gui.library.info().active").unwrap(),
        "standard"
    );
    assert!(
        !config.path().join("library").exists(),
        "the TUI's choice is untouched"
    );
    g.gui
        .run_lua(
            "t",
            "bone.chat.add('note', { text = 'x' })
             local v = bone.ui.views.user({ kind = 'user', text = 'hello there' }, { width = 60 })
             assert(v.node and v.node.justify == 'end', 'a bubble on the right')
             assert(v.lines and #v.lines > 0, 'lines for the TUI')
             local a = bone.ui.views.assistant({ kind = 'assistant', text = '# Title\\n\\nSome **bold** text.\\n\\n- one\\n- two\\n\\n```\\ncode\\n```' }, { width = 60 })
             assert(#a.node == 5, 'heading, text, two items, code: ' .. #a.node)",
        )
        .unwrap();
    // Apart from the TUI's looks: none of its drawing modules are loaded,
    // and every part draws without an error.
    for m in [
        "bone.ui",
        "bone.ui.views",
        "bone.ui.layout",
        "bone.ui.statusline",
        "bone.ui.tray",
        "bone.ui.sessions",
        "bone.ui.turn_timing",
    ] {
        let gone = g
            .gui
            .eval(&format!("package.loaded['{m}'] == nil"))
            .unwrap();
        assert_eq!(gone, "true", "{m} is the TUI's");
    }
    g.gui
        .run_lua(
            "t",
            "bone.chat.add('note', { text = 'x' })
             local t = bone.ui.views.tool({ kind = 'tool', id = 'c1', name = 'shell', arguments = { command = 'ls -la' },
               done = true, output = 'a\\nb\\nc', is_error = false }, { width = 60 })
             assert(t.node[1][1][2].spans[1].text == 'Ran', 'a card titled by what it did')
             local e = bone.ui.views.tool({ kind = 'tool', id = 'c2', name = 'edit_file',
               arguments = { path = 'x.rs', old_string = 'a', new_string = 'b' }, done = true }, { width = 60 })
             local card = e.node[1]
             assert(#e.node == 1 and #card == 3, 'one card: its line, a hairline and one code block, not a band per line')
             assert(card.bg == 'CodeBlock' and card.radius == 8, 'one dark rounded card')
             local block = card[3]
             assert(block[1].radius == nil and block[1].bg == 'DiffDelete', 'a flat highlighted removal')
             assert(block[2].bg == 'DiffAdd' and block[2][1].text == '  + b', 'the addition')
             local function edit(id, old, new)
               return bone.ui.views.tool({ kind = 'tool', id = id, name = 'edit_file', done = true,
                 arguments = { path = 'x.rs', old_string = old, new_string = new } }, { width = 80 }).node[1][3]
             end
             local ctx = edit('c3', 'fn a()\\n  keep\\n  old\\n  tail\\nend', 'fn a()\\n  keep\\n  new\\n  tail\\nend')
             assert(#ctx == 6, 'context around the change, got ' .. #ctx)
             assert(ctx[1][1].hl == 'Dim' and ctx[1].bg == nil and ctx[1][1].text == '    fn a()', 'grey context')
             assert(ctx[2][1].text == '      keep' and ctx[2][1].hl == 'Dim', 'grey context')
             assert(ctx[3].bg == 'DiffDelete' and ctx[3][1].text == '  -   old', 'highlighted removal')
             assert(ctx[4].bg == 'DiffAdd' and ctx[4][1].text == '  +   new', 'highlighted addition')
             assert(ctx[5][1].hl == 'Dim' and ctx[6][1].text == '    end', 'grey context after')
             local mid = edit('c4', 'keep\\nold\\nmid\\nold2\\nkeep2', 'keep\\nnew\\nmid\\nnew2\\nkeep2')
             assert(#mid == 7 and mid[4][1].hl == 'Dim' and mid[4][1].text == '    mid', 'unchanged line once, in grey')
             assert(mid[3].bg == 'DiffAdd' and mid[5].bg == 'DiffDelete', 'the two changes stay highlighted')
             local far = edit('c5',
               table.concat({'a','b','c','d','e','f','g','h','i','j','k','l','m','n','o'}, '\\n'),
               table.concat({'a','B','c','d','e','f','g','h','i','j','k','l','m','N','o'}, '\\n'))
             assert(far[7][1].text == '···' and far[7][1].hl == 'Dim' and far[7].bg == nil, 'a gap in the context')
             assert(far[6][1].text == '    e' and far[8][1].text == '    k', 'context resumes after the gap')
             bone.ui.sessions()",
        )
        .unwrap();
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("bone.gui.focused()").unwrap(), "sidebar-search");

    // Settings, help and health are the GUI's own windows: each opens,
    // draws, takes its keys and closes without an error.
    for (open, keys) in [
        ("require('bone.config').open()", "down down tab esc"),
        ("bone.ui.help()", "tab down enter esc esc"),
        ("bone.ui.health()", "esc"),
    ] {
        g.gui.run_lua("t", &format!("win = {open}")).unwrap();
        g.frame();
        g.frame();
        assert_eq!(
            g.gui.eval("bone.ui.is_open(win)").unwrap(),
            "true",
            "{open}"
        );
        g.gui
            .run_lua(
                "t",
                &format!("for k in ('{keys}'):gmatch('%S+') do bone.press(k) end"),
            )
            .unwrap();
        g.frame();
        assert!(
            g.gui.lua_errors().is_empty(),
            "{open}: {:?}",
            g.gui.lua_errors()
        );
        let shown = (g.gui.status.as_ref()).map_or(String::new(), |s| s.text.clone());
        assert!(!shown.contains("failed"), "{open}: {shown}");
    }
    assert_eq!(
        g.gui
            .eval("require('bone.config') == require('bone.desktop.settings')")
            .unwrap(),
        "true"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    let shown = (g.gui.status.as_ref()).map_or(String::new(), |s| s.text.clone());
    assert!(!shown.contains("failed"), "{shown}");

    // `standard` is the same window: the GUI has no look of the TUI's.
    g.gui.run_lua("t", "bone.cmd('gui use standard')").unwrap();
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.library.info().active").unwrap(),
        "standard"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_window_receives_arrow_keys() {
    use eframe::egui::{Key, Modifiers};

    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            "local pages = require('bone.settings_page')
             local new = pages.new
             pages.new = function(...)
               local page = new(...)
               settings_state = page.st
               return page
             end
             win = require('bone.config').open()",
        )
        .unwrap();
    g.frame();
    g.frame();
    for (key, expected) in [
        (Key::ArrowDown, "1:2"),
        (Key::ArrowUp, "1:1"),
        (Key::ArrowRight, "2:1"),
        (Key::ArrowLeft, "1:1"),
    ] {
        g.key(key, Modifiers::NONE);
        g.frame();
        assert_eq!(
            g.gui
                .eval("settings_state.tab .. ':' .. settings_state.sel")
                .unwrap(),
            expected,
            "{key:?} should navigate settings"
        );
    }
    g.key(Key::Escape, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.eval("bone.ui.is_open(win)").unwrap(), "false");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// What the node view draws reaches screen readers: chat text and the
/// text painted in boxes.
#[tokio::test(flavor = "multi_thread")]
async fn lua_drawn_rows_are_accessible() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.enable_accesskit();
    g.gui
        .run_lua(
            "t",
            "bone.chat.add('note', { text = 'a chat row' })
             bone.gui.view(function() return { { text = 'conf status words' } } end)",
        )
        .unwrap();
    g.frame();
    g.frame();
    let all = g.accessible.join(" | ");
    assert!(all.contains("conf status words"), "painted text: {all}");
}

/// `/term` opens a terminal in a panel: the program's screen draws there,
/// and the keys typed while it has the keyboard go to the program.
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_in_a_panel() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui.run_lua("t", "bone.cmd('term cat')").unwrap();
    g.frame();
    assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "term1");
    g.text("typed into it");
    g.key(eframe::egui::Key::Enter, eframe::egui::Modifiers::NONE);
    let screen = |g: &mut Headless| {
        g.gui
            .eval(
                "(function() local s = require('bone.terminal').job('term1'):screen()
                  local t = {} for _, l in ipairs(s and s.lines or {}) do
                    for _, i in ipairs(l) do t[#t + 1] = i[1] end t[#t + 1] = '\\n' end
                  return table.concat(t) end)()",
            )
            .unwrap()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        g.settle(Duration::from_millis(30)).await;
        let s = screen(&mut g);
        // cat echoes the line back: the typed text shows twice.
        let painted = g
            .painted
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if s.matches("typed into it").count() >= 2 && painted.matches("typed into it").count() >= 2
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the terminal screen: {s}; actually painted: {painted}"
        );
    }
    assert_eq!(
        g.gui.draft, "",
        "the keys went to the terminal, not the prompt"
    );
    g.gui.run_lua("t", r#"
        local window = require('bone.nodes.window')
        local cell = bone.gui.window_info().cell
        local function cursor()
          local tree = window.panel('term1', { width = 400, height = 180, cell = cell })
          for _, row in ipairs(tree[1][1]) do
            assert(row.font == 'mono' and row.height == cell.h and row.line_height == cell.h and not row.wrap)
            for _, span in ipairs(row.spans) do
              if type(span) == 'table' and span.hl == 'TermCursor' then
                local normal = bone.hl.get('Normal') or {}
                assert(span.bg == (normal.fg or '#eeeeee') and span.color == (normal.bg or '#161616'),
                  'node spans need explicit reversed colors for a visible cursor')
                return true
              end
            end
          end
          return false
        end
        assert(cursor(), 'focused terminal cursor')
        bone.ui.panel.focus(nil)
        assert(not cursor(), 'no cursor without keyboard focus')
    "#).unwrap();
    // A click in the terminal gives its existing panel the keyboard again.
    g.frame();
    let terminal = g
        .painted
        .iter()
        .find(|(text, _)| text.contains("typed into it"))
        .unwrap()
        .1;
    g.click_at(terminal.center().x, terminal.center().y);
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "term1");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    g.gui.run_lua("t", "bone.cmd('term close')").unwrap();
    g.frame();
    assert_eq!(
        g.gui.eval("bone.ui.panel.get('term1') == nil").unwrap(),
        "true"
    );
}

/// Both spellings of the terminal shortcut open, release and refocus the
/// same visible panel, through the GUI's real keyboard path.
#[tokio::test(flavor = "multi_thread")]
async fn terminal_shortcuts_focus_the_visible_panel() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    for key in [Key::CloseBracket, Key::Num5] {
        g.key(key, Modifiers::CTRL);
        g.frame();
        g.frame();
        assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "term1");
        g.gui
            .run_lua(
                "t",
                r#"
            require('bone.terminal').job('term1'):write("printf 'shortcut-visible\\n'\r")
        "#,
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            g.settle(Duration::from_millis(30)).await;
            if g.painted
                .iter()
                .any(|(text, _)| text.contains("shortcut-visible"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "terminal text was not painted"
            );
        }
        g.key(key, Modifiers::CTRL);
        g.frame();
        assert_eq!(
            g.gui.eval("bone.ui.panel.focused() == nil").unwrap(),
            "true"
        );
        g.key(key, Modifiers::CTRL);
        g.frame();
        assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "term1");
        assert_eq!(g.gui.eval("#bone.ui.panel.list()").unwrap(), "1");
        g.gui.run_lua("t", "bone.cmd('term close')").unwrap();
        g.frame();
    }
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// winit can emit both an Alt-modified key and its text. The text must
/// not become the second key of a window-manager prefix (Alt+W then W).
#[tokio::test(flavor = "multi_thread")]
async fn alt_prefix_does_not_consume_its_own_text() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui.run_lua("t", r#"
        cycled = 0
        local term = require('bone.terminal')
        term.passthrough['alt+w'] = true
        for _, context in ipairs({ 'main', 'panel' }) do
          bone.keymap.set('alt+w w', function() cycled = cycled + 1 end, { context = context })
          bone.keymap.set('alt+w t', function() term.open(nil, { cmd = 'cat' }) end, { context = context })
        end
    "#).unwrap();
    for panel_count in 1..=2 {
        g.key(Key::W, Modifiers::ALT);
        g.text("w");
        g.frame();
        assert_eq!(g.gui.eval("cycled").unwrap(), "0");
        g.text("t");
        g.frame();
        assert_eq!(
            g.gui.eval("#bone.ui.panel.list()").unwrap(),
            panel_count.to_string()
        );
    }
    // An explicit second W still resolves the cycle binding.
    g.key(Key::W, Modifiers::ALT);
    g.text("w");
    g.frame();
    g.text("w");
    g.frame();
    assert_eq!(g.gui.eval("cycled").unwrap(), "1");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// Every dock renders node content; fractions size the dock in pixels,
/// while top/bottom cell sizes use rows rather than columns.
#[tokio::test(flavor = "multi_thread")]
async fn node_panels_on_all_four_docks() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            r#"
        for _, side in ipairs({ 'left', 'right', 'top', 'bottom' }) do
          local dock = side
          bone.ui.panel.open({ id = dock, dock = dock, size = 0.2,
            node = function(ctx)
              assert(ctx.width > 0 and ctx.height > 0 and ctx.cell)
              return { text = 'dock-' .. dock }
            end })
        end
        local window = require('bone.nodes.window')
        local ctx = { width = 1000, height = 700, cell = { w = 8, h = 18 } }
        assert(window.docked('left', ctx)[1].width == 200)
        assert(window.docked('bottom', ctx)[1].height == 140)
        bone.ui.panel.update('top', { size = 3 })
        -- Rows, and the card around them: its inset, padding and border.
        assert(window.docked('top', ctx)[1].height == 3 * 18 + 34)
    "#,
        )
        .unwrap();
    g.frame();
    g.frame();
    for side in ["left", "right", "top", "bottom"] {
        let text = format!("dock-{side}");
        let (_, rect) = g
            .painted
            .iter()
            .find(|(t, _)| t == &text)
            .unwrap_or_else(|| panic!("missing {text}: {:?}", g.painted));
        assert!(
            rect.min.x >= 0.0 && rect.min.y >= 0.0 && rect.max.x <= 1000.0 && rect.max.y <= 700.0,
            "{side} was outside the window: {rect:?}"
        );
    }
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// Opposing docks cannot reserve more than the available axis. The panel
/// callbacks converge on the padded allocation, not their original request.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_opposing_docks_fit_the_window() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r#"
        local window = require('bone.nodes.window')
        require('bone.desktop.sidebar').node = function() return nil, 0 end
        window.area = function(ctx)
          dock_area = ctx
          return { grow = 1, height = 0 }
        end
        dock_seen = {}
        -- Bottom panels stand in the chat's column, over the composer.
        for _, side in ipairs({ 'left', 'right', 'top' }) do
          local id = side
          bone.ui.panel.open({ id = id, dock = id, size = 0.75,
            node = function(ctx)
              dock_seen[id] = ctx
              return { text = id }
            end })
        end
    "#,
        )
        .unwrap();
    g.frame();
    g.frame();
    g.gui
        .run_lua(
            "t",
            r#"
        assert(dock_area.width == 0 and dock_area.height == 700 - 525)
        for id, seen in pairs(dock_seen) do
          local r = assert(bone.gui.rect('panel:' .. id))
          assert(math.abs(seen.width - r.w) < 0.01, id)
          assert(math.abs(seen.height - r.h) < 0.01, id)
          assert(seen.width >= 0 and seen.height >= 0, id)
        end
        assert(dock_seen.left.width == 712 and dock_seen.right.width == 212)
        assert(dock_seen.top.height == 491)
    "#,
        )
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// The area width is what the sidebar's lane leaves; callers get precisely
/// the room the flex solver gives the area node.
#[tokio::test(flavor = "multi_thread")]
async fn window_area_accounts_for_sidebar_and_margin() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui.run_lua("t", r#"
        local window = require('bone.nodes.window')
        local sidebar = require('bone.desktop.sidebar')
        sidebar.node = function() return { width = 300 }, 300 end
        window.area = function(ctx)
          sidebar_area = ctx
          return { id = 'test-area', grow = 1, height = 0 }
        end
        bone.ui.panel.open({ id = 'left', dock = 'left', size = 0.1, node = function() return {} end })
        bone.ui.panel.open({ id = 'top', dock = 'top', size = 0.2, node = function() return {} end })
    "#).unwrap();
    g.frame();
    g.frame();
    g.gui
        .run_lua(
            "t",
            r#"
        local r = assert(bone.gui.rect('test-area'))
        assert(sidebar_area.width == 600 and sidebar_area.height == 560)
        assert(math.abs(sidebar_area.width - r.w) < 0.01, sidebar_area.width .. ' ~= ' .. r.w)
        assert(math.abs(sidebar_area.height - r.h) < 0.01, sidebar_area.height .. ' ~= ' .. r.h)
    "#,
        )
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A window manager embeds a focusable panel without docking it twice.
#[tokio::test(flavor = "multi_thread")]
async fn node_panel_can_be_embedded() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            r#"
        local window = require('bone.nodes.window')
        bone.ui.panel.open({ id = 'embedded', dock = 'bottom', focusable = true,
          node = function(ctx)
            assert(ctx.id == 'embedded')
            seen_panel = { width = ctx.width, height = ctx.height }
            panel_builds = (panel_builds or 0) + 1
            assert(ctx.focused == (bone.ui.panel.focused() == 'embedded'))
            return { text = 'embedded-content' }
          end })
        window.embedded.embedded = true
        assert(#window.docked('bottom', { width = 1000, height = 700 }) == 0)
        bone.gui.view(function(ctx)
          local node = window.panel('embedded', { width = 320, height = 120, cell = ctx.cell })
          assert(node.id == 'panel:embedded' and node.width == nil and node.height == 0 and node.grow == 1)
          assert(node.bg == nil and node.padding == nil and node.border == nil)
          return { direction = 'column', padding = { 10, 20, 30, 40 },
            { height = 50 }, node }
        end)
        function check_panel_size()
          local r = assert(bone.gui.rect('panel:embedded'))
          assert(math.abs(seen_panel.width - r.w) < 0.01, seen_panel.width .. ' ~= ' .. r.w)
          assert(math.abs(seen_panel.height - r.h) < 0.01, seen_panel.height .. ' ~= ' .. r.h)
          return true
        end
    "#,
        )
        .unwrap();
    g.frame();
    g.frame();
    assert!(g.painted.iter().any(|(text, _)| text == "embedded-content"));
    assert_eq!(g.gui.eval("check_panel_size()").unwrap(), "true");
    assert_eq!(g.gui.eval("seen_panel.width").unwrap(), "940");
    assert_eq!(g.gui.eval("seen_panel.height").unwrap(), "610");
    let builds = g.gui.eval("panel_builds").unwrap();
    for _ in 0..4 {
        g.frame();
    }
    assert_eq!(
        g.gui.eval("panel_builds").unwrap(),
        builds,
        "layout must settle"
    );
    g.resize(800.0, 500.0);
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("check_panel_size()").unwrap(), "true");
    assert_eq!(g.gui.eval("seen_panel.width").unwrap(), "740");
    assert_eq!(g.gui.eval("seen_panel.height").unwrap(), "410");
    g.click_at(160.0, 100.0);
    g.frame();
    assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "embedded");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// Embedding is a reservation, not panel ownership: stale or repeated
/// releases cannot detach a newer manager, and no release closes the panel.
#[tokio::test(flavor = "multi_thread")]
async fn embedded_panel_release_is_owned_and_idempotent() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui.run_lua("t", r#"
        local window = require('bone.nodes.window')
        assert(window.attach_panel == nil)
        bone.ui.panel.open({ id = 'owned', dock = 'left', node = function() return { text = 'owned' } end })
        local first = window.embed('owned')
        local second = window.embed('owned')
        first()
        assert(window.embedded.owned)
        assert(#window.docked('left', { width = 1000, height = 700 }) == 0)
        second()
        second()
        assert(not window.embedded.owned and bone.ui.panel.get('owned'))
        assert(#window.docked('left', { width = 1000, height = 700 }) == 1)
        local third = window.embed('owned')
        first()
        second()
        assert(window.embedded.owned)
        third()
        window.embedded.owned = true -- legacy callers still suppress docking
        assert(#window.docked('left', { width = 1000, height = 700 }) == 0)
        window.embedded.owned = nil
    "#).unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A manager may keep a panel in its tree while the shared API hides it.
#[tokio::test(flavor = "multi_thread")]
async fn hidden_embedded_panel_is_not_rendered() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r#"
        local window = require('bone.nodes.window')
        hidden_builds = 0
        bone.ui.panel.open({ id = 'hidden', dock = 'bottom', node = function()
          hidden_builds = hidden_builds + 1
          return { text = 'hide-me' }
        end })
        release_hidden = window.embed('hidden')
        bone.gui.view(function(ctx)
          return { direction = 'column', window.panel('hidden', ctx) }
        end)
    "#,
        )
        .unwrap();
    g.frame();
    g.frame();
    assert!(g.painted.iter().any(|(text, _)| text == "hide-me"));
    g.gui
        .run_lua("t", "bone.ui.panel.get('hidden'):hide()")
        .unwrap();
    let builds = g.gui.eval("hidden_builds").unwrap();
    for _ in 0..3 {
        g.frame();
    }
    assert_eq!(g.gui.eval("hidden_builds").unwrap(), builds);
    assert!(!g.painted.iter().any(|(text, _)| text == "hide-me"));
    assert_eq!(
        g.gui.eval("bone.gui.rect('panel:hidden') == nil").unwrap(),
        "true"
    );
    g.gui
        .run_lua("t", "bone.ui.panel.get('hidden'):show()")
        .unwrap();
    g.frame();
    g.frame();
    assert!(g.painted.iter().any(|(text, _)| text == "hide-me"));
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A panel opened with `node` draws it in place of lines, and a click on a
/// node in it reaches its `on_click`.
#[tokio::test(flavor = "multi_thread")]
async fn a_panel_of_nodes() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            "bone.ui.panel.open({ id = 'rich', dock = 'left', size = 30,
               node = function(ctx)
                 seen_width = ctx.width
                 local out = { direction = 'column' }
                 for i = 1, 3 do
                   out[i] = { bg = 'Selection', radius = 10, padding = { 30, 12 }, margin = { 4, 0, 4, 0 },
                     on_click = function() clicked = i end, { text = 'card ' .. i } }
                 end
                 return out
               end })",
        )
        .unwrap();
    g.frame();
    g.frame();
    assert!(g.gui.eval("seen_width").unwrap().parse::<f32>().unwrap() > 100.0);
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    // The first card is near the top of the panel: click it.
    g.click(5, 2);
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("clicked").unwrap(), "1");
}

/// `/rename` updates the open chat's title and the sidebar row. Renaming
/// from the sidebar's field already refreshed the list; the command must too.
#[tokio::test(flavor = "multi_thread")]
async fn rename_command_updates_the_session_title() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            r#"
            bone.request("session/create", {}, function(info, err)
              assert(not err, tostring(err))
              bone.api.open_session(info.session_id)
            end)
            "#,
        )
        .unwrap();
    g.wait_for(Duration::from_secs(5), |gui| {
        gui.chats[gui.current].session_id().is_some()
    })
    .await
    .unwrap();
    g.gui.run_lua("t", "bone.ui.sessions()").unwrap();
    g.settle(Duration::from_millis(200)).await;
    g.gui
        .run_lua("t", "bone.cmd('/rename First Title')")
        .unwrap();
    g.wait_for(Duration::from_secs(5), |gui| gui.title() == "First Title")
        .await
        .unwrap();
    g.settle(Duration::from_millis(300)).await;
    assert!(
        g.painted.iter().any(|(t, _)| t == "First Title"),
        "first title missing: {:?}",
        g.painted.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>()
    );
    g.gui
        .run_lua("t", "bone.cmd('/rename Second Title')")
        .unwrap();
    g.wait_for(Duration::from_secs(5), |gui| gui.title() == "Second Title")
        .await
        .unwrap();
    g.settle(Duration::from_millis(400)).await;
    let painted: Vec<_> = g.painted.iter().map(|(t, _)| t.clone()).collect();
    assert!(
        painted.iter().any(|t| t == "Second Title"),
        "sidebar kept the old title; painted {painted:?}"
    );
    assert!(
        !painted.iter().any(|t| t == "First Title"),
        "old title still shown; painted {painted:?}"
    );
}

/// Implicit terminals opt into generic pane placement; explicit docks do not.
/// Placement failure closes the process/panel without replacing the public API.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn terminal_uses_the_panel_attachment_hook() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r#"
        local window, term = require('bone.nodes.window'), require('bone.terminal')
        local original = term.open
        local releases = {}
        window.attach_panel = function(id)
          attached = (attached or 0) + 1
          assert(bone.gui.panel_node(id), 'node registered before attachment')
          releases[id] = window.embed(id)
        end
        local tiled = term.open(nil, { cmd = 'cat' })
        assert(attached == 1 and window.embedded[tiled.id] and original == term.open)
        assert(tiled:info().title:match('cat'))
        local docked = term.open({ id = 'explicit' }, { cmd = 'cat' })
        assert(attached == 1 and not window.embedded.explicit)
        term.close(docked.id)
        term.close(tiled.id)
        releases[tiled.id]()
        window.attach_panel = function() error('reject placement') end
        local ok, err = pcall(term.open, nil, { cmd = 'cat' })
        assert(not ok and tostring(err):match('reject placement'))
        assert(not bone.ui.panel.get('term1') and not term.job('term1'))
        assert(original == term.open)
        window.attach_panel = nil
    "#,
        )
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}
