-- skills, core side: a registry of skills (instructions for particular
-- kinds of work) that the model loads when a task calls for them. While a
-- session has any, the system prompt lists them and a `skill` tool returns
-- one's full text and its files. Nothing happens until some are registered.
--
--   bone.config.skill_dirs = { "~/.bone/skills" }   -- each <dir>/<name>/SKILL.md
--
-- Other plugins and core.lua can use the API below (bone.skill.*).

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

--- Skills: instructions for particular kinds of work that the model loads
--- when a task calls for them. None exist unless registered:
---   bone.skill.register { name, description, content = "..." }
---   bone.skill.register { name, description?, path = "dir/with/SKILL.md" or "file.md",
---                         enabled = function(ctx) return ... end }  -- ctx = { session_id, cwd }
---   bone.skill.load_dir("~/skills")   every <dir>/<name>/SKILL.md (front matter: name, description)
---   bone.skill.unregister(name), bone.skill.list()
--- While a session has a skill, the system prompt lists the skills (name and
--- description) and a `skill` tool returns one's full text and its files.
--- bone.config.skills = { prompt = false } or { tool = false } turns either off.
bone.skill = {}
bone._skills = {}
bone.config.skills = { prompt = true, tool = true }

local function skill_enabled(s, ctx)
  if s.enabled == nil then
    return true
  end
  local ok, yes = pcall(s.enabled, ctx or {})
  return ok and yes and true or false
end

local function enabled_skills(ctx)
  local out = {}
  for _, s in pairs(bone._skills) do
    if skill_enabled(s, ctx) then
      out[#out + 1] = s
    end
  end
  table.sort(out, function(a, b)
    return a.name < b.name
  end)
  return out
end

-- The full text of a skill, and the files that come with it.
local function skill_text(s)
  local body = s.content
  if not body then
    local text = read_file(s.path)
    if not text then
      return nil, "cannot read " .. s.path
    end
    local _, rest = front_matter(text)
    body = rest
  end
  local out = { "# Skill: " .. s.name, "", body }
  if s.dir then
    local files = {}
    for _, e in ipairs(bone.fs.list(s.dir) or {}) do
      if e.name ~= "SKILL.md" and e.name:sub(1, 1) ~= "." then
        files[#files + 1] = s.dir .. "/" .. e.name .. (e.type == "dir" and "/" or "")
      end
    end
    if #files > 0 then
      out[#out + 1] = ""
      out[#out + 1] = "Files that come with this skill (read them when the steps above need them):"
      for _, f in ipairs(files) do
        out[#out + 1] = "- " .. f
      end
    end
  end
  return table.concat(out, "\n")
end

local skill_hooks = false
local function ensure_skill_hooks()
  if skill_hooks then
    return
  end
  skill_hooks = true
  -- First among system hooks, so later ones see (and may change) the list.
  bone.hook("system", function(ev)
    local cfg = bone.config.skills or {}
    if cfg.prompt == false then
      return
    end
    local list = enabled_skills(ev)
    if #list == 0 then
      return
    end
    local lines = {
      ev.prompt,
      "",
      "## Skills",
      "",
      "Skills are instructions for particular kinds of work. When a task matches one, "
        .. "call the `skill` tool with its name before starting, then follow it.",
      "",
    }
    for _, sk in ipairs(list) do
      lines[#lines + 1] = "- " .. sk.name .. ": " .. sk.description
    end
    return { prompt = table.concat(lines, "\n") }
  end, { priority = 1000 })
  bone.on_ready(function()
    local cfg = bone.config.skills or {}
    if cfg.tool == false or next(bone._skills) == nil or bone._tools.skill then
      return
    end
    bone.tool.register({
      name = "skill",
      description = "Load the full instructions of one of the skills listed in the system prompt.",
      parameters = {
        type = "object",
        properties = { name = { type = "string", description = "The skill's name" } },
        required = { "name" },
      },
      needs_approval = false,
      run = function(args, ctx)
        local sk = bone._skills[args.name or ""]
        if not sk or not skill_enabled(sk, ctx) then
          return nil, "no skill named " .. tostring(args.name)
        end
        return skill_text(sk)
      end,
    })
  end)
end

function bone.skill.register(spec)
  assert(type(spec) == "table" and type(spec.name) == "string" and spec.name:match("^[%w_%-%.]+$"),
    "bone.skill.register { name = [A-Za-z0-9_-.]+, description, content or path }")
  assert(spec.content or spec.path, "skill " .. spec.name .. " needs content or a path")
  local sk = { name = spec.name, description = spec.description, content = spec.content, enabled = spec.enabled }
  if spec.path then
    local path = home(spec.path):gsub("/$", "")
    if path:match("%.md$") then
      sk.path, sk.dir = path, nil
    else
      sk.path, sk.dir = path .. "/SKILL.md", path
    end
    if not sk.description then
      local meta = front_matter(read_file(sk.path) or "")
      sk.description = meta.description
    end
  end
  sk.description = sk.description or ""
  bone._skills[sk.name] = sk
  ensure_skill_hooks()
end

--- Register every <dir>/<name>/SKILL.md. Returns how many.
function bone.skill.load_dir(dir)
  dir = home(dir):gsub("/$", "")
  local n = 0
  for _, e in ipairs(assert(bone.fs.list(dir))) do
    local file = dir .. "/" .. e.name .. "/SKILL.md"
    local text = e.type == "dir" and read_file(file)
    if text then
      local meta = front_matter(text)
      bone.skill.register({
        name = meta.name or e.name,
        description = meta.description,
        path = dir .. "/" .. e.name,
      })
      n = n + 1
    end
  end
  return n
end

function bone.skill.unregister(name)
  bone._skills[name] = nil
end

function bone.skill.list()
  local out = {}
  for _, sk in ipairs(enabled_skills(nil)) do
    out[#out + 1] = { name = sk.name, description = sk.description, path = sk.path }
  end
  return out
end

function bone.skill._all()
  local out = {}
  local names = {}
  for name in pairs(bone._skills) do
    names[#names + 1] = name
  end
  table.sort(names)
  for _, name in ipairs(names) do
    local sk = bone._skills[name]
    out[#out + 1] = { name = sk.name, description = sk.description, path = sk.path }
  end
  return out
end


-- The folders core.lua named, once the config is final.
bone.config.skill_dirs = bone.config.skill_dirs or {}
bone.on_ready(function()
  for _, dir in ipairs(bone.config.skill_dirs or {}) do
    local ok, err = pcall(bone.skill.load_dir, dir)
    if not ok then
      print("skills plugin: " .. tostring(err))
    end
  end
end)

-- For the TUI half.
bone.rpc.register("skills.list", function()
  return bone.skill._all()
end)
