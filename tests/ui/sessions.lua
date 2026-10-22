-- Sessions in chats, the message log and running Lua, the same in every
-- frontend: a session opens in a chat of its own (shown or not), the chat
-- on screen reports it, and each chat keeps its own draft.
local made, shown = {}, {}
bone.on("chat/shown", function(ev) shown[#shown + 1] = ev end)
bone.request("session/create", {}, function(r) made.a = r.session_id end)

return {
  function()
    assert(made.a, "a session")
    bone.request("session/create", {}, function(r) made.b = r.session_id end)
  end,
  function()
    -- Not shown: a chat that follows it, off screen.
    made.chat = bone.chat.open(made.b)
    assert(type(made.chat) == "number", "a chat number")
  end,
  function()
    -- Once loaded, the chat is the session's.
    eq(bone.chat.open(made.b), made.chat, "opened once")
    bone.prompt.set("draft for the first chat")
    bone.api.open_session(made.a)
  end,
  function()
    eq(bone.chat.session().session_id, made.a, "shown")
    eq(shown[#shown].session_id, made.a, "chat/shown fired")
    eq(bone.prompt.get(), "", "a chat of its own has its own draft")
    bone.api.open_session(made.b)
  end,
  function()
    eq(bone.chat.session().session_id, made.b, "switched")
    bone.api.show("shown, not logged")
    local log = bone.api.log(50)
    for _, line in ipairs(log) do
      assert(line ~= "shown, not logged", "show does not log")
    end
    bone.api.exec_lua("conf_lua_ran = 1 + 1")
    eq(conf_lua_ran, 2, "exec_lua ran")
    eq(bone.chat.at(10000, 10000), nil, "nothing far off screen")
  end,
}
