-- bone.ui.popup: Lua windows (shared ops; each frontend paints them).
local events = {}
for _, name in ipairs({ "panel/opened", "panel/updated", "panel/closed", "focus/changed" }) do
  bone.on(name, function(ev)
    events[#events + 1] = name .. ":" .. tostring(ev.kind or ev.context)
  end)
end
local drawn
local id = bone.ui.popup({
  lines = function(ctx)
    drawn = ctx
    return { "a window", { { "with ", "Normal" }, { "groups", "Accent" } } }
  end,
  width = 20,
  focus = true,
})
eq(type(id), "number", "an id")
eq(bone.ui.is_open(id), true, "open")
eq(bone.keymap.current(), "popup", "a focused window has the keyboard")
eq(events[1], "panel/opened:popup", "opened")
eq(events[2], "focus/changed:popup", "focus moved to it")
eq(pcall(bone.ui.popup, { width = 3 }), false, "a window needs lines")

return {
  function()
    eq(drawn and drawn.width, 20, "drawn with the room it was given")
    eq(bone.ui.update(id, { focus = false }), true, "updated")
    eq(bone.keymap.current(), "main", "back to the prompt")
    bone.ui.close(id)
    eq(bone.ui.is_open(id), false, "closed")
    eq(bone.ui.update(id, {}), false, "a closed window cannot be updated")
    local last = events[#events]
    eq(last, "panel/closed:popup", "closed event")
  end,
}
