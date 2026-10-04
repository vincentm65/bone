-- style: a look for bone, built only from the Lua API, drawn over the
-- runtime's standard UI.
-- Install: cp -r examples/plugins/style ~/.bone/plugins/

require("style.views") -- how each chat item looks (user, reasoning, assistant, tools, notices)
require("style.ui") -- statusline and divider

-- The classic arrangement: the divider (with the running turn) between the
-- chat and the prompt and the statusline at the bottom. The standard UI's
-- layout has no divider, so a complete look sets its own.
bone.ui.layout = {
  "top",
  { cols = { "left", "chat", "right" }, sep = "│" },
  "divider",
  "above_prompt",
  "prompt",
  "statusline",
}

bone.ui.prompt = {
  prefix = { { "› ", "UserPrompt" } },
  placeholder = { { "Message bone… (enter sends, alt+enter for a new line, / for commands)", "Placeholder" } },
}

-- What an empty session shows, above the prompt.
bone.ui.regions.top = {
  size = "auto",
  max = 3,
  render = function()
    local s = bone.api.session()
    if s or #bone.chat.items({}) > 0 then
      return {}
    end
    return {
      "",
      { { "  New session. Type a message and press enter.", "Dim" } },
      { { "  /help lists commands · ctrl+o opens an earlier session", "Dim" } },
    }
  end,
}
