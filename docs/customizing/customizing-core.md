# Customizing the core: tools, hooks, providers, prompts

Where: `~/.bone/core.lua` (global) or a plugin's `core.lua`. The core is the
server: it owns sessions, turns, providers, tools and the protocol. Lua has
full trust and tool calls run without asking (the `approve` plugin asks
first). The core reloads when `core.lua` changes, if it runs in the same
process as the TUI.

Reference: `docs/lua.md` "Core (`core.lua`)". Source:
`crates/bone-core/src/` (`agent.rs` the turn loop, `tools/` the built-in
tools, `scripting.rs` the Lua bridge). Built-in tools the model already has:
`read_file`, `write_file`, `edit_file`, `shell`.

## Providers and the system prompt

```lua
bone.config.providers.qwen = {
  base_url = "http://localhost:8081/v1",  -- any OpenAI-compatible /chat/completions
  model = "Qwen3.8-27B-exl3-3.8bpw",
  api_key = os.getenv("SOME_KEY"),        -- optional
  reasoning_effort = "medium",            -- optional
  stream_usage = true,                    -- send stream_options.include_usage
}
bone.config.provider = "qwen"      -- which entry to use (optional if there is one)
bone.config.system_prompt = "..."  -- or function(ctx) return "..." end; ctx = { cwd, session_id }
```

`bone.config.system_prompt` replaces the built-in system prompt (the working
directory is always appended after it). `BONE_SYSTEM_PROMPT` overrides it for
one run. Settings and the `system` hook can change it per turn.

## Tools

```lua
bone.tool.register {
  name = "git_log",
  description = "Show recent commits.",
  parameters = { type = "object", properties = { n = { type = "integer" } } },  -- JSON Schema
  needs_approval = false,   -- read by the approve plugin (default: ask)
  parallel = true,          -- only reads: may run alongside other such calls (default false)
  run = function(args, ctx)  -- ctx = { cwd, session_id, call_id }
    local r = bone.system("git log --oneline -n " .. (args.n or 10), { cwd = ctx.cwd })
    if r.code ~= 0 then return nil, r.stderr end   -- an error result
    return r.stdout                                -- or a table (sent as JSON)
  end,
}
```

A Lua tool with the same name as a built-in (`read_file`, `write_file`,
`edit_file`, `shell`) replaces it. `error()` inside `run` becomes an error
result for the model. Consecutive read-only calls (`parallel = true`, or
`readOnlyHint` MCP tools) run at the same time, up to 8; anything else runs in
order.

Inside `run` (and hooks) you can wait without blocking anything else:
`bone.system(cmd, { cwd, stdin, timeout })` → `{ code, stdout, stderr }`,
`bone.sleep(ms)`, `bone.http({ url, method, headers, body, timeout })` →
`{ status, headers, body }`, and `bone.ask(question)` to pause until a user
answers (clients get an `ask/requested` event; a TUI plugin shows the
question, usually with a popup).

## Hooks

A hook runs at a point in the core with an event table. It returns `nil` (no
change), a table of fields to change, or `{ deny = "why" }` to stop that step.
`bone.hook(name, fn, { priority = 10 })`, higher priority first. A hook that
errors counts as a refusal.

| Point | Event | `deny` means |
|---|---|---|
| `turn_start` | `{ session_id, cwd, text }`, before the user's message is saved | the turn fails with "why" |
| `system` | `{ session_id, cwd, prompt }`, once per turn | the turn fails |
| `context` | `{ session_id, messages }`, before each model call | the turn fails |
| `request` | `{ session_id, messages, tools }`, before each call, after `context` | the turn fails |
| `request_error` | `{ session_id, error, attempt, model }`, a call failed before output | the turn fails |
| `stream` | `{ session_id, turn_id, text, reasoning }`, as output streams | (ignored) |
| `session_start` | `{ session_id, cwd, new }` | (ignored) |
| `queue_add` | `{ session_id, text, mode }` | it is refused |
| `message` | `{ session_id, content, reasoning, tool_calls, usage }`, the reply before it is saved | the turn fails |
| `tool_call` | `{ session_id, cwd, id, name, arguments }` | the call is refused; the model sees "why" |
| `tool_result` | `{ session_id, id, name, arguments, output, is_error }` | the model sees "why" as an error |
| `turn_end` | `{ session_id, turn_id, outcome }` | (changes ignored) |

`request` and `request_error` may return `{ provider = "name" }` to send the
call to another `bone.config.providers` entry; `request_error` may return
`{ retry = ms }` (at most 5 retries per call).

```lua
bone.hook("tool_call", function(ev)
  if ev.name == "shell" and ev.arguments.command:match("rm %-rf /") then
    return { deny = "not allowed" }
  end
end)
bone.hook("request", function(ev)   -- add context to every request
  table.insert(ev.messages, 2, { role = "system", content = "Today is " .. os.date("%A") })
  return { messages = ev.messages }
end)
bone.hook("tool_result", function(ev)   -- trim big outputs
  if #ev.output > 20000 then return { output = ev.output:sub(1, 20000) .. "\n[cut]" } end
end)
```

Any other name is a custom point: `bone.hook("my_point", fn)` and
`local ev, denied = bone.run_hooks("my_point", ev)`.

## Sessions

Hooks and tools can read and change a session's stored transcript (they wait
without blocking):

```lua
bone.session.messages(id)                 -- the messages, as providers see them
bone.session.append(id, { role = "user", content = "Remember: be brief." })
bone.session.compact(id, { { role = "user", content = "Summary of earlier work: ..." } })
```

The message queue is open to Lua too: `bone.queue.add(id, text, mode)`
(`"steer"` or `"next"`; an idle session starts a turn), `bone.queue.list(id)`,
`bone.queue.remove(id, queue_id)`, `bone.queue.clear(id)`.

Sub-agent sessions (this is how a `subagent` tool works):

```lua
local info = bone.session.create({
  title = "find callers",
  owner = { session_id = ctx.session_id, call_id = ctx.call_id, name = "reviewer" },
})
local r, err = bone.session.run(info.session_id, "Find every caller of panel_at")
-- r = { session_id, turn_id, text (the final answer), outcome = { status, message? } }
```

`run` starts a turn on an idle session and waits for it without blocking
anything else, so several can run at once (a tool registered with
`parallel = true`). Cancelling the calling turn cancels this one. The TUI
lists a sub-agent above the prompt and opens it on a click.

## Custom providers

Any API can be a model provider:

```lua
bone.provider.register("myapi", {
  complete = function(req, emit)
    -- req = { messages, tools = { { name, description, parameters } }, options, session_id }
    local s = bone.http_stream({ url = req.options.base_url .. "/chat", method = "POST", body = { ... } })
    if s.status ~= 200 then error("HTTP " .. s.status .. ": " .. s:text()) end
    local text = ""
    for data in s:events() do            -- each server-sent event's data, as it arrives
      local chunk = bone.json.decode(data).text
      text = text .. chunk
      emit({ text = chunk })             -- or emit({ reasoning = ... })
    end
    return { content = text, tool_calls = {}, usage = { input_tokens = 0, output_tokens = 0 } }
  end,
})
bone.config.providers.mine = { type = "myapi", model = "m", base_url = "https://example.com" }
```

The TUI can also call a model outside any session: `bone.model.complete(req,
on_delta, on_done)` → handle (see the popups guide).

## Calling the model, asking, MCP

- `bone.model.complete(req, on_delta, on_done)` and `bone.model.list(cb)` in
  the core too (same as the TUI's).
- `bone.ask(question)` pauses until a user answers (or `nil` if the turn is
  cancelled first).
- MCP servers: `bone.mcp.add(name, { command, args, env })` (or `{ url,
  headers }`); `bone.mcp.load(path)` adds every server of an mcpServers JSON
  file; `bone.mcp.remove(name)`. Their tools reach the model, and a server's
  `readOnlyHint` marks its tools parallel-safe.

## Environment overrides

`BONE_BASE_URL`, `BONE_MODEL`, `BONE_API_KEY`, `BONE_REASONING_EFFORT`,
`BONE_SYSTEM_PROMPT`, `BONE_DATA_DIR` override `core.lua` for one run;
`BONE_APPROVAL=auto` turns the approve plugin off.