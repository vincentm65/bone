-- ask-model: ask a model something on the side, without touching the
-- session. "/ask question" asks it; "/ask" alone asks to explain the latest
-- answer in the session. The answer streams into a pager.
--
--   bone.o.ask_provider = "cheap"   -- a bone.config.providers key ("" for the current one)

bone.o.define("ask_provider", "", { desc = "provider for /ask (empty: the current one)" })

bone.cmd.create("ask", function(c)
  local question = c.args
  if question == "" then
    local last = bone.chat.items({ kind = "assistant", last = 1 })[1]
    if not last then
      return bone.notify("usage: /ask question (alone, it explains the latest answer)", "error")
    end
    question = "Explain this answer more simply:\n\n" .. last.text
  end
  local answer = ""
  local pager = bone.ui.pager("…", { title = "ask" })
  bone.model.complete({
    provider = bone.o.ask_provider ~= "" and bone.o.ask_provider or nil,
    system = "Answer briefly and directly.",
    prompt = question,
  }, function(d)
    if d.text then
      answer = answer .. d.text
      pager:set(answer)
    end
  end, function(r, err)
    pager:set(r and r.content or ("error: " .. tostring(err)))
  end)
end, { desc = "ask a model on the side (ask-model plugin)" })
