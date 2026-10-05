-- Small pure-Lua helpers shared by the core and the TUI.
-- Available as `bone.util` on both sides; `bone.inspect` is `util.inspect`.

local M = {}

local function is_identifier(k)
  return type(k) == "string" and k:match("^[%a_][%w_]*$") ~= nil
end

local function sorted_keys(t)
  local keys = {}
  for k in pairs(t) do
    keys[#keys + 1] = k
  end
  table.sort(keys, function(a, b)
    local ta, tb = type(a), type(b)
    if ta ~= tb then
      return ta < tb
    end
    if ta == "number" or ta == "string" then
      return a < b
    end
    return tostring(a) < tostring(b)
  end)
  return keys
end

--- Human-readable representation of any value, for debugging.
function M.inspect(value, indent, seen)
  indent = indent or ""
  seen = seen or {}
  local t = type(value)
  if t == "string" then
    return string.format("%q", value)
  elseif t ~= "table" then
    return tostring(value)
  elseif seen[value] then
    return "<cycle>"
  end
  seen[value] = true
  local n = #value
  local keys = sorted_keys(value)
  if #keys == 0 then
    seen[value] = nil
    return "{}"
  end
  local inner = indent .. "  "
  local parts = {}
  for _, k in ipairs(keys) do
    local v = M.inspect(value[k], inner, seen)
    if type(k) == "number" and k >= 1 and k <= n and k % 1 == 0 then
      parts[#parts + 1] = inner .. v
    elseif is_identifier(k) then
      parts[#parts + 1] = inner .. k .. " = " .. v
    else
      parts[#parts + 1] = inner .. "[" .. M.inspect(k, inner, seen) .. "] = " .. v
    end
  end
  seen[value] = nil
  return "{\n" .. table.concat(parts, ",\n") .. "\n" .. indent .. "}"
end

--- Split `s` on a plain (non-pattern) separator.
function M.split(s, sep)
  local out, start = {}, 1
  while true do
    local i, j = s:find(sep, start, true)
    if not i then
      out[#out + 1] = s:sub(start)
      return out
    end
    out[#out + 1] = s:sub(start, i - 1)
    start = j + 1
  end
end

function M.trim(s)
  return (s:gsub("^%s+", ""):gsub("%s+$", ""))
end

function M.startswith(s, prefix)
  return s:sub(1, #prefix) == prefix
end

--- Shallow-merge tables left to right into a new table.
function M.extend(...)
  local out = {}
  for i = 1, select("#", ...) do
    local t = select(i, ...)
    if t then
      for k, v in pairs(t) do
        out[k] = v
      end
    end
  end
  return out
end

--- Human-readable duration: `45s`, `1m 05s`, `1h 04m`.
function M.duration(secs)
  if not secs then
    return ""
  end
  secs = math.floor(secs)
  local h, m, s = math.floor(secs / 3600), math.floor(secs % 3600 / 60), secs % 60
  if h > 0 then
    return string.format("%dh %02dm", h, m)
  elseif m > 0 then
    return string.format("%dm %02ds", m, s)
  end
  return s .. "s"
end

--- Wall-clock time of an epoch second, e.g. `3:42 pm`.
function M.format_time(secs)
  return os.date("%-I:%M %p", secs or os.time()):lower()
end

--- Split a Markdown file's front matter from its body:
--- "---\nname: x\ndescription: y\n---\nbody" -> { name = "x", ... }, "body".
--- Only simple `key: value` lines; quotes around a value are dropped.
function M.front_matter(text)
  local meta = {}
  text = text:gsub("\r\n", "\n")
  if text:sub(1, 4) ~= "---\n" then
    return meta, text
  end
  local close_start, close_end = text:find("\n%-%-%-[ \t]*\n", 4)
  if not close_start then
    close_start, close_end = text:find("\n%-%-%-[ \t]*$", 4)
  end
  if not close_start then
    return meta, text
  end
  for line in text:sub(5, close_start):gmatch("[^\n]+") do
    local k, v = line:match("^([%w_%-]+):%s*(.-)%s*$")
    if k then
      meta[k] = v:match('^"(.*)"$') or v:match("^'(.*)'$") or v
    end
  end
  return meta, text:sub(close_end + 1)
end

return M
