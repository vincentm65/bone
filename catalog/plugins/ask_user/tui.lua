-- Generic ask_user renderer. Choice questions use the standard picker; text
-- and multi-select questions use a small popup editor whose result is sent
-- back as a string (multi-select values are comma-separated).
local open = {}

local function close(id)
  if open[id] then bone.ui.close(open[id]); open[id] = nil end
end

local function answer(ev, value)
  bone.request("ask/respond", { ask_id = ev.ask_id, answer = value })
  close(ev.ask_id)
end

local function text_popup(ev, q)
  local input = ""
  local id
  local function render(ctx)
    local lines = {
      { { q.text or "Question", "PopupTitle" } },
      {},
      { { input .. "▌", "Normal" } },
      {},
      { { "Enter submit   Esc cancel", "Dim" } },
    }
    return bone.ui.box(lines, { title = q.type == "multi_select" and "Choose (comma separated)" or "Answer", width = math.min(90, ctx.width - 4) })
  end
  local function finish(value)
    answer(ev, value)
  end
  id = bone.ui.popup({
    lines = render,
    guard = 200,
    keys = {
      enter = function() finish(input) end,
      esc = function() finish("cancelled") end,
      backspace = function() input = input:sub(1, -2); bone.ui.refresh() end,
      ["ctrl+c"] = function() finish("cancelled") end,
    },
    on_key = function(key)
      if type(key) == "string" and #key == 1 and key:byte() >= 32 then
        input = input .. key
        bone.ui.refresh()
        return true
      end
      return false
    end,
  })
  open[ev.ask_id] = id
end

bone.on("ask/requested", function(ev)
  local q = ev.question
  if type(q) ~= "table" or q.kind ~= "ask_user" then return end
  if q.type == "single_select" and type(q.options) == "table" and #q.options > 0 then
    local items = {}
    for _, item in ipairs(q.options) do
      items[#items + 1] = type(item) == "table" and (item.label or item.value) or tostring(item)
    end
    open[ev.ask_id] = bone.ui.select(items, {
      prompt = q.text or "Choose",
      on_choice = function(item)
        if item then answer(ev, item) else answer(ev, "cancelled") end
      end,
    })
  else
    text_popup(ev, q)
  end
end)

bone.on("ask/resolved", function(ev) close(ev.ask_id) end)
