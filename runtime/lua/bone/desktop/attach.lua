-- Adding files to the message: the composer's "+" button and a bare
-- /attach open the system's file dialog. Images are uploaded as
-- attachments; any other file's path goes into the message, for the agent
-- to read.
local M = {}

M.images = { png = true, jpg = true, jpeg = true, webp = true }

local function is_image(path)
  local ext = path:match("%.(%w+)$")
  return ext ~= nil and M.images[ext:lower()] == true
end

--- Add `paths` to the message.
function M.add(paths)
  local named = {}
  for _, path in ipairs(paths) do
    if is_image(path) then
      bone.prompt.attach(path)
    else
      local session = bone.chat.session()
      local prefix = ((session and session.cwd) or ""):gsub("/$", "") .. "/"
      if #prefix > 1 and path:sub(1, #prefix) == prefix then path = path:sub(#prefix + 1) end
      named[#named + 1] = path:find("%s") and ('"' .. path .. '"') or path
    end
  end
  if #named > 0 then
    local draft = bone.prompt.get()
    local sep = (draft == "" or draft:match("%s$")) and "" or " "
    bone.prompt.set(draft .. sep .. table.concat(named, " ") .. " ")
  end
end

--- The file dialog; what is picked is added.
function M.pick()
  bone.gui.open_file({ title = "Attach files", multiple = true }, function(paths)
    if type(paths) == "string" then paths = { paths } end
    if type(paths) == "table" and #paths > 0 then M.add(paths) end
  end)
end

return M
