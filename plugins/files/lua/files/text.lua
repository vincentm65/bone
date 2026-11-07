-- Reading a file's text, and the lines of it. The editor reads the file
-- itself, so the text it edits is exactly what is on the disk.

local M = {}

--- The most a preview reads; a bigger file is shown in part.
M.PREVIEW_LIMIT = 256 * 1024

--- The text of `path`, or nil and why not.
function M.read(path)
  local file, err = io.open(path, "rb")
  if not file then return nil, tostring(err) end
  local content = file:read("*a")
  file:close()
  if not content then return nil, "could not read " .. tostring(path) end
  return content
end

--- Whether `content` holds bytes that are not text.
function M.is_binary(content)
  return tostring(content or ""):find("%z") ~= nil
end

--- The text of a file to show: at most `M.PREVIEW_LIMIT` of it, and
--- whether it was cut.
function M.for_preview(content)
  content = tostring(content or "")
  if #content <= M.PREVIEW_LIMIT then return content, false end
  return content:sub(1, M.PREVIEW_LIMIT), true
end

--- `content` as its lines, and whether it ended with a newline. The two
--- together are the text again (M.join).
function M.split(content)
  content = tostring(content or ""):gsub("\r\n", "\n")
  if content == "" then return { "" }, false end
  local lines = {}
  for line in (content .. "\n"):gmatch("(.-)\n") do lines[#lines + 1] = line end
  local trailing = false
  if lines[#lines] == "" then
    table.remove(lines)
    trailing = true
  end
  if #lines == 0 then lines = { "" } end
  return lines, trailing
end

--- The lines back as text.
function M.join(lines, trailing)
  local text = table.concat(lines and #lines > 0 and lines or { "" }, "\n")
  if trailing then text = text .. "\n" end
  return text
end

--- Where the character before the cursor at `col` begins (a byte offset).
function M.prev_offset(line, col)
  if col <= 0 then return 0 end
  local i = col
  while i > 1 do
    local byte = line:byte(i)
    if not byte or byte < 0x80 or byte >= 0xC0 then break end
    i = i - 1
  end
  return i - 1
end

--- Where the character at the cursor ends (a byte offset).
function M.next_offset(line, col)
  local i, n = col + 1, #line
  if i > n then return col end
  local j = i + 1
  while j <= n do
    local byte = line:byte(j)
    if not byte or byte < 0x80 or byte >= 0xC0 then break end
    j = j + 1
  end
  return j - 1
end

return M
