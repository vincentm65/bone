-- /history is a compatibility alias for Bone 3's session picker.  The core
-- remains authoritative; selecting a row opens the existing session.
bone.cmd.create("history", function()
  bone.request("session/list", {}, function(list, err)
    if err then return bone.notify("history: " .. tostring(err), "error") end
    local items = {}
    for _, s in ipairs(list or {}) do
      local title = s.title or "(untitled)"
      local when = s.updated_at or s.created_at or ""
      items[#items + 1] = { session_id = s.session_id, title = title, when = when }
    end
    if #items == 0 then return bone.notify("no previous sessions") end
    local picker = bone.ui.select(items, {
      prompt = "History",
      format = function(item)
        return item.title .. (item.when ~= "" and ("  " .. item.when) or "")
      end,
      on_choice = function(item)
        if item then bone.api.open_session(item.session_id) end
      end,
    })
    return picker
  end)
end, { desc = "resume a recent conversation" })
