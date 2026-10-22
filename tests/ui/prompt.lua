-- The prompt from Lua, the same in every frontend: the cursor and the
-- selection, editing at them, positions and offsets, and recalling sent
-- messages with up and down.
bone.prompt.set("hello world\nsecond")
local c = bone.prompt.cursor()
eq(c.row, 1, "set leaves the cursor on the last line")
eq(c.col, 6, "at its end")
eq(bone.prompt.offset({ row = 1, col = 0 }), 12, "an offset counts the line break")
local p = bone.prompt.position(6)
eq(p.row, 0, "offset 6 is on the first line")
eq(p.col, 6, "after 'hello '")

bone.prompt.set_cursor({ row = 0, col = 5 })
bone.prompt.insert(",")
eq(bone.prompt.get(), "hello, world\nsecond", "insert at the cursor")
eq(bone.prompt.cursor().col, 6, "the cursor after it")

bone.prompt.select({ row = 0, col = 7 }, { row = 0, col = 12 })
local sel = bone.prompt.selection()
eq(sel.text, "world", "the selection's text")
bone.prompt.insert("there")
eq(bone.prompt.get(), "hello, there\nsecond", "insert replaces the selection")
eq(bone.prompt.selection(), nil, "and clears it")

bone.prompt.set_range({ row = 1, col = 0 }, { row = 1, col = 6 }, "2nd")
eq(bone.prompt.get_range(0, 5), "hello", "get_range by offsets")
eq(bone.prompt.lines()[2], "2nd", "set_range replaced the second line")

-- Up on the first line recalls what was sent; down comes back.
bone.prompt.history_add("sent before")
bone.prompt.set("draft")
bone.press("up")
eq(bone.prompt.get(), "sent before", "up recalls the last sent message")
eq(bone.prompt.info().history, true, "browsing history")
bone.press("down")
eq(bone.prompt.get(), "draft", "down brings the draft back")
bone.prompt.set("")

-- A long paste is a placeholder in the prompt, the whole text when sent.
return {
  function()
    __paste = string.rep("x", 600)
  end,
  function()
    local shown = bone.prompt.info().text
    assert(shown:match("^%[Pasted text #1 %+600 chars%]$"), "a placeholder: " .. shown)
    eq(bone.prompt.get(), string.rep("x", 600), "the text in full")
    bone.prompt.set("")
  end,
}
