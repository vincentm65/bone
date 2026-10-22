-- The desktop library's screen: the chat, the composer as a rounded card
-- with attached images and the `/` suggestions above it, the tray and the
-- status bar under it, the sessions sidebar on the left; a welcome when a
-- chat is empty, /health and help, a menu bar, and a notification when an
-- answer finishes while the window is in the background.
local M = {}

--- Pick image files to attach with the system's dialog.
function M.attach()
  bone.gui.open_file({
    title = "Attach images",
    multiple = true,
    filters = { { "Images", { "png", "jpg", "jpeg", "webp" } } },
  }, function(paths)
    for _, p in ipairs(paths or {}) do bone.prompt.attach(p) end
  end)
end

-- A turn of the session on screen finished while the window was away.
local function finished(ev)
  local s = bone.chat.session()
  local info = bone.gui.window_info()
  if info.focused or not s or ev.session_id ~= s.session_id then return end
  local status = type(ev.outcome) == "table" and ev.outcome.status
  if status == "cancelled" then return end
  local title = status == "failed" and "bone: the turn failed" or "bone finished"
  bone.gui.notify(title, status == "failed" and ev.outcome.message or (s.title or ""))
end

-- The `/` command suggestions, as a list on the panel surface.
local function suggestions(ctx)
  local name_w = 0
  for _, it in ipairs(ctx.items) do
    name_w = math.max(name_w, bone.text.width(it.name) + 2)
  end
  local lines = {}
  for i, it in ipairs(ctx.items) do
    local on = i == ctx.selected
    local hl, dim = on and "PanelSelected" or "Panel", on and "PanelSelectedDim" or "PanelDim"
    local name = "/" .. it.name
    lines[i] = {
      { "   " .. name .. string.rep(" ", name_w - bone.text.width(name) + 2), hl },
      { it.desc, dim },
      { fill = " ", hl = dim },
    }
  end
  return lines
end

-- /health: this frontend's checks, then the core's, in a window.
local MARK = { ok = { "✓  ", "ToolDone" }, warn = { "!  ", "WarningMsg" }, error = { "✗  ", "ErrorMsg" } }
local function health_lines(title, items, out)
  out[#out + 1] = { { title, "Accent" } }
  for _, it in ipairs(items) do
    local m = MARK[it.status] or MARK.warn
    out[#out + 1] = { m, { it.name .. "   ", "Normal" }, { (tostring(it.message):gsub("\n.*", "")), "Dim" } }
  end
  out[#out + 1] = {}
end

local function health()
  local mine = bone.api.health()
  for _, it in ipairs(bone._run_health()) do mine[#mine + 1] = it end
  local lines = {}
  health_lines("This window", mine, lines)
  local pager = bone.ui.pager({ unpack(lines), { { "Core: checking…", "Dim" } } }, { title = "Health" })
  bone.request("health/check", {}, function(core, err)
    local all = { unpack(lines) }
    health_lines("Core", err and { { name = "core", status = "error", message = err } } or core or {}, all)
    pager:set(all)
  end)
end

function M.setup()
  bone.ui.prompt = {
    prefix = { { "  ", "InputText" } },
    continuation = { { "  ", "InputText" } },
    placeholder = { { "Message bone…", "Placeholder" } },
    background = "InputBackground",
    border = { style = "rounded", hl = "PromptBorder" },
    padding = { 0, 1 },
    min = 1,
  }
  bone.ui.suggestions = suggestions
  bone.ui.health = health
  function bone.ui.help(topic)
    local help = require("bone.help")
    if topic and topic:match("%S") then return help.topic(topic) end
    return help.open()
  end
  bone.ui.sessions = require("bone.desktop.sidebar").open

  -- A new chat: a welcome in the middle of the chat.
  bone.ui.regions.chat_empty = {
    render = function()
      local s = bone.api.session()
      if s or bone.chat.session().items > 0 then return {} end
      return {
        { { "What can I help with?", "Normal" } },
        {},
        { { "/help for commands  ·  ctrl+o for your chats  ·  ctrl+] for a terminal", "Dim" } },
      }
    end,
  }
  -- Scrolled up: a way back to the latest.
  bone.ui.regions.input_gap = {
    size = 1,
    render = function()
      return bone.chat.view().follow and {} or { { { "  ↓ Jump to latest", "Accent" } } }
    end,
  }
  bone.on("mouse", function(ev)
    if ev.region == "input_gap" and ev.button == "left" and ev.action == "down"
        and not bone.chat.view().follow then
      bone.action("scroll_bottom")
      return true
    end
  end)
  bone.ui.regions.attachments = {
    render = function()
      local out = {}
      for i, image in ipairs(bone.prompt.images()) do
        out[#out + 1] = { { ("   ▣  %s  ·  %d×%d    /detach %d"):format(image.name, image.width, image.height, i), "Dim" } }
      end
      return out
    end,
  }
  bone.ui.layout = { "chat", "input_gap", "above_prompt", "attachments", "prompt", "tray", "statusline" }

  bone.on("turn/finished", finished)
  bone.gui.menu({
    { "File", {
      { "New chat", "new_session" },
      { "Open chat…", "sessions" },
      { "Terminal", "/term" },
      "-",
      { "Quit", "quit" },
    } },
    { "Edit", {
      { "Attach image…", function() M.attach() end },
      { "Paste image", "paste_clipboard" },
    } },
    { "View", {
      { "Tool detail", function()
        local next_detail = { summary = "rows", rows = "full", full = "summary" }
        bone.o.tool_detail = next_detail[bone.o.tool_detail] or "rows"
      end },
      { "Reasoning", function() bone.o.apply("show_reasoning!") end },
      { "Health", "/health" },
    } },
    { "Help", {
      { "Help", "/help" },
      { "Commands", "/help commands" },
    } },
  })
end

return M
