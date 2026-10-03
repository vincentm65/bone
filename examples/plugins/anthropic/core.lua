-- anthropic: a model provider for the Anthropic Messages API, written in
-- Lua with bone.provider.register and bone.http_stream.
--
--   bone.config.providers.claude = {
--     type = "anthropic",
--     model = "claude-sonnet-5-5",
--     api_key = os.getenv("ANTHROPIC_API_KEY"),
--     -- base_url = "https://api.anthropic.com/v1",
--     -- max_tokens = 16000,
--     -- thinking = 8000,   -- extended thinking budget in tokens
--   }
--   bone.config.provider = "claude"

local function decode(s)
  local ok, v = pcall(bone.json.decode, s)
  return ok and v or nil
end

-- bone messages -> Anthropic: the system prompt apart, tool results as
-- user messages, consecutive messages of one role merged.
local function convert(messages)
  local system, out = {}, {}
  local function add(role, block)
    local last = out[#out]
    if last and last.role == role then
      table.insert(last.content, block)
    else
      out[#out + 1] = { role = role, content = { block } }
    end
  end
  for _, m in ipairs(messages) do
    if m.role == "system" then
      system[#system + 1] = m.content
    elseif m.role == "user" then
      add("user", { type = "text", text = m.content })
    elseif m.role == "assistant" then
      if m.content and m.content ~= "" then
        add("assistant", { type = "text", text = m.content })
      end
      for _, c in ipairs(m.tool_calls or {}) do
        add("assistant", { type = "tool_use", id = c.id, name = c.name, input = decode(c.arguments) or {} })
      end
    elseif m.role == "tool" then
      add("user", { type = "tool_result", tool_use_id = m.call_id, content = m.content, is_error = m.is_error or false })
    end
  end
  return table.concat(system, "\n\n"), out
end

local function complete(req, emit)
  local o = req.options
  local system, messages = convert(req.messages)
  local tools = {}
  for _, t in ipairs(req.tools) do
    tools[#tools + 1] = { name = t.name, description = t.description, input_schema = t.parameters }
  end
  local body = {
    model = o.model,
    max_tokens = o.max_tokens or 16000,
    system = system ~= "" and system or nil,
    messages = messages,
    tools = #tools > 0 and tools or nil,
    stream = true,
  }
  if o.thinking then
    body.thinking = { type = "enabled", budget_tokens = o.thinking }
  end
  local s = bone.http_stream({
    url = ((o.base_url and o.base_url ~= "") and o.base_url or "https://api.anthropic.com/v1"):gsub("/$", "")
      .. "/messages",
    method = "POST",
    headers = {
      ["x-api-key"] = o.api_key or "",
      ["anthropic-version"] = "2023-06-01",
      ["content-type"] = "application/json",
    },
    body = body,
  })
  if s == nil then
    return nil -- cancelled
  end
  if s.status ~= 200 then
    error("HTTP " .. s.status .. ": " .. (s:text() or ""))
  end

  local result = { content = "", reasoning = "", tool_calls = {}, usage = { input_tokens = 0, output_tokens = 0 } }
  local blocks = {} -- index -> { type, id, name, json }
  for data in s:events() do
    local ev = decode(data) or {}
    if ev.type == "message_start" and ev.message and ev.message.usage then
      result.usage.input_tokens = ev.message.usage.input_tokens or 0
    elseif ev.type == "content_block_start" then
      local b = ev.content_block or {}
      blocks[ev.index] = { type = b.type, id = b.id, name = b.name, json = "" }
    elseif ev.type == "content_block_delta" then
      local d, b = ev.delta or {}, blocks[ev.index] or {}
      if d.type == "text_delta" then
        result.content = result.content .. d.text
        emit({ text = d.text })
      elseif d.type == "thinking_delta" then
        result.reasoning = result.reasoning .. d.thinking
        emit({ reasoning = d.thinking })
      elseif d.type == "input_json_delta" then
        b.json = b.json .. d.partial_json
      end
    elseif ev.type == "content_block_stop" then
      local b = blocks[ev.index]
      if b and b.type == "tool_use" then
        table.insert(result.tool_calls, { id = b.id, name = b.name, arguments = b.json ~= "" and b.json or "{}" })
      end
    elseif ev.type == "message_delta" and ev.usage then
      result.usage.output_tokens = ev.usage.output_tokens or result.usage.output_tokens
    elseif ev.type == "error" then
      error((ev.error and ev.error.message) or data)
    end
  end
  return result
end

bone.provider.register("anthropic", { complete = complete })
