-- Web search. With `uv` on the PATH it runs a real DuckDuckGo search through
-- the `ddgs` package (uv fetches it on first use; nothing is installed into
-- the user's Python). Without uv it falls back to DuckDuckGo's Instant Answer
-- endpoint, which only knows encyclopedia-style topics, so many queries
-- return nothing there.
local function url_encode(s)
  return (tostring(s):gsub("([^%w%-%._~ ])", function(c)
    return string.format("%%%02X", c:byte())
  end):gsub(" ", "+"))
end

local function shell_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

-- The query goes in on stdin as JSON, so nothing in it reaches the shell.
local SEARCH_PY = [[
import json, sys
from ddgs import DDGS
req = json.load(sys.stdin)
for r in DDGS().text(req["q"], max_results=req["n"]):
    print(json.dumps(r))
]]

--- Results from ddgs, nil when uv is missing, or nil and an error.
local function ddgs_search(query, limit)
  local uv = bone.system("command -v uv", { timeout = 5000 })
  if not uv or uv.code ~= 0 then
    return nil
  end
  local r = bone.system("uv run --quiet --with ddgs python3 -c " .. shell_quote(SEARCH_PY), {
    stdin = bone.json.encode({ q = query, n = limit }),
    timeout = 60000,
  })
  if not r then
    return nil, "web search cancelled"
  end
  if r.timed_out then
    return nil, "web search timed out"
  end
  if r.code ~= 0 then
    return nil, "web search failed: " .. tostring(r.stderr or ""):sub(-300)
  end
  local rows = {}
  for line in tostring(r.stdout or ""):gmatch("[^\n]+") do
    local ok, item = pcall(bone.json.decode, line)
    if ok and type(item) == "table" then
      rows[#rows + 1] = string.format("%d. %s\n   %s\n   %s", #rows + 1, item.title or "", item.href or "", item.body or "")
    end
  end
  return #rows > 0 and table.concat(rows, "\n") or "No results found."
end

--- DuckDuckGo's Instant Answer endpoint: topics and an abstract, if any.
local function instant_answer(query, limit)
  local response = bone.http({
    url = "https://api.duckduckgo.com/?q=" .. url_encode(query) .. "&format=json&no_html=1&skip_disambig=1",
    timeout = 15000,
  })
  if not response or response.status < 200 or response.status >= 300 then
    return nil, "web search failed (HTTP " .. tostring(response and response.status or "connection") .. ")"
  end
  local ok, data = pcall(bone.json.decode, response.body or "{}")
  if not ok then return nil, "web search returned invalid JSON" end
  local rows = {}
  local function add(item)
    if #rows >= limit or type(item) ~= "table" then return end
    local text = item.Text or item.FirstURL
    if text and text ~= "" then
      rows[#rows + 1] = string.format("%d. %s\n   %s", #rows + 1, text, item.FirstURL or "")
    end
  end
  for _, item in ipairs(data.RelatedTopics or {}) do
    if item.Topics then
      for _, nested in ipairs(item.Topics) do add(nested) end
    else
      add(item)
    end
  end
  if data.AbstractText and data.AbstractText ~= "" then
    rows[#rows + 1] = "Summary: " .. data.AbstractText .. (data.AbstractURL and ("\n" .. data.AbstractURL) or "")
  end
  return #rows > 0 and table.concat(rows, "\n") or "No results found."
end

bone.tool.register({
  name = "web_search",
  description = "Search the web with DuckDuckGo and return result titles, URLs and snippets.",
  parameters = {
    type = "object",
    properties = {
      query = { type = "string", minLength = 1 },
      num_results = { type = "integer", minimum = 1, maximum = 10 },
    },
    required = { "query" },
    additionalProperties = false,
  },
  needs_approval = false,
  parallel = true,
  run = function(args)
    local query = tostring(args.query or "")
    if query:match("^%s*$") then return nil, "query must be non-empty" end
    -- The model may ask for a number; else the saved setting (/config), else 5.
    local default = tonumber(bone.settings.get("web_search.num_results")) or 5
    local limit = math.max(1, math.min(10, tonumber(args.num_results) or default))
    local out, err = ddgs_search(query, limit)
    if out then
      return out
    end
    local fallback, fallback_err = instant_answer(query, limit)
    if fallback and (fallback ~= "No results found." or not err) then
      return fallback
    end
    return nil, err or fallback_err
  end,
})
