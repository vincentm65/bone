-- bone.ui.layout laid out by the shared solver, read back with
-- bone.ui.geometry() (character units, any screen size).
bone.ui.regions.conf_top = {
  size = 2,
  render = function(ctx)
    return { "first", "second" }
  end,
}
bone.ui.statusline = function(ctx)
  return { { "status", "StatusLine" } }
end
bone.ui.layout = { "conf_top", "chat", "prompt", "statusline" }

return {
  function()
    local at = {}
    for _, l in ipairs(bone.ui.geometry().leaves) do
      at[l.name] = l
    end
    eq(at.conf_top and at.conf_top.y, 0, "the region is first")
    eq(at.conf_top.height, 2, "at its size")
    eq(at.chat.y, 2, "the chat below it")
    eq(at.prompt.y, at.chat.y + at.chat.height, "the prompt below the chat")
    eq(at.statusline.height, 1, "a one-row statusline")
    eq(at.statusline.y, at.prompt.y + at.prompt.height, "last")
    eq(at.chat.width, at.conf_top.width, "rows span the width")
  end,
}
