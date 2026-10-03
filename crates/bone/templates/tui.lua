-- bone TUI config: options, keys, commands, colors and events.
-- Runs after the built-in defaults. Try lines live with /lua <code>.
-- Reference: docs/lua.md in the bone repository.

-- Options (same as /set):
-- bone.o.show_reasoning = false
-- bone.o.tool_preview_lines = 4

-- Keys: a key name ("ctrl+s", "alt+enter", "f2"), then a builtin action
-- name, a "/command", or a Lua function.
-- bone.keymap.set("f2", "/sessions")
-- bone.keymap.set("ctrl+l", "/new")

-- Colors: /colorscheme black (default) or ansi; or tweak single groups:
-- bone.hl.set("UserMessage", { fg = "#eeeeee", bg = "#202020" })

-- The screen starts blank: plain text, no statusline, divider or prompt
-- prefix. Draw them in Lua (docs/lua.md, "Drawing the screen"), or install
-- the style plugin: cp -r <bone repo>/examples/plugins/style ~/.bone/plugins/
-- bone.ui.prompt = { prefix = "> " }
-- bone.ui.statusline = function(ctx) return { " " .. ctx.title, "%=", " " } end

-- A command: /where shows the session's directory.
bone.cmd.create("where", function()
  local s = bone.api.session()
  bone.notify(s and (s.title or "untitled") .. " — " .. s.cwd or "new session (not started yet)")
end, { desc = "show the current session and directory" })
