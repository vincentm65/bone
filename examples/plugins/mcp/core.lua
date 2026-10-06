-- mcp, core side: load MCP servers from JSON files in the common
-- "mcpServers" format, once your core.lua has said which.
--
--   bone.config.mcp_files = { "~/.config/mcp.json", ".mcp.json" }   -- relative to the config dir
--   bone.config.mcp_options = { lazy = true }                        -- for every server they add

bone.config.mcp_files = {}

bone.on_ready(function()
  -- The TUI's /mcp-add command keeps its small, user-owned config here.
  local saved = bone.config_dir .. "/mcp.json"
  local f = io.open(saved, "r")
  if f then
    f:close()
    local ok, err = pcall(bone.mcp.load, saved, bone.config.mcp_options)
    if not ok then
      print("mcp plugin: " .. tostring(err))
    end
  end
  for _, file in ipairs(bone.config.mcp_files or {}) do
    local path = file
    if not path:match("^[/~]") then
      path = bone.config_dir .. "/" .. path
    end
    local ok, err = pcall(bone.mcp.load, path, bone.config.mcp_options)
    if not ok then
      print("mcp plugin: " .. tostring(err))
    end
  end
end)
