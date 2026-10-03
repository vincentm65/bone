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

-- Coroutines the core runs jobs in (hooks, tools, system_prompt). Only
-- these may wait; other code (core.lua while it loads, your own coroutines)
-- runs the blocking versions.
bone._jobs = setmetatable({}, { __mode = "k" })

local function in_job()
  local co = coroutine.running()
  return co ~= nil and bone._jobs[co] == true
end

local function wait(spec)
  local r, err = coroutine.yield({ wait = spec })
  if err then
    error(err, 3)
  end
  return r
end

--- Ask the user something and wait for the answer. Works in hooks, tools
--- and a system_prompt function. `question` is any table; clients receive
--- it in an `ask/requested` event and reply with `ask/respond`. Returns the
--- answer, or nil if the turn was cancelled first.
function bone.ask(question)
  if not in_job() then
    error("bone.ask only works inside hooks, tools and system_prompt", 2)
  end
  return coroutine.yield({ ask = question })
end

--- Run a shell command: { code, stdout, stderr } (or { timed_out = true }).
--- opts: { cwd, stdin, timeout = ms }. In hooks and tools this waits without
--- blocking anything else, and a cancelled turn kills the command (the call
--- returns nil).
function bone.system(cmd, opts)
  opts = opts or {}
  if in_job() then
    return wait({ system = cmd, cwd = opts.cwd, stdin = opts.stdin, timeout = opts.timeout })
  end
  return bone._system_sync(cmd, opts)
end

--- Wait `ms` milliseconds (without blocking in hooks and tools).
function bone.sleep(ms)
  if in_job() then
    return wait({ sleep = ms })
  end
  return bone._sleep_sync(ms)
end

--- An HTTP request: { url, method = "GET", headers = {}, body = string or
--- table (sent as JSON), timeout = ms } -> { status, headers, body }.
--- Only in hooks and tools. Connection errors raise; HTTP errors don't.
function bone.http(req)
  if not in_job() then
    error("bone.http only works inside hooks, tools and system_prompt", 2)
  end
  return wait({ http = req })
end

--- Read a streaming HTTP response one server-sent event at a time (for
--- providers). Same request fields as bone.http. Returns
--- { status, headers } with methods:
---   s:next()   the next event's data (a string), or nil at the end
---   s:events() an iterator over the rest: for data in s:events() do ... end
---   s:text()   the rest of the body (e.g. an error message)
---   s:close()
--- Only in hooks, tools and providers; each read waits without blocking.
function bone.http_stream(req)
  if not in_job() then
    error("bone.http_stream only works inside hooks, tools and providers", 2)
  end
  local r = wait({ http_stream = req })
  local id = r.stream
  local s = { status = r.status, headers = r.headers, _handle = bone._stream_handle(id) }
  function s:next()
    return wait({ stream_next = id })
  end
  function s:events()
    return function()
      return s:next()
    end
  end
  function s:text()
    return wait({ stream_text = id })
  end
  function s:close()
    bone._stream_close(id)
  end
  return s
end

bone.provider = {}
bone._providers = {}

--- Add a model provider. Use it with `type = name` in a providers entry:
---   bone.config.providers.claude = { type = "anthropic", model = "...", api_key = ... }
--- spec.complete(req, emit) runs as a job (bone.http_stream etc. wait):
---   req = { messages, tools = { { name, description, parameters } },
---           options (the providers entry), session_id }
---   messages as in the protocol: { role = "system" | "user", content },
---     { role = "assistant", content, reasoning, tool_calls = { { id, name, arguments (JSON string) } } },
---     { role = "tool", call_id, content, is_error }
---   emit({ text = "..." }) or emit({ reasoning = "..." }) streams output
---   return { content, reasoning, tool_calls = { { id, name, arguments } },
---            usage = { input_tokens, output_tokens } }
function bone.provider.register(name, spec)
  assert(type(name) == "string" and type(spec) == "table" and type(spec.complete) == "function",
    "bone.provider.register(name, { complete = function(req, emit) ... end })")
  bone._providers[name] = spec
end

bone._health = {}

--- Add a check to /health (and the `health/check` method). fn() returns a
--- status ("ok", "warn", "error", or true/false) and a message. It runs as a
--- job, so it may use bone.system and bone.http.
---   bone.health("my server", function()
---     local r = bone.http({ url = "http://localhost:8080/health", timeout = 2000 })
---     return r.status == 200, "status " .. r.status
---   end)
function bone.health(name, fn)
  assert(type(name) == "string" and type(fn) == "function", "bone.health(name, fn)")
  bone._health[#bone._health + 1] = { name = name, fn = fn }
end

--- Run every check: a list of { name, status, message }.
function bone._run_health()
  local out = {}
  for _, c in ipairs(bone._health) do
    local ok, status, message = pcall(c.fn)
    if not ok then
      status, message = "error", "check failed: " .. tostring(status)
    elseif status == true or status == nil then
      status = "ok"
    elseif status == false then
      status = "error"
    elseif status ~= "ok" and status ~= "warn" and status ~= "error" then
      status, message = "warn", "bad status " .. tostring(status) .. ": " .. tostring(message)
    end
    out[#out + 1] = { name = c.name, status = status, message = message ~= nil and tostring(message) or "" }
  end
  return out
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

function bone._provider_entry(name, req, emit)
  local p = bone._providers[name]
  if not p then
    error("no Lua provider named " .. name)
  end
  return p.complete(req, emit)
end

function bone._health_entry()
  return bone._run_health()
end

function bone._system_prompt(ctx)
  local p = bone.config.system_prompt
  if type(p) == "function" then
    return p(ctx)
  end
  return p or ""
end

-- print() writes to <config dir>/core.log.

--- Plugins. bone.plugin.current() is { name, dir, kind } while a plugin's
--- core.lua runs, else nil. bone.state.load(name) / bone.state.save(name,
--- value) keep JSON in ~/.bone/state/core/<name>.json.
bone.plugin = {
  current = function()
    return bone._loading
  end,
}

--- fn() runs once after every core.lua (plugins' and yours) has run, before
--- the core serves anything: the place to read the final bone.config.
bone._ready = {}
function bone.on_ready(fn)
  assert(type(fn) == "function", "bone.on_ready(fn)")
  bone._ready[#bone._ready + 1] = fn
end
