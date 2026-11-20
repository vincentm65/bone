-- Background shell jobs as the chats show them: the call that started one
-- (shell mode "start", answered "background process started: ID") stands
-- for it, live, until it ends, like a command run in the foreground. Used
-- by both apps' chat views.
local M = {}

--- The job tool call `t` started: its process entry while the core still
--- has it, else nil.
function M.of(t)
  if t.name ~= "shell" or not t.done or t.is_error or not bone.processes then return nil end
  local args = type(t.arguments) == "table" and t.arguments or {}
  if args.mode ~= "start" then return nil end
  local out = t.output
  if out == nil and t.index then
    -- A summary query strips output; this one is a line.
    local full = bone.chat.item(t.index, { full = true })
    out = full and full.output
  end
  local id = (out or ""):match("background process started: (%S+)")
  if not id then return nil end
  for _, p in ipairs(bone.processes.list() or {}) do
    if p.id == id then return p end
  end
end

--- "running", "failed" or "done": a job that exits non-zero, times out or
--- fails has failed; one stopped on purpose is done.
function M.state(p)
  if p.running then return "running" end
  if (p.state == "exited" and p.code ~= 0) or p.state == "timed_out" or p.state == "failed" then
    return "failed"
  end
  return "done"
end

--- Its last line while it runs, then how it ended ("exit 101 after 5s").
function M.line(p, state)
  if state == "running" then return p.tail or "" end
  local took = p.elapsed_ms and (" after " .. bone.util.duration(p.elapsed_ms / 1000)) or ""
  if p.state == "exited" then return "exit " .. tostring(p.code) .. took end
  if p.state == "timed_out" then return "timed out" .. took end
  if p.state == "cancelled" then return "stopped" .. took end
  return "failed: " .. tostring(p.error or p.signal or "unknown") .. took
end

return M
