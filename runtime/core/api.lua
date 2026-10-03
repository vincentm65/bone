-- The core Lua API. Loaded before runtime/core/defaults.lua, plugins and
-- ~/.bone/core.lua. The core reads `bone.config` once the files have run.

bone.config = {
  --- name -> { base_url, model, api_key?, reasoning_effort?, stream_usage? }
  --- Any OpenAI-compatible /chat/completions endpoint.
  providers = {},
  --- Which provider to use (a key of `providers`).
  provider = nil,
  --- A string, or function(ctx) -> string with ctx = { cwd, session_id }.
  --- The working directory is appended either way.
  system_prompt = nil,
  --- Where sessions are stored. Defaults to the config directory.
  data_dir = nil,
}

bone.tool = {}
bone._tools = {}

--- Add a tool the model can call.
---   name, description: strings
---   parameters: JSON Schema table for the arguments object
---   run = function(args, ctx) -> string | table   (ctx = { cwd, session_id })
--- Other fields are kept for plugins to read (the approve plugin looks at
--- `needs_approval`). Errors (error() or returning nil, "message") are
--- reported to the model.
function bone.tool.register(spec)
  assert(type(spec) == "table", "bone.tool.register expects a table")
  assert(type(spec.name) == "string" and spec.name:match("^[%w_%-]+$"), "tool name must be [A-Za-z0-9_-]+")
  assert(type(spec.run) == "function", "tool " .. spec.name .. " needs a run function")
  bone._tools[spec.name] = spec
end

bone._hooks = {}

--- Run fn(ev) at a point in the core. Built-in points:
---   "turn_start"  { session_id, cwd, text }           before the user's message is saved
---   "request"     { session_id, messages, tools }     before each call to the model
---   "message"     { session_id, content, reasoning, tool_calls }
---                                                     the model's reply, before it is saved
---   "tool_call"   { session_id, cwd, id, name, arguments }
---   "tool_result" { session_id, id, name, arguments, output, is_error }
---   "turn_end"    { session_id, turn_id, outcome }    after a turn (changes ignored)
--- A hook returns nil (no change), a table of fields to change in `ev`
--- (later hooks see the changes), or { deny = "why" } to stop that step: the
--- turn, the model call, or the tool call (the model sees "why"). Hooks run
--- in registration order (plugins first, then core.lua) and may wait on the
--- user with bone.ask. Any other name is a custom point for bone.run_hooks.
function bone.hook(name, fn)
  assert(type(name) == "string" and name ~= "", "hook name must be a string")
  assert(type(fn) == "function", "hook needs a function")
  local list = bone._hooks[name] or {}
  list[#list + 1] = fn
  bone._hooks[name] = list
end

--- Run the hooks for `name` on `ev`. Returns ev (as changed) and, if a hook
--- refused, the reason.
function bone.run_hooks(name, ev)
  for _, f in ipairs(bone._hooks[name] or {}) do
    local ok, r = pcall(f, ev)
    if not ok then
      return ev, name .. " hook failed: " .. tostring(r)
    end
    if type(r) == "table" then
      if r.deny then
        return ev, r.deny == true and ("Denied by a " .. name .. " hook.") or tostring(r.deny)
      end
      for k, v in pairs(r) do
        ev[k] = v
      end
    end
  end
  return ev, nil
end

--- Ask the user something and wait for the answer. Works in hooks, tools
--- and a system_prompt function. `question` is any table; clients receive
--- it in an `ask/requested` event and reply with `ask/respond`. Returns the
--- answer, or nil if the turn was cancelled first.
function bone.ask(question)
  if not coroutine.running() then
    error("bone.ask only works inside hooks, tools and system_prompt", 2)
  end
  return coroutine.yield({ ask = question })
end

-- Entry points the core calls (each in its own coroutine) --------------------

function bone._run_tool(name, args, ctx)
  local spec = bone._tools[name]
  if not spec then
    return nil, "no Lua tool named " .. name
  end
  return spec.run(args, ctx)
end

function bone._hooks_entry(name, ev)
  local out, deny = bone.run_hooks(name, ev)
  return { event = out, deny = deny }
end

function bone._system_prompt(ctx)
  local p = bone.config.system_prompt
  if type(p) == "function" then
    return p(ctx)
  end
  return p or ""
end

-- bone.system(cmd, { cwd = dir }) -> { code, stdout, stderr } is provided by
-- the core. print() writes to <config dir>/core.log.
