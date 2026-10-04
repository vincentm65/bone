bone.o.define("shotgun_targets", "", { type = "string", desc = "comma-separated provider names" })

local function split(value)
  local out = {}
  for name in tostring(value or ""):gmatch("[^,%s]+") do out[#out + 1] = name end
  return out
end

bone.cmd.create("shotgun", function(c)
  if c.args == "" then return bone.notify("usage: /shotgun question", "error") end
  local targets = split(bone.o.shotgun_targets)
  if #targets == 0 then return bone.notify("set shotgun_targets first (for example: local,cheap)", "error") end
  local answer = {}
  local pending = #targets
  for _, provider in ipairs(targets) do
    bone.model.complete({ provider = provider, system = "Answer directly and state your key evidence.", prompt = c.args }, nil, function(result, err)
      answer[#answer + 1] = err and (provider .. ": ERROR " .. tostring(err))
        or (provider .. ":\n" .. (result.content or ""))
      pending = pending - 1
      if pending ~= 0 then return end
      local synthesis = table.concat(answer, "\n\n")
      bone.model.complete({ system = "Synthesize the independent answers into one accurate response. Resolve disagreements explicitly.", prompt = c.args .. "\n\nIndependent answers:\n" .. synthesis }, nil, function(final, final_err)
        bone.chat.add("shotgun", { text = final_err and ("shotgun: " .. tostring(final_err)) or (final.content or synthesis) })
      end)
    end)
  end
end, { desc = "ask several providers and synthesize their answers" })

bone.ui.views.shotgun = function(item, ctx)
  return bone.text.wrap({ { item.text or "", "Normal" } }, ctx.width, { first = "  " })
end
