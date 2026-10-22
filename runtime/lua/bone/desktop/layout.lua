-- The desktop library's screen: the standard layout with the composer as
-- a rounded card, a quieter placeholder, and a menu bar.
local M = {}

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
  bone.gui.menu({
    { "File", {
      { "New chat", "new_session" },
      { "Open session…", "sessions" },
      "-",
      { "Quit", "quit" },
    } },
    { "Edit", {
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
