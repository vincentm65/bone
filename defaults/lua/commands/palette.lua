-- Command palette — a menu defined entirely in bundled Lua.
--
-- This is the Phase 7 pattern: the menu's *definition* lives in Lua (seeded to
-- ~/.bone-rust/lua/commands/ like the other defaults) and draws itself through
-- the ViewModel UI API (bone.api.ui), while Rust still renders the float. Users
-- can copy/edit this file to redefine the menu, the Neovim way.
--
-- It also returns a plain-text `display`, so it degrades gracefully wherever the
-- ViewModel isn't being rendered yet (e.g. headless).

local ENTRIES = {
  { key = "/usage",   desc = "token usage for this conversation" },
  { key = "/history", desc = "browse past conversations" },
  { key = "/memory",  desc = "manage memory" },
  { key = "/review",  desc = "review the working diff" },
  { key = "/shotgun", desc = "multi-model search/blast/judge review" },
  { key = "/compact", desc = "compact the conversation" },
}

local function build_lines()
  local lines = { "Command Palette", "" }
  for _, e in ipairs(ENTRIES) do
    table.insert(lines, string.format("  %-10s %s", e.key, e.desc))
  end
  return lines
end

bone.register_command("palette", {
  description = "Open the command palette (a Lua-defined menu)",
  handler = function(_, _ctx)
    local lines = build_lines()

    -- Draw the menu as a centered float via the UI API. Guarded so the command
    -- still works if the UI API is unavailable.
    if bone.api and bone.api.ui and bone.api.ui.open_float then
      bone.api.ui.open_float({
        id = "palette",
        title = "Palette",
        lines = lines,
        anchor = "center",
        width = 56,
        height = #lines + 2,
        border = true,
      })
    end

    return { display = table.concat(lines, "\n"), submit = false }
  end,
})
