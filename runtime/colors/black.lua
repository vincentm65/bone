-- "black": the palette from the original bone. Load with bone.colorscheme("black") (the default).
local p = {
  fg = "#eeeeee",
  muted = "#a0a0a0",
  subtle = "#666666",
  border = "#333333",
  accent = "#7aa2f7",
  good = "#9ece6a",
  warn = "#e0af68",
  error = "#f7768e",
  selection = "#1a1a1a",
  path = "#7dcfff",
  variable = "#bb9af7",
  number = "#ff9e64",
  user_bg = "#1a1a1a",
  diff_add = "#7ee787",
  diff_add_bg = "#1a1a1a",
  diff_del = "#ff7b72",
  diff_del_bg = "#222222",
}

local hl = bone.hl.set
hl("Normal", { fg = p.fg })
hl("Dim", { fg = p.subtle })
hl("Accent", { fg = p.accent, bold = true })
hl("UserPrompt", { fg = p.accent, bg = p.user_bg, bold = true })
hl("UserMessage", { fg = p.fg, bg = p.user_bg })
hl("InputBackground", { fg = p.fg, bg = p.user_bg })
hl("InputText", { fg = p.fg, bg = p.user_bg })
hl("Reasoning", { fg = p.subtle, italic = true })
hl("ToolName", { fg = p.fg })
hl("ToolArgs", { fg = p.muted })
hl("ToolPath", { fg = p.path })
hl("ToolSummary", { fg = p.subtle })
hl("ToolOutput", { fg = p.muted })
hl("ToolGutter", { fg = p.border })
hl("ToolRunning", { fg = p.subtle })
hl("ToolError", { fg = p.error })
hl("DiffAdd", { fg = p.diff_add, bg = p.diff_add_bg })
hl("DiffDelete", { fg = p.diff_del, bg = p.diff_del_bg })
hl("ShellProgram", { fg = p.good })
hl("ShellPath", { fg = p.path })
hl("ShellFlag", { fg = p.accent })
hl("ShellString", { fg = p.good })
hl("ShellVariable", { fg = p.path })
hl("ShellComment", { fg = p.subtle })
hl("ShellOperator", { fg = p.muted })
hl("MdHeading", { fg = p.accent, bold = true })
hl("MdBold", { fg = p.fg, bold = true })
hl("MdItalic", { fg = p.fg, italic = true })
hl("MdCode", { fg = p.path })
hl("MdCodeBlock", { fg = p.muted })
hl("MdQuote", { fg = p.muted, italic = true })
hl("MdBullet", { fg = p.accent })
hl("MdLink", { fg = p.path, underline = true })
hl("MdTable", { fg = p.fg })
hl("MdTableHeader", { fg = p.fg, bold = true })
hl("Notice", { fg = p.warn })
hl("ErrorMsg", { fg = p.error })
hl("WarningMsg", { fg = p.warn })
hl("WinSeparator", { fg = p.border })
hl("StatusLine", { fg = p.fg, bold = true })
hl("StatusLineDim", { fg = p.subtle })
hl("Selection", { bg = "#2a2a2a" })
hl("Placeholder", { fg = p.subtle })
hl("PopupBorder", { fg = p.border })
hl("PopupTitle", { fg = p.path, bold = true })
