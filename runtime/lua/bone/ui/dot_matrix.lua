-- The new-chat canvas: a slowly curling field of Braille dots behind the
-- hint. Autonomous flow, with no mouse handlers.
local M = {}
-- Smooth value noise: random at lattice points, continuous between them.
-- The lattice is a shuffled table rather than trig. Its generator has a
-- fixed seed and time is the wall clock, so a Lua reload (an edit, a plugin
-- install) picks up the field exactly where it was.
local state = 12345
local function rand(n)
  state = state * 16807 % 2147483647 -- stays exact in a double
  return state % n
end
local perm, vals = {}, {}
for i = 0, 255 do perm[i], vals[i] = i, rand(65536) / 65535 end
for i = 255, 1, -1 do
  local j = rand(i + 1)
  perm[i], perm[j] = perm[j], perm[i]
end
local floor = math.floor
local function noise(x, y)
  local ix, iy = floor(x), floor(y)
  local u, v = x - ix, y - iy
  u, v = u * u * u * (u * (u * 6 - 15) + 10), v * v * v * (v * (v * 6 - 15) + 10)
  local px, px1 = perm[ix % 256], perm[(ix + 1) % 256]
  local a, b = vals[(px + iy) % 256], vals[(px1 + iy) % 256]
  local c, d = vals[(px + iy + 1) % 256], vals[(px1 + iy + 1) % 256]
  return (a + (b - a) * u) * (1 - v) + (c + (d - c) * u) * v
end
-- Ten field greys, darkest first; the hint text has its own two.
local colors = {
  "#070707", "#090909", "#0b0b0b", "#0d0d0d", "#0f0f0f",
  "#111111", "#131313", "#151515", "#171717", "#1a1a1a",
}
-- Highlight names and runs of full cells, built once rather than per cell.
local HL, RUN = {}, setmetatable({}, {
  __index = function(t, n) t[n] = ("⣿"):rep(n); return t[n] end,
})
for i = 1, #colors do HL[i] = "NewChatDot" .. i end
-- 20 frames a second; the field's cost is small next to the TUI's own.
local FRAME_MS = 50

-- The hint in the middle, as { text, highlight } lines.
local function hint()
  if bone.settings.get("setup.skipped") then
    return {
      { "No model provider configured.", "NewChatTitle" },
      { "Run /setup to connect one · /health to diagnose setup", "NewChatHint" },
    }
  end
  return {
    { "New session. Type a message and press enter.", "NewChatTitle" },
    { "/help lists commands · ctrl+o opens an earlier session", "NewChatHint" },
  }
end

local function line_width(line)
  local w = 0
  for _, span in ipairs(line) do w = w + bone.text.width(type(span) == "table" and span[1] or span) end
  return w
end

-- What plugins add under the hint (bone.ui.new_chat), in name order, each
-- block after a blank row. A block wider than `max` is left out, as is one
-- whose function fails.
local function sections(max)
  local names = {}
  for name in pairs(bone.ui.new_chat or {}) do names[#names + 1] = name end
  table.sort(names)
  local out = {}
  for _, name in ipairs(names) do
    local ok, lines = pcall(bone.ui.new_chat[name], { width = max })
    if ok and type(lines) == "table" and #lines > 0 then
      local fits = true
      for _, l in ipairs(lines) do fits = fits and line_width(l) <= max end
      if fits then
        out[#out + 1] = {}
        for _, l in ipairs(lines) do out[#out + 1] = l end
      end
    end
  end
  return out
end

-- The hint's rows (lines of spans, each centered) on a blank patch that
-- keeps the field clear of the text.
local PAD_X, PAD_Y = 3, 1
local function box(lines)
  local width = 0
  for _, l in ipairs(lines) do width = math.max(width, line_width(l)) end
  width = width + PAD_X * 2
  local blank = { { (" "):rep(width), "NewChatDot1" } }
  local rows = {}
  for _ = 1, PAD_Y do rows[#rows + 1] = blank end
  for _, l in ipairs(lines) do
    local w = line_width(l)
    local left = math.floor((width - w) / 2)
    local row = { { (" "):rep(left), "NewChatDot1" } }
    for _, span in ipairs(l) do row[#row + 1] = span end
    row[#row + 1] = { (" "):rep(width - left - w), "NewChatDot1" }
    rows[#rows + 1] = row
  end
  for _ = 1, PAD_Y do rows[#rows + 1] = blank end
  return rows, width
end

function M.setup()
  for i, color in ipairs(colors) do bone.hl.set("NewChatDot" .. i, { fg = color }) end
  bone.hl.set("NewChatTitle", { fg = "#c8c8c8" })
  bone.hl.set("NewChatHint", { fg = "#999999" })
  return function(ctx)
    -- The chat this pane draws: Rust only calls this while it has no rows.
    if ctx.session and ctx.session.session_id then return {} end
    local cols, rows = ctx.width, ctx.height
    if cols < 1 or rows < 1 then return {} end
    local t = bone.now() / 1000
    local lines = {}
    for _, l in ipairs(hint()) do lines[#lines + 1] = { l } end
    local hint_lines = #lines
    for _, l in ipairs(sections(cols - PAD_X * 2)) do lines[#lines + 1] = l end
    local boxed, box_width = box(lines)
    -- Too short for the plugins' sections: the hint alone.
    if #boxed > rows then
      for i = #lines, hint_lines + 1, -1 do lines[i] = nil end
      boxed, box_width = box(lines)
    end
    -- Too small for the box (a side panel open): the plain hint.
    if box_width > cols or #boxed > rows then
      local out = {}
      for _, l in ipairs(hint()) do
        for _, row in ipairs(bone.text.wrap({ { l[1], "Dim" } }, cols)) do out[#out + 1] = row end
      end
      return out
    end
    local box_x, box_y = math.floor((cols - box_width) / 2), math.floor((rows - #boxed) / 2)
    -- The curl terms depend on only the row or the column.
    local curl_x, curl_y = {}, {}
    for col = 0, cols - 1 do curl_y[col] = math.sin(col / 2 * 0.07 - t * 0.11) * 1.6 - t * 0.09 end
    for y = 0, rows - 1 do curl_x[y] = math.sin(y * 0.08 + t * 0.13) * 1.6 + t * 0.12 end
    -- Eight fine Braille dots per cell, with softly curling grey currents.
    local levels = #colors
    local function field(line, y, from, to)
      local run, run_level = 0, nil
      local cy, vy = curl_x[y], y * 0.09
      for col = from, to - 1 do
        -- A cell is half as wide as tall, so columns count half.
        local u = col * 0.045 + cy
        local v = vy + curl_y[col]
        local wave = noise(u, v) * 0.75 + noise(u * 2.1 - t * 0.08, v * 2.1 + 31) * 0.25
        -- The octaves rarely leave 0.15-0.85, so that span covers the shades.
        local level = floor((wave - 0.15) / 0.7 * levels) + 1
        if level < 1 then level = 1 elseif level > levels then level = levels end
        if level ~= run_level and run_level then
          line[#line + 1] = { RUN[run], HL[run_level] }
          run = 0
        end
        run, run_level = run + 1, level
      end
      if run_level then line[#line + 1] = { RUN[run], HL[run_level] } end
    end
    local out = {}
    for y = 0, rows - 1 do
      local line = {}
      local row = boxed[y - box_y + 1]
      if row then
        field(line, y, 0, box_x)
        for _, span in ipairs(row) do line[#line + 1] = span end
        field(line, y, box_x + box_width, cols)
      else
        field(line, y, 0, cols)
      end
      out[#out + 1] = line
    end
    if not ctx.popup then bone.ui.refresh_in(FRAME_MS) end
    return out
  end
end

return M
