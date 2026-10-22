-- Bone's built-in Lua interface. Every visual decision lives in these Lua
-- modules and can be replaced from ~/.bone/lua or a plugin.
local M = {}

-- /health: this frontend's checks, then the core's (over the protocol).
local MARK = { ok = { "✓ ", "DiffAdd" }, warn = { "! ", "WarningMsg" }, error = { "✗ ", "ErrorMsg" } }

local function health_lines(title, items, out)
  out[#out + 1] = { { title, "Accent" } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    local rows = {}
    for line in (tostring(it.message) .. "\n"):gmatch("(.-)\n") do
      rows[#rows + 1] = line
    end
    out[#out + 1] = { m, { it.name .. ": ", "Normal" }, { rows[1] or "", "Dim" } }
    for i = 2, #rows do
      out[#out + 1] = { { "    " .. rows[i], "Dim" } }
    end
  end
  out[#out + 1] = {}
end

function M.setup(opts)
  opts = opts or {}
  -- Reasoning is hidden by default (the option's default); only an
  -- explicit choice here changes it, so ctrl+r survives a reload.
  if opts.show_reasoning ~= nil then
    bone.o.show_reasoning = opts.show_reasoning
  end
  if not opts.colorscheme then
    bone.colorscheme("black")
  else
    bone.colorscheme(opts.colorscheme)
  end
  require("bone.ui.views")
  require("bone.ui.turn_timing")
  require("bone.ui.statusline")
  require("bone.ui.tray")
  -- Loaded now so its Processes page sees background work from the start.
  require("bone.ui.sessions")
  require("bone.ui.layout").setup(opts)
  -- The conversation sidebar (ctrl+o, /sessions), implemented entirely in Lua.
  function bone.ui.sessions()
    return require("bone.ui.sessions").open()
  end

  -- /help and F1: the Lua help browser; /help topic opens a docs section.
  function bone.ui.help(topic)
    local help = require("bone.help")
    if topic and topic:match("%S") then
      return help.topic(topic)
    end
    return help.open()
  end

  -- Matching slash commands while typing "/…", attached to the prompt.
  -- ctx = { items = { { name, desc } }, selected, width, height };
  function bone.ui.suggestions(ctx)
    local name_w, desc_w = 0, 0
    for _, it in ipairs(ctx.items) do
      name_w = math.max(name_w, bone.text.width(it.name) + 1)
      desc_w = math.max(desc_w, bone.text.width(it.desc))
    end
    local lines = {}
    for i, it in ipairs(ctx.items) do
      local hl = i == ctx.selected and "Selection" or "InputBackground"
      local prefix = "  "
      local name = "/" .. it.name
      local name_text = prefix .. name
      local used = bone.text.width(prefix) + name_w + 1 + desc_w
      lines[i] = {
        { name_text .. string.rep(" ", name_w + 1 - bone.text.width(name)), hl },
        { it.desc .. string.rep(" ", desc_w - bone.text.width(it.desc)), hl },
        { string.rep(" ", math.max(0, ctx.width - used)), hl },
      }
    end
    return lines
  end

  function bone.ui.health()
    local mine = bone.api.health()
    for _, it in ipairs(bone._run_health()) do
      mine[#mine + 1] = it
    end
    local lines = {}
    health_lines(bone.api_info().side:upper(), mine, lines)
    local pager = bone.ui.pager({ unpack(lines), { { "Core: checking…", "Dim" } } }, { title = "Health" })
    bone.request("health/check", {}, function(core, err)
      local all = { unpack(lines) }
      if err then
        health_lines("Core", { { name = "core", status = "error", message = err } }, all)
      else
        health_lines("Core", core or {}, all)
      end
      pager:set(all)
    end)
  end
  return M
end

return M
