-- conversation_history — interactive live-pane selector for recent SQLite
-- conversation history. The agent calls this; the user picks a conversation with
-- the arrow keys, and the selected transcript is returned to the agent as JSON.

local menu = require("ui.menu")

local function trim(s)
  local out = (s or ""):gsub("^%s+", ""):gsub("%s+$", "")
  return out
end

local function utf8_prefix(s, max_chars)
  s = s or ""
  local byte_pos = utf8.offset(s, max_chars + 1)
  if byte_pos then
    return s:sub(1, byte_pos - 1), true
  end
  return s, false
end

local function one_line(s)
  s = trim(s or ""):gsub("%s+", " ")
  local out, truncated = utf8_prefix(s, 69)
  if truncated then
    out = out .. "..."
  end
  return out
end

local function format_time(s)
  if not s or s == "" then return "unknown" end
  s = s:gsub("T", " "):gsub("Z$", "")
  return s:sub(1, 16)
end

local function load_messages(ctx, id)
  local out = {}
  local messages = ctx.session.messages(id, { limit = 1000 }) or {}
  for _, msg in ipairs(messages) do
    if msg.role == "user" or msg.role == "assistant" or msg.role == "tool" then
      table.insert(out, {
        role = msg.role,
        content = msg.content or "",
        name = msg.tool_name,
        tool_call_id = msg.tool_call_id,
      })
    end
  end
  return out
end

-- Skip synthetic messages injected by context compaction (see compact.lua) and
-- other internal prompts so they don't clutter the picker or previews.
local function is_hidden_prompt(s)
  s = (s or ""):lower()
  if s:find("you are a context summarizer", 1, true) then return true end
  if s:find("summarize the conversation below", 1, true) then return true end
  if s:find("summarize older conversation", 1, true) then return true end
  if s:find("auto compaction", 1, true) then return true end
  if s:find("compact description", 1, true) then return true end
  if s:find("[context summary]", 1, true) then return true end
  return false
end

local function first_user_summary(ctx, id)
  local messages = ctx.session.messages(id, { limit = 80 }) or {}
  for _, msg in ipairs(messages) do
    if msg.role == "user" then
      local line = one_line(msg.content)
      if line ~= "" then
        if is_hidden_prompt(line) then return nil end
        return line
      end
    end
  end
  return nil
end

bone.register_tool({
  name = "conversation_history",
  description = "Open an interactive live-pane selector for recent SQLite conversation history. User chooses with arrow keys; returns the selected conversation transcript.",
  parameters = {
    type = "object",
    properties = {},
    additionalProperties = false,
  },
  safety = "read_only",
  display = { show = false, show_result = false },
  execute = function(_, ctx)
    if not ctx.session or not ctx.session.list or not ctx.session.messages then
      return "error: conversation history is unavailable"
    end

    local conversations = ctx.session.list({ limit = 100 }) or {}
    if #conversations == 0 then
      return "No conversation history found."
    end

    local options = {}
    local ids = {}
    for _, conv in ipairs(conversations) do
      local summary = first_user_summary(ctx, conv.id)
      if summary then
        table.insert(options, string.format(
          "#%s  %s  %s/%s  %s",
          tostring(conv.id),
          format_time(conv.started_at),
          conv.provider or "?",
          conv.model or "?",
          summary
        ))
        table.insert(ids, conv.id)
        if #options >= 50 then break end
      end
    end

    if #options == 0 then
      return "No conversation history found."
    end

    local ok, choice = pcall(menu.select, ctx, {
      question = "Select conversation history",
      options = options,
      allow_custom = false,
    })
    menu.clear(ctx)

    if not ok then
      return "error: history selector failed: " .. tostring(choice)
    end
    if type(choice) ~= "table" then
      return "error: history selector unavailable"
    end
    if choice.cancelled then
      return "[user cancelled]"
    end

    local selected
    for i, option in ipairs(options) do
      if option == choice.value then
        selected = i
        break
      end
    end
    if not selected then
      return "No conversation selected."
    end

    local id = ids[selected]
    local messages = load_messages(ctx, id)
    if #messages == 0 then
      return "Selected conversation has no loadable messages."
    end

    return cjson.encode({
      conversation_id = id,
      messages = messages,
      instruction = "Use this selected prior conversation as context for the user's next request.",
    })
  end,
})
