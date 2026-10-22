-- The desktop library's tool calls: a card per call, its title (a status
-- mark, what it did, on what) in the window's font, and below it, as
-- bone.o.tool_detail says, the output, diff or report in monospace.
-- "summary" shows titles only (failures and edits in full), "rows" a few
-- lines of each, "full" everything; ctrl+t steps through, and a click opens
-- or folds one call.
local M = {}

local md = require("bone.desktop.markdown")
local timing = require("bone.timing")

-- Output lines a call shows in "rows".
M.rows = 8

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

-- What the call acted on, relative to the session's folder.
local function target(item)
  local a = type(item.arguments) == "table" and item.arguments or {}
  for _, k in ipairs(KEYS) do
    if type(a[k]) == "string" and a[k] ~= "" then
      local v = a[k]:gsub("%s+", " ")
      local s = bone.chat.session()
      local prefix = ((s and s.cwd) or ""):gsub("/$", "") .. "/"
      if #prefix > 1 and v:sub(1, #prefix) == prefix then v = v:sub(#prefix + 1) end
      return v
    end
  end
  return ""
end

local function lines(text)
  local out = {}
  for line in ((text or ""):gsub("%s+$", "") .. "\n"):gmatch("(.-)\n") do out[#out + 1] = line end
  return out
end

-- The last `n` lines of `text` (all with n = nil), and how many were cut.
local function tail(text, n)
  local all = lines(text)
  if not n or #all <= n then return table.concat(all, "\n"), 0 end
  return table.concat(all, "\n", #all - n + 1), #all - n
end

-- An edit as diff lines: what went, then what came.
local function diff(item, n)
  local a = type(item.arguments) == "table" and item.arguments or {}
  local old, new = a.old_string or a.old or "", a.new_string or a.new or a.content or ""
  local spans, count = {}, 0
  local function add(text, sign, hl)
    if n and count >= n then return end
    count = count + 1
    spans[#spans + 1] = { text = (count > 1 and "\n" or "") .. sign .. " " .. text, hl = hl }
  end
  for _, l in ipairs(old ~= "" and lines(old) or {}) do add(l, "-", "DiffDelete") end
  for _, l in ipairs(new ~= "" and lines(new) or {}) do add(l, "+", "DiffAdd") end
  return spans
end

local toggled = {}

local function mode(item)
  local d = bone.o.tool_detail or "summary"
  if toggled[item.id] then
    d = d == "summary" and "rows" or (d == "rows" and "full" or "summary")
    if bone.o.tool_detail == "full" then d = "summary" end
  end
  return d
end

function M.view(item, ctx)
  local running, failed = not item.done, item.is_error
  -- Marks the proportional fonts all have.
  local mark = running and { "•  ", "ToolRunning" } or failed and { "×  ", "ToolError" } or { "•  ", "ToolDone" }
  local label = M.labels[item.name] or item.name
  local title = {
    { text = mark[1], hl = mark[2] },
    { text = label, hl = "ToolName", bold = true },
  }
  local what = target(item)
  if what ~= "" then
    title[#title + 1] = { text = "  " .. what, hl = item.name == "shell" and "ShellProgram" or "ToolPath" }
  end
  if item.duration_ms and item.duration_ms >= 1000 then
    title[#title + 1] = { text = "  " .. bone.util.duration(item.duration_ms / 1000), hl = "Dim" }
  end
  local paras = { { spans = title, size = md.small_size } }

  local d = mode(item)
  local edit = item.name == "edit_file" or item.name == "write_file"
  local n = d == "full" and nil or M.rows
  if (d ~= "summary" or failed or edit) then
    if edit and not failed then
      local spans = diff(item, n)
      if #spans > 0 then
        paras[#paras + 1] = { spans = spans, font = "mono", size = md.small_size - 1, bg = "CodeBlock", pad = 8, gap = 6 }
      end
    elseif item.name == "subagent" and item.done and not failed and d ~= "summary" then
      for _, p in ipairs(md.paragraphs(tail(item.output, n), "ToolOutput")) do
        p.size, p.indent = md.small_size, (p.indent or 0) + 14
        paras[#paras + 1] = p
      end
    else
      local body, cut = tail(item.output or item.live or "", n)
      if body ~= "" then
        local spans = {}
        if cut > 0 then spans[1] = { text = "… " .. cut .. " more lines (click for all)\n", hl = "Dim" } end
        spans[#spans + 1] = { text = body, hl = failed and "ToolError" or "ToolOutput" }
        paras[#paras + 1] = { spans = spans, font = "mono", size = md.small_size - 1, bg = "CodeBlock", pad = 8, gap = 6 }
      end
    end
  end
  local text = timing.footer(item, ctx)
  if text then
    paras[#paras + 1] = { spans = { { text, "Dim" } }, size = md.small_size, gap = 10 }
  end
  local fallback = { { { mark[1] .. label .. (what ~= "" and (" " .. what) or ""), "ToolName" } } }
  return {
    block = { paragraphs = paras, pad = { 2, 4 }, margin = { 4, 2 } },
    lines = fallback,
  }
end

-- A click on a call opens or folds it (a sub-agent's title opens its chat:
-- bone.background).
local press
bone.on("mouse", function(ev)
  local down = press
  press = ev.action == "down" and not ev.popup and not ev.panel and ev or nil
  if ev.action ~= "up" or ev.button ~= "left" or not down or down.index ~= ev.index
      or not ev.index or ev.popup or ev.panel then
    return
  end
  local item = bone.chat.item(ev.index)
  if not item or item.kind ~= "tool" then return end
  toggled[item.id] = not toggled[item.id] or nil
  bone.chat.redraw(ev.index)
  return true
end)

-- ctrl+t: every call one step more (or less) detailed.
local next_detail = { summary = "rows", rows = "full", full = "summary" }
bone.keymap.set("ctrl+t", function()
  toggled = {}
  bone.o.tool_detail = next_detail[bone.o.tool_detail] or "rows"
  bone.settings.set("gui.tool_detail", bone.o.tool_detail)
end)

return M
