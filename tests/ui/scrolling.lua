-- Scrolling the chat from Lua, the same in every frontend: by rows, to an
-- item, to the top and back to the end, and what the view reports.
for i = 1, 80 do
  bone.chat.add("conf_line", { text = "line " .. i })
end
bone.ui.views.conf_line = function(item) return { { { item.text, "Normal" } } } end
bone.chat.redraw()

return {
  function()
    local v = bone.chat.view()
    eq(v.follow, true, "a new chat follows the end")
    assert(v.rows >= 80, "every item drew: " .. v.rows)
    assert(v.height > 0, "the chat has rows on screen")
    bone.chat.scroll("top")
  end,
  function()
    local v = bone.chat.view()
    eq(v.top, 0, "scrolled to the top")
    eq(v.follow, false, "no longer following")
    eq(v.first, 1, "the first item shows")
    bone.chat.scroll(5)
  end,
  function()
    eq(bone.chat.view().top, 5, "down five rows")
    assert(bone.chat.scroll_to(40), "item 40 is drawn")
  end,
  function()
    local v = bone.chat.view()
    assert(v.first <= 40 and v.last >= 40, "item 40 shows: " .. v.first .. "-" .. v.last)
    eq(bone.chat.scroll_to(100000), false, "no such item")
    bone.chat.scroll("bottom")
  end,
  function()
    eq(bone.chat.view().follow, true, "back to following the end")
  end,
}
