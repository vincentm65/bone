-- Bone's built-in Lua interface. Every visual decision lives in these Lua
-- modules and can be replaced from ~/.bone/lua or a plugin.
local M = {}

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
  return M
end

return M
