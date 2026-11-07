-- What a file is, by its name: the preview the viewer draws and the
-- editor that may open it.

local M = {}

local MARKDOWN = { ".md", ".markdown", ".mdown", ".mkd" }
local HTML = { ".html", ".htm", ".xhtml" }
local IMAGE = { ".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp" }
local CODE = {
  ".c", ".cc", ".cpp", ".cs", ".css", ".go", ".h", ".hpp", ".html", ".ini",
  ".java", ".js", ".json", ".jsx", ".kt", ".lua", ".m", ".mm", ".php", ".pl",
  ".py", ".rb", ".rs", ".scala", ".sh", ".sql", ".swift", ".toml", ".ts",
  ".tsx", ".txt", ".vue", ".xml", ".yaml", ".yml",
}

local function has(list, name)
  for _, ext in ipairs(list) do
    if name:lower():sub(-#ext) == ext then return true end
  end
  return false
end

--- What to do with a file called `name`: "markdown", "html", "image",
--- "pdf", "code" or "binary" (nothing to show).
function M.of(name)
  name = tostring(name or "")
  if has(MARKDOWN, name) then return "markdown" end
  if has(HTML, name) then return "html" end
  if name:lower():sub(-4) == ".pdf" then return "pdf" end
  if has(IMAGE, name) then return "image" end
  if has(CODE, name) then return "code" end
  return "binary"
end

--- Whether the file may be edited as text.
function M.editable(kind)
  return kind == "markdown" or kind == "html" or kind == "code"
end

return M
