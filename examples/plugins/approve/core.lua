-- approve: ask before tool calls that change things.
--
-- Not installed by default (bone runs every tool call). Install:
--   cp -r examples/plugins/approve ~/.bone/plugins/
-- Settings (in ~/.bone/core.lua):
--   bone.config.approve.enabled = false          -- never ask
--   bone.config.approve.tools.shell = false      -- don't ask for shell
--   bone.config.approve.allow = function(ev)     -- skip asking when true
--     return ev.name == "shell" and ev.arguments.command:match("^git status")
--   end
-- BONE_APPROVAL=auto also turns it off for one run. Lua tools ask unless
-- registered with needs_approval = false.
--
-- The question sent to clients: { kind = "approval", title, tool, arguments }.
-- Answers: "allow", "deny", or "always" (allow this tool for the rest of the
-- session). The TUI side of this plugin (tui.lua) shows the prompt.

bone.config.approve = {
  enabled = true,
  tools = { write_file = true, edit_file = true, shell = true, read_file = false },
  allow = nil,
}

local always = {} -- session id -> { tool name = true }

local function needs_approval(cfg, ev)
  local name = ev.name
  local want = cfg.tools[name]
  if want ~= nil then
    return want
  end
  -- MCP tools ask unless their server marks them read-only.
  if ev.mcp then
    local hints = ev.mcp.annotations or {}
    return hints.readOnlyHint ~= true
  end
  local spec = bone._tools[name]
  return spec ~= nil and spec.needs_approval ~= false
end

bone.hook("tool_call", function(ev)
  local cfg = bone.config.approve
  if not cfg or not cfg.enabled or os.getenv("BONE_APPROVAL") == "auto" then
    return
  end
  if not needs_approval(cfg, ev) then
    return
  end
  local session = always[ev.session_id]
  if (session and session[ev.name]) or (cfg.allow and cfg.allow(ev)) then
    return
  end

  local answer = bone.ask({
    kind = "approval",
    title = "Allow " .. ev.name .. "?",
    tool = ev.name,
    arguments = ev.arguments,
  })
  if answer == "allow" then
    return
  elseif answer == "always" then
    always[ev.session_id] = session or {}
    always[ev.session_id][ev.name] = true
    return
  elseif answer == nil then
    return { deny = "Cancelled by the user before this tool call ran." }
  end
  return { deny = "The user denied this tool call." }
end)
