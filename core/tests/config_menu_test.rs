use mlua::Lua;

const CONFIG_LUA: &str = include_str!("../defaults/lua/commands/config.lua");

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
        assert(table.concat(saved_handlers, ",") == "codex,grok_build,openai,anthropic")
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
