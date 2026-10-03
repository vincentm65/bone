-- compact, core side: replace a long transcript's older part with a summary
-- written by a model. Two ways in:
--   /compact in the TUI (it calls the "compact" function registered here
--   with bone.rpc), and
--   automatically, when a model call fails because the context is too long
--   (a request_error hook compacts, then retries).
--
--   bone.config.compact = {
--     keep = 2,              -- user turns kept word for word (the latest ones)
--     provider = nil,        -- which provider writes the summary (default: the current one)
--     auto = true,           -- compact and retry on context-length errors
--   }

bone.config.compact = { keep = 2, provider = nil, auto = true }

local SUMMARIZE = [[Summarize the conversation below for the assistant that will continue it.
Keep: the user's goals and constraints, decisions made, files and commands involved,
what is done and what is left. Be concise; use short bullet points.]]

local function render(messages)
  local out = {}
  for _, m in ipairs(messages) do
    if m.role == "user" then
      out[#out + 1] = "USER: " .. (m.content or "")
    elseif m.role == "assistant" then
      local calls = {}
      for _, c in ipairs(m.tool_calls or {}) do
        calls[#calls + 1] = c.name .. " " .. (c.arguments or "")
      end
      out[#out + 1] = "ASSISTANT: " .. (m.content or "") .. (#calls > 0 and (" [called " .. table.concat(calls, "; ") .. "]") or "")
    elseif m.role == "tool" then
      out[#out + 1] = "TOOL RESULT: " .. (m.content or ""):sub(1, 2000)
    end
  end
  return table.concat(out, "\n")
end

--- Compact a session. Returns a short report, or nil and why not.
function bone.compact(session_id)
  local cfg = bone.config.compact or {}
  local messages = bone.session.messages(session_id)
  -- Cut at a user message, so no tool call is separated from its result.
  local users = {}
  for i, m in ipairs(messages) do
    if m.role == "user" then
      users[#users + 1] = i
    end
  end
  local keep = cfg.keep or 2
  if #users <= keep then
    return nil, "nothing to compact yet"
  end
  local cut = users[#users - keep + 1]
  local old = {}
  for i = 1, cut - 1 do
    old[#old + 1] = messages[i]
  end
  local r, err = bone.model.complete({ provider = cfg.provider, system = SUMMARIZE, prompt = render(old) })
  if not r then
    return nil, "the summary failed: " .. tostring(err)
  end
  local new = { { role = "user", content = "Summary of the earlier conversation:\n\n" .. r.content } }
  for i = cut, #messages do
    new[#new + 1] = messages[i]
  end
  bone.session.compact(session_id, new)
  return ("compacted %d messages into a summary; kept the last %d turns"):format(#old, keep)
end

-- What /compact calls (lua/call "compact").
bone.rpc.register("compact", function(_, ctx)
  if not ctx.session_id then
    return "no session to compact"
  end
  local report, why = bone.compact(ctx.session_id)
  return report or why
end)

local CONTEXT_FULL = { "context length", "context window", "maximum context", "too many tokens", "prompt is too long" }

bone.hook("request_error", function(ev)
  local cfg = bone.config.compact or {}
  if cfg.auto == false or ev.attempt > 1 then
    return
  end
  local e = ev.error:lower()
  for _, p in ipairs(CONTEXT_FULL) do
    if e:find(p, 1, true) then
      if bone.compact(ev.session_id) then
        return { retry = 0 }
      end
      return
    end
  end
end)
