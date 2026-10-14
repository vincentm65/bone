-- On start, in the background: is a newer bone out, or a catalog plugin
-- update? An npm install (BONE_INSTALL_KIND=npm) can update in place; a
-- build from a git checkout is told how far behind origin/main it is.
-- Plugins that need a newer bone wait until bone is updated.
-- `update.check = false` in settings turns this off.

local catalog = require("bone.catalog")

local M = {}

local PACKAGE = "bone-agent"

-- cb(the newer version on npm, or nil)
local function npm_latest(cb)
  local url = "https://registry.npmjs.org/" .. PACKAGE .. "/latest"
  bone.http({ url = url, timeout = 10000 }, function(res)
    local ok, v = pcall(bone.json.decode, res and res.status == 200 and res.body or "")
    local latest = ok and type(v) == "table" and v.version or nil
    cb(latest and not catalog.at_least(bone.version, latest) and latest or nil)
  end)
end

-- cb(how many commits this build is behind origin/main, or nil)
local function behind(cb)
  local commit = (bone.build.commit or ""):gsub("%-dirty$", "")
  if not commit:match("^%x+$") or bone.build.source == "" then
    return cb(nil)
  end
  local cmd = "git fetch --quiet origin && git rev-list --count " .. commit .. "..origin/main"
  bone.system(cmd, { cwd = bone.build.source, timeout = 30000 }, function(r)
    cb(r and r.code == 0 and tonumber(r.stdout) or nil)
  end)
end

-- Install the updates: bone first, then plugins; then ask for a restart.
local function apply(latest, plugins)
  local function plugin(i)
    local e = plugins[i]
    if not e then
      return bone.notify(latest and ("bone " .. latest .. " is installed; restart bone to use it")
        or "plugins updated")
    end
    catalog.install(e, function(ok, err)
      if not ok or err then
        bone.notify(catalog.label(e) .. ": " .. tostring(err), "error")
      end
      plugin(i + 1)
    end)
  end
  if not latest then
    return plugin(1)
  end
  if package.config:sub(1, 1) == "\\" then
    bone.notify("Windows keeps the running bone locked: quit, then run `bone update`")
    return plugin(1)
  end
  bone.notify("updating bone to " .. latest .. "...")
  bone.system("npm install -g " .. PACKAGE .. "@" .. latest, { timeout = 300000 }, function(r)
    if not (r and r.code == 0) then
      local why = r and (r.stderr ~= "" and r.stderr or r.stdout) or "npm did not run"
      return bone.notify("bone update failed (try `bone update`): " .. why, "error")
    end
    plugin(1)
  end)
end

local function ask(latest, plugins)
  local names = {}
  for _, e in ipairs(plugins) do
    names[#names + 1] = catalog.label(e) .. " " .. tostring(e.version)
  end
  local what = latest and ("bone " .. bone.version .. " → " .. latest) or ""
  if #names > 0 then
    what = what .. (latest and "; " or "") .. "plugins: " .. table.concat(names, ", ")
  end
  if bone.settings.get("update.skip") == what then
    return
  end
  bone.ui.select({ "Update now", "Not now", "Skip this version" }, {
    prompt = "Update available: " .. what,
    footer = "enter pick · esc not now",
    height = 8,
    on_choice = function(choice)
      if choice == "Update now" then
        apply(latest, plugins)
      elseif choice == "Skip this version" then
        bone.settings.set("update.skip", what)
      end
    end,
  })
end

function M.check()
  if bone.settings.get("update.check") == false then
    return
  end
  catalog.index(function(entries)
    local plugins = {}
    for _, e in ipairs(entries or {}) do
      if e.state == "update" and e.kind ~= "tui" and catalog.at_least(bone.version, e.min_bone_version) then
        plugins[#plugins + 1] = e
      end
    end
    if os.getenv("BONE_INSTALL_KIND") == "npm" then
      return npm_latest(function(latest)
        if latest or #plugins > 0 then
          ask(latest, plugins)
        end
      end)
    end
    behind(function(n)
      if n and n > 0 then
        bone.notify(("bone is %d commit%s behind origin/main: git pull && scripts/install.sh in %s")
          :format(n, n == 1 and "" or "s", bone.build.source))
      end
      if #plugins > 0 then
        ask(nil, plugins)
      end
    end)
  end)
end

return M
