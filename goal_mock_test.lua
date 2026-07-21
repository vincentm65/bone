-- Faithful mock runner for lua/commands/goal.lua's /goal start path.
-- Mocks bone + ctx using REAL filesystem ops so runtime errors surface.

package.path = package.path

local CONFIG_DIR = "/data/data/com.termux/files/home/.bone-rust"
local PROBE_DIR  = CONFIG_DIR .. "/goals"          -- same as real goal_path default

-- ---- bone global mock -------------------------------------------------------
local bone = { agent_depth = 0 }
local hooks = {}
function bone.on(ev, fn) hooks[ev] = hooks[ev] or {}; table.insert(hooks[ev], fn) end
function bone.register_command(name, spec) bone._cmd = spec end
bone.log = { info = function() end, warn = function() end, error = function() end }
bone.api = { submit = function(t) io.stderr:write("[submit] " .. tostring(t) .. "\n") end }
_G.bone = bone

-- ---- ctx mock (real fs ops) -------------------------------------------------
local function real_exists(p)
  local f = io.open(p, "r"); if f then f:close(); return true end
  return false
end

local ctx = { config_dir = CONFIG_DIR, cwd = CONFIG_DIR }
ctx.session = { current = function() return { id = "default", provider = "x", model = "y" } end }
ctx.fs = {
  exists = function(p) return real_exists(p) end,
  is_file = function(p) return real_exists(p) end,
  is_dir  = function(p) return real_exists(p) end,
}
ctx.shell = function(cmd)
  -- actually run it
  local h = io.popen(cmd .. " 2>&1"); local out = h and h:read("*a") or ""; if h then h:close() end
  return { stdout = out, stderr = "", exit_code = 0 }
end
ctx.write_file = function(p, content)
  local f = io.open(p, "w"); if not f then error("cannot write " .. p) end
  f:write(content); f:close(); return true
end
ctx.read_file = function(p)
  local f = io.open(p, "r"); if not f then error("cannot read " .. p) end
  local s = f:read("*a"); f:close(); return s
end
ctx.tools = {
  call = function(name, args)
    -- emulate edit_file mode=rewrite
    if name == "edit_file" and args.mode == "rewrite" then
      local f = io.open(args.path, "w"); if not f then return { ok=false, content="fail" } end
      f:write(args.content); f:close(); return { ok=true, content="ok" }
    end
    return { ok = false, content = "unknown tool " .. name }
  end,
}

-- ---- load & run the real command file --------------------------------------
local ok, err = pcall(dofile, CONFIG_DIR .. "/lua/commands/goal.lua")
if not ok then
  print("LOAD ERROR: " .. tostring(err)); os.exit(1)
end
print("loaded; command registered = " .. tostring(bone._cmd ~= nil))

-- clean any prior default goal file so we exercise the fresh-start branch
os.remove(PROBE_DIR .. "/default.md")

-- invoke the start handler
print("\n=== invoking /goal 'Fix the login bug' ===")
local rok, rerr = pcall(function()
  local res = bone._cmd.handler("Fix the login bug", ctx)
  print("handler returned type: " .. type(res))
  print("handler returned: " .. tostring(res))
end)
if not rok then
  print("\n*** HANDLER ERROR (reproduced) ***")
  print(rerr)
else
  print("\n(no error) file written? " .. tostring(real_exists(PROBE_DIR .. "/default.md")))
end

-- now fire before_turn + turn_end to exercise the loop hooks
if hooks.before_turn then
  print("\n=== firing before_turn ===")
  local bok, berr = pcall(hooks.before_turn[1], {}, ctx)
  if not bok then print("*** before_turn ERROR ***\n" .. berr)
  else print("before_turn ok; returned: " .. tostring(bok)) end
end
if hooks.turn_end then
  print("\n=== firing turn_end (content = working) ===")
  local tok, terr = pcall(hooks.turn_end[1], { ok = true, content = "did some work\nGOAL_STATUS: working" }, ctx)
  if not tok then print("*** turn_end ERROR ***\n" .. terr) else print("turn_end ok") end
end
