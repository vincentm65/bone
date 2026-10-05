-- How the transcript looks. Rust hands each item over as data and draws
-- whatever lines come back; all spacing, prefixes and colors are here.
-- Override any of these from tui.lua, e.g.:
--   bone.ui.views.reasoning = function(item, ctx) return nil end   -- hide it
--
-- item: { kind, index, text, streaming }           user/reasoning/assistant
--       { kind, index, text, error }                notice
--       { kind, index, id, name, arguments, raw_arguments,
--         output, is_error, done }                   tool
--       { kind, index, id, text, mode, position }  queued (mode: "steer" or "next")
-- ctx:  { width, region, prev = { kind } }
-- Returns a list of lines; each line is a list of { "text", "Group" }.

local views = bone.ui.views
local wrap = bone.text.wrap

local function append(out, rows)
  for _, r in ipairs(rows) do
    out[#out + 1] = r
  end
  return out
end

-- Like Rust's str::lines: split on "\n", no trailing empty line.
local function lines(s)
  local t = {}
  if s == nil or s == "" then
    return t
  end
  for l in (s .. "\n"):gmatch("(.-)\n") do
    t[#t + 1] = l
  end
  if s:sub(-1) == "\n" then
    t[#t] = nil
  end
  return t
end

local function trim(s)
  return (s:gsub("^%s+", ""):gsub("%s+$", ""))
end

-- A blank line between exchanges; tool calls stack under their message.
local function starts(kind, ctx)
  local p = ctx.prev
  if not p then
    return {}
  end
  if kind == "tool" and (p.kind == "assistant" or p.kind == "reasoning" or p.kind == "tool") then
    return {}
  end
  return { {} }
end

-- ---- markdown -------------------------------------------------------------

local function md_spans(spans, base)
  local out = {}
  for _, s in ipairs(spans) do
    if s.code then
      out[#out + 1] = { s.text, "MdCode" }
    elseif s.link then
      out[#out + 1] = { s.text, "MdLink" }
      if s.link ~= s.text then
        out[#out + 1] = { " <" .. s.link .. ">", "Dim" }
      end
    elseif s.bold then
      out[#out + 1] = { s.text, "MdBold" }
    elseif s.italic then
      out[#out + 1] = { s.text, "MdItalic" }
    else
      out[#out + 1] = { s.text, base }
    end
  end
  return out
end

-- Lay a table block out to fixed-width lines. Column widths come from the
-- widest cell; if the table is wider than `width`, the rightmost columns are
-- truncated with an ellipsis. Cells are plain spans (no wrapping).
local function md_table(b, width, indent)
  local max_cols = 0
  for _, row in ipairs(b.rows) do
    max_cols = math.max(max_cols, #row)
  end
  local w = {}
  for c = 1, max_cols do
    w[c] = 0
    for _, row in ipairs(b.rows) do
      local cell = row[c]
      if cell then
        local n = 0
        for _, s in ipairs(cell) do
          n = n + bone.text.width(s.text)
        end
        w[c] = math.max(w[c], n)
      end
    end
  end
  -- separators: one " │ " (3 wide) between columns.
  local seps = math.max(0, max_cols - 1) * 3
  local total = #indent + seps
  for c = 1, max_cols do
    total = total + w[c]
  end
  if total > width then
    -- Truncate from the right; each kept column stays at least 2 wide.
    local budget = width - #indent - seps
    for c = max_cols, 1, -1 do
      local rest = 0
      for k = 1, c - 1 do
        rest = rest + math.min(w[k], 2)
      end
      local room = math.max(2, budget - rest)
      w[c] = math.min(w[c], room)
      budget = budget - w[c]
    end
  end
  local out = {}
  for i, row in ipairs(b.rows) do
    local is_head = b.header and i == 1
    local hl = is_head and "MdTableHeader" or "MdTable"
    local line = { { indent, "Normal" } }
    for c = 1, max_cols do
      local cell = row[c] or {}
      local spans = md_spans(cell, hl)
      local n = 0
      for _, s in ipairs(spans) do
        n = n + bone.text.width(s[1])
      end
      for _, s in ipairs(spans) do
        line[#line + 1] = s
      end
      line[#line + 1] = { string.rep(" ", math.max(0, w[c] - n)), hl }
      if c < max_cols then
        line[#line + 1] = { " │ ", "Dim" }
      end
    end
    out[#out + 1] = line
  end
  return out
end

--- Markdown text to lines, every row prefixed with `indent`.
function bone.ui.markdown(text, width, indent)
  indent = indent or ""
  local pre = { { indent, "Normal" } }
  local out = {}
  for _, b in ipairs(bone.markdown.parse(text)) do
    if b.kind == "blank" then
      out[#out + 1] = {}
    elseif b.kind == "heading" then
      append(out, wrap(md_spans(b.spans, "MdHeading"), width, { first = pre, rest = pre }))
    elseif b.kind == "rule" then
      out[#out + 1] = { { indent, "Normal" }, { string.rep("─", math.min(width - #indent, 40)), "Dim" } }
    elseif b.kind == "quote" then
      local p = { { indent, "Normal" }, { "▏ ", "MdQuote" } }
      append(out, wrap(md_spans(b.spans, "MdQuote"), width, { first = p, rest = p }))
    elseif b.kind == "table" then
      append(out, md_table(b, width, indent))
    elseif b.kind == "item" then
      local lead = string.rep(" ", b.indent)
      local bullet = b.ordered and (b.marker .. " ") or "• "
      local first = { { indent .. lead, "Normal" }, { bullet, "MdBullet" } }
      local rest = { { string.rep(" ", #indent + #lead + bone.text.width(bullet)), "Normal" } }
      append(out, wrap(md_spans(b.spans, "Normal"), width, { first = first, rest = rest }))
    elseif b.kind == "code" then
      local p = { { indent, "Normal" }, { "▎ ", "ToolGutter" } }
      for _, l in ipairs(b.lines) do
        append(out, wrap({ { l, "MdCodeBlock" } }, width, { first = p, rest = p }))
      end
    else
      local first = { { indent .. string.rep(" ", b.indent), "Normal" } }
      append(out, wrap(md_spans(b.spans, "Normal"), width, { first = first, rest = pre }))
    end
  end
  return out
end

-- ---- messages -------------------------------------------------------------

function views.user(item, ctx)
  local out = starts("user", ctx)
  local band = "UserMessage"
  for i, line in ipairs(lines(item.text)) do
    local first = i == 1 and { { "› ", "UserPrompt" } } or { { "  ", band } }
    append(out, wrap({ { line, band } }, ctx.width, { first = first, rest = { { "  ", band } }, pad = band }))
  end
  return out
end

function views.reasoning(item, ctx)
  if not bone.o.show_reasoning or trim(item.text) == "" then
    return nil
  end
  local out = starts("reasoning", ctx)
  for _, line in ipairs(lines(trim(item.text))) do
    append(out, wrap({ { line, "Reasoning" } }, ctx.width, { first = { { "  ", "Normal" } } }))
  end
  return out
end

function views.assistant(item, ctx)
  local out = starts("assistant", ctx)
  local text = trim(item.text)
  if text == "" then
    if item.streaming then
      out[#out + 1] = { { "  …", "Dim" } }
    end
    return out
  end
  local md = bone.ui.markdown(text, ctx.width, "  ")
  if item.streaming then
    if #md > 0 then
      table.insert(md[#md], { "▍", "Dim" })
    else
      md[1] = { { "  ▍", "Dim" } }
    end
  end
  return append(out, md)
end

function views.notice(item, ctx)
  local hl = item.error and "ErrorMsg" or "Notice"
  return append(starts("notice", ctx), wrap({ { item.text, hl } }, ctx.width, { first = { { "  ! ", hl } } }))
end

function views.queued(item, ctx)
  local note = item.mode == "steer" and "joins this turn" or "next turn"
  local out = starts("queued", ctx)
  for i, line in ipairs(lines(item.text)) do
    local first = i == 1 and { { "◦ ", "Dim" } } or { { "  ", "Dim" } }
    append(out, wrap({ { line, "Dim" } }, ctx.width, { first = first }))
  end
  out[#out + 1] = { { "  " .. note, "StatusLineDim" } }
  return out
end

-- ---- tools ----------------------------------------------------------------

local function more(n)
  return { { "⋮ +" .. n .. " line" .. (n == 1 and "" or "s"), "ToolSummary" } }
end

--- Up to `max` rows of text: all of it if it fits, else the first row, a
--- "⋮ +N lines" marker and the last rows.
function bone.ui.preview(text, max, group)
  local ls = lines(text)
  while #ls > 0 and trim(ls[#ls]) == "" do
    ls[#ls] = nil
  end
  while #ls > 0 and trim(ls[1]) == "" do
    table.remove(ls, 1)
  end
  max = math.max(max, 1)
  local row = function(l)
    return { { l, group } }
  end
  local out = {}
  if #ls <= max then
    for _, l in ipairs(ls) do
      out[#out + 1] = row(l)
    end
  elseif max < 3 then
    for i = 1, max - 1 do
      out[#out + 1] = row(ls[i])
    end
    out[#out + 1] = more(#ls - (max - 1))
  else
    local tail = max - 2
    out[1] = row(ls[1])
    out[2] = more(#ls - 1 - tail)
    for i = #ls - tail + 1, #ls do
      out[#out + 1] = row(ls[i])
    end
  end
  return out
end

local preview = bone.ui.preview

-- The shell tool ends its output with "[exit code: N]".
local function split_exit_code(text)
  local t = (text or ""):gsub("%s+$", "")
  local body, last = t:match("^(.*)\n([^\n]*)$")
  if not body then
    body, last = "", t
  end
  local code = last:match("^%[exit code: (%-?%d+)%]$")
  if not code then
    return text or "", nil
  end
  if trim(body) == "(no output)" then
    body = ""
  end
  return body, tonumber(code)
end

local function read_summary(text)
  if trim(text) == "(empty file)" then
    return "empty"
  end
  local first, last, n = nil, nil, 0
  for _, l in ipairs(lines(text)) do
    local num = l:match("^%s*(%d+)\t")
    if num then
      first = first or num
      last = num
      n = n + 1
    end
  end
  local total = text:match("%[showing lines %d+%-%d+ of (%d+);")
  if first and total then
    return "lines " .. first .. "–" .. last .. " of " .. total
  elseif first then
    return n == 1 and "1 line" or (n .. " lines")
  end
  return "0 lines"
end

local function file_header(name, path, summary)
  local t = { { name .. " ", "ToolName" }, { path or "", "ToolPath" } }
  if summary then
    t[#t + 1] = { " (" .. summary .. ")", "ToolSummary" }
  end
  return t
end

-- Only the lines that changed: drop lines shared at the start and end.
local function diff_lines(old, new)
  local a, b = lines(old), lines(new)
  local s = 0
  while s < #a and s < #b and a[s + 1] == b[s + 1] do
    s = s + 1
  end
  local e = 0
  while e < #a - s and e < #b - s and a[#a - e] == b[#b - e] do
    e = e + 1
  end
  local removed, added = {}, {}
  for i = s + 1, #a - e do
    removed[#removed + 1] = a[i]
  end
  for i = s + 1, #b - e do
    added[#added + 1] = b[i]
  end
  return removed, added
end

--- Lines an anchored edit removed and added: from the diff in its result
--- (`-text`, `+LINE#HASH|text`), or from its arguments while it runs.
local function anchored_diff(args, out)
  local removed, added = {}, {}
  if out then
    for _, l in ipairs(lines(out)) do
      local text = l:match("^%+%d+#%w%w|(.*)$")
      if text then
        added[#added + 1] = text
      elseif l:sub(1, 1) == "-" then
        removed[#removed + 1] = l:sub(2)
      end
    end
    return removed, added
  end
  local edits = args.edits
  if type(edits) ~= "table" then
    edits = { args }
  end
  for _, e in ipairs(edits) do
    if type(e) == "table" then
      for _, l in ipairs(type(e.text) == "string" and lines(e.text) or {}) do
        added[#added + 1] = l
      end
      local from = tonumber(tostring(e.at or ""):match("^%s*(%d+)"))
      local to = tonumber(tostring(e["end"] or ""):match("^%s*(%d+)")) or from
      for _ = 1, from and to and math.max(to - from + 1, 0) or 0 do
        removed[#removed + 1] = "…"
      end
    end
  end
  return removed, added
end

local function diff_rows(removed, added, max, width)
  local room = math.max(width - 6, 1)
  local rows = {}
  local function row(sign, text, group)
    local body = bone.text.truncate(sign .. " " .. text, room)
    -- Pad so the colored band spans the row.
    rows[#rows + 1] = { { body .. string.rep(" ", room - bone.text.width(body)), group } }
  end
  for _, l in ipairs(removed) do
    row("-", l, "DiffDelete")
  end
  for _, l in ipairs(added) do
    row("+", l, "DiffAdd")
  end
  max = math.max(max, 2)
  if #rows > max then
    local hidden = #rows - (max - 1)
    for i = #rows, max, -1 do
      rows[i] = nil
    end
    rows[#rows + 1] = more(hidden)
  end
  return rows
end

--- The built-in content for a tool call: { title = spans, lines = { spans... } }.
function bone.ui.tool_content(item, ctx)
  local args = type(item.arguments) == "table" and item.arguments or {}
  local out = item.output
  local style = item.is_error and "ToolError" or "ToolOutput"
  local name = item.name
  if name == "shell" then
    local cmd = lines(args.command or "")
    local title = { { "$ ", "ToolSummary" } }
    append(title, bone.text.shell(cmd[1] or ""))
    if #cmd > 1 then
      title[#title + 1] = { " (+" .. (#cmd - 1) .. " lines)", "ToolSummary" }
    end
    local body, code = split_exit_code(out)
    if code and code ~= 0 then
      title[#title + 1] = { "  exit " .. code, "ToolError" }
    end
    return { title = title, lines = item.done and preview(body, bone.o.tool_preview_lines, style) or {} }
  elseif name == "read_file" then
    local summary = item.done and not item.is_error and read_summary(out) or nil
    return { title = file_header(name, args.path, summary), lines = {} }
  elseif name == "write_file" then
    local n = #lines(args.content or "")
    return { title = file_header(name, args.path, n .. " line" .. (n == 1 and "" or "s")), lines = {} }
  elseif name == "edit_file" then
    local removed, added
    if args.old_string then
      removed, added = diff_lines(args.old_string or "", args.new_string or "")
    else
      removed, added = anchored_diff(args, item.done and not item.is_error and out)
    end
    local summary = "+" .. #added .. " −" .. #removed
    local times = tonumber((out or ""):match("^Replaced (%d+) "))
    if times and times > 1 then
      summary = summary .. " ×" .. times
    end
    local body = {}
    if item.done and not item.is_error then
      body = diff_rows(removed, added, bone.o.diff_preview_lines, ctx.width)
    end
    return { title = file_header(name, args.path, summary), lines = body }
  end
  local summary = args.command or args.path or (item.raw_arguments:gsub("\n", " "))
  return {
    title = { { name .. " ", "ToolName" }, { summary, "ToolArgs" } },
    lines = item.done and preview(out or "", bone.o.tool_preview_lines, style) or {},
  }
end

local function as_spans(v, group)
  if type(v) == "string" then
    return { { v, group } }
  end
  return v or {}
end

--- One header row (two at most), then a few gutter rows of output.
function views.tool(item, ctx)
  local custom = bone.ui.tool_views[item.name]
  local content = custom and custom(item, ctx) or bone.ui.tool_content(item, ctx)
  local body = {}
  for _, l in ipairs(content.lines or {}) do
    body[#body + 1] = as_spans(l, "ToolOutput")
  end
  -- Errors always show what went wrong.
  if item.is_error and #body == 0 and not custom then
    body = preview(item.output or "", math.max(bone.o.tool_preview_lines, 2), "ToolError")
  end

  local marker
  if not item.done then
    marker = { "  ◌ ", "ToolRunning" }
  elseif item.is_error then
    marker = { "  ✕ ", "ToolError" }
  else
    marker = { "    ", "Normal" }
  end
  local out = starts("tool", ctx)
  local header = wrap(as_spans(content.title, "ToolName"), ctx.width, { first = { marker }, rest = { { "    ", "Normal" } } })
  for i = 1, math.min(#header, 2) do
    out[#out + 1] = header[i]
  end
  if #header > 2 then
    table.insert(out[#out], { "…", "ToolSummary" })
  end
  local gutter = item.is_error and "ToolError" or "ToolGutter"
  local room = math.max(ctx.width - 6, 1)
  for i, l in ipairs(body) do
    local row = { { i == #body and "    ╰ " or "    │ ", gutter } }
    append(row, bone.text.clip(l, room))
    out[#out + 1] = row
  end
  return out
end
