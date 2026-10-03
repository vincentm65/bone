-- compact, TUI side: /compact summarizes the older part of the session on
-- screen (the work happens in the core; see core.lua).
bone.cmd.create("compact", function()
  local s = bone.chat.session()
  if not s or not s.session_id then
    return bone.notify("nothing to compact yet")
  end
  if s.running then
    return bone.notify("wait for the turn to finish", "error")
  end
  bone.notify("compacting…")
  bone.rpc.call("compact", {}, function(report, err)
    bone.notify(report or ("compact: " .. tostring(err)), report and "info" or "error")
  end)
end, { desc = "summarize the older part of this session (compact plugin)" })
