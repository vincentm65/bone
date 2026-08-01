# Agents and Delegation

Bone has one main agent per active conversation and may run delegated agents for
bounded independent work. Delegation is part of the core runtime: the daemon
owns the child execution, approval mode, cancellation, job record, and result.

## Main and delegated agents

The main agent runs at `agent_depth = 0` and owns the user-facing turn. A
subagent runs with `agent_depth > 0`, a separate prompt/transcript, and an
explicit task. Delegated agents report a result to the caller; they do not become
another frontend or another authority for the parent conversation.

Subagents cannot spawn nested subagents. Core rejects recursive delegation even
if an extension attempts it. `bone.headless` is true outside the interactive
TUI; headless callers wait for delegated work because there is no idle UI loop to
auto-inject results.

## Dispatch APIs

Lua can run work synchronously or start a background job:

```lua
local result = ctx.agent.run("Summarize these files", {
    approval = "safe",
    timeout_ms = 300000,
    tools = { "read_file", "shell" },
})

local job = ctx.agent.spawn("Inspect independent modules", {
    title = "module inspection",
    approval = "safe",
    tools = { "read_file" },
})
```

`ctx.agent.run` and `run_stream` return `{ ok, content, error }`. `spawn`
returns `{ ok, id, error }`. `ctx.agent.jobs()` returns queued/running/done/error
job snapshots. `ctx.agent.wait(ids, opts)` blocks for selected jobs and reports
finished jobs, pending ids, timeout, or caller cancellation. `followup(id,
prompt)` continues a completed job only when its saved transcript belongs to the
same conversation.

The native `subagent` tool uses the same contract: batch independent dispatches
in one call, use `wait` when the next operation depends on their results, and
otherwise let completed background results be delivered when the main agent is
idle. Do not poll in a loop.

## Boundaries and safety

Every delegated run has an approval mode and a tool allow-list. `tools` controls
what the delegated model sees; it does not weaken the approval policy. A child
with `safe` approval still requires approval for dangerous operations. The
parent's selected provider/model can be inherited or explicitly overridden.

Inactivity timeouts are bounded and stop a stalled run; an optional wall timeout
is a hard deadline even while output is active. Output limits may cap a child
response without changing the parent's provider request. Delegated runs share
the configured provider concurrency limit.

Keep delegated prompts self-contained. State the objective, relevant paths,
read-only or edit permissions, expected output, and whether the child may use
shell/tools. Ask for concrete file and line references when the result is for
review.

## Background jobs

A background job is owned by its originating conversation. Its id is used for
status, wait, follow-up, and cancellation; clients must not treat an id from one
conversation as globally actionable. Results are truncated when auto-injected
and may have a full result file when the runtime spills large output.

When the TUI is idle, an unconsumed completed result is injected as a new turn.
`wait` consumes results returned to the caller. Explicit cancellation consumes
the eventual result so a cancelled child is not injected later. The first quit
request warns when jobs are active; quitting again terminates them with the
process.

The subagent pane is rendered by Rust from the job registry. Extensions should
not try to recreate it with a pane of the same source. Cancellation is
conversation-scoped and cooperative: the wait can be cancelled without killing
the child, while an explicit job cancel requests termination of that job.

## Extension-facing metadata

`bone.agent_depth`, `bone.headless`, `ctx.runtime.info()`, and `ctx.agent.jobs()`
are read-only views of execution state. A delegated tool or hook must not assume
that a TUI is attached, that a session database exists, or that background
results can be displayed immediately.

See [Extension API](extension-api.md) for tool registration and
[Architecture](architecture.md) for session ownership.
