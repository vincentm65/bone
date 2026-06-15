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

-- List recent conversations, newest first.
-- Returns array of { id, provider, model, started_at, ended_at }.
function M.list(ctx, limit)
  limit = math.max(1, math.min(limit or 20, 100))
  return ctx.db.query(
    "SELECT id, provider, model, started_at, ended_at " ..
    "FROM conversations ORDER BY id DESC LIMIT ?",
    { limit }
  ) or {}
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
