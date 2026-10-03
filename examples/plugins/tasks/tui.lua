-- tasks: a task list in a panel beside the chat, kept between sessions.
--
--   /task fix the parser     add a task
--   /tasks                   show or hide the panel (it reopens next time)
--   /task                    give the panel the keyboard
--
-- In the panel: up/down select, enter or space toggles done, s puts the
-- task in the prompt, d deletes it, x clears done ones, esc goes back.

local st = bone.plugin.state() -- ~/.bone/state/tui/tasks.json
st.items = st.items or {}
local sel = 1
local panel

local function save()
  bone.plugin.save_state()
end

local function clamp()
  sel = math.max(1, math.min(sel, #st.items))
end

-- Keep the selected row in view (the title row is not content).
local function follow()
  local info = panel and panel:info()
  if not info or not info.height then
    return
  end
  local rows = info.height - 1
  if sel - 1 < info.top then
    panel:scroll(sel - 1 - info.top)
  elseif sel > info.top + rows then
    panel:scroll(sel - info.top - rows)
  end
end

local function render(ctx)
  if #st.items == 0 then
    return { { { "nothing to do", "Dim" } }, { { "/task text adds one", "Dim" } } }
  end
  clamp()
  local lines = {}
  for i, t in ipairs(st.items) do
    local hl = t.done and "Dim" or "Normal"
    if ctx.focused and i == sel then
      hl = "Selection"
    end
    lines[i] = { { (t.done and "✓ " or "· ") .. t.text, hl }, { fill = " ", hl = hl } }
  end
  return lines
end

local function move(by)
  sel = sel + by
  clamp()
  follow()
end

local function title()
  local open = 0
  for _, t in ipairs(st.items) do
    if not t.done then
      open = open + 1
    end
  end
  return "Tasks " .. open .. "/" .. #st.items
end

local function changed()
  save()
  if panel then
    panel:update({ title = title() })
  end
end

local keys = {
  up = function()
    move(-1)
  end,
  down = function()
    move(1)
  end,
  enter = function()
    local t = st.items[sel]
    if t then
      t.done = not t.done
      changed()
    end
  end,
  d = function()
    if st.items[sel] then
      table.remove(st.items, sel)
      clamp()
      changed()
    end
  end,
  x = function()
    local keep = {}
    for _, t in ipairs(st.items) do
      if not t.done then
        keep[#keep + 1] = t
      end
    end
    st.items = keep
    clamp()
    changed()
  end,
  s = function()
    local t = st.items[sel]
    if t then
      bone.prompt.set(t.text)
      bone.ui.panel.focus(nil)
    end
  end,
}
keys.space = keys.enter

local function show()
  if panel and panel:is_open() then
    panel:show()
  else
    panel = bone.ui.panel.open({
      id = "tasks",
      dock = "right",
      size = 32,
      title = title(),
      render = render,
      keys = keys,
    })
  end
  st.open = true
  save()
end

local function hide()
  if panel then
    panel:hide()
  end
  st.open = false
  save()
end

bone.cmd.create("tasks", function()
  if panel and panel:is_open() and not panel:info().hidden then
    hide()
  else
    show()
  end
end, { desc = "show or hide the task list" })

bone.cmd.create("task", function(c)
  if c.args == "" then
    show()
    panel:focus()
    return
  end
  table.insert(st.items, { text = c.args, done = false })
  sel = #st.items
  show()
  changed()
  follow()
end, { desc = "add a task (no text: focus the list)" })

if st.open then
  show()
end
