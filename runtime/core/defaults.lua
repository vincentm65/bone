-- Default core settings. ~/.bone/core.lua runs after this;
-- ~/.bone/runtime/core/defaults.lua replaces this file.

bone.config.system_prompt = [[
You are bone, a coding assistant working in the user's terminal. Use the tools
to inspect files, make changes and run commands. Read files before editing
them. Keep changes minimal and focused, and keep answers concise.

If the user wants to customize bone, first read ]] .. bone.config_dir .. [[/docs/customizing.md.
For TUI customization, also read ]] .. bone.config_dir .. [[/docs/customizing/customizing-tui.md.]]

-- A background shell job the agent started has stopped: tell that session,
-- so the agent need not poll. Jobs cancelled on purpose stay quiet.
-- Set bone.config.notify_job_exit = false in core.lua to turn this off.
bone.config.notify_job_exit = true
bone.hook("process_exit", function(ev)
  if not bone.config.notify_job_exit or ev.state == "cancelled" then
    return
  end
  local how = ev.state == "exited" and ("exit code " .. tostring(ev.code))
    or ev.state == "timed_out" and "timed out"
    or ("failed: " .. tostring(ev.error or ev.signal))
  local text = ("Background job %s (`%s`) finished after %ds: %s."):format(
    ev.id, ev.command, math.floor((ev.elapsed_ms or 0) / 1000), how)
  if ev.output and ev.output ~= "" then
    text = text .. "\n\nLast output:\n```\n" .. ev.output .. "\n```"
  end
  pcall(bone.queue.add, ev.session_id, text, "steer", nil, "job")
end)
