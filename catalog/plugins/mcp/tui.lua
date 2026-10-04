-- mcp, TUI side: /mcp shows the MCP servers, their state and tools in a
-- panel (r refreshes, esc closes).
local panel

local function render(list)
  local lines = {}
  for _, s in ipairs(list) do
    local hl = ({ ready = "DiffAdd", failed = "ErrorMsg", starting = "WarningMsg" })[s.state] or "Dim"
    lines[#lines + 1] = { { s.name .. "  ", "Accent" }, { s.state, hl }, { "  " .. #s.tools .. " tools", "Dim" } }
    if s.error then
      lines[#lines + 1] = { { "  " .. s.error, "ErrorMsg" } }
    end
    for _, t in ipairs(s.tools) do
      lines[#lines + 1] = { { "  " .. t, "ToolName" } }
    end
  end
  if #lines == 0 then
    lines[1] = { { "no MCP servers (bone.mcp.add or bone.config.mcp_files in core.lua)", "Dim" } }
  end
  return lines
end

local function refresh()
  bone.request("mcp/list", {}, function(list, err)
    if panel and panel:is_open() then
      panel:set_lines(err and { { { tostring(err), "ErrorMsg" } } } or render(list))
    end
  end)
end

bone.cmd.create("mcp", function()
  if panel and panel:is_open() then
    panel:close()
    return
  end
  panel = bone.ui.panel.open({
    id = "mcp",
    dock = "right",
    size = 40,
    title = "MCP servers",
    lines = { { { "loading…", "Dim" } } },
    focus = true,
    keys = {
      r = refresh,
      esc = function(p)
        p:close()
      end,
    },
  })
  refresh()
end, { desc = "MCP servers and their tools (mcp plugin)" })
