-- bone.elements: one view for both frontends, drawn by each one's library,
-- and default highlight groups under the colorscheme.
local E = require("bone.elements")
local side = bone.api_info().side

-- A default group sits under the colorscheme and follows its link live.
bone.hl.set("ConfDefault", { link = "Success", bold = true, default = true })
eq(bone.hl.get("ConfDefault").fg, bone.hl.get("Success").fg, "a default follows its link")
bone.colorscheme("black")
eq(bone.hl.get("ConfDefault").fg, bone.hl.get("Success").fg, "and keeps following it")
bone.hl.set("ConfDefault", { fg = "#123456" })
eq(bone.hl.get("ConfDefault").fg, "#123456", "a group set the usual way wins over a default")
bone.hl.reset()
eq(bone.hl.get("ConfDefault").bold, true, "a reset keeps the defaults")
bone.colorscheme("black")

local pressed, picked = {}, nil
local function tree()
  return {
    kind = "column",
    { kind = "tabs", names = { "One", "Two" }, current = picked or 1, on_select = function(i) picked = i end },
    { kind = "section", title = "Things",
      { kind = "checklist", items = {
        { text = "first", state = "done" }, { text = "second", state = "active" },
      }, on_press = function(i) pressed[#pressed + 1] = "check" .. i end },
      { kind = "list", items = { { text = "a", detail = "x", on = true }, { text = "b" } },
        on_press = function(i) pressed[#pressed + 1] = "list" .. i end },
    },
    { kind = "row", { kind = "badge", text = "new" }, { kind = "meter", value = 0.5, label = "half" },
      { kind = "button", label = "Go", on_press = function() pressed[#pressed + 1] = "go" end } },
    { kind = "diff", rows = { { sign = "+", text = "added" }, { separator = true }, { sign = "-", text = "gone" } } },
    { kind = "field", label = "Name", value = "", empty = "none" },
    { kind = "code", text = "x = 1" },
    { kind = "divider" }, { kind = "spinner" }, { kind = "hints", text = "↑↓ move · enter press" },
  }
end

-- Both renderers draw every kind, in either frontend.
local st = {}
local lines = E.lines(tree(), 30, st, true)
eq(#st.targets, 7, "two tabs, two checks, two entries and a button are targets")
local text = {}
for _, l in ipairs(lines) do
  local row = {}
  for _, s in ipairs(l) do row[#row + 1] = type(s) == "table" and (s[1] or "") or s end
  text[#text + 1] = table.concat(row)
end
text = table.concat(text, "\n")
eq(text:find("✓ first", 1, true) ~= nil, true, "a done check")
eq(text:find("[\194\160Go\194\160]", 1, true) ~= nil, true, "a button (never broken across lines)")
eq(text:find("Name: none", 1, true) ~= nil, true, "an empty field")
if side == "gui" then
  local node = E.node(tree(), { width = 300, height = 400 }, {}, true)
  eq(type(node), "table", "a node")
end

-- The cursor: up/down through the targets, left/right on tabs, enter presses.
eq(E.key(st, "right"), true, "right on a tab")
eq(picked, 2, "picks the next tab")
E.lines(tree(), 30, st, true)
for _ = 1, 2 do E.key(st, "down") end
eq(E.key(st, "enter"), true, "enter")
eq(pressed[1], "check2", "presses the target the cursor is on")
for _ = 1, 9 do E.key(st, "down") end
E.key(st, "space")
eq(pressed[2], "go", "the cursor stops at the last target")
eq(E.key({ targets = {} }, "down"), false, "a view with no targets leaves the keys")

-- A list that keeps its plugin's selection: the cursor starts on it, moving
-- selects, and the list's own keys come first.
local sel, folded = 2, nil
local function picker()
  return { kind = "split",
    { kind = "list", selected = sel, on_select = function(i) sel = i end,
      on_key = function(key, i) if key == "left" then folded = i return true end end,
      items = { { text = "one" }, { text = "two", depth = 1, badges = { { text = "new" } } }, { text = "three" } } },
    { kind = "column", { kind = "text", text = "a long detail that does not fit", truncate = true },
      { kind = "markdown", text = "# Title\n\nsome *text*" } },
  }
end
local ps = {}
local plines = E.lines(picker(), 60, ps, true)
eq(ps.cursor, 2, "the cursor starts on the selection")
E.key(ps, "down")
eq(sel, 3, "moving selects")
E.lines(picker(), 60, ps, true)
eq(E.key(ps, "left"), true, "the list's keys")
eq(folded, 3, "with the entry the cursor is on")
eq(#plines >= 3, true, "two columns")
local narrow = E.lines({ kind = "text", text = "a long detail that does not fit", truncate = true }, 10, {}, true)
eq(#narrow, 1, "a truncated text keeps to one line")
if side == "gui" then eq(type(E.node(picker(), { width = 400, height = 300 }, {}, true)), "table", "as nodes") end

-- A panel and a window given a view.
local p = bone.ui.panel.open({ id = "conf_view", dock = "right", size = 24, view = tree, focus = true })
eq(p:info() ~= nil, true, "a panel opens from a view")
if side == "gui" then
  local draw = bone.gui.panel_node("conf_view")
  eq(type(draw), "function", "and docks as nodes")
  eq(type(draw({ id = "conf_view", width = 200, height = 300, focused = true })), "table", "drawn as a node")
end
local w = bone.ui.popup({ view = tree, title = "Elements", width = 40 })
eq(bone.ui.is_open(w), true, "a window opens from a view")

return {
  function()
    eq(bone.ui.update(w, { view = function() return { kind = "text", text = "changed" } end }), true, "a new view")
    bone.ui.close(w)
    p:close()
    eq(bone.ui.panel.get("conf_view"), nil, "closed")
  end,
}
