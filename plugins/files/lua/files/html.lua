-- HTML as plain text: the tags go, the text stays. Enough of a preview
-- to read a page without a browser in the window.

local M = {}

local ENTITIES = {
  ["&amp;"] = "&", ["&lt;"] = "<", ["&gt;"] = ">", ["&quot;"] = '"',
  ["&apos;"] = "'", ["&nbsp;"] = " ", ["&hellip;"] = "…",
  ["&mdash;"] = "—", ["&ndash;"] = "–", ["&rsquo;"] = "’",
  ["&lsquo;"] = "‘", ["&ldquo;"] = "“", ["&rdquo;"] = "”",
  ["&middot;"] = "·", ["&bull;"] = "•",
}

-- The tags that end a line.
local BLOCKS = {
  "p", "div", "section", "article", "header", "footer", "main", "aside",
  "nav", "ul", "ol", "li", "dl", "dt", "dd", "table", "thead", "tbody",
  "tr", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "blockquote", "form",
  "figure", "figcaption", "hr",
}

--- `html` as text: the markup gone, the words left, in paragraphs.
function M.to_text(html)
  local text = tostring(html or "")
  -- What is not text at all.
  text = text:gsub("<!%-%-.-%-%->", " ")
  text = text:gsub("<%s*[sS][cC][rR][iI][pP][tT].-</%s*[sS][cC][rR][iI][pP][tT]%s*>", " ")
  text = text:gsub("<%s*[sS][tT][yY][lL][eE].-</%s*[sS][tT][yY][lL][eE]%s*>", " ")
  -- A line break is a line.
  text = text:gsub("<%s*[bB][rR]%s*/?>", "\n")
  -- The block tags end their line.
  for _, tag in ipairs(BLOCKS) do
    text = text:gsub("<%s*" .. tag .. "[^>]*>", "\n")
    text = text:gsub("</%s*" .. tag .. "%s*>", "\n")
  end
  -- What is left of the tags goes.
  text = text:gsub("<[^>]*>", "")
  -- The entities that show up in prose.
  for entity, char in pairs(ENTITIES) do
    text = text:gsub(entity, char)
  end
  text = text:gsub("&#(%d+);", function(n)
    return string.char(tonumber(n) or 32)
  end)
  -- The whitespace the layout left behind.
  text = text:gsub("[ \t]+", " ")
  text = text:gsub("\n[ \t]+", "\n")
  text = text:gsub("\n%s*\n%s*\n+", "\n\n")
  text = text:gsub("^%s+", "")
  text = text:gsub("%s+$", "")
  return text
end

return M
