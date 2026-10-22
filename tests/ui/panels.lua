-- bone.ui.panel: docked panels (shared state and ops; each frontend paints).
local drawn
local p = bone.ui.panel.open({
  id = "conf_panel",
  dock = "right",
  size = 16,
  title = "Conformance",
  render = function(ctx)
    drawn = ctx
    local lines = {}
    for i = 1, 200 do
      lines[i] = "row " .. i
    end
    return lines
  end,
})
eq(p:info().dock, "right", "docked right")
eq(pcall(bone.ui.panel.open, { id = "conf_panel", lines = {} }), false, "ids are unique")
eq(pcall(bone.ui.panel.open, { id = "conf_bad" }), false, "a panel needs content")

return {
  function()
    local info = p:info()
    eq(info.visible, true, "drawn")
    eq(info.width, 16, "at its size")
    eq(drawn and drawn.width, 16, "content drawn at the panel's width")
    eq(info.rows, 200, "knows its rows")
    local chat
    for _, l in ipairs(bone.ui.geometry().leaves) do
      if l.name == "chat" then chat = l end
    end
    eq(chat.x + chat.width < info.col, true, "the chat gave it room")
    bone.ui.panel.focus("conf_panel")
    eq(bone.ui.panel.focused(), "conf_panel", "focused")
    eq(bone.keymap.current(), "panel", "a focused panel takes the panel context")
    p:scroll(5)
  end,
  function()
    eq(p:info().top, 5, "scrolled")
    p:hide()
    eq(bone.ui.panel.focused(), nil, "a hidden panel loses the keyboard")
    eq(bone.keymap.current(), "main", "back to the prompt")
  end,
  function()
    eq(p:info().visible, false, "hidden panels are not drawn")
    p:close()
    eq(bone.ui.panel.get("conf_panel"), nil, "closed")
  end,
}
