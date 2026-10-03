-- templates, TUI side: every prompt template the core has becomes a /name
-- command. Running it puts the expanded text in the prompt, to edit and send.
-- Names that are already commands are skipped. The list is refreshed when
-- the core reloads.
local made = {}

local function sync()
  bone.templates.list(function(list, err)
    if err then
      return bone.notify("templates: " .. tostring(err), "error")
    end
    for _, name in ipairs(made) do
      pcall(bone.cmd.del, name)
    end
    made = {}
    for _, t in ipairs(list) do
      local name = t.name:lower():gsub("[^%w_%-]", "-")
      local ok = pcall(bone.cmd.create, name, function(c)
        bone.templates.expand(t.name, c.args, function(text, e)
          if text then
            bone.prompt.set(text)
          else
            bone.notify(tostring(e), "error")
          end
        end)
      end, {
        desc = (t.description ~= "" and t.description or "prompt template")
          .. (#t.args > 0 and (" <" .. table.concat(t.args, "> <") .. ">") or ""),
      })
      if ok then
        made[#made + 1] = name
      end
    end
  end)
end

sync()
bone.on("core/reloaded", sync)
