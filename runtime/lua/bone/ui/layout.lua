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

  -- Drawn inside the chat viewport, after side panels have taken their space.
  bone.ui.regions.chat_empty = {
    render = function()
      local s = bone.api.session()
      if s or bone.chat.session().items > 0 then return {} end
      if bone.settings.get("setup.skipped") then
        return {
          { { "No model provider configured.", "Dim" } },
          { { "Run /setup to connect one · /health to diagnose setup", "Dim" } },
        }
      end
      return {
        { { "New session. Type a message and press enter.", "Dim" } },
        { { "/help lists commands · ctrl+o opens an earlier session", "Dim" } },
      }
    end
  }

  bone.ui.regions.input_gap = {
    size = 1,
    render = function()
      return bone.chat.view().follow and {} or { { { "  [↓ End]", "Dim" } } }
    end,
  }
  bone.ui.regions.attachments = {
    render = function(ctx)
      local out = {}
      for i, image in ipairs(bone.prompt.images()) do
        local label = ("  [%d: %s · %d×%d] · /detach %d"):format(i, image.name, image.width, image.height, i)
        local rows = bone.text.wrap({ { label, "Dim" } }, ctx.width)
        for _, row in ipairs(rows) do out[#out + 1] = row end
      end
      return out
    end,
  }
  bone.ui.layout = { "top", "chat", "input_gap", "above_prompt", "prompt", "attachments", "tray", "statusline" }
  bone.on("mouse", function(ev)
    if ev.region == "input_gap" and ev.button == "left" and ev.action == "down"
        and not bone.chat.view().follow and ev.col >= 3 and ev.col <= 9 then
      bone.action("scroll_bottom")
      return true
    end
  end)
end

return M
