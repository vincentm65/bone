-- "graphite": the desktop library's default. A pure black window; what
-- floats (the composer, menus, dialogs) is one near-black surface with a
-- hairline. Three greys carry the hierarchy; color is only for state: blue
-- for focus and unread, green done, amber waiting, red failed. Load with
-- bone.colorscheme("graphite").
local p = {
  bg = "#000000", side = "#0a0a0a", panel = "#0f0f0f", raised = "#1a1a1a",
  -- Hairlines: white at 8% (a divider) and 14% (an outline) on black.
  line = "#141414", border = "#262626",
  fg = "#ededed", muted = "#a1a1a1", dim = "#7a7a7a",
  blue = "#4c8dff", path = "#a1a1a1",
  green = "#5fd08a", yellow = "#f0b84a", orange = "#f0b84a", red = "#ff6b6b",
  -- Syntax only (shell lines).
  purple = "#c4b1f0", cyan = "#9ccfdd",
  bubble = "#1a1a1a",
  diff_add = "#7ee787", diff_add_bg = "#172b23", diff_del = "#ff7b72", diff_del_bg = "#34212a",
}

local hl = bone.hl.set
-- Normal's background is the window's.
hl("Normal", { fg = p.fg, bg = p.bg })
hl("Dim", { fg = p.dim })
-- Secondary text: a row that is not the open one, a description.
hl("Muted", { fg = p.muted })
-- Accent is emphasis, not a color: the bright grey. Blue is kept for state
-- (running, focus, selection).
hl("Accent", { fg = p.fg })
hl("UserPrompt", { fg = p.muted })
hl("UserMessage", { fg = p.fg, bg = p.bubble })
hl("UserBubble", { fg = p.fg, bg = p.bubble })
hl("InputBackground", { fg = p.fg, bg = p.panel })
hl("InputText", { fg = p.fg, bg = p.panel })
hl("InputPrompt", { fg = p.muted, bg = p.panel })
hl("PromptBorder", { fg = p.border })
hl("Placeholder", { fg = p.dim })
-- What the window draws itself: the prompt's cursor, selected text.
hl("TextSelection", { fg = p.fg, bg = "#1d3560" })
hl("Cursor", { fg = p.fg })
hl("Reasoning", { fg = p.muted, italic = true })
hl("ToolName", { fg = p.fg, bold = true })
hl("ToolArgs", { fg = p.muted })
hl("ToolPath", { fg = p.cyan })
hl("ToolSummary", { fg = p.dim })
hl("ToolOutput", { fg = p.muted })
hl("ToolGutter", { fg = p.line })
hl("ToolRunning", { fg = p.yellow })
hl("ToolError", { fg = p.red })
hl("ToolDone", { fg = p.green })
hl("ToolCard", { bg = p.panel })
hl("Success", { fg = p.green })
hl("DiffAdd", { fg = p.diff_add, bg = p.diff_add_bg })
hl("DiffDelete", { fg = p.diff_del, bg = p.diff_del_bg })
-- The input's Git change count (+added −removed).
hl("InputAdded", { fg = p.diff_add, bg = p.panel })
hl("InputRemoved", { fg = p.diff_del, bg = p.panel })
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
hl("MdCode", { fg = p.fg })
hl("MdCodeBlock", { fg = p.fg })
hl("CodeBlock", { fg = p.fg, bg = "#141414" })
hl("MdQuote", { fg = p.muted, italic = true })
hl("MdBullet", { fg = p.muted })
hl("MdLink", { fg = p.fg, underline = true })
hl("MdTable", { fg = p.fg })
hl("MdTableHeader", { fg = p.fg, bold = true })
hl("Notice", { fg = p.yellow })
hl("ErrorMsg", { fg = p.red })
hl("WarningMsg", { fg = p.yellow })
hl("WinSeparator", { fg = p.line })
hl("StatusLine", { fg = p.muted, bg = p.bg })
-- The title bar the window draws on macOS and Windows: the title, the bar.
hl("TitleBar", { fg = p.dim, bg = p.bg })
hl("StatusLineDim", { fg = p.dim, bg = p.bg })
hl("Selection", { bg = p.raised })
hl("PopupBorder", { fg = p.border })
hl("PopupTitle", { fg = p.fg, bold = true })
-- The docked sidebar: a step above the window, a hairline at its edge.
hl("Sidebar", { fg = p.muted, bg = p.side })
-- Panels (the tray, suggestions).
hl("Panel", { fg = p.fg, bg = p.panel })
hl("PanelDim", { fg = p.dim, bg = p.panel })
hl("PanelAccent", { fg = p.fg, bg = p.panel })
hl("PanelSelected", { fg = p.fg, bg = p.raised })
hl("PanelSelectedDim", { fg = p.muted, bg = p.raised })
hl("PanelRunning", { fg = p.blue, bg = p.panel })
-- A chat's state in the sessions sidebar: running, waiting for an
-- approval, finished while you were elsewhere.
hl("SessionRunning", { fg = p.fg })
hl("SessionWaiting", { fg = p.orange })
hl("SessionUnread", { fg = p.blue })
