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
--
-- Important: pick the top-N conversation ids first via index-only MAX(id)/COUNT
-- aggregates, then compute status/preview/usage only for those rows. The old
-- form joined every message for every conversation before LIMIT, which scans
-- the full messages table (and its large content blobs) on big histories.
function M.list(ctx, limit)
  limit = math.max(1, math.min(limit or 50, 100))
  return ctx.db.query([[
    WITH recent AS (
      SELECT conversation_id,
             MAX(id) AS last_msg_id,
             COUNT(*) AS total_message_count
      FROM messages
      GROUP BY conversation_id
      ORDER BY last_msg_id DESC
      LIMIT ?
    ),
    usage AS (
      SELECT conversation_id,
             SUM(prompt_tokens + completion_tokens) AS total_token_count
      FROM usage_events
      WHERE conversation_id IN (SELECT conversation_id FROM recent)
      GROUP BY conversation_id
    )
    SELECT c.id, c.provider, c.model, c.started_at, c.ended_at,
           lm.created_at AS last_activity,
           r.total_message_count,
           COALESCE(u.total_token_count, 0) AS total_token_count,
           CASE
             WHEN NOT EXISTS (
               SELECT 1 FROM messages m
               WHERE m.conversation_id = c.id AND m.role = 'user'
                 AND m.content <> '' AND m.content NOT LIKE '[Context summary]%'
             ) THEN 'empty'
             WHEN COALESCE((
               SELECT MAX(m.seq) FROM messages m
               WHERE m.conversation_id = c.id AND m.role = 'assistant'
             ), -1) < (
               SELECT MAX(m.seq) FROM messages m
               WHERE m.conversation_id = c.id AND m.role = 'user'
                 AND m.content <> '' AND m.content NOT LIKE '[Context summary]%'
             ) THEN 'interrupted'
             ELSE 'completed'
           END AS status,
           (SELECT p.content FROM messages p
            WHERE p.conversation_id = c.id AND p.role = 'user'
              AND p.content <> '' AND p.content NOT LIKE '[Context summary]%'
            ORDER BY p.seq LIMIT 1) AS preview
    FROM recent r
    JOIN conversations c ON c.id = r.conversation_id
    JOIN messages lm ON lm.id = r.last_msg_id
    LEFT JOIN usage u ON u.conversation_id = c.id
    ORDER BY r.last_msg_id DESC
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
