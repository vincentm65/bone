-- A ui.lua's worth of customizing, the same in every frontend: a layout
-- with a region, a panel, a window, a chat view, keymaps, and a mouse
-- handler.
local seen = {}
bone.ui.regions.conf_banner = { size = 1, render = function() return { "banner" } end }
bone.ui.layout = { "conf_banner", "chat", "prompt" }
bone.ui.views.conf_card = function(item, ctx)
  seen.view_width = ctx.width
  return { { { "card " .. item.text, "Accent" } } }
end
bone.chat.add("conf_card", { text = "one" })
bone.ui.panel.open({ id = "conf_side", dock = "left", size = 12, lines = { "side" } })
bone.keymap.set("ctrl+g", function() seen.key = true end)
bone.on("mouse", function(ev)
  if ev.action == "down" then
    seen.click = ev.region
    seen.row, seen.col = ev.row, ev.col
  end
end)

local function leaf(name)
  for _, l in ipairs(bone.ui.geometry().leaves) do
    if l.name == name then return l end
  end
end

return {
  function()
    local banner, chat, prompt = leaf("conf_banner"), leaf("chat"), leaf("prompt")
    eq(banner.y, 0, "the region on top")
    eq(chat.y, 1, "the chat below it")
    eq(prompt.y, chat.y + chat.height, "the prompt below the chat")
    eq(chat.x, 13, "beside the 12-column panel and its separator")
    eq(seen.view_width, chat.width, "the view drew at the chat's width")
    bone.press("ctrl+g")
    eq(seen.key, true, "the keymap ran")
    local win = bone.ui.popup({ lines = { "window" }, focus = true, keys = { y = function() seen.popup_key = true end } })
    bone.press("y")
    eq(seen.popup_key, true, "the window's key ran")
    bone.ui.close(win)
    __click = { prompt.x + 3, prompt.y + 1 }
  end,
  function()
    eq(seen.click, "prompt", "the mouse handler saw the prompt")
    eq(seen.row, 2, "with the row in it")
    eq(seen.col, 4, "and the column")
  end,
}
