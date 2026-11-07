-- The core half of the files plugin: the two things the desktop cannot do
-- by itself — reading a PDF's text, and handing a file to the desktop's
-- own application. The PDF is read by the plugin itself (files.pdf), so
-- no other program is needed; the file is handed over with the desktop's
-- own command, which the core runs without blocking anything else.

local pdf = require("files.pdf")

--- A single quote around a path, for the shell.
local function quoted(path)
  return "'" .. tostring(path):gsub("'", "'\\''") .. "'"
end

--- Run a command: its result, or nil and the reason.
local function run(command, timeout)
  local r = bone.system(command, { timeout = timeout })
  if not r then return nil, "the command did not run" end
  if r.timed_out then return nil, "the command took too long" end
  return r
end

--- The text of a PDF, read here: the file's objects, its streams and the
--- text they hold.
bone.rpc.register("files.pdftext", function(args, ctx)
  local text, err = pdf.text(tostring(args.path or ""))
  if not text then
    return { error = "The PDF could not be read: " .. tostring(err or "it is not a PDF") }
  end
  return { text = text }
end)

--- Open a file with the desktop's own app (the system's default one).
bone.rpc.register("files.open", function(args, ctx)
  local file = io.open(args.path, "rb")
  if not file then
    return { error = "There is no such file: " .. tostring(args.path) }
  end
  file:close()
  -- The app is started in the background and its output is kept away from
  -- ours: it may live much longer than the command that started it.
  local r, err = run(
    "xdg-open " .. quoted(args.path) .. " >/dev/null 2>&1 </dev/null &", 5000)
  if not r then return { error = err } end
  if r.code ~= 0 then
    local why = (r.stderr or ""):gsub("%s+$", "")
    return { error = "Could not open the file: " .. (why ~= "" and why or ("exit code " .. tostring(r.code))) }
  end
  return { opened = true }
end)
