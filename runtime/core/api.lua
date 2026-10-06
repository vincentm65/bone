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
---   run = function(args, ctx) -> string | table   (ctx = { cwd, session_id, call_id })
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
---   "system"      { session_id, cwd, prompt }         once per turn: the system prompt
---   "context"     { session_id, messages }            before each model call: rewrite history for it
---   "request"     { session_id, messages, tools }     before each call to the model
---   "request_error" { session_id, error, attempt, model }
---                                                     a model call failed before any output;
---                                                     return { retry = ms } to try again
---   "stream"      { session_id, turn_id, text, reasoning }
---                                                     output so far, in batches (results ignored)
---   "session_start" { session_id, cwd, new }          first use of a session (results ignored)
---   "message"     { session_id, content, reasoning, tool_calls, usage }
---                                                     the model's reply, before it is saved
---   "tool_call"   { session_id, cwd, id, name, arguments }
---   "tool_result" { session_id, id, name, arguments, output, is_error }
---   "turn_end"    { session_id, turn_id, outcome }    after a turn (changes ignored)
--- A hook returns nil (no change), a table of fields to change in `ev`
--- (later hooks see the changes), or { deny = "why" } to stop that step: the
--- turn, the model call, or the tool call (the model sees "why"). Hooks run
--- by priority (opts.priority, higher first, default 0), then in
--- registration order (plugins first, then core.lua), and may wait on the
--- user with bone.ask. Any other name is a custom point for bone.run_hooks.
bone._hook_seq = 0
function bone.hook(name, fn, opts)
  assert(type(name) == "string" and name ~= "", "hook name must be a string")
  assert(type(fn) == "function", "hook needs a function")
  local priority = opts and opts.priority or 0
  assert(type(priority) == "number", "hook priority must be a number")
  local list = bone._hooks[name] or {}
  bone._hook_seq = bone._hook_seq + 1
  list[#list + 1] = { fn = fn, priority = priority, seq = bone._hook_seq }
  table.sort(list, function(a, b)
    if a.priority ~= b.priority then
      return a.priority > b.priority
    end
    return a.seq < b.seq
  end)
  bone._hooks[name] = list
  if bone._hook_added then
    bone._hook_added(name)
  end
end

--- Run the hooks for `name` on `ev`. Returns ev (as changed) and, if a hook
--- refused, the reason.
function bone.run_hooks(name, ev)
  for _, h in ipairs(bone._hooks[name] or {}) do
    local ok, r = pcall(h.fn, ev)
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

--- The saved settings (settings.json in the config dir), read-only here:
--- bone.settings.get("web_search.num_results") or nil, bone.settings.all().
--- The TUI changes them (bone.settings.set there, or settings/set).
bone.settings = {
  all = function()
    return bone._settings()
  end,
  get = function(path)
    local at = bone._settings()
    for key in tostring(path):gmatch("[^.]+") do
      if type(at) ~= "table" then
        return nil
      end
      at = at[key]
    end
    return at
  end,
}

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

--- A session's transcript, for hooks and tools (they wait without blocking):
---   bone.session.messages(id)          -> the messages, as providers see them
---   bone.session.append(id, message)   add a message the model will see next
---   bone.session.compact(id, messages) replace the transcript (the file keeps
---                                      the old one behind a checkpoint)
---   bone.session.create(opts)          a new session: opts = { cwd?, title?,
---                                      owner? = { session_id, call_id?, name? } }
---                                      (a subagent's; cwd defaults to the
---                                      owner's). Returns its info.
---   bone.session.run(id, text)         start a turn on an idle session and wait
---                                      for it: { session_id, turn_id, text,
---                                      outcome = { status, message? } }, text
---                                      being the final answer. Cancelling the
---                                      calling turn cancels this one.
--- During a turn the last two only work between model calls (system,
--- context, request and request_error hooks, and turn_start) or after it
--- (turn_end); clients get a session/updated event.
bone.session = {}
local function session_op(op, id, extra)
  if not in_job() then
    error("bone.session only works inside hooks, tools and providers", 3)
  end
  local spec = extra or {}
  spec.op, spec.id = op, id
  return wait({ session = spec })
end
function bone.session.messages(id)
  return session_op("messages", id)
end
function bone.session.append(id, message)
  return session_op("append", id, { message = message })
end
function bone.session.compact(id, messages)
  return session_op("compact", id, { messages = messages })
end
function bone.session.create(opts)
  opts = opts or {}
  assert(type(opts) == "table", "bone.session.create(opts)")
  return session_op("create", nil, { cwd = opts.cwd, title = opts.title, owner = opts.owner })
end
function bone.session.run(id, text)
  if not in_job() then
    error("bone.session.run only works inside hooks, tools and providers", 2)
  end
  assert(type(id) == "string" and type(text) == "string", "bone.session.run(id, text)")
  return wait({ session_run = { id = id, text = text } })
end

--- A session's message queue (as queue/add and friends; clients see the
--- changes). In hooks, tools and providers:
---   bone.queue.add(id, text, mode)   mode "steer" (default) or "next";
---                                     idle sessions start a turn: { turn_id }, else { id }
---   bone.queue.list(id)              { { id, text, mode, created_at } }
---   bone.queue.remove(id, queue_id), bone.queue.clear(id)
bone.queue = {
  add = function(id, text, mode)
    local ev, why = bone.run_hooks("queue_add", { session_id = id, text = text, mode = mode or "steer" })
    if why then
      error(why, 2)
    end
    return session_op("queue_add", id, { text = ev.text, mode = ev.mode })
  end,
  list = function(id)
    return session_op("queue_list", id)
  end,
  remove = function(id, queue_id)
    return session_op("queue_remove", id, { queue_id = queue_id })
  end,
  clear = function(id)
    return session_op("queue_clear", id)
  end,
}

--- Call a model from hooks, tools and providers (they wait without
--- blocking; a cancelled turn stops the call). req:
---   provider  a key of bone.config.providers (default: the one turns use)
---   messages  { { role, content }, ... }, or prompt = "..." (and system = "...")
---   tools     { { name, description, parameters } } the model may ask for
---             (nothing runs them; you get tool_calls back)
---   options   overrides: model, reasoning_effort, a Lua provider's options
---   on_delta  function({ text = ... } or { reasoning = ... }) as it streams
--- Returns { content, reasoning, tool_calls, usage }, or nil and an error.
--- bone.model.stream(req) returns a handle instead: h:next() (the next
--- delta, nil at the end), h:events(), h:result(), h:close().
--- bone.model.list() -> { { name, model, type, current } }.
bone.model = {}
bone._depth = setmetatable({}, { __mode = "k" })

local ModelStream = {}
ModelStream.__index = ModelStream
function ModelStream:next()
  return wait({ model_next = self.id })
end
function ModelStream:events()
  return function()
    return self:next()
  end
end
function ModelStream:result()
  local ok, r = pcall(wait, { model_result = self.id })
  if not ok then
    return nil, tostring(r)
  end
  return r
end
function ModelStream:close()
  wait({ model_close = self.id })
end

function bone.model.stream(req)
  if not in_job() then
    error("bone.model only works inside hooks, tools and providers", 2)
  end
  assert(type(req) == "table", "bone.model.stream(req)")
  local messages = req.messages
  if not messages then
    assert(type(req.prompt) == "string", "a model call needs messages or a prompt")
    messages = {}
    if req.system then
      messages[1] = { role = "system", content = req.system }
    end
    messages[#messages + 1] = { role = "user", content = req.prompt }
  end
  local r = wait({ model_open = {
    provider = req.provider,
    messages = messages,
    tools = req.tools,
    options = req.options,
    depth = (bone._depth[coroutine.running()] or 0) + 1,
    -- Made by a provider: its usage is counted in the provider's result.
    in_provider = bone._depth[coroutine.running()] ~= nil,
  } })
  return setmetatable({ id = r.stream }, ModelStream)
end

function bone.model.complete(req)
  local s = bone.model.stream(req)
  for d in s:events() do
    if req.on_delta then
      req.on_delta(d)
    end
  end
  return s:result()
end

function bone.model.list()
  return bone._models()
end

--- MCP servers. None run unless you add them:
---   bone.mcp.add("github", { command = "github-mcp", args = { "stdio" }, env = { TOKEN = ... } })
---   bone.mcp.add("docs", { url = "https://example.com/mcp", headers = { authorization = ... } },
---                { tools = { allow = { "search" } }, lazy = true, timeout = 60000 })
---   bone.mcp.load(path)        add every server of an mcpServers JSON file
---   bone.mcp.remove(name)
--- Their tools reach the model as <server>_<tool>. In hooks and tools:
---   bone.mcp.call(server, tool, args) -> { text, is_error }
---   bone.mcp.list() -> { { name, state, error, tools } }
bone.mcp = {}
bone._mcp = {}

function bone.mcp.add(name, spec, opts)
  assert(type(name) == "string" and name:match("^[%w_%-]+$"), "MCP server names are [A-Za-z0-9_-]+")
  assert(type(spec) == "table" and (spec.command or spec.url), "bone.mcp.add(name, { command, args } or { url })")
  opts = opts or {}
  local tools = opts.tools or {}
  bone._mcp[name] = {
    command = spec.command,
    args = spec.args,
    env = spec.env,
    cwd = spec.cwd,
    url = spec.url,
    headers = spec.headers,
    allow = tools.allow,
    deny = tools.deny,
    lazy = opts.lazy,
    timeout = opts.timeout,
  }
end

function bone.mcp.remove(name)
  bone._mcp[name] = nil
end

--- Add the servers of a JSON file in the common format:
--- { "mcpServers": { "name": { "command", "args", "env" } | { "url", "headers" } } }
function bone.mcp.load(path, opts)
  local f = assert(io.open((path:gsub("^~", os.getenv("HOME") or "~")), "r"))
  local data = bone.json.decode(f:read("*a"))
  f:close()
  local servers = data.mcpServers or data.servers or {}
  for name, spec in pairs(servers) do
    if not spec.disabled then
      bone.mcp.add(name, spec, opts)
    end
  end
end

function bone.mcp.list()
  return bone._mcp_list()
end

function bone.mcp.call(server, tool, args)
  if not in_job() then
    error("bone.mcp.call only works inside hooks, tools and providers", 2)
  end
  return wait({ mcp_call = { server = server, tool = tool, arguments = args } })
end

--- Functions clients can call (the lua/call method), for plugins whose TUI
--- half needs the core to do something:
---   bone.rpc.register("myplugin.do", function(args, ctx) return { ok = true } end)
--- fn runs as a job (it may wait: bone.system, bone.model, bone.session ...),
--- gets the call's args and ctx = { session_id, cwd }, and returns the
--- result (any JSON value). Names are free-form; prefix them with the
--- plugin's name.
bone.rpc = {}
bone._rpc = {}

function bone.rpc.register(name, fn)
  assert(type(name) == "string" and name ~= "" and type(fn) == "function", "bone.rpc.register(name, fn)")
  bone._rpc[name] = fn
end

function bone.rpc.unregister(name)
  bone._rpc[name] = nil
end

function bone._rpc_entry(name, args, ctx)
  local fn = bone._rpc[name]
  if not fn then
    error("no function " .. tostring(name) .. " registered with bone.rpc", 0)
  end
  return fn(args, ctx)
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
  -- Model calls this provider makes are nested one deeper.
  bone._depth[coroutine.running()] = req.depth or 0
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

--- fn() runs when this configuration is replaced (core/reload, a core
--- plugin loaded, unloaded or reloaded). Lua values do not survive a reload:
--- keep what must last with bone.state. Errors are reported, not fatal.
bone._shutdown = {}
function bone.on_shutdown(fn)
  assert(type(fn) == "function", "bone.on_shutdown(fn)")
  bone._shutdown[#bone._shutdown + 1] = fn
  bone._hook_added("_shutdown")
end

-- Called by the core before it switches to a new configuration. Returns
-- the errors.
function bone._shutdown_entry()
  local errors = {}
  for _, fn in ipairs(bone._shutdown) do
    local ok, e = pcall(fn)
    if not ok then
      errors[#errors + 1] = "on_shutdown: " .. tostring(e)
    end
  end
  return errors
end

--- fn() runs once after every core.lua (plugins' and yours) has run, before
--- the core serves anything: the place to read the final bone.config.
bone._ready = {}
function bone.on_ready(fn)
  assert(type(fn) == "function", "bone.on_ready(fn)")
  bone._ready[#bone._ready + 1] = fn
end
