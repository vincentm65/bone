-- templates, core side: a registry of prompt templates (reusable prompts
-- with arguments), expanded in the core so every client gets the same text.
--
--   bone.config.template_dirs = { "~/.bone/prompts" }   -- *.md files
--
-- Other plugins and core.lua can use the API below (bone.template.*).

local front_matter = require("bone.util").front_matter

local function read_file(path)
  local f = io.open(path, "r")
  if not f then
    return nil
  end
  local text = f:read("*a")
  f:close()
  return text
end

local function home(path)
  return (path:gsub("^~", os.getenv("HOME") or "~"))
end

--- Prompt templates: reusable prompts with arguments. None exist unless
--- registered:
---   bone.template.register { name, description, args = { "path" }, body = "Review $1 ..." }
---   bone.template.register { name, body = function(args, ctx) return "..." end }
---   bone.template.load_dir("~/prompts")   every *.md (front matter: name, description, args)
---   bone.template.expand(name, "argument text", ctx) -> text
--- In a string body: $1..$9 (the words of the argument text, "double quotes"
--- keep words together), $@ or $ARGUMENTS (all of it), {{name}} (by args).
--- A function body gets { raw, argv, <name> = ... } and ctx = { session_id, cwd }
--- and may wait (bone.system, bone.http, bone.model).
bone.template = {}
bone._templates = {}

function bone.template.register(spec)
  assert(type(spec) == "table" and type(spec.name) == "string" and spec.name:match("^[%w_%-%.:]+$"),
    "bone.template.register { name = [A-Za-z0-9_-.:]+, body }")
  assert(type(spec.body) == "string" or type(spec.body) == "function", "template " .. spec.name .. " needs a body")
  bone._templates[spec.name] = {
    name = spec.name,
    description = spec.description or "",
    args = spec.args or {},
    body = spec.body,
  }
end

function bone.template.load_dir(dir)
  dir = home(dir):gsub("/$", "")
  local n = 0
  for _, e in ipairs(assert(bone.fs.list(dir))) do
    local base = e.name:match("^(.-)%.md$")
    local text = e.type == "file" and base and read_file(dir .. "/" .. e.name)
    if text then
      local meta, body = front_matter(text)
      local args = {}
      for a in (meta.args or ""):gmatch("[^%s,]+") do
        args[#args + 1] = a
      end
      bone.template.register({ name = meta.name or base, description = meta.description, args = args, body = body })
      n = n + 1
    end
  end
  return n
end

function bone.template.unregister(name)
  bone._templates[name] = nil
end

function bone.template.list()
  return bone.template._all()
end

-- Words of the argument text; "double quotes" keep spaces.
local function split_args(raw)
  local out, cur, quoted, any = {}, {}, false, false
  for c in raw:gmatch(".") do
    if c == '"' then
      quoted, any = not quoted, true
    elseif c:match("%s") and not quoted then
      if any then
        out[#out + 1] = table.concat(cur)
      end
      cur, any = {}, false
    else
      cur[#cur + 1], any = c, true
    end
  end
  if any then
    out[#out + 1] = table.concat(cur)
  end
  return out
end
bone._split_args = split_args

function bone.template.expand(name, raw, ctx)
  local t = bone._templates[name]
  if not t then
    error("no template named " .. tostring(name), 2)
  end
  raw = raw or ""
  local argv = split_args(raw)
  local named = {}
  for i, n in ipairs(t.args) do
    named[n] = argv[i]
  end
  if type(t.body) == "function" then
    local args = { raw = raw, argv = argv }
    for k, v in pairs(named) do
      args[k] = v
    end
    return tostring(t.body(args, ctx or {}))
  end
  local function lit(s)
    return (s:gsub("%%", "%%%%"))
  end
  local text = t.body:gsub("%$ARGUMENTS", lit(raw)):gsub("%$@", lit(raw))
  for i = 9, 1, -1 do
    text = text:gsub("%$" .. i, lit(argv[i] or ""))
  end
  text = text:gsub("{{%s*([%w_%-]+)%s*}}", function(k)
    return named[k] or ""
  end)
  return text
end

function bone.template._all()
  local names = {}
  for name in pairs(bone._templates) do
    names[#names + 1] = name
  end
  table.sort(names)
  local out = {}
  for _, name in ipairs(names) do
    local t = bone._templates[name]
    out[#out + 1] = { name = t.name, description = t.description, args = t.args }
  end
  return out
end



-- The folders core.lua named, once the config is final.
bone.config.template_dirs = bone.config.template_dirs or {}
bone.on_ready(function()
  for _, dir in ipairs(bone.config.template_dirs or {}) do
    local ok, err = pcall(bone.template.load_dir, dir)
    if not ok then
      print("templates plugin: " .. tostring(err))
    end
  end
end)

-- For the TUI half: { { name, description, args } } and the expanded text.
bone.rpc.register("templates.list", function()
  return bone.template._all()
end)
bone.rpc.register("templates.expand", function(args, ctx)
  return bone.template.expand(args.name, args.args or "", ctx)
end)
