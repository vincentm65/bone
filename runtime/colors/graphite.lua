-- "graphite": the desktop library's default. A black window with dark grey
-- surfaces in two levels (panel, raised), soft accents. Load with
-- bone.colorscheme("graphite").
local p = {
  bg = "#000000", panel = "#0c0c0c", raised = "#161616", border = "#1c1c1c",
  fg = "#eeeeee", muted = "#b8b8b8", dim = "#8a8a8a",
  -- Accent and code match the TUI black palette's light blue.
  blue = "#c8dcf5", path = "#abc4e3",
  green = "#9bd5b0", yellow = "#e8c184", orange = "#f0b98b", red = "#ffb0b7",
  purple = "#cfbaf3", cyan = "#a5d6e4",
  bubble = "#121212",
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
-- What the window draws itself: the prompt's cursor, selected text.
hl("TextSelection", { fg = p.blue, bg = "#2f3a4a" })
hl("Cursor", { fg = p.fg })
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
hl("Success", { fg = p.green })
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
hl("MdCode", { fg = p.path })
hl("MdCodeBlock", { fg = p.fg })
hl("CodeBlock", { fg = p.fg, bg = p.panel })
hl("MdQuote", { fg = p.muted, italic = true })
hl("MdBullet", { fg = p.muted })
hl("MdLink", { fg = p.path, underline = true })
hl("MdTable", { fg = p.fg })
hl("MdTableHeader", { fg = p.fg, bold = true })
hl("Notice", { fg = p.yellow })
hl("ErrorMsg", { fg = p.red })
hl("WarningMsg", { fg = p.yellow })
hl("WinSeparator", { fg = "#141414" })
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
-- A chat's state in the sessions sidebar: running, waiting for an
-- approval, finished while you were elsewhere.
hl("SessionRunning", { fg = p.yellow })
hl("SessionWaiting", { fg = p.orange })
hl("SessionUnread", { fg = p.fg })
