-- bone.chat: items Lua adds, queries, sessions (shared ops in bone-ui).
local id = bone.chat.add("conf_note", { text = "hello", n = 1 })
eq(type(id), "string", "an id")
local items = bone.chat.items({ kind = "conf_note" })
eq(#items, 1, "found by kind")
eq(items[1].text, "hello", "with its fields")

eq(bone.chat.update(id, { text = "changed" }), true, "updated")
eq(bone.chat.items({ kind = "conf_note" })[1].text, "changed", "fields merged")
eq(bone.chat.update("no-such-item", {}), false, "unknown ids are not found")
eq(pcall(bone.chat.add, "user", {}), false, "built-in kinds are refused")

local s = bone.chat.session()
eq(s.new, true, "a new chat, before its first message")
eq(s.current, true, "on screen")
eq(s.items, 1, "counts the item")
local all = bone.chat.sessions()
eq(#all >= 1, true, "lists open chats")
eq(#bone.chat.turns(), 0, "no turns yet")

eq(bone.chat.remove(id), true, "removed")
eq(#bone.chat.items({ kind = "conf_note" }), 0, "gone")
