-- "graphite": the bone Android app's colors, the desktop library's
-- default. Neutral charcoal surfaces in three levels (background, panel,
-- raised), soft accents. Load with bone.colorscheme("graphite").
local p = {
  bg = "#121212", panel = "#1c1c1c", raised = "#2c2c2c", border = "#3a3a3a",
  fg = "#eeeeee", muted = "#b8b8b8", dim = "#8a8a8a",
  blue = "#d4d4d4", green = "#9bd5b0", yellow = "#e8c184", red = "#ffb0b7",
  purple = "#cfbaf3", cyan = "#a5d6e4", orange = "#f0b98b",
  bubble = "#333333",
  diff_add = "#7ee787", diff_add_bg = "#172b23", diff_del = "#ff7b72", diff_del_bg = "#34212a",
}

local hl = bone.hl.set
-- Normal's background is the window's.
hl("Normal", { fg = p.fg, bg = p.bg })
hl("Dim", { fg = p.dim })
hl("Accent", { fg = p.blue, bold = true })
hl("UserPrompt", { fg = p.muted })
hl("UserMessage", { fg = p.fg, bg = p.bubble })
hl("UserBubble", { fg = p.fg, bg = p.bubble })
hl("InputBackground", { fg = p.fg, bg = p.panel })
hl("InputText", { fg = p.fg, bg = p.panel })
hl("InputPrompt", { fg = p.muted, bg = p.panel })
hl("PromptBorder", { fg = p.border })
hl("Placeholder", { fg = p.dim })
hl("Reasoning", { fg = p.muted, italic = true })
hl("ToolName", { fg = p.fg, bold = true })
hl("ToolArgs", { fg = p.muted })
hl("ToolPath", { fg = p.cyan })
hl("ToolSummary", { fg = p.dim })
hl("ToolOutput", { fg = p.muted })
hl("ToolGutter", { fg = p.border })
hl("ToolRunning", { fg = p.yellow })
hl("ToolError", { fg = p.red })
hl("ToolDone", { fg = p.green })
hl("ToolCard", { bg = p.panel })
hl("DiffAdd", { fg = p.diff_add, bg = p.diff_add_bg })
hl("DiffDelete", { fg = p.diff_del, bg = p.diff_del_bg })
hl("ShellProgram", { fg = p.green })
hl("ShellPath", { fg = p.cyan })
hl("ShellFlag", { fg = p.blue })
hl("ShellString", { fg = p.green })
hl("ShellVariable", { fg = p.cyan })
hl("ShellComment", { fg = p.dim })
hl("ShellOperator", { fg = p.muted })
hl("MdHeading", { fg = p.fg, bold = true })
hl("MdBold", { fg = p.fg, bold = true })
hl("MdItalic", { fg = p.fg, italic = true })
hl("MdCode", { fg = p.orange })
hl("MdCodeBlock", { fg = p.fg })
hl("CodeBlock", { fg = p.fg, bg = p.panel })
hl("MdQuote", { fg = p.muted, italic = true })
hl("MdBullet", { fg = p.muted })
hl("MdLink", { fg = p.cyan, underline = true })
hl("MdTable", { fg = p.fg })
hl("MdTableHeader", { fg = p.fg, bold = true })
hl("Notice", { fg = p.yellow })
hl("ErrorMsg", { fg = p.red })
hl("WarningMsg", { fg = p.yellow })
hl("WinSeparator", { fg = "#262626" })
hl("StatusLine", { fg = p.muted, bg = p.bg })
hl("StatusLineDim", { fg = p.dim, bg = p.bg })
hl("Selection", { bg = p.raised })
hl("PopupBorder", { fg = p.border })
hl("PopupTitle", { fg = p.fg, bold = true })
-- Panels (the sessions sidebar, the tray, suggestions).
hl("Panel", { fg = p.fg, bg = p.panel })
hl("PanelDim", { fg = p.dim, bg = p.panel })
hl("PanelAccent", { fg = p.fg, bg = p.panel, bold = true })
hl("PanelSelected", { fg = p.fg, bg = p.raised, bold = true })
hl("PanelSelectedDim", { fg = p.muted, bg = p.raised })
hl("PanelRunning", { fg = p.yellow, bg = p.panel })
