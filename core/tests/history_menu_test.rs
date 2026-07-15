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
              label = "Beta", description = "anthropic/model-b", search_text = "id-2", value = 2,
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
