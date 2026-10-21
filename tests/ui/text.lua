-- bone.text and bone.markdown: pure, shared by every frontend.
eq(bone.text.width("abc"), 3, "width")
eq(bone.text.width("日本"), 4, "wide width")
eq(bone.text.truncate("abcdef", 4), "abc…", "truncate")

local lines = bone.text.wrap({ { "one two three", "Normal" } }, 9)
eq(#lines, 2, "wrapped rows")

local blocks = bone.markdown.parse("# Title\n\nsome **bold** text")
eq(blocks[1].kind, "heading", "first block")
eq(blocks[1].level, 1, "heading level")

return {
  function()
    -- A step sees what the chunk left behind.
    eq(#lines, 2, "kept between steps")
  end,
}
