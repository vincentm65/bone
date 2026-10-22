-- The shared key engine, through bone.press: keymaps with Lua functions,
-- key sequences, named contexts, raw interceptors, windows' and panels' keys.
local log = {}
local function note(s) return function() log[#log + 1] = s end end

bone.keymap.set("ctrl+x z", note("sequence"))
bone.keymap.set("ctrl+x", note("prefix alone"))
bone.press("ctrl+x")
bone.press("z")
eq(log[1], "sequence", "a sequence runs its mapping")

bone.keymap.context("conf-keys", { fallback = "main" })
bone.keymap.set("ctrl+y", note("in context"), { context = "conf-keys" })
bone.keymap.focus("conf-keys")
eq(bone.keymap.current(), "conf-keys", "a named context has the keyboard")
bone.press("ctrl+y")
eq(log[2], "in context", "its keys apply")
bone.keymap.clear()
eq(bone.keymap.current(), "main", "back to main")

local raw = bone.keymap.raw(function(name)
  if name == "ctrl+r" then
    log[#log + 1] = "raw " .. name
    return true
  end
end)
bone.press("ctrl+r")
eq(log[3], "raw ctrl+r", "raw interceptors see keys first")
eq(bone.keymap.raw_del(raw), true, "and go")

local win = bone.ui.popup({ lines = { "keys" }, focus = true, keys = { q = note("window q") },
  on_key = function(name) log[#log + 1] = "window " .. name; return true end })
bone.press("q")
bone.press("w")
eq(log[4], "window q", "a window's own keys")
eq(log[5], "window w", "then its on_key")
bone.ui.close(win)

local p = bone.ui.panel.open({ id = "conf_keys_panel", lines = { "x" }, focus = true,
  keys = { j = function(panel) log[#log + 1] = "panel j" end } })
bone.press("j")
eq(log[6], "panel j", "a focused panel's keys")
p:close()

local acted
bone.ui.actions.new_session = function() acted = true; return true end
bone.action("new_session")
eq(acted, true, "Lua can take a built-in action")
bone.ui.actions.new_session = nil
