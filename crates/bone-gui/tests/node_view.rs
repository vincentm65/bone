//! The GUI's Lua host: what it loads and in which order, the bare library,
//! and reloading with the TUI's rule (a reload with a new error keeps the
//! configuration in use).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bone_core::Core;
use bone_core::scripting;
use bone_gui::{GuiOptions, Headless};
use bone_proto::methods::{MessageCompletedParams, TurnFinishedParams};
use bone_proto::types::{ChatMessage, TurnOutcome};
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

/// The composer grows on the first wrap, using the width left beside
/// its attach button, and shrinks again without overflowing its card.
#[tokio::test(flavor = "multi_thread")]
async fn composer_contains_input_at_each_wrap() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.resize(400.0, 700.0);
    for chars in (1..=100).chain((0..100).rev()) {
        g.gui
            .run_lua("t", &format!("bone.prompt.set(string.rep('a', {chars}))"))
            .unwrap();
        g.frame();
        g.frame();
        g.gui
            .run_lua(
                "t",
                r#"
          local card = assert(bone.gui.rect('prompt'))
          local input = assert(bone.gui.rect('composer-input'))
          assert(input.y >= card.y + 10, 'input overflows the top of the card')
          assert(input.y + input.h <= card.y + card.h - 10, 'input overflows the bottom of the card')
          assert(math.abs((input.y + input.h / 2) - (card.y + card.h / 2)) < 1,
            'input is not vertically centered in the card')
          if not first_height then first_height = card.h end
          if card.h > first_height then grew = true end
        "#,
            )
            .unwrap_or_else(|e| panic!("{chars} characters: {e}"));
    }
    assert_eq!(g.gui.eval("tostring(grew)").unwrap(), "true");
    assert_eq!(
        g.gui
            .eval("tostring(bone.gui.rect('prompt').h == first_height)")
            .unwrap(),
        "true"
    );
}

/// The welcome copy and composer retain usable bounds on phone-sized windows.
#[tokio::test(flavor = "multi_thread")]
async fn narrow_new_session_scales_its_welcome_and_composer() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    for width in [200.0, 120.0, 80.0, 40.0] {
        g.resize(width, 700.0);
        for _ in 0..3 {
            g.frame();
        }
        let geometry = g
            .gui
            .eval("(function() local p=bone.gui.rect('prompt'); local i=bone.gui.rect('composer-input'); return table.concat({p.x,p.w,i.x,i.w},',') end)()")
            .unwrap();
        let values: Vec<f32> = geometry.split(',').map(|v| v.parse().unwrap()).collect();
        assert!(
            values[0] >= 0.0 && values[0] + values[1] <= width,
            "{width}: {geometry}"
        );
        assert!(
            values[2] >= values[0] && values[2] + values[3] <= values[0] + values[1],
            "{width}: {geometry}"
        );
        assert!(values[3] > 0.0, "{width}: input collapsed: {geometry}");
        let title = g
            .painted
            .iter()
            .find(|(text, _)| text.contains("New session"))
            .map(|(_, rect)| *rect)
            .expect("new-session welcome is painted");
        assert!(
            title.min.x >= 0.0 && title.max.x <= width,
            "{width}: {title:?}"
        );
    }
}

/// The canvas supplies the picker's 50% opacity only once; the chat keeps
/// its opaque surface, without an outline, and the sidebar is a card on it.
#[tokio::test(flavor = "multi_thread")]
async fn translucent_picker_does_not_stack_backgrounds() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      assert(bone.o.opacity == 0.9)
      local info = bone.gui.window_info
      bone.gui.window_info = function() return { opacity = 0.5 } end
      local sidebar = require('bone.desktop.sidebar')
      sidebar.slide_ms = 1
      sidebar.open()
      local root = require('bone.nodes.window').view({ width = 1000, height = 700 })
      bone.gui.window_info = info
      local function walk(node, f)
        if type(node) ~= 'table' then return nil end
        local hit = f(node)
        if hit then return hit end
        for _, child in ipairs(node) do
          local found = walk(child, f)
          if found then return found end
        end
      end
      -- Docks and the sidebar sit inside the root, not at a fixed index.
      local main = walk(root, function(n)
        if n.overflow == 'hidden' and type(n.bg) == 'string' and n.bg:match('^#') then return n end
      end)
      assert(root.bg == nil, 'root must not cover the translucent canvas')
      assert(main and main.border == nil, 'no chat outline')
      local side = walk(main, function(n)
        if n.id == 'sessions-card' then return n end
      end)
      assert(side, 'the sidebar sits on the chat, not on the canvas')
      assert(main.bg:match('^#%x%x%x%x%x%x$'), 'chat stays opaque')
    "##,
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_chats_keeps_each_scroll_position() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    let fill = |chat: &mut bone_gui::ChatBuffer, name: &str| {
        for i in 0..40 {
            let turn = i as u64 + 1;
            chat.turn_started(turn, &format!("{name} user {i}"));
            chat.message_completed(&MessageCompletedParams {
                session_id: name.into(),
                turn_id: turn,
                message: ChatMessage::Assistant {
                    content: format!("{name} reply {i}"),
                    reasoning: String::new(),
                    tool_calls: vec![],
                    reasoning_state: None,
                },
                usage: None,
            });
            chat.turn_finished(&TurnFinishedParams {
                session_id: name.into(),
                turn_id: turn,
                outcome: TurnOutcome::Completed,
            });
        }
    };
    fill(&mut g.gui.chats[0], "one");
    g.gui.chats.push(bone_gui::ChatBuffer::new(None));
    fill(&mut g.gui.chats[1], "two");
    g.gui.view.invalidate();
    for _ in 0..4 {
        g.frame();
    }

    g.gui.run_lua("t", "bone.gui.scroll('chat', 200)").unwrap();
    g.frame();
    let first = g.gui.eval("bone.gui.scroll_info('chat').offset").unwrap();
    assert!(first.parse::<f32>().unwrap() > 0.0);

    g.gui.show_chat(1);
    g.frame();
    g.gui.run_lua("t", "bone.gui.scroll('chat', 100)").unwrap();
    g.frame();
    let second = g.gui.eval("bone.gui.scroll_info('chat').offset").unwrap();
    assert!(second.parse::<f32>().unwrap() > 0.0);

    g.gui.show_chat(0);
    g.frame();
    let restored = g.gui.eval("bone.gui.scroll_info('chat').offset").unwrap();
    assert_eq!(restored, first);
}

/// `bone.nodes.chat{ chat = n }` draws another chat as list `chat:<n>`, its
/// views reading that chat; once it comes on screen, the list it had is
/// the `chat` list.
#[tokio::test(flavor = "multi_thread")]
async fn another_chat_draws_beside_the_one_on_screen() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui.chats.push(bone_gui::ChatBuffer::new(None));
    g.gui
        .run_lua(
            "t",
            r##"
      bone.ui.views.probe = function(item)
        return { lines = { item.text .. " in " .. bone.chat.session().chat } }
      end
      bone.chat.add('probe', { text = 'first' })
      local nodes = require('bone.nodes')
      bone.gui.view(function(ctx)
        return { direction = "row",
          nodes.chat({ chat = 0, width = 0, grow = 1 }),
          nodes.chat({ chat = 1, width = 0, grow = 1 }),
        }
      end)
    "##,
        )
        .unwrap();
    g.gui.show_chat(1);
    g.gui
        .run_lua("t", "bone.chat.add('probe', { text = 'second' })")
        .unwrap();
    g.gui.show_chat(0);
    for _ in 0..3 {
        g.frame();
    }
    let at = |g: &Headless, s: &str| (g.painted.iter()).find(|(t, _)| t.contains(s)).map(|p| p.1);
    let (a, b) = (
        at(&g, "first in 0").unwrap(),
        at(&g, "second in 1").unwrap(),
    );
    assert!(a.min.x < b.min.x);
    g.gui
        .run_lua("t", "assert(bone.gui.scroll_info('chat:1'))")
        .unwrap();

    g.gui.show_chat(1);
    g.gui
        .run_lua(
            "t",
            "assert(bone.gui.scroll_info('chat:1') == nil and bone.gui.scroll_info('chat:0'))",
        )
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    assert!(at(&g, "first in 0").is_some() && at(&g, "second in 1").is_some());
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// Tiled chats keep their composer and status bar geometry across focus,
/// including wrapped drafts; clicking an inactive editor activates its chat.
#[tokio::test(flavor = "multi_thread")]
async fn tiled_composers_stay_put_when_focus_changes() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui.chats.push(bone_gui::ChatBuffer::new(None));
    g.gui.chats[1].turn_started(1, "running");
    g.gui
        .run_lua(
            "t",
            r#"
      local window = require('bone.nodes.window')
      local bar = require('bone.nodes.statusbar').node
      require('bone.nodes.statusbar').node = function(session, cell)
        assert(session and session.chat ~= nil)
        local node = bar(session, cell)
        node.id = 'status-' .. session.chat
        return node
      end
      bone.gui.view(function(ctx)
        local root = { direction = 'row' }
        for n = 0, 1 do
          root[#root + 1] = window.column(ctx, { chat = n, width = ctx.width / 2,
            on_focus = function() bone.api.open_session(tostring(n)) end })
        end
        return root
      end)
      function geometry(n)
        local here = bone.chat.session().chat == n
        local input = bone.gui.rect('composer-input' .. (here and '' or n))
        local bar = bone.gui.rect('status-' .. n)
        return table.concat({ input.x, input.y, input.w, input.h, bar.x, bar.y, bar.w, bar.h }, ',')
      end
    "#,
        )
        .unwrap();
    for draft in ["", "first line\nsecond line", &"wrapped draft ".repeat(150)] {
        g.gui.show_chat(0);
        g.gui
            .run_lua(
                "t",
                &format!("bone.prompt.set({})", serde_json::to_string(draft).unwrap()),
            )
            .unwrap();
        for _ in 0..3 {
            g.frame();
        }
        let before = [
            g.gui.eval("geometry(0)").unwrap(),
            g.gui.eval("geometry(1)").unwrap(),
        ];
        g.gui.show_chat(1);
        for _ in 0..3 {
            g.frame();
            assert_eq!(g.gui.eval("geometry(0)").unwrap(), before[0]);
            assert_eq!(g.gui.eval("geometry(1)").unwrap(), before[1]);
        }
        let x = g.gui.eval("(function() local r = bone.gui.rect('composer-input0'); return r.x + r.w / 2 end)()").unwrap().parse().unwrap();
        let y = g.gui.eval("(function() local r = bone.gui.rect('composer-input0'); return r.y + r.h / 2 end)()").unwrap().parse().unwrap();
        g.click_at(x, y);
        for _ in 0..3 {
            g.frame();
        }
        assert_eq!(g.gui.eval("bone.chat.session().chat").unwrap(), "0");
        assert_eq!(g.gui.eval("bone.prompt.get()").unwrap(), draft);
        g.text("x");
        g.frame();
        assert!(g.gui.eval("bone.prompt.get()").unwrap().contains('x'));
    }
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn scroll_follow_and_input() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui.run_lua("t", r##"
      bone.gui.view(function(ctx)
        return { direction = "column", bg = "#111",
          { id = "list", overflow = "scroll", follow = true, grow = 1, gap = 4,
            { height = 400, bg = "#333", on_click = function(p) clicked = p.x end, on_hover = function(b) hovered = b end, cursor = "pointer" },
            { height = 400, bg = "#444", on_scroll = function(d) end },
          },
          { input = true, placeholder = "Message", padding = 8, bg = "#222", radius = 8 },
        }
      end)
    "##).unwrap();
    g.frame();
    g.frame();
    g.gui.run_lua("t", "local i = bone.gui.scroll_info('list'); assert(i and i.max > 0 and i.offset == i.max, vim and '' or tostring(i and i.offset)..'/'..tostring(i and i.max))").unwrap();
    g.gui
        .run_lua("t", "bone.gui.scroll('list', 'top')")
        .unwrap();
    g.frame();
    g.gui
        .run_lua("t", "assert(bone.gui.scroll_info('list').offset == 0)")
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_list_makes_only_the_items_in_view() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      made = 0
      builds = 0
      bone.gui.view(function(ctx)
        builds = builds + 1
        return { bg = "#111",
          { id = "chat", grow = 1, follow = true,
            list = { count = 10000, estimate = 40, key = "a", item = function(i)
              made = made + 1
              return { padding = 8, { text = "item " .. i } }
            end } },
        }
      end)
    "##,
        )
        .unwrap();
    for _ in 0..8 {
        g.frame();
    }
    g.gui
        .run_lua("t", "assert(made < 200, 'made ' .. made)")
        .unwrap();
    // At the end: the offset is the maximum.
    g.gui
        .run_lua("t", "local i = bone.gui.scroll_info('chat'); assert(i.max > 100000 and i.offset == i.max, i.offset .. '/' .. i.max)")
        .unwrap();
    // Still: Lua is not asked to build again.
    g.gui.run_lua("t", "builds0, made0 = builds, made").unwrap();
    for _ in 0..5 {
        g.frame();
    }
    g.gui
        .run_lua(
            "t",
            "assert(builds == builds0 and made == made0, builds .. ' ' .. made)",
        )
        .unwrap();
    // Changing the list's key forgets what was measured; going to the top
    // shows the first items.
    g.gui
        .run_lua("t", "bone.gui.scroll('chat', 'top'); bone.gui.invalidate()")
        .unwrap();
    for _ in 0..4 {
        g.frame();
    }
    g.gui
        .run_lua("t", "assert(bone.gui.scroll_info('chat').offset == 0)")
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_nodes_library_draws_the_chat() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(config.path().join("gui-library"), "standard\n").unwrap();
    let mut g = gui(config.path(), work.path()).await;
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    g.frame();
    g.send("hello there");
    for _ in 0..30 {
        g.frame();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
    g.gui
        .run_lua(
            "t",
            "local s = bone.chat.session(); assert(s.items >= 1, s.items); \
             local i = bone.gui.scroll_info('chat'); assert(i, 'no chat list')",
        )
        .unwrap();
    let hello = (g.painted.iter()).find(|(t, _)| t.contains("hello there"));
    let hint = (g.painted.iter()).find(|(t, _)| t.contains("Message bone"));
    let (hello, hint) = (
        hello.expect("the message is painted").1,
        hint.expect("the composer is painted").1,
    );
    assert!(hello.max.y < hint.min.y, "{hello:?} {hint:?}");
    // Your message is on the right.
    assert!(hello.min.x > 500.0, "{hello:?}");

    // The sessions sidebar docks on the left.
    g.gui.run_lua("t", "bone.ui.sessions()").unwrap();
    for _ in 0..4 {
        g.frame();
    }
    let chats = (g.painted.iter()).find(|(t, _)| t == "Processes");
    assert!(chats.expect("the sidebar is painted").1.min.x < 200.0);
    // The / menu shows its matches above the composer.
    g.gui
        .run_lua("t", "bone.ui.panel.focus(nil); bone.prompt.set('/he')")
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    let menu = (g.painted.iter()).find(|(t, _)| t.contains("/help"));
    assert!(menu.expect("the menu is painted").1.max.y < hint.min.y);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_node_view_paints_where_the_tree_puts_things() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      bone.gui.view(function(ctx)
        return { direction = "column",
          { grow = 1, padding = 10, { text = "top left" } },
          { height = 50, bg = "#336699", direction = "row", justify = "end", padding = 5,
            { text = "bottom right" } },
        }
      end)
    "##,
        )
        .unwrap();
    g.frame();
    g.frame();
    let find = |s: &str| g.painted.iter().find(|(t, _)| t == s).map(|(_, r)| *r);
    let top = find("top left").expect("top text painted");
    let bottom = find("bottom right").expect("bottom text painted");
    assert!(
        (top.min.x - 10.0).abs() < 1.0 && (top.min.y - 10.0).abs() < 1.0,
        "{top:?}"
    );
    // The bar is 50 tall at the bottom of 700; its text ends 5 from the right.
    assert!((bottom.max.x - 995.0).abs() < 3.0, "{bottom:?}");
    assert!(bottom.min.y > 650.0, "{bottom:?}");
    assert!(
        g.boxes
            .iter()
            .any(|b| (b.height() - 50.0).abs() < 0.5 && b.width() > 990.0)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_from_the_core_is_a_modal_dialog() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    let ev = bone_client::Event {
        method: "ask/requested".into(),
        params: Some(serde_json::json!({
            "ask_id": 7,
            "question": { "text": "Allow shell?", "options": ["allow", "deny"] },
        })),
    };
    g.gui.handle_event(&ev);
    for _ in 0..4 {
        g.frame();
    }
    let text = |g: &bone_gui::Headless, s: &str| g.painted.iter().any(|(t, _)| t == s);
    assert!(
        text(&g, "Allow shell?") && text(&g, "allow") && text(&g, "deny"),
        "{:?}",
        g.painted
    );
    // The dialog is centered, over a backdrop that covers the window.
    assert!(
        g.boxes
            .iter()
            .any(|b| b.width() >= 999.0 && b.height() >= 699.0)
    );
    // No stray box from the window that catches its keys.
    assert!(
        !g.boxes
            .iter()
            .any(|b| b.width() < 30.0 && b.height() < 30.0 && b.width() > 0.0),
        "{:?}",
        g.boxes
    );
    let ev = bone_client::Event {
        method: "ask/resolved".into(),
        params: Some(serde_json::json!({ "ask_id": 7 })),
    };
    g.gui.handle_event(&ev);
    for _ in 0..3 {
        g.frame();
    }
    assert!(!text(&g, "Allow shell?"), "closed when answered elsewhere");
}

/// A node-only modal takes keys before the panel underneath it, including
/// its opening guard; paste must not reach a terminal's global paste hook.
#[tokio::test(flavor = "multi_thread")]
async fn a_node_modal_takes_keys_before_a_focused_panel() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r#"
      panel_keys, pastes, modal_keys = {}, {}, {}
      shortcuts, clicks = 0, 0
      bone.keymap.set('ctrl+g', function() shortcuts = shortcuts + 1 end, { context = 'panel' })
      bone.ui.panel.open({ id = 'terminal', dock = 'bottom', focusable = true,
        lines = { 'terminal' }, on_key = function(k)
          if k == 'ctrl+g' then return false end
          panel_keys[#panel_keys + 1] = k
          return true
        end })
      bone.ui.panel.focus('terminal')
      bone.on('paste', function(ev) pastes[#pastes + 1] = ev.text end)
      modal = false
      bone.gui.view(function()
        if not modal then return { text = 'terminal' } end
        if modal == 'empty' then return { block_input = true, text = 'Empty modal' } end
        return { block_input = true,
          { id = 'dialog', autofocus = true, key_guard = 150,
            on_key = function(k)
              modal_keys[#modal_keys + 1] = k
              return k == 'left' or k == 'enter'
            end,
            { text = 'Modal dialog' } },
          { id = 'other', text = 'Other', on_click = function() clicks = clicks + 1 end },
        }
      end)
    "#,
        )
        .unwrap();
    g.frame();
    g.frame();
    // Ordinary panel input, including paste, still uses the shared engine.
    g.text("x");
    g.key(Key::ArrowLeft, Modifiers::NONE);
    g.paste("panel paste");
    g.frame();
    assert_eq!(
        g.gui.eval("table.concat(panel_keys, ',')").unwrap(),
        "x,left"
    );
    assert_eq!(g.gui.eval("pastes[1]").unwrap(), "panel paste");

    g.gui
        .run_lua("t", "modal = true; bone.gui.invalidate()")
        .unwrap();
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("bone.gui.focused()").unwrap(), "dialog");
    assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "terminal");
    g.text("g");
    g.key(Key::Enter, Modifiers::NONE);
    g.paste("guarded paste");
    g.frame();
    assert_eq!(g.gui.eval("#modal_keys").unwrap(), "0", "opening guard");
    assert_eq!(g.gui.eval("#panel_keys").unwrap(), "2");
    assert_eq!(g.gui.eval("#pastes").unwrap(), "1");

    tokio::time::sleep(Duration::from_millis(200)).await;
    g.text("z"); // The callback declines it, but the modal still traps it.
    g.key(Key::ArrowLeft, Modifiers::NONE);
    g.key(Key::Enter, Modifiers::NONE);
    g.paste("modal paste");
    g.frame();
    assert_eq!(
        g.gui.eval("table.concat(modal_keys, ',')").unwrap(),
        "z,left,enter"
    );
    assert_eq!(g.gui.eval("#panel_keys").unwrap(), "2");
    assert_eq!(g.gui.eval("#pastes").unwrap(), "1");
    assert_eq!(g.gui.draft, "", "the prompt is also untouched");

    // Declined editing keys stay trapped; tab and arrows navigate the modal.
    g.key(Key::ArrowRight, Modifiers::NONE);
    g.key(Key::Backspace, Modifiers::NONE);
    g.key(Key::Escape, Modifiers::NONE);
    g.key(Key::Tab, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.eval("bone.gui.focused()").unwrap(), "other");
    g.key(Key::Enter, Modifiers::NONE);
    g.key(Key::ArrowUp, Modifiers::NONE);
    g.frame();
    assert_eq!(g.gui.eval("clicks").unwrap(), "1");
    assert_eq!(g.gui.eval("bone.gui.focused()").unwrap(), "dialog");
    assert_eq!(g.gui.eval("#panel_keys").unwrap(), "2");

    // Non-editing shortcuts declined by the modal still reach the engine.
    g.key(Key::G, Modifiers::CTRL);
    g.frame();
    assert_eq!(g.gui.eval("shortcuts").unwrap(), "1");

    // A modal without focusable nodes also shields the underlying terminal.
    g.gui
        .run_lua("t", "modal = 'empty'; bone.gui.invalidate()")
        .unwrap();
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("tostring(bone.gui.focused())").unwrap(), "nil");
    g.text("empty");
    for key in [Key::ArrowLeft, Key::Enter, Key::Tab, Key::Escape] {
        g.key(key, Modifiers::NONE);
    }
    g.paste("empty modal paste");
    g.frame();
    assert_eq!(g.gui.eval("#panel_keys").unwrap(), "2");
    assert_eq!(g.gui.eval("#pastes").unwrap(), "1");

    g.gui
        .run_lua("t", "modal = false; bone.gui.invalidate()")
        .unwrap();
    g.frame();
    g.frame();
    g.text("y");
    g.key(Key::ArrowRight, Modifiers::NONE);
    g.paste("panel again");
    g.frame();
    assert_eq!(
        g.gui.eval("table.concat(panel_keys, ',')").unwrap(),
        "x,left,y,right"
    );
    assert_eq!(g.gui.eval("pastes[2]").unwrap(), "panel again");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A modal text field keeps native editing and paste even while a terminal
/// remains the shared engine's focused panel.
#[tokio::test(flavor = "multi_thread")]
async fn a_modal_text_field_edits_before_a_focused_panel() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r#"
      panel_keys, pastes, value = {}, {}, ''
      bone.ui.panel.open({ id = 'terminal', dock = 'bottom', focusable = true,
        lines = { 'terminal' }, on_key = function(k)
          panel_keys[#panel_keys + 1] = k
          return true
        end })
      bone.ui.panel.focus('terminal')
      bone.on('paste', function(ev) pastes[#pastes + 1] = ev.text end)
      bone.gui.view(function()
        return { block_input = true,
          { id = 'search', width = 300, height = 40, text_input = {
            focus = true, on_change = function(text) value = text end,
          } },
        }
      end)
    "#,
        )
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    g.text("abc");
    g.frame();
    g.key(Key::ArrowLeft, Modifiers::NONE);
    g.frame();
    g.key(Key::Backspace, Modifiers::NONE);
    g.frame();
    g.paste("X");
    g.frame();
    assert_eq!(g.gui.eval("value").unwrap(), "aXc");
    assert_eq!(g.gui.eval("#panel_keys").unwrap(), "0");
    assert_eq!(g.gui.eval("#pastes").unwrap(), "0");
    assert_eq!(g.gui.eval("bone.ui.panel.focused()").unwrap(), "terminal");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A click anywhere on a sidebar row reaches the row, on its text too.
#[tokio::test(flavor = "multi_thread")]
async fn the_sidebar_rows_take_clicks_on_their_text() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui.run_lua("t", "bone.ui.sessions()").unwrap();
    // It slides out from under the chat first.
    for _ in 0..20 {
        g.frame();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(g.gui.eval("bone.gui.focused()").unwrap(), "sidebar-search");
    let at = (g.painted.iter())
        .find(|(t, r)| t == "+" && r.min.x < 280.0)
        .expect("the new chat button")
        .1;
    g.click_at(at.center().x, at.center().y);
    for _ in 0..3 {
        g.frame();
    }
    // The button's handler gave the keyboard back to the prompt.
    assert_eq!(
        g.gui.eval("tostring(bone.gui.focused())").unwrap(),
        "nil",
        "the click reached the row"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// Drag capture survives a Lua rebuild and a paint-only drop preview. Drag
/// positions are window-relative, release positions remain node-relative.
#[tokio::test(flavor = "multi_thread")]
async fn dragging_survives_rebuilds_and_releases_outside_the_node() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      moves, releases, order = {}, {}, {}
      bone.gui.view(function()
        return { padding = 40,
          { id = 'area', grow = 1,
            { id = 'title', width = 300, height = 28, bg = '#333',
              { text = 'Drag this title' },
              on_press = function() bone.gui.invalidate() end,
              on_drag = function(p)
                moves[#moves + 1] = p
                order[#order + 1] = 'drag'
                bone.gui.invalidate()
              end,
              on_release = function(p)
                releases[#releases + 1] = p
                order[#order + 1] = 'release'
                bone.gui.invalidate()
              end,
            },
            #moves > 0 and { position = 'absolute', left = 400, top = 100,
              width = 200, height = 200, border = 2, border_color = '#48f', z = 20 } or nil,
          },
        }
      end)
    "##,
        )
        .unwrap();
    g.frame();
    g.frame();
    g.button_at(100.0, 50.0, true);
    g.frame();
    for x in [500.0, 550.0, 600.0] {
        g.hover_at(x, 200.0);
        g.frame();
        g.frame(); // rebuilt tree, and no movement while still held
    }
    // Release somewhere other than the last drag position, over the overlay.
    g.button_at(620.0, 220.0, false);
    g.frame();
    g.frame();
    g.gui
        .run_lua(
            "t",
            r##"
      local r = assert(bone.gui.rect('area'))
      assert(r.x == 40 and r.y == 40)
      assert(#moves == 4 and moves[1].x == 500 and moves[3].x == 600 and moves[3].y == 200)
      assert(moves[4].x == 620 and moves[4].y == 220)
      assert(#releases == 1 and releases[1].x == 580 and releases[1].y == 180)
      assert(table.concat(order, ',') == 'drag,drag,drag,drag,release')
      moves, releases, order = {}, {}, {}
    "##,
        )
        .unwrap();
    // No held-motion frame at all: movement and up arrive in one frame.
    g.button_at(100.0, 50.0, true);
    g.frame();
    g.button_at(620.0, 220.0, false);
    g.frame();
    g.frame();
    g.gui
        .run_lua(
            "t",
            r##"
      assert(#moves == 1 and moves[1].x == 620 and moves[1].y == 220)
      assert(#releases == 1 and table.concat(order, ',') == 'drag,release')
      moves, releases, order = {}, {}, {}
    "##,
        )
        .unwrap();
    // Small click jitter is not a drag, even on a release-motion frame.
    g.button_at(100.0, 50.0, true);
    g.frame();
    g.button_at(101.0, 51.0, false);
    g.frame();
    g.frame();
    g.gui
        .run_lua("t", "assert(#moves == 0 and #releases == 1)")
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// `hover`, `active` and `focus` restyle a node while the pointer is over,
/// pressing or has clicked it; `on_press` and `on_release` fire.
#[tokio::test(flavor = "multi_thread")]
async fn states_restyle_a_node_and_press_events_fire() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      ev = {}
      bone.gui.view(function()
        return { bg = "#101010", padding = 20,
          { width = 100, height = 50, bg = "#222222", cursor = "pointer",
            hover = { bg = "#334455" }, active = { bg = "#553344" }, focus = { bg = "#335544" },
            on_press = function() ev[#ev + 1] = "press" end,
            on_release = function() ev[#ev + 1] = "release" end },
        }
      end)
    "##,
        )
        .unwrap();
    let fill = |g: &bone_gui::Headless| {
        g.fills
            .iter()
            .find(|(r, _)| (r.width() - 100.0).abs() < 0.5 && (r.height() - 50.0).abs() < 0.5)
            .map(|(_, c)| *c)
    };
    g.frame();
    g.frame();
    assert_eq!(fill(&g), Some([0x22, 0x22, 0x22, 0xff]));
    g.hover_at(60.0, 40.0);
    g.frame();
    g.frame();
    assert_eq!(fill(&g), Some([0x33, 0x44, 0x55, 0xff]), "hover");
    g.button_at(60.0, 40.0, true);
    g.frame();
    g.frame();
    assert_eq!(fill(&g), Some([0x55, 0x33, 0x44, 0xff]), "pressed");
    g.button_at(60.0, 40.0, false);
    g.frame();
    g.frame();
    assert_eq!(
        fill(&g),
        Some([0x33, 0x55, 0x44, 0xff]),
        "focused after the click"
    );
    assert_eq!(
        g.gui.eval("table.concat(ev, ',')").unwrap(),
        "press,release"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A window of `lines` is drawn as nodes where the shared placement puts it,
/// in the look `bone.nodes.popups.chrome` says.
#[tokio::test(flavor = "multi_thread")]
async fn popups_are_nodes_in_a_look_lua_sets() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui
        .run_lua(
            "t",
            r##"
      require("bone.nodes.popups").chrome.bg = "#aa0000"
      win = bone.ui.popup({ lines = { "a window", { { "with ", "Normal" }, { "groups", "Accent" } } }, width = 20 })
    "##,
        )
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    assert!(
        g.painted.iter().any(|(t, _)| t == "a window"),
        "{:?}",
        g.painted
    );
    let red = g.fills.iter().find(|(_, c)| *c == [0xaa, 0, 0, 0xff]);
    let (rect, _) = red.expect("the card is Lua's color");
    // Twenty cells wide, two rows tall, centered.
    assert!(rect.width() > 100.0 && rect.height() > 20.0, "{rect:?}");
    // Closed, it is gone.
    g.gui.run_lua("t", "bone.ui.close(win)").unwrap();
    for _ in 0..3 {
        g.frame();
    }
    assert!(!g.painted.iter().any(|(t, _)| t == "a window"));
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// An `image` node reads its file once and draws it in the box its declared
/// size gives it.
#[tokio::test(flavor = "multi_thread")]
async fn an_image_node_draws_a_picture_from_a_file() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let png = bone_media::from_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]).unwrap();
    let path = work.path().join("p.png");
    std::fs::write(&path, &png.bytes).unwrap();
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            &format!(
                r##"
      bone.gui.view(function()
        return {{ bg = "#101010", padding = 10, direction = "column",
          {{ width = 80, radius = 6, image = {{ path = "{}", width = 2, height = 1, fit = "contain" }} }},
        }}
      end)
    "##,
                path.display()
            ),
        )
        .unwrap();
    let key = format!("file:{}", path.display());
    g.wait_for(Duration::from_secs(5), |gui| gui.image_shown(&key))
        .await
        .unwrap();
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A view that fails leaves the fallback view, which says why.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_view_shows_the_fallback() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            "bone.gui.view(function() return { direction = 'diagonal' } end)",
        )
        .unwrap();
    g.frame();
    g.frame();
    assert!(
        g.painted.iter().any(|(t, _)| t.contains("no working view")),
        "{:?}",
        g.painted
    );
    assert!(
        g.painted.iter().any(|(t, _)| t.contains("diagonal")),
        "the error is shown"
    );
}

/// Plugins add nodes to the window's slots; the drop event is Lua's to handle.
#[tokio::test(flavor = "multi_thread")]
async fn slots_take_a_plugins_nodes_and_drops_are_events() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      bone.gui.slots.add("statusbar.right", { id = "clock", order = 2, node = function() return { text = "slot-two" } end })
      bone.gui.slots.add("statusbar.right", { id = "early", order = 1, node = { text = "slot-one" } })
      bone.gui.slots.add("window.overlay", { node = function() return { position = "absolute", left = 10, top = 10, text = "overlay-slot" } end })
      bone.gui.slots.add("chat.top", { node = { text = "above-chat" } })
      bone.gui.slots.add("broken", { id = "b", node = function() error("boom") end })
      dropped = nil
      bone.on("drop", function(ev) dropped = ev.paths[1] end)
    "##,
        )
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    let at = |g: &Headless, s: &str| g.painted.iter().find(|(t, _)| t == s).map(|(_, r)| *r);
    let (one, two) = (
        at(&g, "slot-one").expect("first slot"),
        at(&g, "slot-two").expect("second slot"),
    );
    assert!(one.min.x < two.min.x, "ordered: {one:?} {two:?}");
    assert!(at(&g, "overlay-slot").is_some() && at(&g, "above-chat").is_some());
    assert_eq!(
        g.gui.eval("#bone.gui.slots.nodes('broken')").unwrap(),
        "0",
        "a failing slot is skipped"
    );
    assert_eq!(
        g.gui
            .eval("table.concat(bone.gui.slots.list('statusbar.right'), ',')")
            .unwrap(),
        "early,clock"
    );
    g.gui
        .run_lua("t", "bone.gui.slots.remove('statusbar.right', 'clock')")
        .unwrap();
    for _ in 0..3 {
        g.frame();
    }
    assert!(at(&g, "slot-two").is_none(), "removed");
    // A dropped file reaches Lua's handler first.
    g.drop_file(work.path().join("x.txt"));
    g.frame();
    assert!(g.gui.eval("dropped").unwrap().ends_with("x.txt"));
}

/// A text input of the view keeps its own text, tells Lua of each change and
/// of enter, and does not touch the prompt.
#[tokio::test(flavor = "multi_thread")]
async fn a_text_input_edits_natively_and_tells_lua() {
    use eframe::egui::{Key, Modifiers};
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      changes, sent = {}, nil
      bone.gui.view(function()
        return { bg = "#101010", padding = 20, direction = "column", gap = 10,
          { id = "search", width = 300, bg = "#222", padding = 8, radius = 6,
            text_input = { placeholder = "Search", on_change = function(t) changes[#changes + 1] = t end,
              on_submit = function(t) sent = t end } },
          { id = "notes", width = 300, bg = "#222", padding = 8, text_input = { multiline = true, value = "first line", max_rows = 4 } },
        }
      end)
    "##,
        )
        .unwrap();
    g.frame();
    g.frame();
    let hint = (g.painted.iter())
        .find(|(t, _)| t == "Search")
        .expect("the hint")
        .1;
    g.click_at(hint.center().x, hint.center().y);
    g.frame();
    g.text("hello");
    g.frame();
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.input_text('search')").unwrap(),
        "hello"
    );
    assert_eq!(g.gui.eval("changes[#changes]").unwrap(), "hello");
    g.key(Key::Enter, Modifiers::NONE);
    g.frame();
    g.frame();
    assert_eq!(g.gui.eval("sent").unwrap(), "hello", "enter sent it");
    assert_eq!(g.gui.draft, "", "the prompt was left alone");
    // Lua can set it, and the multiline one started with its value.
    g.gui
        .run_lua("t", "bone.gui.set_input_text('search', 'from lua')")
        .unwrap();
    g.frame();
    g.frame();
    assert!(
        g.painted.iter().any(|(t, _)| t == "from lua"),
        "{:?}",
        g.painted
    );
    assert_eq!(
        g.gui.eval("bone.gui.input_text('notes')").unwrap(),
        "first line"
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// The sidebar's search is a native text input that has the keyboard when
/// the sidebar opens: typing filters, even though the panel has the keys.
#[tokio::test(flavor = "multi_thread")]
async fn typing_in_the_sidebar_searches() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.frame();
    g.gui.run_lua("t", "bone.ui.sessions()").unwrap();
    for _ in 0..20 {
        g.frame();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    g.text("zzz");
    g.frame();
    g.frame();
    g.frame();
    assert_eq!(
        g.gui.eval("bone.gui.input_text('sidebar-search')").unwrap(),
        "zzz"
    );
    assert!(
        g.painted.iter().any(|(t, _)| t == "No matching chats"),
        "{:?}",
        g.painted
    );
    assert_eq!(g.gui.draft, "", "not the prompt");
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// A canvas node is drawn by Lua's shapes, in its box; a fast one is drawn
/// again as time goes, a still one only once.
#[tokio::test(flavor = "multi_thread")]
async fn a_canvas_is_drawn_by_lua() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      still_draws, live_draws, seen_w = 0, 0, nil
      local draw = bone.gui.draw
      bone.gui.view(function()
        return { bg = "#101010", padding = 20, direction = "column", gap = 10,
          { width = 200, height = 100, canvas = { draw = function(ctx)
              still_draws = still_draws + 1
              seen_w = ctx.width
              return { draw.rect{ x = 10, y = 10, w = 50, h = 30, fill = "#336699", radius = 4 },
                       draw.circle{ x = 100, y = 50, r = 20, fill = "#aa3333" },
                       draw.line{ x1 = 0, y1 = 0, x2 = 199, y2 = 99, color = "#ffffff", width = 2 },
                       draw.text{ x = 5, y = 90, text = "canvas-text", size = 12, color = "#eeeeee" } }
            end } },
          { width = 50, height = 20, canvas = { fps = 60, draw = function(ctx)
              live_draws = live_draws + 1
              return { draw.rect{ x = 0, y = 0, w = 10 + (ctx.time * 10) % 30, h = 5, fill = "#ffcc00" } }
            end } },
          { width = 50, height = 20, canvas = { draw = function() error("bad shape") end } },
        }
      end)
    "##,
        )
        .unwrap();
    for _ in 0..6 {
        g.frame();
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        g.painted.iter().any(|(t, _)| t == "canvas-text"),
        "{:?}",
        g.painted
    );
    // The rectangle is where the box is (20, 20) plus (10, 10).
    let blue = (g.fills.iter())
        .find(|(_, c)| *c == [0x33, 0x66, 0x99, 0xff])
        .expect("the rect");
    assert!(
        (blue.0.min.x - 30.0).abs() < 0.5 && (blue.0.width() - 50.0).abs() < 0.5,
        "{:?}",
        blue.0
    );
    assert_eq!(g.gui.eval("seen_w").unwrap(), "200");
    // A still canvas is drawn when the view is built, not every frame.
    let before = g.gui.eval("still_draws").unwrap();
    for _ in 0..4 {
        g.frame();
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        g.gui.eval("still_draws").unwrap(),
        before,
        "a still canvas is not drawn again"
    );
    let live = g.gui.eval("live_draws").unwrap().parse::<u32>().unwrap();
    assert!(live >= 3, "a 60 fps canvas is drawn again: {live}");
    // The failing one said so, once.
    let errors = g
        .gui
        .lua_errors()
        .iter()
        .filter(|e| e.contains("bad shape"))
        .count();
    assert_eq!(errors, 1, "{:?}", g.gui.lua_errors());
}

/// A font family Lua registers can be named by any node.
#[tokio::test(flavor = "multi_thread")]
async fn lua_registers_font_families() {
    let font = [
        "/usr/share/fonts/liberation/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/TTF/LiberationSerif-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf",
        "/usr/share/fonts/TTF/DejaVuSerif.ttf",
    ]
    .into_iter()
    .find(|p| std::path::Path::new(p).exists());
    let Some(font) = font else {
        return; // no font file to register on this machine
    };
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            &format!(
                r##"
      bone.gui.font("serif", "{font}")
      bone.gui.view(function()
        return {{ bg = "#101010", padding = 10, direction = "column",
          {{ text = "plain sans", size = 20 }},
          {{ text = "plain sans", size = 20, font = "serif" }},
          {{ text = "unknown family", size = 20, font = "nonesuch" }},
        }}
      end)
    "##
            ),
        )
        .unwrap();
    for _ in 0..4 {
        g.frame();
    }
    let widths: Vec<f32> = g
        .painted
        .iter()
        .filter(|(t, _)| t == "plain sans")
        .map(|(_, r)| r.width())
        .collect();
    assert_eq!(widths.len(), 2, "{:?}", g.painted);
    assert!(
        (widths[0] - widths[1]).abs() > 0.5,
        "the serif face is wider or narrower: {widths:?}"
    );
    assert!(
        g.gui.status.as_ref().is_none_or(|s| !s.error),
        "{:?}",
        g.gui.status
    );
    assert!(g.gui.lua_errors().is_empty(), "{:?}", g.gui.lua_errors());
}

/// `transition` fades a changed background over its duration instead of
/// jumping: part way a moment after the hover, the new color once it is over.
#[tokio::test(flavor = "multi_thread")]
async fn transitions_fade_what_changes() {
    let (config, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut g = gui(config.path(), work.path()).await;
    g.gui
        .run_lua(
            "t",
            r##"
      bone.gui.view(function()
        return { bg = "#101010", padding = 20,
          { width = 100, height = 50, bg = "#000000", hover = { bg = "#c8c8c8" },
            transition = { duration = 300, easing = "linear" } },
        }
      end)
    "##,
        )
        .unwrap();
    let fill = |g: &Headless| {
        g.fills
            .iter()
            .find(|(r, _)| (r.width() - 100.0).abs() < 0.5 && (r.height() - 50.0).abs() < 0.5)
            .map(|(_, c)| c[0])
    };
    g.frame();
    g.frame();
    assert_eq!(fill(&g), Some(0), "at rest");
    g.hover_at(60.0, 40.0);
    g.frame();
    tokio::time::sleep(Duration::from_millis(120)).await;
    g.frame();
    let mid = fill(&g).expect("painted");
    assert!(mid > 20 && mid < 190, "part way: {mid}");
    tokio::time::sleep(Duration::from_millis(400)).await;
    g.frame();
    assert_eq!(fill(&g), Some(0xc8), "arrived");
    // And back again when the pointer leaves.
    g.hover_at(500.0, 500.0);
    g.frame();
    tokio::time::sleep(Duration::from_millis(400)).await;
    g.frame();
    assert_eq!(fill(&g), Some(0));
}
