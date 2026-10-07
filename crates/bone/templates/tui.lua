-- bone TUI config: options, keys, commands, colors and events.
-- Runs after the built-in defaults. Try lines with bone.api.exec_lua("code").
-- Reference: docs/lua.md in the bone repository.

-- Options (set from Lua):
-- bone.o.show_reasoning = false
-- bone.o.tool_preview_lines = 4

-- Keys: a key name ("ctrl+s", "alt+enter", "f2"), then a builtin action
-- name, a "/command", or a Lua function.
-- bone.keymap.set("f2", "/sessions")
-- bone.keymap.set("ctrl+l", "/new")

-- Colors: bone.colorscheme('black') (default) or 'ansi'; or tweak single groups:
-- bone.hl.set("UserMessage", { fg = "#eeeeee", bg = "#202020" })

-- The defaults draw the standard UI (runtime/lua/bone/ui/). Replace any
-- piece here (docs/lua.md, "Drawing the screen"), override a module under
-- ~/.bone/runtime/ starting from bone.builtin(...).
-- bone.ui.prompt = { prefix = "> " }
-- bone.ui.statusline = function(ctx) return { " " .. ctx.title, "%=", " " } end

-- A command: /where shows the session's directory.
bone.cmd.create("where", function()
  local s = bone.api.session()
  bone.notify(s and (s.title or "untitled") .. " — " .. s.cwd or "new session (not started yet)")
end, { desc = "show the current session and directory" })
