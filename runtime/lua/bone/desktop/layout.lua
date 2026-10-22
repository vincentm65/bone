-- The desktop library's screen: the standard layout with the composer as
-- a rounded card, a quieter placeholder, a menu bar, and a notification
-- when an answer finishes while the window is in the background.
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

function M.setup()
  bone.on("turn/finished", finished)
  bone.ui.prompt = {
    prefix = { { "  ", "InputText" } },
    continuation = { { "  ", "InputText" } },
    placeholder = { { "Message bone…", "Placeholder" } },
    background = "InputBackground",
    border = { style = "rounded", hl = "PromptBorder" },
    padding = { 0, 1 },
    min = 1,
  }
  bone.gui.menu({
    { "File", {
      { "New chat", "new_session" },
      { "Open session…", "sessions" },
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
      { "Health", "/health" },
    } },
    { "Help", {
      { "Help", "/help" },
      { "Commands", "/help commands" },
    } },
  })
end

return M
