-- Standard prompt and empty-session layout.
local M = {}

function M.setup(opts)
  opts = opts or {}
  bone.ui.prompt = opts.prompt or {
    prefix = { { "› ", "UserPrompt" } },
    continuation = { { "  ", "InputText" } },
    placeholder = { { "Message bone… (enter sends, alt+enter for a new line, / for commands)", "Placeholder" } },
    -- Three-row composer: a padded row above, the text row, and a padded
    -- row below. Rust fills all three rows with this Lua highlight group.
    background = "InputBackground",
    padding = { 1, 1 },
    min = 1,
  }

  bone.ui.regions.top = {
    size = "auto",
    max = 3,
    render = function()
      local s = bone.api.session()
      if s or bone.chat.session().items > 0 then return {} end
      if bone.settings.get("setup.skipped") then
        return {
          "",
          { { "  No model provider configured.", "Dim" } },
          { { "  Run /setup to connect one · /health to diagnose setup", "Dim" } },
        }
      end
      return {
        "",
        { { "  New session. Type a message and press enter.", "Dim" } },
        { { "  /help lists commands · ctrl+o opens an earlier session", "Dim" } },
      }
    end
  }

  bone.ui.regions.input_gap = {
    size = 1,
    render = function() return {} end,
  }
  bone.ui.layout = { "top", "chat", "input_gap", "above_prompt", "prompt", "tray", "statusline" }
end

return M
