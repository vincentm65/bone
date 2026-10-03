-- approve: the TUI side. Shows each approval question from the core as a
-- popup and sends back the answer. Other kinds of questions are left to
-- other plugins.

local open = {} -- ask id -> popup id

local function close(ask_id)
  if open[ask_id] then
    bone.ui.close(open[ask_id])
    open[ask_id] = nil
  end
end

local function lines(s)
  local t = {}
  for l in ((s or "") .. "\n"):gmatch("(.-)\n") do
    t[#t + 1] = l
  end
  if t[#t] == "" then
    t[#t] = nil
  end
  return t
end

local function first(list, n)
  local out = {}
  for i = 1, math.min(#list, n) do
    out[i] = list[i]
  end
  return out
end

--- What the popup shows for a call.
local function details(q)
  local args = type(q.arguments) == "table" and q.arguments or {}
  local out = {}
  if q.tool == "shell" then
    for i, l in ipairs(lines(args.command)) do
      local row = { { i == 1 and "$ " or "  ", "ToolSummary" } }
      for _, span in ipairs(bone.text.shell(l)) do
        row[#row + 1] = span
      end
      out[#out + 1] = row
    end
  elseif q.tool == "edit_file" then
    out[#out + 1] = { { args.path or "", "ToolPath" } }
    out[#out + 1] = {}
    if args.old_string then
      for _, l in ipairs(first(lines(args.old_string), 6)) do
        out[#out + 1] = { { "- " .. l, "DiffDelete" } }
      end
      for _, l in ipairs(first(lines(args.new_string), 6)) do
        out[#out + 1] = { { "+ " .. l, "DiffAdd" } }
      end
    else
      -- Anchored edits: where each goes, then its new lines.
      local edits = type(args.edits) == "table" and args.edits or { args }
      for _, e in ipairs(edits) do
        if type(e) == "table" then
          local where = e.at and ("at " .. tostring(e.at) .. (e["end"] and (" .. " .. tostring(e["end"])) or ""))
            or e.after and ("after " .. tostring(e.after))
            or ("before " .. tostring(e.before))
          out[#out + 1] = { { where, "ToolSummary" } }
          local new = lines(type(e.text) == "string" and e.text or "")
          if #new == 0 then
            out[#out + 1] = { { "- (delete)", "DiffDelete" } }
          end
          for _, l in ipairs(first(new, 4)) do
            out[#out + 1] = { { "+ " .. l, "DiffAdd" } }
          end
        end
      end
    end
  elseif q.tool == "write_file" then
    local content = lines(args.content)
    out[#out + 1] = { { (args.path or "") .. " (" .. #content .. " lines)", "ToolPath" } }
    out[#out + 1] = {}
    for _, l in ipairs(first(content, 8)) do
      out[#out + 1] = { { "+ " .. l, "DiffAdd" } }
    end
  else
    out[#out + 1] = { { bone.json.encode(q.arguments or {}), "ToolArgs" } }
  end
  return first(out, 14)
end

bone.on("ask/requested", function(ev)
  local q = ev.question
  if type(q) ~= "table" or q.kind ~= "approval" then
    return
  end
  local function answer(a)
    return function()
      bone.request("ask/respond", { ask_id = ev.ask_id, answer = a })
      close(ev.ask_id)
    end
  end
  local body = details(q)
  body[#body + 1] = {}
  body[#body + 1] = { { "y allow   a always   n deny   esc deny", "Dim" } }
  open[ev.ask_id] = bone.ui.popup({
    lines = function(ctx)
      local w = math.min(90, ctx.width - 4)
      return bone.ui.box(body, { title = q.title or "Allow?", width = w })
    end,
    keys = { y = answer("allow"), a = answer("always"), n = answer("deny"), esc = answer("deny") },
    -- Ignore keys for a moment so text being typed can't answer it.
    guard = 300,
  })
end)

-- Answered elsewhere (another client) or cancelled.
bone.on("ask/resolved", function(ev)
  close(ev.ask_id)
end)
