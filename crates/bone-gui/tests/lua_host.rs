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
    assert_eq!(g.gui.transcript(), "plain words");

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

    // An edit is picked up by itself once it has settled.
    std::fs::write(&file, "bone.o.define('conf_n', 1)\nmarker = 'third'").unwrap();
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
    g.frame();
    let bottom = g.gui.lua.chat_top;
    assert!(bottom > 20, "not at the end: {bottom}");
    g.key(Key::PageUp, Modifiers::NONE);
    g.frame();
    assert!(g.gui.lua.chat_top < bottom, "pageup stayed at {bottom}");
    g.key(Key::End, Modifiers::CTRL);
    g.frame();
    assert_eq!(g.gui.lua.chat_top, bottom, "ctrl+end did not return");
}

/// A narrower window has fewer columns, re-wraps the chat and fires
/// `resize`; a HiDPI display keeps the layout, which is in logical units.
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
    let long = "word ".repeat(120);
    g.gui
        .run_lua(
            "t",
            &format!("bone.chat.add('note', {{ text = '{long}' }})"),
        )
        .unwrap();
    g.frame();
    let cols = g.gui.lua.screen_cols;
    let rows = g.gui.chats[0].row_count();

    g.resize(500.0, 700.0);
    g.frame();
    let narrow = g.gui.lua.screen_cols;
    assert!(narrow < cols * 6 / 10, "{cols} -> {narrow} columns");
    assert!(g.gui.chats[0].row_count() > rows, "the chat re-wrapped");
    let resized = g.gui.eval("resized").unwrap();
    assert_eq!(resized, format!("{narrow}x{}", g.gui.lua.screen_size.1));

    g.set_scale(2.0);
    g.frame();
    g.frame();
    assert_eq!(
        g.gui.lua.screen_cols, narrow,
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
    let row = (g.gui.transcript().lines())
        .position(|l| l.contains(&format!("[Image: {left}")))
        .expect("the image's row");
    let chat = (g.gui.lua.geometry.leaves.iter())
        .find(|(n, _)| n == "chat")
        .map(|(_, r)| *r)
        .unwrap();
    g.click(chat.x + 4, chat.y + (row - g.gui.lua.chat_top) as u16);
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
/// GUI its own (desktop).
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
    assert_eq!(g.gui.eval("bone.library.info().drawn").unwrap(), "desktop");
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

/// A view's `block` is rich text the GUI wraps itself, as many rows as it
/// needs; its `lines` are for a frontend that does not draw blocks.
#[tokio::test(flavor = "multi_thread")]
async fn a_view_block_is_wrapped_rich_text() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    let long = "word ".repeat(200);
    g.gui
        .run_lua(
            "t",
            &format!(
                r#"bone.ui.views.conf_block = function(item)
                  return {{ block = {{
                      paragraphs = {{ {{ spans = {{ {{ item.text, "Normal" }} }} }}, {{ spans = {{ "second" }}, gap = 6 }} }},
                      bg = "UserMessage", radius = 12, pad = {{ 8, 12 }}, align = "right", max_width = 0.8,
                    }}, lines = {{ {{ {{ "fallback", "Normal" }} }} }} }}
                end
                bone.chat.add("conf_block", {{ text = "{long}" }})"#
            ),
        )
        .unwrap();
    g.frame();
    g.frame();
    let rows = g.gui.chats[0].row_count();
    assert!(rows > 3, "a long paragraph wraps over several rows: {rows}");
    let t = g.gui.transcript();
    assert!(t.contains("word word") && t.contains("second"), "{t}");
    assert!(
        !t.contains("fallback"),
        "the GUI draws the block, not the lines"
    );
}

/// The GUI draws with the desktop library unless another is chosen, and
/// `/gui use` chooses apart from the TUI's choice: its views draw blocks,
/// and it loads cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_library() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    // No choice at all: desktop.
    std::fs::write(config.path().join("gui-library"), "").unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.library.info().active").unwrap(),
        "desktop"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    std::fs::remove_file(config.path().join("gui-library")).unwrap();
    g.gui.run_lua("t", "bone.cmd('gui use standard')").unwrap();
    g.frame();
    g.gui.run_lua("t", "bone.cmd('gui use desktop')").unwrap();
    g.frame();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    assert_eq!(
        g.gui.eval("bone.gui.library.info().active").unwrap(),
        "desktop"
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
             assert(v.block and v.block.align == 'right', 'a bubble')
             assert(v.lines and #v.lines > 0, 'lines for the TUI')
             local a = bone.ui.views.assistant({ kind = 'assistant', text = '# Title\\n\\nSome **bold** text.\\n\\n- one\\n- two\\n\\n```\\ncode\\n```' }, { width = 60 })
             assert(#a.block.paragraphs == 5, 'heading, text, two items, code: ' .. #a.block.paragraphs)",
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
             assert(t.block.paragraphs[1].spans[2].text == 'Ran', 'a card titled by what it did')
             local e = bone.ui.views.tool({ kind = 'tool', id = 'c2', name = 'edit_file',
               arguments = { path = 'x.rs', old_string = 'a', new_string = 'b' }, done = true }, { width = 60 })
             assert(#e.block.paragraphs == 2, 'an edit shows its diff')
             assert(type(bone.ui.statusline({ width = 80, spinner = '*' })) == 'table', 'a status bar')
             bone.ui.sessions()",
        )
        .unwrap();
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "sessions");

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

    g.gui.run_lua("t", "bone.cmd('gui use standard')").unwrap();
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.library.info().active").unwrap(),
        "standard"
    );
    assert_eq!(
        g.gui
            .eval("bone.ui.views.user({ kind = 'user', text = 'hi' }, { width = 40 }).block == nil")
            .unwrap(),
        "true"
    );
}

/// What Lua draws reaches screen readers: chat rows, blocks, and the rows
/// painted in regions (the status line).
#[tokio::test(flavor = "multi_thread")]
async fn lua_drawn_rows_are_accessible() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.enable_accesskit();
    g.gui
        .run_lua(
            "t",
            "bone.ui.statusline = function() return { { 'conf status words', 'StatusLine' } } end
             bone.chat.add('note', { text = 'a chat row' })",
        )
        .unwrap();
    g.frame();
    g.frame();
    let all = g.accessible.join(" | ");
    assert!(all.contains("conf status words"), "the status line: {all}");
    assert!(all.contains("a chat row"), "the chat: {all}");
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
        if s.matches("typed into it").count() >= 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the terminal shows: {s}"
        );
    }
    assert_eq!(
        g.gui.draft, "",
        "the keys went to the terminal, not the prompt"
    );
    g.gui.run_lua("t", "bone.cmd('term close')").unwrap();
    g.frame();
    assert_eq!(
        g.gui.eval("bone.ui.panel.get('term1') == nil").unwrap(),
        "true"
    );
}

/// A panel opened with `blocks` draws them as rich text in place of lines,
/// and a click on one reaches its `on_block`.
#[tokio::test(flavor = "multi_thread")]
async fn a_panel_of_blocks() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            "bone.ui.panel.open({ id = 'rich', dock = 'left', size = 30,
               blocks = function(ctx)
                 seen_width = ctx.width
                 local out = {}
                 for i = 1, 3 do
                   out[i] = { id = i, selected = i == 2, block = {
                     paragraphs = { { spans = { { 'card ' .. i, 'Normal' } } } },
                     bg = 'Selection', radius = 10, pad = { 30, 12 }, margin = { 4, 4 } } }
                 end
                 return out
               end,
               on_block = function(id) clicked = id end })",
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
