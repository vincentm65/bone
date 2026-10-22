-- The desktop library's colors: the Android app's graphite palette
-- (neutral charcoal surfaces in three levels, soft accents), as highlight
-- groups over the standard "black" scheme. Change any with bone.hl.set in
-- ~/.bone/gui.lua.
local M = {}

M.palette = {
  bg = "#121212", panel = "#1c1c1c", raised = "#2c2c2c", border = "#444444",
  fg = "#eeeeee", muted = "#b8b8b8", dim = "#8a8a8a",
  blue = "#d4d4d4", green = "#9bd5b0", yellow = "#e8c184", red = "#ffb0b7",
  purple = "#cfbaf3", cyan = "#a5d6e4", orange = "#f0b98b",
  bubble = "#333333", code = "#1c1c1c",
  diff_add = "#7ee787", diff_add_bg = "#172b23", diff_del = "#ff7b72", diff_del_bg = "#34212a",
}

function M.setup()
  local p, hl = M.palette, bone.hl.set
  -- Normal's background is the window's.
  hl("Normal", { fg = p.fg, bg = p.bg })
  hl("Dim", { fg = p.dim })
  hl("Accent", { fg = p.blue, bold = true })
  hl("UserPrompt", { fg = p.muted, bg = p.bubble })
  hl("UserMessage", { fg = p.fg, bg = p.bubble })
  hl("UserBubble", { fg = p.fg, bg = p.bubble })
  hl("InputBackground", { fg = p.fg, bg = p.panel })
  hl("InputText", { fg = p.fg, bg = p.panel })
  hl("PromptBorder", { fg = p.border })
  hl("Placeholder", { fg = p.dim })
  hl("Reasoning", { fg = p.muted, italic = true })
  hl("ToolName", { fg = p.fg })
  hl("ToolArgs", { fg = p.muted })
  hl("ToolPath", { fg = p.cyan })
  hl("ToolSummary", { fg = p.dim })
  hl("ToolOutput", { fg = p.muted })
  hl("ToolGutter", { fg = p.border })
  hl("ToolRunning", { fg = p.yellow })
  hl("ToolError", { fg = p.red })
  hl("DiffAdd", { fg = p.diff_add, bg = p.diff_add_bg })
  hl("DiffDelete", { fg = p.diff_del, bg = p.diff_del_bg })
  hl("MdHeading", { fg = p.fg, bold = true })
  hl("MdBold", { fg = p.fg, bold = true })
  hl("MdItalic", { fg = p.fg, italic = true })
  hl("MdCode", { fg = p.orange })
  hl("MdCodeBlock", { fg = p.fg })
  hl("CodeBlock", { fg = p.fg, bg = p.code })
  hl("MdQuote", { fg = p.muted, italic = true })
  hl("MdBullet", { fg = p.muted })
  hl("MdLink", { fg = p.cyan, underline = true })
  hl("Notice", { fg = p.yellow })
  hl("ErrorMsg", { fg = p.red })
  hl("WarningMsg", { fg = p.yellow })
  hl("WinSeparator", { fg = "#2c2c2c" })
  hl("StatusLine", { fg = p.muted, bg = p.bg })
  hl("StatusLineDim", { fg = p.dim, bg = p.bg })
  hl("PopupBorder", { fg = p.border })
  hl("PopupTitle", { fg = p.fg, bold = true })
  hl("Selection", { bg = "#2a3f55" })
end

return M
