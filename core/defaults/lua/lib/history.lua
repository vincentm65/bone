-- lib/history — conversation history helpers over the ctx.db.query primitive.
-- Replaces the former ctx.session.list / ctx.session.messages Rust helpers.
local M = {}

local function decode_tool_calls(raw)
  if type(raw) ~= "string" or raw == "" then return nil end

  local ok, decoded = pcall(cjson.decode, raw)
  if not ok or type(decoded) ~= "table" then return nil end

  local out = {}
  for _, call in ipairs(decoded) do
    if type(call) == "table" then
      local id = call.id
      local name = call.name
      if type(id) == "string" and id ~= "" and type(name) == "string" and name ~= "" then
        local arguments = call.arguments
        if type(arguments) == "string" then
          local args_ok, parsed = pcall(cjson.decode, arguments)
          if args_ok then arguments = parsed end
        end
        table.insert(out, {
          id = id,
          name = name,
          arguments = arguments,
        })
      end
    end
  end

  if #out == 0 then return nil end
  return out
end

-- Conversations ordered by latest activity. Counts, preview, and completion
-- state are aggregated in one query so callers do not need per-row lookups.
function M.list(ctx, limit)
  limit = math.max(1, math.min(limit or 50, 100))
  return ctx.db.query([[
    SELECT c.id, c.provider, c.model, c.started_at, c.ended_at,
           COALESCE(MAX(m.created_at), c.started_at) AS last_activity,
           COUNT(m.id) AS total_message_count,
           COALESCE((SELECT SUM(u.prompt_tokens + u.completion_tokens)
                     FROM usage_events u WHERE u.conversation_id = c.id), 0)
             AS total_token_count,
           CASE
             WHEN SUM(CASE WHEN m.role = 'user' AND m.content <> ''
                            AND m.content NOT LIKE '[Context summary]%'
                           THEN 1 ELSE 0 END) = 0 THEN 'empty'
             WHEN COALESCE(MAX(CASE WHEN m.role = 'assistant' THEN m.seq END), -1)
                  < MAX(CASE WHEN m.role = 'user' AND m.content <> ''
                              AND m.content NOT LIKE '[Context summary]%'
                             THEN m.seq END) THEN 'interrupted'
             ELSE 'completed'
           END AS status,
           (SELECT p.content FROM messages p
            WHERE p.conversation_id = c.id AND p.role = 'user'
              AND p.content <> '' AND p.content NOT LIKE '[Context summary]%'
            ORDER BY p.seq LIMIT 1) AS preview
    FROM conversations c
    LEFT JOIN messages m ON m.conversation_id = c.id
    GROUP BY c.id
    HAVING COUNT(m.id) > 0
    ORDER BY last_activity DESC
    LIMIT ?
  ]], { limit }) or {}
end

-- Messages for a conversation in chronological order.
-- Returns array of { seq, role, content, tool_name, tool_call_id, tool_calls }.
function M.messages(ctx, id, limit)
  limit = math.max(1, math.min(limit or 200, 1000))
  local rows = ctx.db.query(
    "SELECT seq, role, content, tool_name, tool_call_id, tool_calls " ..
    "FROM messages WHERE conversation_id = ? ORDER BY seq ASC LIMIT ?",
    { id, limit }
  ) or {}

  for _, row in ipairs(rows) do
    row.tool_calls = decode_tool_calls(row.tool_calls)
  end

  return rows
end

return M
