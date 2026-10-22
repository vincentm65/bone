-- The turn footers under each turn's last item ("Worked for 3s · Finished
-- at 23:17"), drawn for the TUI: the timing is bone.timing.
local timing = require("bone.timing")
local kinds = { "user", "reasoning", "assistant", "tool", "notice" }

for _, kind in ipairs(kinds) do
  local view = bone.ui.views[kind]
  bone.ui.views[kind] = function(item, ctx)
    local out = view(item, ctx)
    local text = timing.footer(item, ctx)
    if not text then return out end
    out = out or {} -- Folded tools and hidden reasoning can carry footers.
    out[#out + 1] = {}
    for _, row in ipairs(bone.text.wrap({ { text, "Dim" } }, ctx.width, {
      first = { { "  ", "Dim" } }, rest = { { "  ", "Dim" } },
    })) do
      out[#out + 1] = row
    end
    return out
  end
end
