-- switch, TUI side: /provider picks which provider entry the core's switch
-- entry routes to. The choice is kept in the shared state, so it holds for
-- every TUI on this config dir and across restarts.

local function info()
  return bone.state.load("switch-providers", { shared = true })
end

local function current()
  return bone.state.load("switch", { shared = true }).current or info().default
end

local function choose(name)
  for _, p in ipairs(info().providers or {}) do
    if p.name == name then
      bone.state.save("switch", { current = name }, { shared = true })
      bone.notify("provider: " .. name .. (p.model and (" (" .. p.model .. ")") or ""))
      return
    end
  end
  bone.notify("no provider " .. name .. " (/provider lists them)", "error")
end

bone.cmd.create("provider", function(c)
  if c.args ~= "" then
    return choose(c.args)
  end
  local cur = current()
  bone.ui.select(info().providers or {}, {
    prompt = "Provider",
    empty = "nothing to pick: is the switch entry set up in core.lua?",
    format = function(p)
      return (p.name == cur and "● " or "  ") .. p.name .. "  " .. (p.model or "") .. "  " .. p.type
    end,
    on_choice = function(p)
      if p then
        choose(p.name)
      end
    end,
  })
end, {
  desc = "pick the model provider (switch plugin)",
  complete = function()
    local out = {}
    for _, p in ipairs(info().providers or {}) do
      out[#out + 1] = { value = p.name, desc = p.model }
    end
    return out
  end,
})

--- For a statusline: the provider in use.
bone.switch = { current = current }
