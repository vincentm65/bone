-- The desktop library's tool calls, folded the way the TUI's are
-- (bone.o.tool_detail, ctrl+t, a click): "summary" folds each run of calls
-- into one line ("Read 3 files, ran 2 shell commands"), "rows" is a card per
-- call with its output cut short (shell output to its first and last two
-- lines, other output to its first five), "full" shows everything. Edits
-- always show their diff and failures their output. A card is a title (a
-- status mark, what it did, on what) in the window's font, and below it the
-- output, diff or report in monospace.
local M = {}

local md = require("bone.desktop.markdown")
local nodes = require("bone.nodes")
local timing = require("bone.timing")
local style = require("bone.nodes.style")

local GUTTER_MAX = 5 -- shell output rows before the first 2, "⋮ +N", last 2
local PREVIEW_MAX = 5 -- other output rows
local EDIT_MAX = 24 -- diff rows of an edit
local AGENT_MAX = 3 -- paragraphs of a sub-agent's report
local HINT = " (click or ctrl+t)"

if not pcall(bone.o.info, "tool_detail") or not bone.o.info("tool_detail") then
  bone.o.define("tool_detail", "summary", {
    desc = "tool calls: summary, rows or full (ctrl+t)",
    choices = { "summary", "rows", "full" },
    on_change = function() bone.ui.refresh() end,
  })
end

--- What a call's title says it did, by tool name.
M.labels = {
  read_file = "Read", write_file = "Wrote", edit_file = "Edited", shell = "Ran",
  grep = "Searched", rg = "Searched", glob = "Listed", list_files = "Listed",
  web_search = "Searched the web", web_fetch = "Fetched", subagent = "Agent",
}

local KEYS = { "path", "file_path", "command", "pattern", "query", "url", "task", "prompt" }

local function args_of(item)
  return type(item.arguments) == "table" and item.arguments or {}
end

-- A path cut to `max` characters in the middle ("/tmp/…/dir/file.txt"),
-- the file name kept.
local function shorten(v, max)
  if #v <= max then return v end
  local segs = {}
  for seg in v:gmatch("[^/]+") do segs[#segs + 1] = seg end
  local lead = v:sub(1, 1) == "/" and "/" or ""
  if #segs < 3 or #segs[#segs] + #lead + #segs[1] + 4 > max then
    return "…" .. v:sub(-(max - 1))
  end
  local tail, i = { segs[#segs] }, #segs - 1
  while i > 1 and #lead + #segs[1] + 3 + #table.concat(tail, "/") + #segs[i] + 1 <= max do
    table.insert(tail, 1, segs[i])
    i = i - 1
  end
  return lead .. segs[1] .. "/…/" .. table.concat(tail, "/")
end

-- What the call acted on, relative to the session's folder; a path cut to
-- `max` characters when there is one.
local function target(item, max)
  local a = args_of(item)
  for _, k in ipairs(KEYS) do
    if type(a[k]) == "string" and a[k] ~= "" then
      local v = a[k]:gsub("%s+", " ")
      local s = bone.chat.session()
      local prefix = ((s and s.cwd) or ""):gsub("/$", "") .. "/"
      if #prefix > 1 and v:sub(1, #prefix) == prefix then v = v:sub(#prefix + 1) end
      if max and (k == "path" or k == "file_path") then v = shorten(v, max) end
      return v
    end
  end
  return ""
end

-- ---- text ------------------------------------------------------------------

local function lines(text)
  local out = {}
  for line in ((text or ""):gsub("%s+$", "") .. "\n"):gmatch("(.-)\n") do out[#out + 1] = line end
  return out
end

-- Lines without blank ones at either end.
local function trimmed_lines(text)
  local ls = lines(text)
  while #ls > 0 and ls[#ls]:match("^%s*$") do ls[#ls] = nil end
  while #ls > 0 and ls[1]:match("^%s*$") do table.remove(ls, 1) end
  return ls
end

local function plural(n, noun)
  return n .. " " .. noun .. (n == 1 and "" or "s")
end

-- The shell tool ends its output with "[exit code: N]".
local function split_exit_code(text)
  local t = (text or ""):gsub("%s+$", "")
  local body, last = t:match("^(.*)\n([^\n]*)$")
  if not body then body, last = "", t end
  local code = last:match("^%[exit code: (%-?%d+)%]$")
  if not code then body = text or "" end
  if body:match("^%s*%(no output%)%s*$") then body = "" end
  return body, tonumber(code)
end

-- Stored legacy read_file rows are "LINE#HASH|text".
local function anchored(l)
  local n, text = l:match("^(%d+)#%w%w|(.*)$")
  return tonumber(n), text
end

-- read_file's output as rows { n, text, notice }: numbered lines, and the
-- notices around them (an empty file, a cut-short read).
local function read_rows(text, args)
  if text == "(empty file)" then return { { text = text, notice = true } } end
  local ls, notice = lines(text), nil
  local last = ls[#ls]
  if #ls > 1 and ls[#ls - 1] == "" and last and (
      last:match("^%[showing lines %d+%-%d+ of %d+; use offset to read more%]$")
      or last:match("^%[showing lines %d+%-%d+ of %d+; %d+ lines? not shown")
    ) then
    notice = table.remove(ls)
    table.remove(ls)
  end
  local legacy, has_anchor = true, false
  for _, l in ipairs(ls) do
    if anchored(l) then
      has_anchor = true
    elseif l ~= "" and not l:match("^%[lines %d+%-%d+ of %d+%]$") then
      legacy = false
    end
  end
  legacy = legacy and has_anchor
  local rows, next_n = {}, math.max(tonumber(args.offset) or 1, 1)
  for _, l in ipairs(ls) do
    local n, body
    if legacy then
      n, body = anchored(l)
    else
      n, body = next_n, l
      next_n = next_n + 1
    end
    rows[#rows + 1] = { n = n, text = body or l, notice = not n }
  end
  if notice then rows[#rows + 1] = { text = notice, notice = true } end
  return rows
end

local function read_summary(rows)
  local first, last, n = nil, nil, 0
  for _, r in ipairs(rows) do
    if r.n then first, last, n = first or r.n, r.n, n + 1 end
  end
  if n == 0 then return "0 lines" end
  return "lines " .. first .. "-" .. last .. " · " .. n .. " read"
end

-- Unchanged lines kept around a change. A longer gap is a hunk break.
local CONTEXT = 3
-- Past this many line pairs, fall back to one hunk instead of a full diff.
local DIFF_LIMIT = 200000

local function common_edges(a, b)
  local s = 0
  while s < #a and s < #b and a[s + 1] == b[s + 1] do s = s + 1 end
  local e = 0
  while e < #a - s and e < #b - s and a[#a - e] == b[#b - e] do e = e + 1 end
  return s, e
end

-- Line diff, deletions before additions where a line was replaced.
local function lcs_rows(a, b)
  local n, m = #a, #b
  local len = {}
  for i = 0, n do
    local row, prev = {}, len[i - 1]
    len[i] = row
    for j = 0, m do
      if i == 0 or j == 0 then
        row[j] = 0
      elseif a[i] == b[j] then
        row[j] = prev[j - 1] + 1
      else
        local up, left = prev[j], row[j - 1]
        row[j] = up > left and up or left
      end
    end
  end
  local rev, i, j = {}, n, m
  while i > 0 or j > 0 do
    if i > 0 and j > 0 and a[i] == b[j] then
      rev[#rev + 1] = { sign = " ", text = a[i] }
      i, j = i - 1, j - 1
    elseif j > 0 and (i == 0 or len[i][j - 1] >= len[i - 1][j]) then
      rev[#rev + 1] = { sign = "+", text = b[j] }
      j = j - 1
    else
      rev[#rev + 1] = { sign = "-", text = a[i] }
      i = i - 1
    end
  end
  local rows = {}
  for k = #rev, 1, -1 do rows[#rows + 1] = rev[k] end
  return rows
end

-- Changes, with CONTEXT lines around each. One omitted line is shown (an
-- ellipsis would be no shorter); a longer omission is "···".
local function trim_context(rows)
  local keep, any = {}, false
  for i, r in ipairs(rows) do
    if r.sign ~= " " then
      any = true
      for j = math.max(1, i - CONTEXT), math.min(#rows, i + CONTEXT) do keep[j] = true end
    end
  end
  if not any then return {} end
  local out, skipped = {}, {}
  for i, r in ipairs(rows) do
    if keep[i] then
      if #out > 0 and #skipped > 1 then
        out[#out + 1] = { separator = true }
      elseif #out > 0 and #skipped == 1 then
        out[#out + 1] = skipped[1]
      end
      skipped = {}
      out[#out + 1] = r
    else
      skipped[#skipped + 1] = r
    end
  end
  return out
end

-- Changed lines, with a little unchanged text around them in sign " ".
local function diff_lines(old, new)
  local a, b = lines(old), lines(new)
  local s, e = common_edges(a, b)
  local ma, mb = {}, {}
  for i = s + 1, #a - e do ma[#ma + 1] = a[i] end
  for i = s + 1, #b - e do mb[#mb + 1] = b[i] end
  if #ma == 0 and #mb == 0 then return {} end
  local mid
  if #ma * #mb <= DIFF_LIMIT then
    mid = lcs_rows(ma, mb)
  else
    mid = {}
    for _, t in ipairs(ma) do mid[#mid + 1] = { sign = "-", text = t } end
    for _, t in ipairs(mb) do mid[#mid + 1] = { sign = "+", text = t } end
  end
  local rows = {}
  for i = math.max(1, s - CONTEXT + 1), s do rows[#rows + 1] = { sign = " ", text = a[i] } end
  for _, r in ipairs(mid) do rows[#rows + 1] = r end
  local tail = #a - e
  for i = tail + 1, math.min(#a, tail + CONTEXT) do rows[#rows + 1] = { sign = " ", text = a[i] } end
  return trim_context(rows)
end

-- ---- rows ------------------------------------------------------------------
-- A body is a list of rows, each a list of spans; it is drawn as one
-- monospace band.

local function row(text, hl)
  return { { text = (text:gsub("\t", "    ")), hl = hl } }
end

local function more_row(n, noun, expanded)
  return { { text = "⋮ +" .. plural(n, noun) .. (expanded and "" or HINT), hl = "Dim" } }
end

-- The first and last two rows, "⋮ +N lines" between, unless expanded.
local function head_tail(rows, expanded)
  if expanded or #rows <= GUTTER_MAX then return rows end
  return { rows[1], rows[2], more_row(#rows - 4, "line"), rows[#rows - 1], rows[#rows] }
end

-- The first `max` rows, "⋮ +N more lines" after them, unless expanded.
local function head(rows, max, expanded)
  if expanded or #rows <= max then return rows end
  local out = {}
  for i = 1, max do out[i] = rows[i] end
  out[#out + 1] = more_row(#rows - max, "more line", expanded)
  return out
end

local function text_rows(text, hl)
  local out = {}
  for _, l in ipairs(trimmed_lines(text)) do out[#out + 1] = row(l, hl) end
  return out
end

-- Rows as one monospace band.
local function band(rows)
  local spans = {}
  for i, r in ipairs(rows) do
    if i > 1 then spans[#spans + 1] = { text = "\n", hl = "ToolOutput" } end
    for _, s in ipairs(r) do spans[#spans + 1] = s end
  end
  return md.band(spans, { size = md.small_size - 1, gap = 6 })
end

-- An edit's changed lines: "- old", "+ new", grey context around them,
-- and a separator between the edits of a batch.
local function diff_rows(args)
  local edits = type(args.edits) == "table" and args.edits
    or (type(args.old_string) == "string" and { args })
    or nil
  local rows, plus, minus, all = {}, 0, 0, false
  for i, e in ipairs(edits or {}) do
    if i > 1 then rows[#rows + 1] = { separator = true } end
    for _, r in ipairs(diff_lines(e.old_string or "", e.new_string or "")) do
      rows[#rows + 1] = r
      if r.sign == "+" then plus = plus + 1 elseif r.sign == "-" then minus = minus + 1 end
    end
    all = all or e.replace_all == true
  end
  return rows, plus, minus, #(edits or {}), all
end

-- One dark code block. Changed rows are highlighted flat across the width
-- (no radius of their own: that scalloped every line). Unchanged rows stay
-- the block's color, in grey. "···" between hunks. Cut to `cap` rows unless
-- expanded.
local function diff_nodes(rows, cap, expanded)
  if #rows == 0 then return {} end
  local shown, cut = {}, 0
  if not expanded and #rows > cap then
    for i = 1, cap do shown[i] = rows[i] end
    cut = #rows - cap
  else
    shown = rows
  end
  local size = md.small_size - 1
  -- Vertical padding covers the corner curve, so a highlighted row's square
  -- background never paints over the block's rounded corners.
  local block = {
    direction = "column", bg = "CodeBlock", radius = style.radius.md,
    padding = { 8, 0 }, margin = { 6, 0, 0, 0 },
    font = "mono", size = size,
  }
  local function add(text, hl, bg, py)
    block[#block + 1] = style.finish({
      bg = bg, padding = { py or 1, 12 },
      { text = text, hl = hl, font = "mono", size = size, selectable = true },
    })
  end
  for _, r in ipairs(shown) do
    if r.separator then
      add("···", "Dim", nil, 6)
    else
      local hl, bg, gutter = "Dim", nil, "    "
      if r.sign == "+" then
        hl, bg, gutter = "DiffAdd", "DiffAdd", "  + "
      elseif r.sign == "-" then
        hl, bg, gutter = "DiffDelete", "DiffDelete", "  - "
      end
      add(gutter .. r.text:gsub("\t", "    "), hl, bg)
    end
  end
  if cut > 0 then add("⋮ +" .. plural(cut, "line") .. HINT, "Dim", nil, 4) end
  return { style.finish(block) }
end

-- ---- cards -----------------------------------------------------------------

local toggled = {}
local last_detail

local function detail()
  local d = bone.o.tool_detail
  d = (d == "rows" or d == "full") and d or "summary"
  if d ~= last_detail then
    toggled = {}
    last_detail = d
  end
  return d
end

-- Item keys are stable within a chat, but can repeat in another session.
local function tool_key(item)
  local s = bone.chat.session()
  return (s and s.session_id or "") .. ":" .. tostring(item.key or item.id)
end

-- A mark the proportional fonts all have.
local function mark(item)
  if not item.done then return { text = "•  ", hl = "ToolRunning" } end
  return { text = item.is_error and "×  " or "•  ", hl = item.is_error and "ToolError" or "ToolDone" }
end

-- A TUI line (a string, `{ "text", "Group" }`, `{ fill = … }`) as spans.
local function line_spans(line, hl)
  if type(line) == "string" then return { { text = line, hl = hl } } end
  local out = {}
  for _, s in ipairs(line or {}) do
    if type(s) == "string" then
      out[#out + 1] = { text = s, hl = hl }
    elseif type(s) == "table" and s.fill == nil then
      out[#out + 1] = { text = s.text or s[1] or "", hl = s.hl or s[2] or hl }
    end
  end
  return out
end

-- A call `bone.ui.tool_views[name]` draws: its `title` (a line, or a list
-- of them) in the window's font, and its `lines` in monospace under it;
-- or a `node` of its own.
local function custom_card(item, content)
  if content.node then return { content.node } end
  -- A list of lines, as the TUI tells them from one line.
  local title = content.title or item.name
  if not (type(title) == "table" and type(title[1]) == "table" and type(title[1][1]) == "table") then
    title = { title }
  end
  local result = {}
  for i, t in ipairs(title) do
    local spans = line_spans(t, "ToolName")
    if i == 1 then table.insert(spans, 1, mark(item)) end
    result[#result + 1] = md.text(spans, { size = md.small_size })
  end
  local rows = {}
  for _, l in ipairs(content.lines or {}) do rows[#rows + 1] = line_spans(l, "ToolOutput") end
  if #rows > 0 then result[#result + 1] = band(rows) end
  return result
end

--- The nodes of one call: its title, then its body. `expanded`: all of the
--- output, not the folded form.
local function card(item, expanded, width)
  local custom = bone.ui.tool_views[item.name]
  local content = custom and custom(item, { width = width, region = "chat", tool_expanded = expanded })
  if content then return custom_card(item, content) end
  local failed = item.is_error
  local a = args_of(item)
  local name = item.name
  local label = M.labels[name] or name
  -- Room for the label, the extras and the mark besides the path.
  local what = target(item, not expanded and math.min(60, math.max(24, width - 40)) or nil)
  local title = { mark(item), { text = label, hl = "ToolName", bold = true } }
  if what ~= "" then
    title[#title + 1] = { text = "  " .. what, hl = name == "shell" and "ShellProgram" or "ToolPath" }
  end
  local function extra(text, hl)
    title[#title + 1] = { text = "  " .. text, hl = hl or "Dim" }
  end

  local body, paras = nil, nil -- rows as one band; or nodes as they are
  local out = item.output or ""
  local ok = item.done and not failed
  if failed then
    body = head_tail(text_rows(out, "ToolError"), expanded)
  elseif name == "shell" then
    local text, code = split_exit_code(item.done and out or (item.live or ""))
    if code and code ~= 0 then extra("exit " .. code, "ToolError") end
    body = head_tail(text_rows(text, code and code ~= 0 and "ToolError" or "ToolOutput"), expanded)
  elseif name == "read_file" then
    local rows = ok and read_rows(out, a) or {}
    if ok then extra(read_summary(rows)) end
    if ok and expanded then
      body = {}
      for _, r in ipairs(rows) do
        if r.n then
          body[#body + 1] = { { text = string.format("%4d  ", r.n), hl = "ToolGutter" }, { text = r.text:gsub("\t", "    "), hl = "ToolOutput" } }
        else
          body[#body + 1] = row(r.text, "Dim")
        end
      end
    end
  elseif name == "write_file" then
    local content = trimmed_lines(a.content or "")
    if ok then extra("+" .. plural(#content, "line"), "DiffAdd") end
    if ok and expanded then
      local rows = {}
      for _, l in ipairs(content) do rows[#rows + 1] = { sign = "+", text = l } end
      paras = diff_nodes(rows, #rows, true)
    end
  elseif name == "edit_file" then
    local rows, plus, minus, n, all = diff_rows(a)
    if ok or not item.done then
      title[#title + 1] = { text = "  ", hl = "ToolArgs" }
      title[#title + 1] = { text = "+" .. plus, hl = "DiffAdd" }
      title[#title + 1] = { text = " -" .. minus, hl = "DiffDelete" }
      if n > 1 then extra(plural(n, "edit")) end
      if all then extra("replace all") end
      paras = diff_nodes(rows, EDIT_MAX, expanded)
    end
  elseif name == "subagent" then
    if ok then
      paras = {}
      for i, n in ipairs(md.nodes(out, "ToolOutput", { size = md.small_size, indent = 14 })) do
        if not expanded and i > AGENT_MAX then break end
        paras[#paras + 1] = n
      end
    end
  elseif ok then
    body = head(text_rows(out, "ToolOutput"), PREVIEW_MAX, expanded)
  end

  if item.duration_ms and item.duration_ms >= 1000 then
    extra(bone.util.duration(item.duration_ms / 1000))
  end
  local result = { md.text(title, { size = md.small_size }) }
  if body and #body > 0 then result[#result + 1] = band(body) end
  for _, p in ipairs(paras or {}) do result[#result + 1] = p end
  return result
end

-- ---- summaries -------------------------------------------------------------

-- Folded into a summary line: everything but edits (their diff always
-- shows) and sub-agents.
local function foldable(t)
  return t.name ~= "edit_file" and t.name ~= "subagent"
end

-- What a run of calls did, in the order first seen: "Read 3 files, ran 2
-- shell commands, called web_search 2 times".
local function summary(calls)
  local order, seen = {}, {}
  local function bump(key, phrase, unique)
    local s = seen[key]
    if not s then
      s = { phrase = phrase, n = 0, unique = {} }
      seen[key] = s
      order[#order + 1] = s
    end
    if unique then
      if not s.unique[unique] then
        s.unique[unique] = true
        s.n = s.n + 1
      end
    else
      s.n = s.n + 1
    end
  end
  for _, t in ipairs(calls) do
    local path = args_of(t).path
    if t.name == "read_file" then
      bump("read", function(n) return "read " .. plural(n, "file") end, type(path) == "string" and path or t.id)
    elseif t.name == "write_file" then
      bump("write", function(n) return "wrote " .. plural(n, "file") end, type(path) == "string" and path or t.id)
    elseif t.name == "shell" then
      bump("shell", function(n) return "ran " .. plural(n, "shell command") end)
    else
      bump("tool:" .. t.name, function(n) return "called " .. t.name .. (n > 1 and " " .. n .. " times" or "") end)
    end
  end
  local parts = {}
  for _, s in ipairs(order) do parts[#parts + 1] = s.phrase(s.n) end
  local text = table.concat(parts, ", ")
  local failed = 0
  for _, t in ipairs(calls) do failed = failed + (t.done and t.is_error and 1 or 0) end
  text = text:sub(1, 1):upper() .. text:sub(2)
  return failed > 0 and (text .. ", " .. failed .. " failed") or text
end

--- In "summary": the line for the first call of a stretch (the calls between
--- edits, reasoning aside); nil for the rest; false when this
--- call is not folded. A click on the line opens every call of it.
local function summary_nodes(item, width)
  if not item.index or not foldable(item) then return false end
  local i = item.index - 1
  local before = bone.chat.item(i)
  while before and before.kind == "reasoning" do
    i = i - 1
    before = bone.chat.item(i)
  end
  if before and before.kind == "tool" and foldable(before) then return nil end
  local stretch = {}
  for _, t in ipairs(bone.chat.items({ around = item.index, from = item.index, kind = { "tool", "reasoning" } })) do
    if t.kind == "tool" then
      if not foldable(t) then break end
      stretch[#stretch + 1] = t
    end
  end
  local running = false
  for _, t in ipairs(stretch) do running = running or not t.done end
  local paras = { md.text({
    { text = "•  ", hl = running and "ToolRunning" or "ToolDone" },
    { text = summary(stretch), hl = "ToolArgs" },
  }, { size = md.small_size }) }
  if toggled[tool_key(item)] then
    for _, t in ipairs(stretch) do
      for _, p in ipairs(card(bone.chat.item(t.index, { full = true }), true, width)) do
        paras[#paras + 1] = p
      end
    end
  end
  return paras
end

function M.view(item, ctx)
  local mode = detail()
  local paras = false
  if mode == "summary" then paras = summary_nodes(item, ctx.width) end
  if paras == nil then return nil end
  if paras == false then
    local expanded = mode == "full"
    if toggled[tool_key(item)] then expanded = not expanded end
    paras = card(item, expanded, ctx.width)
  end
  local text = timing.footer(item, ctx)
  if text then
    paras[#paras + 1] = md.text({ { text, "Dim" } }, { size = md.small_size, gap = 10 })
  end
  local fallback = { { { (M.labels[item.name] or item.name) .. " " .. target(item), "ToolName" } } }
  local node = style.finish({ direction = "column", padding = { 2, 4 }, margin = { 4, 0, 2, 0 } })
  nodes.append(node, paras)
  return { node = node, lines = fallback, click = function() M.toggle(item) end }
end

--- Open or fold a call (a summary: every call of it).
function M.toggle(item)
  detail() -- local choices end when ctrl+t changes the level
  local key = tool_key(item)
  toggled[key] = not toggled[key] or nil
  bone.chat.redraw(item.index)
end

-- ctrl+t: every call one step more (or less) detailed.
local next_detail = { summary = "rows", rows = "full", full = "summary" }
bone.keymap.set("ctrl+t", function()
  bone.o.tool_detail = next_detail[bone.o.tool_detail] or "rows"
  bone.settings.set("gui.tool_detail", bone.o.tool_detail)
end)

return M
