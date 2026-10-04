-- A small, native Bone 3 version of /recap.  It deliberately runs outside a
-- turn, so the summary cannot mutate the session transcript.
bone.o.define("recap_auto", false, { type = "boolean", desc = "show a recap after idle turns" })
bone.o.define("recap_idle_ms", 900000, { type = "integer", desc = "recap idle delay in milliseconds" })

bone.ui.views.recap = function(item, ctx)
  return bone.text.wrap({ { "Recap: " .. (item.text or ""), "Dim" } }, ctx.width, { first = "  " })
end

local seq = 0
local function request()
  local session = bone.chat.session()
  if not session or not session.session_id or session.running then
    return bone.notify("recap: wait for the turn to finish", "error")
  end
  bone.chat.messages(function(messages, err)
    if err then return bone.notify("recap: " .. tostring(err), "error") end
    if not messages or #messages < 2 then return bone.notify("recap: nothing to summarize") end
    local prompt = {
      { role = "system", content = "Summarize coding conversations in one or two concise sentences. Do not call tools." },
    }
    for _, m in ipairs(messages) do
      if m.role == "user" or m.role == "assistant" then
        prompt[#prompt + 1] = { role = m.role, content = m.content or "" }
      end
    end
    prompt[#prompt + 1] = { role = "user", content = "Give a brief recap of what was accomplished and what remains." }
    bone.model.complete({ messages = prompt }, nil, function(result, model_err)
      if model_err then return bone.notify("recap: " .. tostring(model_err), "error") end
      local text = result and (result.content or ""):gsub("^%s+", ""):gsub("%s+$", "")
      if text == "" then return bone.notify("recap: the model returned an empty summary", "error") end
      bone.chat.add("recap", { text = text })
    end)
  end)
end

bone.cmd.create("recap", request, { desc = "show a brief conversation recap" })
bone.on("turn/started", function()
  seq = seq + 1
end)
bone.on("turn/finished", function(ev)
  local status = ev and ev.outcome and (ev.outcome.status or ev.outcome)
  if bone.o.recap_auto ~= true or status ~= "completed" then return end
  local mine = seq
  bone.defer(tonumber(bone.o.recap_idle_ms) or 900000, function()
    if mine == seq then request() end
  end)
end)
