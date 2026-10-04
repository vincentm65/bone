-- skills, TUI side:
--   /skills              list the skills the core has
--   /skill name [task]   ask the model to use that skill (in the prompt, for
--                        you to edit and send)
local names = {}

local function fetch(then_)
  bone.rpc.call("skills.list", {}, function(list, err)
    if err then
      return bone.notify("skills: " .. tostring(err), "error")
    end
    names = {}
    for _, s in ipairs(list) do
      names[#names + 1] = { value = s.name, desc = s.description }
    end
    if then_ then
      then_(list)
    end
  end)
end

bone.cmd.create("skills", function()
  fetch(function(list)
    if #list == 0 then
      return bone.notify("no skills (bone.config.skill_dirs or bone.skill.register in core.lua)")
    end
    local lines = {}
    for _, s in ipairs(list) do
      lines[#lines + 1] = { { s.name, "Accent" }, { "  " .. s.description, "Normal" } }
    end
    bone.ui.pager(lines, { title = "Skills" })
  end)
end, { desc = "list skills (skills plugin)" })

bone.cmd.create("skill", function(c)
  local name, task = c.args:match("^(%S+)%s*(.*)$")
  if not name then
    return bone.notify("usage: /skill name [task]", "error")
  end
  bone.prompt.set("Use the " .. name .. " skill" .. (task ~= "" and (": " .. task) or "."))
end, {
  desc = "ask the model to use a skill (skills plugin)",
  complete = function()
    return names
  end,
})

fetch()
bone.on("core/reloaded", function()
  fetch()
end)
