-- How long each turn worked, for a footer under its last item ("Worked for
-- 3s · Finished at 23:17"), kept per session outside the transcript so
-- another frontend watching unrelated chats cannot overwrite it. M.footer
-- gives the text; each frontend draws it (runtime/lua/bone/ui/turn_timing.lua
-- for the TUI).
local M = {}
local sessions, legacy = {}, bone.state.load("standard-turn-timing")
local name = "standard-turn-timing."
local kinds = { "user", "reasoning", "assistant", "tool", "notice" }

local function state_of(id)
  if not sessions[id] then
    local saved = bone.state.load(name .. id)
    sessions[id] = saved.turns and saved or legacy[id] or { turns = {} }
  end
  return sessions[id]
end

local function save(id)
  bone.state.save(name .. id, sessions[id])
end

local function last_user(id)
  return bone.chat.items({ session = id, kind = "user", last = 1 })[1]
end

local function capture(id)
  if not id then return end
  local s = bone.chat.session({ session = id })
  if not s or not s.running or not s.turn then return end
  local state = state_of(id)
  if state.active and state.active.id == s.turn.id then return end
  state.active = { id = s.turn.id, started = bone.now() - (s.turn.elapsed_ms or 0) }
  local user = last_user(id)
  if user then
    -- Truncated history may reuse an ordinal, even with identical text.
    for index in pairs(state.turns) do
      if tonumber(index) >= user.turn then state.turns[index] = nil end
    end
  end
  save(id)
end

--- The footer under `item` when it is the last of a finished turn, else nil.
function M.footer(item, ctx)
  if not ctx or ctx.region ~= "chat" then return end
  local s = bone.chat.session()
  local id = s and s.session_id
  if not id then return end
  if s.running then capture(id) end
  local data = bone.chat.item(item.index)
  local timing = data and state_of(id).turns[tostring(data.turn)]
  if not timing or not timing.finished then return end
  -- Ordinals and text survive session loads; item keys do not.
  local user = bone.chat.items({ turn = data.turn, kind = "user", first = 1 })[1]
  local last = bone.chat.items({ turn = data.turn, kind = kinds, last = 1 })[1]
  if not user or user.text ~= timing.text or not last or last.index ~= item.index then return end
  return "Worked for " .. bone.util.duration(math.max(0, (timing.finished - timing.started) / 1000))
    .. " · Finished at " .. os.date("%H:%M", math.floor(timing.finished / 1000))
end

bone.on("turn/started", function(ev) capture(ev.session_id) end)
bone.on("turn/finished", function(ev)
  local user = last_user(ev.session_id)
  if not user then return end -- Notifications also cover unopened chats.
  local state = state_of(ev.session_id)
  local active = state.active
  if not active or active.id ~= ev.turn_id then return end
  state.turns[tostring(user.turn)] = {
    text = user.text, started = active.started, finished = bone.now(),
  }
  state.active = nil
  save(ev.session_id)
  bone.ui.refresh()
end)
bone.on("chat/shown", function(ev) capture(ev.session_id) end)
bone.on("ready", function()
  for _, s in ipairs(bone.chat.sessions()) do capture(s.session_id) end
end)

return M
