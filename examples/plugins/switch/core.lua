-- switch, core side: a provider entry that routes every request to another
-- entry of bone.config.providers, chosen in the TUI with /provider. The
-- choice is read before each request, so it applies from the next model
-- call on, in every session.
--
--   bone.config.providers.qwen = { base_url = "http://localhost:8080/v1", model = "qwen" }
--   bone.config.providers.deepseek = { base_url = "https://api.deepseek.com/v1", model = "deepseek-chat", api_key = ... }
--   bone.config.providers.switch = { type = "switch", model = "switch", default = "qwen" }
--   bone.config.provider = "switch"
--
-- OpenAI-compatible entries are called from Lua here; an entry with its own
-- `type` (e.g. the anthropic plugin) goes to that Lua provider.

local function decode(s)
  local ok, v = pcall(bone.json.decode, s)
  return ok and v or nil
end

-- The entries there are to pick from.
local function targets()
  local out = {}
  for name, p in pairs(bone.config.providers) do
    if p.type ~= "switch" then
      out[#out + 1] = { name = name, model = p.model, type = p.type or "openai" }
    end
  end
  table.sort(out, function(a, b)
    return a.name < b.name
  end)
  return out
end

-- An OpenAI-compatible /chat/completions call, streamed.
local function openai(req, emit)
  local o = req.options
  local messages = {}
  for _, m in ipairs(req.messages) do
    if m.role == "assistant" then
      local calls
      for _, c in ipairs(m.tool_calls or {}) do
        calls = calls or {}
        calls[#calls + 1] = { id = c.id, type = "function", ["function"] = { name = c.name, arguments = c.arguments } }
      end
      messages[#messages + 1] = { role = "assistant", content = m.content or "", tool_calls = calls }
    elseif m.role == "tool" then
      messages[#messages + 1] = { role = "tool", tool_call_id = m.call_id, content = m.content }
    else
      messages[#messages + 1] = { role = m.role, content = m.content }
    end
  end
  local tools = {}
  for _, t in ipairs(req.tools) do
    tools[#tools + 1] = {
      type = "function",
      ["function"] = { name = t.name, description = t.description, parameters = t.parameters },
    }
  end
  local body = {
    model = o.model,
    messages = messages,
    tools = #tools > 0 and tools or nil,
    stream = true,
    reasoning_effort = o.reasoning_effort,
    stream_options = o.stream_usage and { include_usage = true } or nil,
  }
  local headers = { ["content-type"] = "application/json" }
  if o.api_key and o.api_key ~= "" then
    headers.authorization = "Bearer " .. o.api_key
  end
  local s = bone.http_stream({
    url = o.base_url:gsub("/$", "") .. "/chat/completions",
    method = "POST",
    headers = headers,
    body = body,
  })
  if s == nil then
    return nil -- cancelled
  end
  if s.status ~= 200 then
    error("HTTP " .. s.status .. ": " .. (s:text() or ""))
  end
  local result = { content = "", reasoning = "", tool_calls = {}, usage = { input_tokens = 0, output_tokens = 0 } }
  local calls = {} -- by index
  for data in s:events() do
    if data == "[DONE]" then
      break
    end
    local ev = decode(data) or {}
    if ev.usage then
      result.usage = { input_tokens = ev.usage.prompt_tokens or 0, output_tokens = ev.usage.completion_tokens or 0 }
    end
    if ev.error then
      error(ev.error.message or data)
    end
    local d = ev.choices and ev.choices[1] and ev.choices[1].delta or {}
    local r = d.reasoning_content or d.reasoning
    if type(r) == "string" and r ~= "" then
      result.reasoning = result.reasoning .. r
      emit({ reasoning = r })
    end
    if type(d.content) == "string" and d.content ~= "" then
      result.content = result.content .. d.content
      emit({ text = d.content })
    end
    for _, tc in ipairs(d.tool_calls or {}) do
      local i = tc.index or 0
      local c = calls[i] or { id = "", name = "", arguments = "" }
      calls[i] = c
      local f = tc["function"] or {}
      c.id = tc.id or c.id
      c.name = c.name .. (f.name or "")
      c.arguments = c.arguments .. (f.arguments or "")
    end
  end
  local order = {}
  for i in pairs(calls) do
    order[#order + 1] = i
  end
  table.sort(order)
  for _, i in ipairs(order) do
    result.tool_calls[#result.tool_calls + 1] = calls[i]
  end
  return result
end

-- The chosen entry's name: the TUI's choice, else the switch entry's default.
local function chosen(options)
  local name = bone.state.load("switch", { shared = true }).current or options.default
  local target = name and bone.config.providers[name]
  if not target or target.type == "switch" then
    error("switch: no provider " .. tostring(name) .. " to route to (/provider picks one)")
  end
  return name, target
end

bone.provider.register("switch", {
  complete = function(req, emit)
    local _, target = chosen(req.options)
    local sub = { messages = req.messages, tools = req.tools, options = target, session_id = req.session_id }
    if target.type then
      local p = bone._providers[target.type]
      if not p then
        error("switch: no Lua provider of type " .. target.type)
      end
      return p.complete(sub, emit)
    end
    return openai(sub, emit)
  end,
})

-- Once the config is final, tell the TUI half what there is to pick from.
bone.on_ready(function()
  local default
  for _, p in pairs(bone.config.providers) do
    if p.type == "switch" then
      default = p.default
    end
  end
  bone.state.save("switch-providers", { providers = targets(), default = default }, { shared = true })
end)

bone.health("switch", function()
  local ok, name, target = pcall(chosen, bone.config.providers[bone.config.provider] or {})
  if not ok then
    return "error", name
  end
  return "ok", "routing to " .. name .. " (" .. (target.model or "?") .. ")"
end)
