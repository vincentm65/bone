local panel
local function show()
  bone.rpc.call("subagent/list", {}, function(list, err)
    if err then return bone.notify("subagent: " .. tostring(err), "error") end
    local lines = {}
    for _, item in ipairs(list or {}) do
      lines[#lines + 1] = { { item.name, "Accent" }, { "  " .. tostring(item.provider or "current"), "Dim" } }
    end
    if #lines == 0 then lines[1] = { { "No subagents configured in core.lua", "Dim" } } end
    if panel and panel:is_open() then panel:set_lines(lines) else
      panel = bone.ui.panel.open({ id = "subagents", dock = "right", size = 32, title = "Subagents", lines = lines, focus = true })
    end
  end)
end
bone.cmd.create("agents", show, { desc = "list configured subagents" })
