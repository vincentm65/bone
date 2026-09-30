use mlua::Lua;

const CONFIG_LUA: &str = include_str!("../defaults/lua/core/init.lua");

fn config_lua() -> Lua {
    let lua = Lua::new();
    lua.load(
        r##"
        test_menu = {}
        package.preload["ui.menu"] = function() return test_menu end
        package.preload["ui.pane"] = function()
          local P = {}
          P.span = function(text, fg, modifiers)
            return { text = text, fg = fg, modifiers = modifiers }
          end
          P.clamp = function(n, lo, hi) return math.max(lo, math.min(n, hi)) end
          P.wait_key = function(ctx) return ctx.ui.key() end
          P.key_name = function(key) return key.code end
          P.is_text_key = function(key)
            return key.code == "Char" and key.char and not key.ctrl and not key.alt
          end
          P.new = function(ctx)
            return {
              set_lines = function(_, lines, visible_rows)
                ctx.renders[#ctx.renders + 1] = { lines = lines, visible_rows = visible_rows }
              end,
              close = function() end,
            }
          end
          return P
        end
        bone = { command = { register = function(name, spec)
          assert(name == "config")
          config_handler = spec.handler
        end } }
        "##,
    )
    .exec()
    .unwrap();
    lua.load(CONFIG_LUA).exec().unwrap();
    lua
}

#[test]
fn providers_page_focuses_active_provider_and_renders_header() {
    let lua = config_lua();
    lua.load(
        r##"
        test_menu.clear = function() end
        local keys, key_index = { { code = "Esc" } }, 0
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.key = function() key_index = key_index + 1; return keys[key_index] end
        ctx.ui.notify = function() end
        ctx.config.get_pages = function()
          return { { namespace = "providers", title = "Providers", fields = {} } }
        end
        ctx.config.list_providers = function()
          return {
            { id = "alpha", model = "a", handler = "openai", base_url = "http://a", active = false },
            { id = "beta", model = "b", handler = "anthropic", base_url = "http://b", active = true },
          }
        end

        config_handler("providers", ctx)
        local lines = ctx.renders[1].lines
        local header = ""
        local selected = ""
        for _, rendered in ipairs(lines) do
          local text = ""
          for _, value in ipairs(rendered.spans or {}) do text = text .. (value.text or "") end
          if text:find("Provider", 1, true) and text:find("Handler", 1, true) then header = text end
          if rendered.bg == "#3A3F4B" then selected = text end
        end
        assert(header:find("Model", 1, true) and header:find("Base URL", 1, true), header)
        assert(selected:find("beta", 1, true), selected)
        "##,
    )
    .exec()
    .unwrap();
}

#[test]
fn provider_handler_cycles_all_supported_values_and_autosaves() {
    let lua = config_lua();
    lua.load(
        r#"
        local select_calls, defaults, saved_handlers = 0, {}, {}
        test_menu.clear = function() end
        test_menu.select = function(_, opts)
          select_calls = select_calls + 1
          defaults[select_calls] = opts.default
          for _, label in ipairs(opts.options) do
            assert(label ~= "Save changes", "provider editor still requires a second save")
          end
          if select_calls <= 4 then
            return { value = opts.options[5], selected = 5 }
          end
          return { cancelled = true }
        end
        test_menu.text_input = function() error("unexpected text input") end

        local keys, key_index = { { code = "Char", char = "e" }, { code = "Esc" } }, 0
        local provider = {
          id = "active", label = "Active", model = "model", base_url = "http://active",
          endpoint = "/v1", handler = "anthropic", active = true,
        }
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.key = function() key_index = key_index + 1; return keys[key_index] end
        ctx.ui.notify = function() end
        ctx.config.get_pages = function()
          return { { namespace = "providers", title = "Providers", fields = {} } }
        end
        ctx.config.list_providers = function() return { provider } end
        ctx.config.set_provider_entry = function(_, entry)
          saved_handlers[#saved_handlers + 1] = entry.handler
          provider.handler = entry.handler
          return true
        end

        local result = config_handler("providers", ctx)
        assert(table.concat(saved_handlers, ",") == "codex,grok_build,claude_code,openai")
        assert(defaults[1] == 1)
        for i = 2, 5 do assert(defaults[i] == 5, "editor focus was not retained") end
        assert(result.action == "config.apply")
        "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn provider_text_edit_autosaves_and_retains_field_focus() {
    let lua = config_lua();
    lua.load(
        r#"
        local select_calls, saved_model = 0, nil
        test_menu.clear = function() end
        test_menu.select = function(_, opts)
          select_calls = select_calls + 1
          if select_calls == 1 then
            assert(opts.default == 1)
            return { value = opts.options[2], selected = 2 }
          end
          assert(opts.default == 2, "model row lost focus after editing")
          assert(opts.options[2] == "model · new-model", opts.options[2])
          return { cancelled = true }
        end
        test_menu.text_input = function(_, opts)
          assert(opts.initial == "old-model")
          return { value = "new-model" }
        end

        local keys, key_index = { { code = "Char", char = "e" }, { code = "Esc" } }, 0
        local provider = {
          id = "active", label = "Active", model = "old-model", base_url = "http://active",
          endpoint = "/v1", handler = "openai", active = true,
        }
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.key = function() key_index = key_index + 1; return keys[key_index] end
        ctx.ui.notify = function() end
        ctx.config.get_pages = function()
          return { { namespace = "providers", title = "Providers", fields = {} } }
        end
        ctx.config.list_providers = function() return { provider } end
        ctx.config.set_provider_entry = function(_, entry)
          saved_model = entry.model
          provider.model = entry.model
          return true
        end

        local result = config_handler("providers", ctx)
        assert(saved_model == "new-model")
        assert(result.action == "config.apply")
        "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn keyless_provider_editor_shows_not_required_without_mutating_metadata() {
    let lua = config_lua();
    lua.load(
        r#"
        local select_calls, saved_entry = 0, nil
        test_menu.clear = function() end
        test_menu.select = function(_, opts)
          select_calls = select_calls + 1
          assert(opts.options[6] == "api_key · (not required)", opts.options[6])
          if select_calls == 1 then
            return { value = opts.options[2], selected = 2 }
          end
          return { cancelled = true }
        end
        test_menu.text_input = function(_, opts)
          assert(opts.initial == "old-model")
          return { value = "new-model" }
        end

        local keys, key_index = { { code = "Char", char = "e" }, { code = "Esc" } }, 0
        local provider = {
          id = "active", label = "Active", model = "old-model", base_url = "http://localhost",
          endpoint = "/v1", handler = "openai", active = true,
          api_key_configured = false, api_key_required = false,
        }
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.key = function() key_index = key_index + 1; return keys[key_index] end
        ctx.ui.notify = function() end
        ctx.config.get_pages = function()
          return { { namespace = "providers", title = "Providers", fields = {} } }
        end
        ctx.config.list_providers = function() return { provider } end
        ctx.config.set_provider_entry = function(_, entry)
          saved_entry = entry
          provider.model = entry.model
          return true
        end

        local result = config_handler("providers", ctx)
        assert(saved_entry ~= nil)
        assert(saved_entry.api_key_required == nil, "capability metadata must not be mutated")
        assert(saved_entry.model == "new-model")
        assert(result.action == "config.apply")
        "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn failed_provider_save_rolls_back_and_keeps_editor_open() {
    let lua = config_lua();
    lua.load(
        r#"
        local select_calls, notices = 0, {}
        test_menu.clear = function() end
        test_menu.select = function(_, opts)
          select_calls = select_calls + 1
          if select_calls == 1 then return { value = opts.options[2], selected = 2 } end
          assert(opts.default == 2)
          assert(opts.options[2] == "model · old-model", opts.options[2])
          return { cancelled = true }
        end
        test_menu.text_input = function() return { value = "unsaved-model" } end

        local keys, key_index = { { code = "Char", char = "e" }, { code = "Esc" } }, 0
        local provider = {
          id = "active", label = "Active", model = "old-model", base_url = "http://active",
          endpoint = "/v1", handler = "openai", active = true,
        }
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.key = function() key_index = key_index + 1; return keys[key_index] end
        ctx.ui.notify = function(message, level)
          notices[#notices + 1] = { message = message, level = level }
        end
        ctx.config.get_pages = function()
          return { { namespace = "providers", title = "Providers", fields = {} } }
        end
        ctx.config.list_providers = function() return { provider } end
        ctx.config.set_provider_entry = function() error("write denied") end

        local result = config_handler("providers", ctx)
        assert(result == nil, "failed save must not mark config as changed")
        assert(#notices == 1 and notices[1].level == "error")
        assert(notices[1].message:find("write denied", 1, true), notices[1].message)
        "#,
    )
    .exec()
    .unwrap();
}

#[test]
fn taps_switch_tabs_act_on_rows_and_exit() {
    let lua = config_lua();
    lua.load(
        r##"
        test_menu.clear = function() end
        local keys = {
          { code = "Click", char = "tab:2" },
          { code = "Click", char = "row:2" },
          { code = "Click", char = "esc" },
        }
        local key_index, cycled, saved = 0, nil, nil
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.key = function() key_index = key_index + 1; return keys[key_index] end
        ctx.ui.notify = function() end
        ctx.config.get_pages = function()
          return {
            { namespace = "ui", title = "UI", fields = { { key = "a", label = "A", type = "bool", value = true } } },
            { namespace = "tools", title = "Tools", fields = {
              { key = "x", label = "X", type = "bool", value = false },
              { key = "y", label = "Y", type = "bool", value = false },
            } },
          }
        end
        ctx.config.cycle_field = function(ns, key, value) cycled = ns .. "." .. key; return not value end
        ctx.config.set_value = function(ns, key, value) saved = ns .. "." .. key .. "=" .. tostring(value); return true end

        local result = config_handler("", ctx)
        assert(cycled == "tools.y", tostring(cycled))
        assert(saved == "tools.y=true", tostring(saved))
        assert(key_index == 3, "Esc tap exits")
        assert(result and result.action == "config.reload_tools", "tools change needs a reload")

        -- Tabs, rows, and the Esc hint carry their tap values.
        local clicks = {}
        for _, rendered in ipairs(ctx.renders[1].lines) do
          if rendered.click then clicks[#clicks + 1] = rendered.click end
          for _, value in ipairs(rendered.spans or {}) do
            if value.click then clicks[#clicks + 1] = value.click end
          end
        end
        local joined = table.concat(clicks, ",")
        assert(joined == "tab:1,tab:2,row:1,enter,esc", joined)
        "##,
    )
    .exec()
    .unwrap();
}

#[test]
fn narrow_provider_page_keeps_tabs_and_edit_hint_tappable() {
    let lua = config_lua();
    lua.load(
        r##"
        local edited = 0
        test_menu.clear = function() end
        local picked = 0
        test_menu.select = function(_, opts)
          if tostring(opts.question):find("section", 1, true) then
            picked = picked + 1
            return { value = "2" }
          end
          edited = edited + 1
          return { cancelled = true }
        end
        local keys = {
          { code = "Click", char = "picker" },
          { code = "Click", char = "edit" },
          { code = "Click", char = "esc" },
        }
        local index = 0
        local ctx = { renders = {}, ui = {}, config = {} }
        ctx.ui.width = function() return 41 end
        ctx.ui.key = function() index = index + 1; return keys[index] end
        ctx.ui.notify = function() end
        ctx.config.get_pages = function()
          return {
            { namespace = "general", title = "General", fields = {} },
            { namespace = "providers", title = "Providers", fields = {} },
            { namespace = "plugins", title = "Plugins", fields = {} },
            { namespace = "status", title = "Status", fields = {} },
          }
        end
        ctx.config.list_providers = function()
          local providers = {}
          for i = 1, 3 do
            providers[i] = { id = "provider" .. i, model = "model", handler = "openai" }
          end
          return providers
        end
        config_handler("", ctx)
        assert(edited == 1, "tapping edit must open the selected provider editor")
        for render_i, render in ipairs(ctx.renders) do
          local seen = {}
          for i, line in ipairs(render.lines) do
            if i <= render.visible_rows then
              local text, has_span_click = "", false
              if line.click then seen[line.click] = true end
              for _, value in ipairs(line.spans or {}) do
                text = text .. value.text
                if value.click then
                  seen[value.click] = true
                  has_span_click = true
                end
              end
              if has_span_click then
                assert(utf8.len(text) <= 41, "clipped tap row: " .. text)
              end
            end
          end
          assert(seen["picker"], "section header is visible and tappable")
          assert(not seen["tab:1"], "clipping tab row is replaced by the picker")
          assert(picked == 1 or render_i == 1, "picker opened once")
          if render_i > 1 then
            assert(seen["edit"], "provider edit hint is visible and tappable")
            assert(seen["esc"], "exit hint is visible and tappable")
            assert(seen["row:1"], "provider rows are still tappable")
          end
        end
        "##,
    )
    .exec()
    .unwrap();
}

#[test]
fn approval_danger_confirmation_is_scoped_and_defaults_to_keep_asking() {
    let lua = config_lua();
    lua.load(
        r##"
        local function exercise(current, choice, expected, should_confirm)
          local saved, confirmations = nil, 0
          test_menu.clear = function() end
          test_menu.select = function(_, opts)
            confirmations = confirmations + 1
            assert(opts.default == 1, "confirmation must default to the safe choice")
            assert(opts.options[1] == "Keep asking")
            assert(opts.options[2] == "Auto-approve")
            assert(opts.question:find("all tool calls in this conversation", 1, true))
            assert(opts.question:find("default for new conversations", 1, true))
            if choice == "Esc" then return { cancelled = true } end
            return { value = choice, selected = choice == "Auto-approve" and 2 or 1 }
          end
          local keys, key_index = { { code = "Enter" }, { code = "Esc" } }, 0
          local ctx = { renders = {}, ui = {}, config = {} }
          ctx.ui.key = function()
            key_index = key_index + 1
            return keys[key_index]
          end
          ctx.ui.notify = function() end
          ctx.config.get_pages = function()
            return { {
              namespace = "general", title = "General", fields = {
                { key = "approval", label = "Approval mode", type = "enum", value = current },
              },
            } }
          end
          ctx.config.cycle_field = function(ns, key, value)
            assert(ns == "general" and key == "approval")
            return value == "safe" and "danger" or "safe"
          end
          ctx.config.set_value = function(ns, key, value)
            saved = ns .. "." .. key .. "=" .. tostring(value)
            return true
          end

          local result = config_handler("", ctx)
          assert(confirmations == (should_confirm and 1 or 0))
          assert(saved == expected, tostring(saved))
          if expected then
            assert(result and result.action == "config.apply")
          else
            assert(result == nil)
          end
        end

        exercise("safe", "Auto-approve", "general.approval=danger", true)
        exercise("safe", "Keep asking", nil, true)
        exercise("safe", "Esc", nil, true)
        exercise("danger", "Keep asking", "general.approval=safe", false)
        "##,
    )
    .exec()
    .unwrap();
}
