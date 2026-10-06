-- mcp, TUI side: /mcp shows the MCP servers, their state and tools in a
-- panel (r refreshes, esc closes).
local panel
local config_file = bone.config_dir .. "/mcp.json"
local refresh

local function read_config()
  local f = io.open(config_file, "r")
  if not f then
    return { mcpServers = {} }
  end
  local text = f:read("*a")
  f:close()
  local ok, data = pcall(bone.json.decode, text)
  if not ok or type(data) ~= "table" then
    return nil, "cannot parse " .. config_file
  end
  data.mcpServers = data.mcpServers or data.servers
  if type(data.mcpServers) ~= "table" then
    return nil, config_file .. " has no mcpServers table"
  end
  return data
end

bone.cmd.create("mcp-add", function(c)
  local argv = c.argv or {}
  if #argv < 2 then
    return bone.notify("usage: /mcp-add NAME COMMAND [ARGS...]", "error")
  end
  local name, command = argv[1], argv[2]
  if not name:match("^[%w_%-]+$") then
    return bone.notify("MCP names are letters, digits, _ and -", "error")
  end
  local data, err = read_config()
  if not data then
    return bone.notify(err, "error")
  end
  if data.mcpServers[name] then
    return bone.notify("MCP server already exists: " .. name, "error")
  end
  local args = {}
  for i = 3, #argv do
    args[#args + 1] = argv[i]
  end
  data.mcpServers[name] = { command = command, args = args }
  local ok, write_err = bone.fs.write(config_file, bone.json.encode(data) .. "\n")
  if not ok then
    return bone.notify(tostring(write_err), "error")
  end
  bone.request("core/reload", {}, function(_, reload_err)
    if reload_err then
      bone.notify("saved " .. name .. ", but reload failed: " .. tostring(reload_err), "error")
    else
      bone.notify("added MCP server " .. name)
      refresh()
    end
  end)
end, { desc = "add a local stdio MCP server: /mcp-add NAME COMMAND [ARGS...]" })

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

refresh = function()
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
