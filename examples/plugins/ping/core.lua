-- ping: a minimal sample plugin. Shows the core plugin API in one place:
-- a config option, a hook, and a command.
--
--   /ping        print the current time in a notification
--   /ping log    print the last tool call in a notification
--
--   bone.config.ping.enabled = false    -- silence the tool_call log hook

bone.config.ping = { enabled = true }

local last_tool

-- Remember the most recent tool call (for /ping log).
bone.hook("tool_call", function(ev)
  last_tool = ev.name
end)

bone.cmd.create("ping", function(c)
  local cfg = bone.config.ping or {}
  if cfg.enabled == false then
    return bone.notify("ping is disabled (bone.config.ping.enabled = false)")
  end
  if c.args == "log" then
    return bone.notify(last_tool and ("last tool: " .. last_tool) or "no tool calls yet")
  end
  return bone.notify("pong at " .. os.date("%Y-%m-%d %H:%M:%S"))
end, {
  desc = "a sample plugin: /ping, /ping log",
  complete = function()
    return { { value = "log", desc = "show the last tool call" } }
  end,
})