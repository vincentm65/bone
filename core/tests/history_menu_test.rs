use std::sync::{Arc, Mutex};

use mlua::{Lua, Table};
use rusqlite::Connection;

const HISTORY_LUA: &str = include_str!("../defaults/lua/lib/history.lua");
const MENU_LUA: &str = include_str!("../defaults/lua/lib/ui/menu.lua");

fn history_sql() -> &'static str {
    let marker = "ctx.db.query([[";
    let start = HISTORY_LUA.find(marker).expect("history list query") + marker.len();
    let end = HISTORY_LUA[start..]
        .find("]], { limit })")
        .map(|offset| start + offset)
        .expect("history list query end");
    HISTORY_LUA[start..end].trim()
}

#[test]
fn history_query_classifies_and_counts_conversations() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE conversations (
            id INTEGER PRIMARY KEY, started_at TEXT NOT NULL, ended_at TEXT,
            provider TEXT NOT NULL, model TEXT NOT NULL
         );
         CREATE TABLE messages (
            id INTEGER PRIMARY KEY, conversation_id INTEGER NOT NULL,
            role TEXT NOT NULL, content TEXT NOT NULL, seq INTEGER NOT NULL,
            created_at TEXT NOT NULL
         );
         CREATE TABLE usage_events (
            conversation_id INTEGER NOT NULL, prompt_tokens INTEGER NOT NULL,
            completion_tokens INTEGER NOT NULL
         );
         CREATE INDEX idx_messages_conversation_seq ON messages(conversation_id, seq);
         INSERT INTO conversations VALUES
            (1, '2026-01-01T00:00:00Z', NULL, 'p1', 'm1'),
            (2, '2026-01-02T00:00:00Z', NULL, 'p2', 'm2'),
            (3, '2026-01-03T00:00:00Z', NULL, 'p3', 'm3'),
            (4, '2026-01-04T00:00:00Z', NULL, 'p4', 'm4'),
            (5, '2026-01-05T00:00:00Z', NULL, 'p5', 'm5');
         INSERT INTO messages VALUES
            (1, 1, 'user', 'first question', 1, '2026-01-01T00:01:00Z'),
            (2, 1, 'assistant', 'answer', 2, '2026-01-01T00:02:00Z'),
            (3, 2, 'user', 'unanswered', 1, '2026-01-02T00:01:00Z'),
            (4, 3, 'user', '[Context summary] old context', 1, '2026-01-03T00:01:00Z'),
            (5, 4, 'user', 'use a tool', 1, '2026-01-04T00:01:00Z'),
            (6, 4, 'assistant', '', 2, '2026-01-04T00:02:00Z'),
            (7, 4, 'tool', 'result', 3, '2026-01-04T00:03:00Z'),
            (8, 4, 'assistant', 'done', 4, '2026-01-04T00:04:00Z');
         INSERT INTO usage_events VALUES
            (1, 100, 25),
            (1, 200, 50),
            (4, 80, 20);",
    )
    .unwrap();

    let mut statement = db.prepare(history_sql()).unwrap();
    let rows = statement
        .query_map([50], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    let find = |id| rows.iter().find(|row| row.0 == id).unwrap();
    assert_eq!(
        find(1),
        &(
            1,
            "2026-01-01T00:02:00Z".into(),
            2,
            375,
            "completed".into(),
            Some("first question".into())
        )
    );
    assert_eq!(find(2).4, "interrupted");
    assert_eq!((find(2).2, find(2).3), (1, 0));
    assert_eq!(
        (find(3).4.as_str(), find(3).3, find(3).5.as_ref()),
        ("empty", 0, None)
    );
    assert_eq!(
        (find(4).4.as_str(), find(4).2, find(4).3),
        ("completed", 4, 100)
    );
    assert!(
        rows.iter().all(|row| row.0 != 5),
        "zero-message rows are hidden"
    );
}

#[test]
fn history_list_sql_uses_candidate_first_cte() {
    let sql = history_sql();
    assert!(
        sql.trim_start()
            .to_ascii_lowercase()
            .starts_with("with recent as"),
        "history.list must use the candidate-first CTE so large histories stay cheap"
    );
    assert!(sql.contains("LIMIT ?"));
    assert!(
        !sql.to_ascii_lowercase().contains("from conversations c"),
        "candidate-first query should drive from recent, not scan conversations first"
    );
}

#[test]
fn history_list_defaults_to_fifty() {
    let lua = Lua::new();
    let module: Table = lua.load(HISTORY_LUA).eval().unwrap();
    let seen = Arc::new(Mutex::new(0_i64));
    let captured = Arc::clone(&seen);
    let query = lua
        .create_function(move |lua, (_sql, params): (String, Table)| {
            *captured.lock().unwrap() = params.get(1)?;
            lua.create_table()
        })
        .unwrap();
    let db = lua.create_table().unwrap();
    db.set("query", query).unwrap();
    let ctx = lua.create_table().unwrap();
    ctx.set("db", db).unwrap();
    module
        .get::<mlua::Function>("list")
        .unwrap()
        .call::<Table>(ctx)
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), 50);
}

fn run_menu(keys: &str) -> (Option<i64>, bool, bool) {
    let lua = Lua::new();
    lua.load(
        r#"
        package.preload["ui.pane"] = function()
          local P = {}
          P.span = function(text, fg, modifiers) return { text = text, fg = fg, modifiers = modifiers } end
          P.line = function(...) return { spans = { ... } } end
          P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
          P.wait_key = function(ctx) return ctx.ui.key() end
          P.key_name = function(key) return key.code end
          P.is_text_key = function(key) return key.code == "Char" and key.char and not key.ctrl and not key.alt end
          P.new = function(ctx)
            return {
              ctx = ctx,
              set_lines = function(_, lines) _G.last_menu_lines = lines end,
              close = function() end,
            }
          end
          return P
        end
        "#,
    )
    .exec()
    .unwrap();
    let menu: Table = lua.load(MENU_LUA).eval().unwrap();
    lua.globals().set("menu", menu).unwrap();
    let result: Table = lua
        .load(format!(
            r##"
        local keys, index = {{ {keys} }}, 0
        local ctx = {{ ui = {{
          key = function() index = index + 1; return keys[index] end,
          width = function() return 80 end,
          pane = function() end,
        }} }}
        local result = menu.select(ctx, {{
          options = {{
            {{
              label = "Alpha", description = "openai/model-a", search_text = "id-1", value = 1,
              description_spans = {{ {{ text = "openai/model-a", fg = "#8CC8FF" }} }},
            }},
            {{
              label = "Beta", label_modifiers = {{ "bold" }},
              description = "anthropic/model-b", search_text = "id-2", value = 2,
              description_spans = {{ {{ text = "anthropic/model-b", fg = "#8CC8FF" }} }},
            }},
            {{
              label = "Gamma", description = "local/model-c", search_text = "id-3", value = 3,
              description_spans = {{ {{ text = "local/model-c", fg = "#8CC8FF" }} }},
            }},
          }},
          searchable = true,
        }})
        local lines = _G.last_menu_lines
        result.style_ok = lines[2].bg == "#3A3F4B"
          and lines[2].spans[3].fg == "white"
          and lines[2].spans[3].modifiers[1] == "bold"
          and lines[3].bg == "#3A3F4B"
          and lines[3].spans[1].fg == "gray"
          and lines[3].spans[2].fg == "#8CC8FF"
        return result
        "##
        ))
        .eval()
        .unwrap();
    (
        result.get::<i64>("value").ok(),
        result.get::<bool>("cancelled").unwrap_or(false),
        result.get::<bool>("style_ok").unwrap_or(false),
    )
}

#[test]
fn multi_select_prefills_checked_values_and_ignores_unknown_values() {
    let lua = Lua::new();
    lua.load(
        r#"
        package.preload["ui.pane"] = function()
          local P = {}
          P.span = function(text, fg, modifiers) return { text = text, fg = fg, modifiers = modifiers } end
          P.line = function(...) return { spans = { ... } } end
          P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
          P.wait_key = function(ctx) return ctx.ui.key() end
          P.key_name = function(key) return key.code end
          P.is_text_key = function() return false end
          P.new = function(ctx)
            return {
              ctx = ctx,
              set_lines = function(_, lines)
                if not _G.initial_menu_lines then _G.initial_menu_lines = lines end
              end,
              close = function() end,
            }
          end
          return P
        end
        "#,
    )
    .exec()
    .unwrap();
    let menu: Table = lua.load(MENU_LUA).eval().unwrap();
    lua.globals().set("menu", menu).unwrap();

    let result: Table = lua
        .load(
            r#"
            local ctx = { ui = {
              key = function() return { code = "Enter" } end,
              width = function() return 80 end,
            } }
            local result = menu.multi_select(ctx, {
              options = {
                "alpha",
                { label = "Beta", value = "beta-value" },
                "gamma",
              },
              default = 3,
              initial_checked = { "alpha", "beta-value", "unknown" },
            })
            local lines = _G.initial_menu_lines
            result.rendered_checked = lines[1].spans[2].text == "[x] "
              and lines[2].spans[2].text == "[x] "
              and lines[3].spans[2].text == "[ ] "
            return result
            "#,
        )
        .eval()
        .unwrap();

    assert!(result.get::<bool>("rendered_checked").unwrap());
    assert_eq!(result.get::<i64>("selected").unwrap(), 3);
    let values: Table = result.get("values").unwrap();
    assert_eq!(values.get::<String>(1).unwrap(), "alpha");
    assert_eq!(values.get::<String>(2).unwrap(), "beta-value");
    assert_eq!(values.raw_len(), 2);
}

#[test]
fn searchable_multi_select_space_unchecks_instead_of_filtering() {
    let lua = Lua::new();
    lua.load(
        r#"
        package.preload["ui.pane"] = function()
          local P = {}
          P.span = function(text, fg, modifiers) return { text = text, fg = fg, modifiers = modifiers } end
          P.line = function(...) return { spans = { ... } } end
          P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
          P.wait_key = function(ctx) return ctx.ui.key() end
          P.key_name = function(key) return key.code end
          P.is_text_key = function(key) return key.code == "Char" and key.char and not key.ctrl and not key.alt end
          P.new = function(ctx)
            return { ctx = ctx, set_lines = function() end, close = function() end }
          end
          return P
        end
        "#,
    )
    .exec()
    .unwrap();
    let menu: Table = lua.load(MENU_LUA).eval().unwrap();
    lua.globals().set("menu", menu).unwrap();

    let result: Table = lua
        .load(
            r#"
            local keys = {
              { code = "Char", char = " " },
              { code = "Enter" },
            }
            local next_key = 0
            local ctx = { ui = {
              key = function()
                next_key = next_key + 1
                return keys[next_key]
              end,
              width = function() return 80 end,
            } }
            return menu.multi_select(ctx, {
              options = { "alpha", "beta" },
              initial_checked = { "alpha", "beta" },
              searchable = true,
            })
            "#,
        )
        .eval()
        .unwrap();

    let values: Table = result.get("values").unwrap();
    assert_eq!(values.raw_len(), 1);
    assert_eq!(values.get::<String>(1).unwrap(), "beta");
}

#[test]
fn preview_menu_switches_options_and_scrolls_styled_content() {
    let lua = Lua::new();
    lua.load(
        r#"
        package.preload["ui.pane"] = function()
          local P = {}
          P.span = function(text, fg, modifiers) return { text = text, fg = fg, modifiers = modifiers } end
          P.line = function(...) return { spans = { ... } } end
          P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
          P.wait_key = function(ctx) return ctx.ui.key() end
          P.key_name = function(key) return key.code end
          P.is_text_key = function() return false end
          P.new = function(ctx)
            return {
              ctx = ctx,
              set_lines = function(_, lines, visible_rows)
                _G.last_preview_lines = lines
                _G.last_preview_visible_rows = visible_rows
              end,
              close = function() end,
            }
          end
          return P
        end
        "#,
    )
    .exec()
    .unwrap();
    let menu: Table = lua.load(MENU_LUA).eval().unwrap();
    lua.globals().set("menu", menu).unwrap();

    let result: Table = lua
        .load(
            r##"
            local keys, index = {
              { code = "Down" },
              { code = "Tab" },
              { code = "PageDown" },
              { code = "Enter" },
            }, 0
            local ctx = { ui = {
              key = function() index = index + 1; return keys[index] end,
              width = function() return 100 end,
            } }
            local preview_lines = {}
            for i = 1, 21 do
              preview_lines[i] = { spans = { { text = "   node " .. i, fg = "#78B373" } } }
            end
            local result = menu.select(ctx, {
              question = "Choose",
              options = {
                { label = "Alpha", value = "a", preview = { title = "Alpha diagram", lines = { "A" } } },
                { label = "Beta", value = "b", preview = { title = "Beta diagram", lines = preview_lines } },
              },
            })
            local lines = _G.last_preview_lines
            local function right_span(row)
              for i, value in ipairs(lines[row].spans) do
                if value.text == " ┃ " then return lines[row].spans[i + 1] end
              end
            end
            result.preview_title = right_span(2).text
            result.preview_line = right_span(3).text
            result.preview_fg = right_span(3).fg
            result.visible_rows = _G.last_preview_visible_rows
            menu.select({ ui = {
              key = function() return { code = "Enter" } end,
              width = function() return 100 end,
            } }, {
              options = {
                { label = "Short", preview = { title = "Short diagram", lines = { "A" } } },
              },
            })
            result.short_visible_rows = _G.last_preview_visible_rows
            result.short_line_count = #_G.last_preview_lines

            local stacked_index = 0
            local stacked_first_height
            menu.select({ ui = {
              key = function()
                stacked_index = stacked_index + 1
                if stacked_index == 1 then stacked_first_height = _G.last_preview_visible_rows end
                return { code = stacked_index == 1 and "Down" or "Enter" }
              end,
              width = function() return 40 end,
            } }, {
              options = {
                { label = "Short", preview = { title = "Short", lines = { "A" } } },
                { label = "Tall", preview = { title = "Tall", lines = { "1", "2", "3", "4", "5" } } },
              },
            })
            result.stacked_first_height = stacked_first_height
            result.stacked_final_height = _G.last_preview_visible_rows
            result.stacked_line_count = #_G.last_preview_lines
            return result
            "##,
        )
        .eval()
        .unwrap();

    assert_eq!(result.get::<String>("value").unwrap(), "b");
    assert_eq!(
        result.get::<String>("preview_title").unwrap(),
        "Beta diagram  2/21"
    );
    assert_eq!(result.get::<String>("preview_line").unwrap(), "   node 2");
    assert_eq!(result.get::<String>("preview_fg").unwrap(), "#78B373");
    assert_eq!(result.get::<i64>("visible_rows").unwrap(), 24);
    assert_eq!(result.get::<i64>("short_visible_rows").unwrap(), 6);
    assert_eq!(result.get::<i64>("short_line_count").unwrap(), 6);
    assert_eq!(result.get::<i64>("stacked_first_height").unwrap(), 10);
    assert_eq!(result.get::<i64>("stacked_final_height").unwrap(), 10);
    assert_eq!(result.get::<i64>("stacked_line_count").unwrap(), 10);
}

#[test]
fn preview_menu_honors_static_stacked_overrides() {
    let lua = Lua::new();
    lua.load(
        r#"
        package.preload["ui.pane"] = function()
          local P = {}
          P.span = function(text, fg, modifiers) return { text = text, fg = fg, modifiers = modifiers } end
          P.line = function(...) return { spans = { ... } } end
          P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
          P.wait_key = function(ctx) return ctx.ui.key() end
          P.key_name = function(key) return key.code end
          P.is_text_key = function() return false end
          P.new = function()
            return {
              set_lines = function(_, lines, visible_rows)
                _G.preview_lines = lines
                _G.preview_visible_rows = visible_rows
              end,
              close = function() end,
            }
          end
          return P
        end
        "#,
    )
    .exec()
    .unwrap();
    let menu: Table = lua.load(MENU_LUA).eval().unwrap();
    lua.globals().set("menu", menu).unwrap();

    let result: Table = lua
        .load(
            r#"
            local ctx = { ui = {
              key = function() return { code = "Esc" } end,
              width = function() return 100 end,
            } }
            local preview_lines = {}
            for i = 1, 20 do preview_lines[i] = "node " .. i end
            local result = menu.select(ctx, {
              question = "Choose",
              visible_rows = 8,
              preview = {
                layout = "stacked",
                focusable = false,
                scrollable = false,
              },
              options = {
                { label = "Alpha", preview = { title = "Diagram", lines = preview_lines } },
              },
            })
            local text = {}
            local has_columns = false
            for _, rendered_line in ipairs(_G.preview_lines) do
              if type(rendered_line) == "table" then
                for _, value in ipairs(rendered_line.spans or {}) do
                  text[#text + 1] = value.text
                  if value.text == " ┃ " then has_columns = true end
                end
              end
            end
            result.rendered = table.concat(text, "\n")
            result.has_columns = has_columns
            result.visible_rows = _G.preview_visible_rows
            return result
            "#,
        )
        .eval()
        .unwrap();

    assert!(result.get::<bool>("cancelled").unwrap());
    assert!(!result.get::<bool>("has_columns").unwrap());
    assert_eq!(result.get::<i64>("visible_rows").unwrap(), 8);
    let rendered = result.get::<String>("rendered").unwrap();
    assert!(rendered.contains("Preview ─ \nDiagram"));
    assert!(rendered.contains("node 1"));
    assert!(!rendered.contains("1/20"));
    assert!(!rendered.contains("Tab switch pane"));
}

#[test]
fn searchable_menu_filters_only_on_enter_and_supports_jk() {
    let filtered = run_menu(
        r#"{ code = "Char", char = "m" },
            { code = "Char", char = "o" },
            { code = "Char", char = "d" },
            { code = "Char", char = "e" },
            { code = "Char", char = "l" },
            { code = "Char", char = "-" },
            { code = "Char", char = "b" },
            { code = "Enter" }"#,
    );
    assert_eq!(filtered.0, Some(2));
    assert!(
        filtered.2,
        "selected label and metadata use full-row styling"
    );

    let cancelled = run_menu(
        r#"{ code = "Char", char = "b" },
            { code = "Esc" }"#,
    );
    assert!(cancelled.1);

    let navigated = run_menu(
        r#"{ code = "Char", char = "j" },
            { code = "Char", char = "j" },
            { code = "Char", char = "k" },
            { code = "Enter" }"#,
    );
    assert_eq!(navigated.0, Some(2));
}
