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
      if s or #bone.chat.items({}) > 0 then return {} end
      return {
        "",
        { { "  New session. Type a message and press enter.", "Dim" } },
        { { "  /help lists commands · ctrl+o opens an earlier session", "Dim" } },
      }
    end,
  }

  -- The live model reasoning, right in the chat after the last item. The
  -- chat scrolls, so it stays visible while the answer streams; ctrl+r (or
  -- /set show_reasoning!) hides it for more room.
  bone.ui.regions.thinking = {
    size = "auto",
    max = 3,
    render = function(ctx)
      if not bone.o.show_reasoning then
        return {}
      end
      local items = bone.chat.items({ kind = "reasoning", last = 1 })
      local item = items[1]
      if not item or not item.streaming or not item.text or item.text == "" then
        return {}
      end
      local text = item.text:gsub("^%s+", "")
      return bone.text.wrap({ { text, "Reasoning" } }, ctx.width, {
        first = { { "│ ", "Reasoning" } },
        rest = { { "  ", "Reasoning" } },
      })
    end,
  }
  bone.ui.regions.input_gap = {
    size = 1,
    render = function() return {} end,
  }
  bone.ui.layout = { "statusline", "top", "chat", "thinking", "divider", "input_gap", "prompt" }
end

return M
