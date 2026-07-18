<!-- bone-agents-reference-version: 3 -->
# Bone Agent Reference

Bone refreshes this reference from the running build. It documents how to
configure and extend Bone.

## Start Here: Task to File

Paths are relative to the resolved Bone config directory.

| Task | File or API |
|---|---|
| Change persistent settings | `/config`, `config.yaml`, or `bone.settings.*` |
| Change providers or credentials | `config/providers.yaml` (restart required) |
| Change shell safety policy | `command-policy.yaml` (restart required) |
| Add an agent-callable tool | `lua/tools/<name>.lua` with `bone.tool.register` |
| Add a slash command | `lua/commands/<name>.lua` with `bone.command.register` |
| Add a theme | `lua/themes/<name>.lua`; load with `bone.theme.load` |
| Add shared Lua logic | `lua/lib/<name>.lua`; load with `require` |
| Add a plugin | `lua/plugins/<name>/init.lua`; load with `bone.plugin.load` |
| Wire keymaps, subagents, plugins, or modules at startup | `init.lua` |
| Manage persistent preferences | `memory/global.md` and `memory/projects/<cwd-key>.md` via `/memory` |

Keep `init.lua` as lightweight wiring. Put substantial implementations in the
purpose-specific `lua/` directories.

### Minimal Examples

```lua
-- init.lua
bone.keymap.set("<C-p>", "toggle_panes")
bone.keymap.set("<C-r>", "/review")
bone.keymap.set("<C-g>", "summarize these changes")
bone.keymap.set("<C-b>", function() return "/usage" end)

bone.subagent.register({
    name = "reviewer",
    description = "Review changes for regressions",
    system_prompt = "Review the requested changes and report verified issues.",
})

bone.theme.load("nord")
bone.plugin.load("my-plugin")
require("my_startup")
```

```lua
-- lua/themes/nord.lua
return { palette = { accent = "#88c0d0", error = "#bf616a" } }
```

```lua
bone.settings.get("general.approval")
bone.settings.set("general.approval", "danger")
bone.settings.reset("general.approval")
bone.submit("Continue with the next task")
```

## Agent Operating Instructions

> **Agent rule:** Treat paths below as relative to the resolved Bone config
> directory unless a path is explicitly absolute.

> **Agent rule:** After editing `config/providers.yaml` or
> `command-policy.yaml`, tell the user to restart Bone.

> **Agent rule:** Prefer the native file tools for file contents. Read a file
> before editing it, and use `shell` only when no dedicated file operation fits.

## Config Location

The resolved config directory is provided in the system prompt.

```
init.lua                 — Lua behavior and orchestration (optional)
lua/tools/               — Custom + catalog Lua tools (installed via /catalog)
lua/commands/            — Custom + bundled/catalog Lua commands
lua/plugins/             — Lua plugins (optional)
lua/lib/                 — Lua library modules (optional, bundled: history.lua, ui/)
config.yaml              — Canonical approval, UI, theme, and keymap settings
config/general.yaml      — `/config` page schema and compaction settings
config/status.yaml       — `/config` page schema for canonical status settings
config/providers.yaml    — LLM provider entries
config/tools.yaml        — Tool enable/disable toggles
config/commands.yaml     — Command enable/disable toggles
command-policy.yaml      — Shell command safety tiers
memory/                  — Optional catalog /memory extension data
```

## Lua Extension System

Bone embeds Lua 5.4 for tool, command, config, theme, and keymap customization. If `init.lua` exists in the config directory, it runs at startup. If missing, bone behaves exactly as before.

### `init.lua` Location

```
~/.bone-rust/init.lua
```

Errors in `init.lua` are logged as warnings; bone continues without Lua support.

### `bone` Global API

```lua
-- Metadata (read-only)
bone.version        -- string: app version
bone.cwd            -- string: startup CWD
bone.config_dir     -- string: config directory path
bone.agent_depth    -- integer: 0 for the main agent, >0 inside a sub-agent
bone.headless       -- boolean: true outside the interactive TUI

-- Logging (outputs to stderr)
bone.log.info("message")
bone.log.warn("message")
bone.log.error("message")

-- Tool registration
bone.tool.register({ ... })

-- Sub-agent registration
bone.subagent.register({ name = "...", description = "...", system_prompt = "...", provider = "...", model = "...", approval = "..." })

-- Command registration
bone.command.register("name", { description = "...", handler = function(args, ctx) ... end })
bone.command.register("name", function(args, ctx) ... end)  -- short form

-- Event hooks
bone.on("event_name", function(event, ctx) ... end)
bone.on("event_name", function(event, ctx) ... end, { subagents = true })  -- also register inside sub-agents
```

### `cjson` Global

A `cjson` global is available for JSON encoding/decoding:
```lua
local json_str = cjson.encode({ key = "value" })
local table = cjson.decode(json_str)
```

### `ctx` API

A `ctx` table is passed as the second argument to tool `execute(params, ctx)` and command `handler(args, ctx)` functions. Event handlers receive a smaller `ctx` (see [Context Availability](#context-availability) below).

To edit existing files, first call `read_file`, then use `ctx.tools.call("edit_file", { path = path, old_text = old, new_text = new })` which goes through the full approval pipeline. There is no convenience `ctx.edit_file` method.

#### Reference Table

| API | Returns | Description |
|---|---|---|
| `ctx.config_dir` | `string` | Bone config directory path |
| `ctx.cwd` | `string` | Startup working directory |
| `ctx.call_id` | `string\|nil` | Current tool call's unique ID (tools only) |
| **`ctx.log.*`** | | Log to stderr |
| `ctx.log.debug(val)` | | Log at debug level |
| `ctx.log.info(val)` | | Log at info level |
| `ctx.log.warn(val)` | | Log at warn level |
| `ctx.log.error(val)` | | Log at error level |
| **`ctx.fs.*`** | | Filesystem helpers (read-only queries) |
| `ctx.fs.exists(path)` | `bool` | Path exists check |
| `ctx.fs.is_file(path)` | `bool` | Path is a regular file |
| `ctx.fs.is_dir(path)` | `bool` | Path is a directory |
| `ctx.fs.read_dir(path)` | `array` | List `{name, path, kind}` entries, sorted by name |
| `ctx.fs.metadata(path)` | `table` | `{path, kind, len, readonly}` |
| **Shell** | | Run commands through native approval + policy |
| `ctx.shell(cmd, opts?)` | `table` | `{stdout, stderr, exit_code}` |
| `ctx.shell_streaming(cmd, cb, opts?)` | `table` | Calls `cb(line)` per stdout line; returns `{stdout, stderr, exit_code}` |
| **Files** | | Read/write |
| `ctx.read_file(path)` | `string` | Read entire file contents (raises Lua error on failure) |
| `ctx.write_file(path, content)` | `true` | Create new file; fails if file exists (raises Lua error) |

| **`ctx.ui.*`** | | UI output |
| `ctx.ui.notify(msg, level?)` | | Show notification (`"info"`, `"warn"`, `"error"`); forwarded to the frontend as a status line when one is attached |
| `ctx.ui.status(msg)` | | Surface a *transient* live status line to the attached frontend (TUI); may be replaced. Stderr fallback when headless |
| `ctx.ui.notice(msg)` | | Surface a *persistent* notice that the frontend keeps in the conversation scrollback (e.g. an auto-compaction announcement). Stderr fallback when headless |
| `ctx.ui.pane(table)` | `true\|(false, string)` | Upsert/clear a live pane (tools only) — see [Live Panes](#live-panes) |
| `ctx.ui.apply(diff)` | `true\|(false, string)` | Apply a protocol `ViewDiff` (`upsert`, `remove`, or `set_highlight`) declaratively |
| `ctx.ui.key()` | `table` | Block for one key event: `{code, char, ctrl, alt, shift}` — see [Live Panes](#live-panes) |
| **Live events** | | During `execute_output_live` only |
| **`ctx.runtime.*`** | | Read-only runtime metadata |
| `ctx.runtime.info()` | `table` | `{session_id, provider, model, agent_depth, approval_mode, execution={kind, depth}}` |
| **`ctx.usage.*`** | | Token usage |
| `ctx.usage.snapshot()` | `table\|nil` | See [Usage Snapshot](#usage-snapshot) below |
| **`ctx.state.*`** | | Session-scoped key-value store |
| `ctx.state.get(key)` | `string\|nil` | Get value |
| `ctx.state.set(key, value)` | `true` | Set value |
| `ctx.state.clear(key)` | `true` | Remove key |
| **`ctx.tools.*`** | | Call registered tools |
| `ctx.tools.definitions()` | `array` | `{name, description, input_schema}` for all tools |
| `ctx.tools.call(name, args, opts?)` | `table` | `{ok, name, call_id, content, is_error}` |
| **`ctx.agent.*`** | | Spawn subagents |
| `ctx.agent.run(prompt, opts?)` | `table` | `{ok, content, error}` |
| `ctx.agent.run_stream(prompt, opts?)` | `table` | Same with event callbacks |
| `ctx.agent.spawn(prompt, opts?)` | `table` | `{ok, id, error}` — non-blocking background job |
| `ctx.agent.followup(id, prompt, opts?)` | `table` | `{ok, id, error}` — continue a completed job from its saved transcript |
| `ctx.agent.jobs()` | `array` | Snapshot of all jobs (`{id, agent, task, status, result, started_at}`) |
| `ctx.agent.wait(ids?, opts?)` | `table` | `{ok, jobs, pending, timed_out, cancelled}` — block until jobs finish |
| **`ctx.config.*`** | | Config access |
| `ctx.config.dir` | `string` | Same as `ctx.config_dir` |
| `ctx.config.get(section, key)` | `value\|nil` | Read a value from `config/<section>.yaml` |
| `ctx.config.get_table(section)` | `table\|nil` | Read entire config section as table |
| `ctx.config.get_pages()` | `array` | Read ordered custom config pages and fields |
| `ctx.config.set_value(section, key, value)` | `true` | Persist a scalar config field |
| `ctx.config.cycle_field(section, key, current)` | `string\|nil` | Next bool/enum value |
| `ctx.config.list_providers()` | `array` | Provider rows with active marker |
| `ctx.config.set_provider_entry(id, entry)` | `true` | Persist provider fields |
| **`ctx.session.*`** | | Conversation history |
| `ctx.session.current()` | `table\|nil` | `{id, provider, model}` for current session |
| `ctx.session.list(opts?)` | `array` | Recent conversations (default limit 20, max 100) |
| `ctx.session.messages(id, opts?)` | `array` | Messages for a conversation (default limit 200, max 1000) |
| **`ctx.conversation.*`** | | Active conversation transcript (not SQLite) |
| `ctx.conversation.current()` | `table\|nil` | `{id, provider, model}` for the active conversation |
| `ctx.conversation.history()` | `array` | In-memory transcript: `{role, content, tool_calls?, name?, tool_call_id?}` |
| `ctx.conversation.submit(text)` | `true` | Queue a later user turn through the daemon-owned inbox |
| `ctx.conversation.load(id)` | `true\|(false, string)` | Ask the daemon to load a conversation (interactive command contexts) |

#### Context Availability

Not all `ctx` fields are available in every handler type:

| | Tool `execute` | Command `handler` | Event `bone.on` handler |
|---|:---:|:---:|:---:|
| `config_dir` | yes | yes | yes |
| `cwd` | yes | yes | — |
| `log` | yes | yes | — |
| `fs` | yes | yes | — |
| `shell` / `shell_streaming` | yes | yes | — |
| `read_file` / `write_file` | yes | yes | — |
| `ui.notify` | yes | yes | yes |
| `ui.status` / `ui.pane` | yes | yes | — |
| `usage` | yes | yes | — |
| `state` | yes | yes | — |
| `tools` | yes | yes | — |
| `agent` | yes | yes | — |
| `config` | yes | yes | `config.dir` only |
| `session` | yes | yes | — |
| `conversation` | yes | yes | — |
| `call_id` | yes | — | — |

Event handlers receive a minimal `ctx` with only `config_dir`, `ui.notify`, and `config.dir`. They cannot execute shell commands, read files, or call tools. This is intentional — event handlers run inline during the event loop and must not block.

**Exception:** `before_turn` handlers receive a **full** `ctx` — the same as tool `execute` and command `handler`. See [before_turn](#before_turn) below.

#### `ctx.conversation`

Provides a snapshot of the active in-memory conversation transcript (not the SQLite history). Available in tool `execute` and command `handler` contexts.

```lua
-- Get the active conversation metadata
local conv = ctx.conversation.current()
-- conv.id, conv.provider, conv.model  (all nil when no active conversation)

-- Get the current transcript messages
local messages = ctx.conversation.history()
-- array of { role = "user"|"assistant"|"tool", content = string, tool_calls?, name?, tool_call_id? }
-- The system prompt is NOT included.
```

The transcript returned by `history()` is the live in-memory history used for the next provider request. Use `submit(text)` to queue a later user turn. Interactive commands can call `load(id)` to ask the daemon to switch conversations and emit the normal transcript/state updates.

#### Shell Options

`ctx.shell` and `ctx.shell_streaming` accept an optional opts table:
```lua
{ timeout_ms = 120000 }  -- min 1000, max 300000
```
Default timeout: 120s for `ctx.shell`, 300s for `ctx.shell_streaming`. Commands run through the same approval and policy system as the native `shell` tool.

#### Managed processes

Use `ctx.process` for work that must outlive a Lua handler. Bone owns the
process group, cancellation, and captured output; extensions receive an id,
not a raw OS handle.

```lua
local job = ctx.process.spawn("npm run dev", { timeout_ms = 3600000 })
local state = ctx.process.status(job.id) -- running, stdout, stderr, exit_code
local output = ctx.process.output(job.id)
local jobs = ctx.process.list()
ctx.process.kill(job.id)
```

The native `shell` tool also accepts `{ background = true }`, returning a
managed process id immediately. Use this only for intentionally detached work
(downloads, servers, long builds); normal commands remain foreground and can
be cancelled with Ctrl+C.

#### `ctx.tools.call`

Call a registered tool by name with typed arguments:
```lua
local result = ctx.tools.call("read_file", { path = "/tmp/test.txt" }, { approval = "safe" })
if result.ok then
    ctx.log.info(result.content)
else
    ctx.log.error(result.content)
end
```
Opts: `{ approval = "safe" | "read_only" | "danger" }`. Max nesting depth: 4 levels of tool calls from Lua.

#### `bone.subagent.register`

Declare a named sub-agent in `init.lua`. The `subagent` tool (auto-created when agents are registered) uses these definitions to dispatch tasks.

```lua
bone.subagent.register({
    name = "researcher",
    description = "Searches the web and summarizes findings",
    system_prompt = "You are a researcher.",
    provider = "openai",
    model = "gpt-4o",
    approval = "safe",
    timeout_ms = 300000,
})
```

Fields: `name` (required, unique), `description` (required), `system_prompt`, `provider`, `model`, `approval`, `timeout_ms`. Duplicates are skipped with a warning.

Sub-agents cannot spawn nested sub-agents. When `bone.agent_depth > 0`, the default `subagent` delegation tool is not registered, and Rust also rejects attempts to spawn another agent from inside a sub-agent.

`bone.headless` is true in non-TUI flows. In headless mode, the default `subagent` tool waits for dispatched work because there is no interactive pane or auto-injection loop to deliver background results later.

#### `ctx.agent.run` / `ctx.agent.run_stream`

Spawn a subagent:
```lua
local result = ctx.agent.run("Summarize this file", {
    approval = "safe",
    system_prompt = "You are a summarizer.",
    timeout_ms = 300000,
    max_tokens = 2048,
})
if result.ok then
    ctx.log.info(result.content)
end
```
Opts: `{ approval, provider, model, system_prompt, timeout_ms, max_tokens }`. Default timeout: 300s, max 900s. Agent requests use an inactivity timeout: an active sub-agent is not stopped merely because a hard wall-clock duration elapsed while it is still streaming output or tool results. `max_tokens` caps the subagent's output tokens (sent as the provider's `max_tokens`); omit it to let the provider/server apply its own default. The cap is applied to the freshly-constructed provider, so it never affects the main turn. Use it to bound a model whose output could run away — e.g. compaction summaries.

`run_stream` accepts additional callback opts: `on_started`, `on_status`, `on_tool_call`, `on_tool_result`, `on_token_usage`, `on_finished`, `on_failed`. Each callback receives a table with event-specific fields.

#### `ctx.agent.spawn` / `ctx.agent.jobs` / `ctx.agent.wait`

Dispatch a non-blocking background job. Results are delivered in one of two ways: blocking on `ctx.agent.wait` (when the caller needs them now), or auto-injection into the conversation when the main agent goes idle.

```lua
local result = ctx.agent.spawn("Research Rust async runtimes", {
    agent = "researcher",
    system_prompt = "You are a researcher.",
    timeout_ms = 300000,
})
-- result: { ok = true, id = "job-1", error = nil }
```

Opts: `{ agent, approval, provider, model, system_prompt, timeout_ms }`. Sub-agents (`agent_depth > 0`) cannot spawn or wait on background jobs.

Query all jobs:
```lua
local jobs = ctx.agent.jobs()
-- jobs: array of { id, agent, task, status, result, started_at, finished_at,
--                  consumed, token_sent, token_received, result_file }
-- status is one of: "queued", "running", "done", "error"
```

Continue a completed job from its saved transcript:
```lua
local next = ctx.agent.followup("job-1", "Now implement the best option", {
    agent = "researcher",
    timeout_ms = 300000,
})
-- next: { ok = true, id = "job-2", error = nil }
```

`followup` is scoped to the current conversation: it can only resume jobs spawned in the same session, and only jobs that completed with a saved transcript.

Block until jobs finish:
```lua
local outcome = ctx.agent.wait({ "job-1", "job-2" }, { timeout_ms = 300000 })
-- outcome: { ok = true, jobs = {...finished jobs...}, pending = {"job-2"},
--            timed_out = false, cancelled = false }
```

`ids` is optional — `ctx.agent.wait(nil)` waits on all currently running jobs. Default timeout 300s, max 900s. Jobs returned by `wait` are marked consumed so they are not auto-injected again. Esc cancels the wait (`cancelled = true`); the jobs themselves keep running and their results auto-inject later. Jobs still running at timeout are listed in `pending` and also auto-inject on completion.

**Auto-injection**: when a background job finishes unconsumed and the TUI is idle, results are injected as a new turn. The agent wakes up automatically — no polling needed. Results are truncated to 16k chars at injection time. Full results are spilled under the system temp directory as `bone-jobs/job-N.txt`; the job's `result_file` field points to that path when present.

**Sub-agent pane**: the interactive TUI pane is rendered by Rust from the job registry, so it keeps updating even while Lua is blocked in a wait. The old Lua hook `bone._subagents_render` is no longer used.

**Quit guard**: if background sub-agent jobs are still running, the first quit request warns instead of exiting. Quit again to exit anyway; running jobs are terminated with the process.

**The `subagent` tool** (auto-created when agents are registered) exposes this to the main agent as three actions: `dispatch` (with optional `wait=true` for dependent work), `wait` (collect pending results), and `status` (non-blocking snapshot). The intended workflow: batch independent tasks into one dispatch; wait when the next step depends on results; otherwise end the turn and let results auto-inject.

Existing user config files are not overwritten when catalog items are installed. If you need the latest version of a catalog tool, run `/catalog` to re-install it.

#### Usage Snapshot

`ctx.usage.snapshot()` returns a table with the current conversation's token usage:
```lua
local u = ctx.usage.snapshot()
-- u.request_count, u.sent, u.received, u.cached, u.cost
-- u.context_length, u.tool_count, u.tool_schema_chars, u.tool_schema_tokens
-- u.system_prompt_chars, u.system_prompt_tokens
-- u.by_provider: array of { provider, model, prompt_tokens, completion_tokens, cached_tokens, cost, request_count }
```
Returns `nil` if usage data is unavailable in the current context.

## Tools

Bone has two categories of tools:

### Native Rust Tools (always available)

These are compiled into bone and do not require any seeding or installation:

- **shell** — Run a non-interactive shell command with bash -lc
- **read_file** — Read a UTF-8 text file
- **write_file** — Create a new UTF-8 text file
- **edit_file** — Replace one exact unique text block in an existing file (`{ path, old_text, new_text }`)
- **process** — List, inspect, or stop managed background shell processes

Use the dedicated file tools as the default interface for file contents:

- Use `read_file` rather than `cat`, `head`, `tail`, or `sed`.
- Use `write_file` rather than `tee`, `printf`, heredocs, or redirection.
- Read first, then use `edit_file` rather than `sed -i`, scripts, heredocs, or redirection.
- Use `shell` only when a file tool explicitly recommends it, for a bulk
  multi-file operation, or when no dedicated file tool supports the operation.
  If a file tool fails, follow its error instead of immediately retrying the
  same operation through `shell`.

### Catalog Lua Tools (optional, installed via `/catalog`)

Optional Lua tools live in the [`bone-catalog`](https://github.com/vincentm65/bone-catalog) repository. They are fetched from raw GitHub content at `https://raw.githubusercontent.com/vincentm65/bone-catalog/main` (overridable via `BONE_CATALOG_URL`) and installed into `~/.bone-rust/lua/tools/` on demand — during onboarding or via the `/catalog` command. Once on disk they are loaded by the normal Lua loader like any user file.

Available catalog tools include:

- **web_search** — Search the web via DuckDuckGo
- **ask_user** — Ask the user a question with selectable options
- **task_list** — Maintain a visible checklist with TUI pane rendering
- **cron** — Manage scheduled bone jobs via crontab
- **browser** — Drive a persistent browser through observe/target actions

To browse and install catalog tools interactively, run `/catalog` in the TUI. To override the catalog source URL, set the `BONE_CATALOG_URL` environment variable to an `http(s)://` base or a local filesystem path.

### Native Rust Tool Details

```lua
-- Native Rust tool, not Lua. Called by the LLM directly.
-- Parameters: command (string, required), timeout_ms (integer, optional),
-- background (boolean, optional)
```

```lua
-- Native Rust tool. Parameters: path, start_line?, max_lines?
-- Output includes the resolved path, shown range, and numbered lines.
```

```lua
-- Native Rust tool. Parameters: path, content
-- Creates a new file and refuses to overwrite an existing path.
```

```lua
-- Native Rust tool. Parameters: path, old_text, new_text
-- Read the file first. old_text must be an exact unique block from the shown
-- lines; new_text replaces it and may be empty to delete it.
```

```lua
-- Native Rust tool. Parameters: action ("list", "status", or "kill"), id?
-- Manages processes started by shell with background = true.
```

## Commands

### /compact (optional catalog extension)

Install `compact.lua` through `/catalog` to add reliable context compaction using a versioned `[Context checkpoint v1]`. Older complete turns are folded into a validated checkpoint while recent complete turns remain verbatim. Existing checkpoints are updated incrementally instead of repeatedly summarizing the full transcript.

Usage:

```
/compact                       # compact now
/compact preview               # inspect boundaries and budgets without changing history
/compact inspect               # show the current checkpoint and protected-item count
/compact pin <exact text>      # preserve text verbatim across later compactions
/compact pins                  # list protected items
/compact unpin <number>        # remove a protected item
```

Reliability properties:

- The checkpoint has required sections for the current objective, constraints, verified facts, files/symbols, validation, completed work, unresolved issues, pending action, and critical verbatim details.
- Summarizer input and output are bounded. Large history is split on UTF-8 boundaries and folded incrementally using the previous validated checkpoint.
- Replacement happens only after deterministic schema, size, and protected-text validation, and only when the resulting provider context estimate is smaller. Failure preserves the original transcript.
- Turn boundaries keep user, assistant, and associated tool messages together. The legacy message-count setting is rounded out to complete turns.
- Summarization runs without tools and with explicit output and wall-clock limits.
- Automatic compaction emits transient status while working and a persistent notice only on failure.

Configuration is stored in `config/general.yaml` and read by the command through
`ctx.config.get("general", key)`. Change it through `/config` or edit that YAML;
these are not `bone.config` fields:

- `compact_trigger_mode` — `absolute` (default) or `percentage`.
- `auto_compact_tokens` — positive token threshold used in absolute mode. Blank disables automatic compaction in that mode.
- `compact_trigger_percentage` — context-capacity percentage used in percentage mode (default `80`).
- `compact_context_window_tokens` — optional model context-capacity override when runtime metadata does not provide one. Percentage mode is disabled if capacity is unknown.
- `compact_safety_tokens` — reserve below context capacity (default `8000`); the percentage threshold never exceeds capacity minus this reserve.
- `compact_keep_tokens` — recent complete-turn token budget preserved verbatim (default `12000`).
- `compact_input_tokens` — maximum summarizer input per folding pass (default `30000`).
- `compact_checkpoint_tokens` — maximum fully rendered checkpoint size (default `2500`). The deprecated `compact_summary_tokens` is accepted as a fallback.
- `compact_generation_tokens` — provider generation allowance for summary and compression passes (default `8000`).
- `auto_compact_keep_messages` — deprecated compatibility setting. When present, it replaces `compact_keep_tokens` with a recent user/assistant message target, rounded to complete turns.

Auto-compaction runs after a user message is appended and before the provider request is built. A per-conversation growth gate prevents repeated retries when context has not grown materially. After a successful replacement, its baseline is reset to the new compacted context estimate.

The transcript replacement is returned as a `conversation.replace` action (see [Return Actions](#command-return-semantics)). The Driver runs `before_turn` on a blocking thread so the UI stays responsive and Esc can cancel an in-flight summarizer. The implementation policy remains entirely in `lua/commands/compact.lua`; Rust supplies generic conversation, request-estimation, agent, and return-action APIs.

### /config

Open the interactive config editor. See [Config, Theme, and Keymaps](#config-theme-and-keymaps) for the full API.

### /memory (optional catalog extension)

Automated long-term memory is not bundled with Bone. Install `memory.lua` through
`/catalog` to add capture, scoped storage, prompt injection, and the `/memory`
commands. The extension owns all memory behavior through Lua, including
`before_turn` prompt injection.

Persistent preferences live in `memory/global.md` and
`memory/projects/<cwd-key>.md`. They can be edited directly or managed through
the catalog-installed `/memory` extension.

Existing `memory.md` and `memory/` data are preserved during migration, while the
legacy `lua/commands/memory.lua` is removed. Legacy `memory.md` is copied to
`memory/global.md` only when the scoped file is absent; existing scoped data is
never overwritten. Install the catalog extension to restore `/memory`.

### /usage (optional catalog extension)

Install `usage.lua` through `/catalog` to show token usage stats for the current
session.



## Creating Custom Tools

Tools are Lua files in `lua/tools/` that call `bone.tool.register()`. The agent calls them as typed functions with args, and they return a string to the agent.

### Minimal Tool

```lua
bone.tool.register({
    name = "my_tool",
    description = "Short description of what the tool does and when to use it.",
    parameters = {
        type = "object",
        properties = {
            query = {
                type = "string",
                description = "What this arg is for",
            },
        },
        required = { "query" },
        additionalProperties = false,
    },
    safety = "read_only",
    execute = function(params, ctx)
        local result = ctx.shell("some-command " .. params.query)
        return result.stdout
    end,
})
```

### Tool Fields

- **name** — unique string identifier. Native tools (`shell`, `read_file`, `write_file`, `edit_file`, `process`) cannot be overridden.
- **description** — shown to the LLM when deciding which tool to call.
- **parameters** — JSON Schema object describing the tool's arguments.
- **safety** — `"read_only"` or `"danger"`. In safe mode only `read_only` tools auto-run; in danger mode everything auto-runs.
- **display** — optional table controlling TUI visibility:
  ```lua
  display = {
      show = true,           -- show a pane for this tool
      show_result = true,    -- show the result in the pane
      args = { "action" },   -- which arg values to display
      template = "{action}", -- format string for the row label
      eager = false,         -- render the row at call time, not on result
  }
  ```
  - **template** — `{key}` interpolates a scalar argument. `{items[].field}`
    expands the chosen field of each element of array arg `items` (quoted,
    joined); use `{items[].a|b}` to try fields `a` then `b` per element. When an
    array placeholder resolves to nothing, the template is skipped and the row
    falls back to the `args` label.
  - **eager** — set `true` for tools whose calls block (e.g. dispatching
    background agents and waiting on them) so the row shows immediately rather
    than only when the call returns. The later result row is suppressed.
- **execute** — `function(params, ctx) -> string`. The function body. Returns the tool result string. Errors are caught and returned as tool errors to the LLM.

### Tool Output

- **Default:** the string returned from `execute` is sent to the agent as the tool result.
- **JSON envelope (for TUI panes):** return a JSON-encoded object with `content`, optional `state`, and optional `pane`:
  ```lua
  local output = {
      content = "Result text for the agent",
      state = cjson.encode(state_table),    -- persisted across calls
      pane = {
          source = "my_tool",
          title = "My Tool",
          visible_rows = 8,
          scroll = 0,
          lines = {
              { spans = { { text = "Label: ", fg = "dark_gray" }, { text = "value", fg = "white" } } },
          },
      },
  }
  return cjson.encode(output)
  ```
  Pane span fields: `text` (required), `fg` (optional), `bg` (optional), `modifiers` (optional array: `"bold"`, `"dim"`, `"italic"`, `"underline"`, `"strike"`). Colors: `"white"`, `"dark_gray"`, `"green"`, `"red"`, `"yellow"`, `"blue"`, `"cyan"`, `"magenta"`.

### Live Panes

The bottom of the TUI hosts a tab-switchable region of panes. Every pane is
identified by a stable `source` string and carries `{ title, lines, visible_rows?, scroll? }`.
The `lines` format is the same as the return-envelope `pane` above (plain
strings or `{ spans = {...} }`).

There are two ways to show a pane:

- **Return-envelope `pane`** (above) — a single snapshot shown *after* `execute`
  returns. Good for static results.
- **`ctx.ui.pane{}`** — emitted *while* the tool runs, as many times as you
  like. This is how you stream progress. Only
  available during tool execution.

Re-emitting the same `source` **replaces** that pane in place (upsert); emitting
it with empty `lines` **removes** it:

```lua
for i = 1, n do
    ctx.ui.pane{ source = "scan", title = ("Scanning (%d/%d)"):format(i, n),
                 lines = { ("checked %d files"):format(i) } }
    -- ...do work...
end
ctx.ui.pane{ source = "scan", title = "", lines = {} }   -- clear when done
```

#### Lua menus — `ui.menu`

Menus are Lua-rendered panes driven by raw key events. Use the bundled
`ui.menu` module for standard select, multi-select, and text input flows:

```lua
local menu = require("ui.menu")

local result = menu.select(ctx, {
    question = "Which branch?",
    options = { "main", "dev" },
    default = 1,                 -- 1-based initial highlighted option (optional)
    visible_rows = 12,          -- requested pane height; defaults to 12
    allow_custom = true,         -- offer a free-text "Custom:" row (optional)
})
-- single_select → { value = "main" }  or  { value = "...", custom = true }
-- multi_select  → menu.multi_select(ctx, {
--     options = { "main", { label = "Development", value = "dev" } },
--     default = 2,                         -- initial highlighted option
--     initial_checked = { "main", "dev" }, -- prechecked normalized option values
--     initial = "prior custom text",       -- prefilled custom input
-- }) returns { values = {...}, custom? = "..." }
-- text_input    → menu.text_input(ctx, { initial = "prior text" }) returns { value = "typed text" }
-- cancelled     → { cancelled = true }
```

An object option may include a generic rich preview. When any option has one,
the menu shows a compact option rail beside the highlighted option's preview
(and stacks them on narrow terminals). Preview lines use the same plain-string
or styled-span format as `ctx.ui.pane`:

```lua
options = {
    {
        label = "Session cookies",
        preview = {
            title = "Architecture",
            lines = {
                "Browser ──▶ Web app ──▶ Redis",
                { spans = { { text = "server-side session", fg = "#78B373" } } },
            },
        },
    },
}
```

The preview layout can be configured independently of the caller. Omitted fields
preserve the defaults:

```lua
local result = menu.select(ctx, {
    question = "Choose a design",
    visible_rows = 14,
    preview = {
        layout = "auto",       -- "auto" (default), "split", or "stacked"
        min_width = 64,         -- split threshold used by "auto"
        focusable = true,       -- Tab can focus the preview
        scrollable = true,      -- arrow/Page keys scroll while focused
    },
    options = options,
})
```

`layout = "split"` always uses columns; `layout = "stacked"` always places the
preview below the options. In `"auto"` mode, `min_width` controls the switch.
Setting either `focusable` or `scrollable` to `false` makes the preview static:
it starts at the first line, has no scroll-position suffix, and is skipped by
Tab. This is useful for short diagrams and other previews that fit within the
configured `visible_rows`. Invalid layout values fall back to `"auto"`.

By default, preview menus size themselves to the tallest option preview and the
space required by the active split or stacked layout, up to the 24-row pane
limit. An explicit `visible_rows` keeps a fixed height. Use **Tab** to focus the
preview and arrow/Page keys to scroll it. Selection, multi-select toggles,
custom input, and return values are unchanged.

For lower-level input, `ctx.ui.key()` blocks until the next key and returns a
table such as `{ code = "Up", char = nil, ctrl = false, alt = false, shift = false }`.
Ctrl+C remains host-owned cancellation; Esc is delivered to Lua.

#### Lifecycle & cancellation

- Clean up panes you create by emitting empty `lines` when done.
- Pressing **Esc** in bundled Lua menus cancels just that menu (returns
  `{ cancelled = true }`), not the whole turn, so your cleanup code still runs.
- If the user **hard-cancels** the turn (Ctrl+C) while your tool is mid-run, its
  execution is dropped before cleanup can run; the host automatically removes any
  panes the tool emitted, so nothing lingers.
- The **sub-agent pane** (`source = "subagents"`) is rendered by Rust from the
  job registry, not via `ctx.ui.pane`. Don't use that `source` for your own panes.

### Session State

Tools that need to remember data between invocations use `ctx.state`:

1. Your tool calls `ctx.state.set("key", serialized_state)`.
2. The host stores it for the session.
3. On the next call, `ctx.state.get("key")` returns the stored string.
4. State does not persist across bone restarts.

## Creating Custom Commands

Commands are slash-commands (`/name args`) registered via `bone.command.register()`. They live as Lua files in `lua/commands/`.

### Long Form

```lua
bone.command.register("deploy", {
    description = "Deploy current project",
    handler = function(args, ctx)
        local result = ctx.shell("./deploy.sh " .. args)
        if result.exit_code ~= 0 then
            ctx.ui.notify(result.stderr, "error")
            return nil
        end
        return result.stdout
    end,
})
```

### Short Form

```lua
bone.command.register("hello", function(args, ctx)
    return "Hello " .. args
end)
```

### Command Return Semantics

| Return | Behavior |
|---|---|
| `nil` | Command handled, no prompt submitted |
| string | Injected as user prompt/output |
| table with `display`/`reply`/`content` and `submit = false` | Show message in UI without submitting a prompt |
| table with `display_role = "assistant"` and `submit = false` | Show `display`/`reply`/`content` as assistant Markdown instead of plain system text |
| table with `action` field | Apply a state-mutating action (see below) |
| error | Show error in UI |

Display-only command output defaults to system text, which is plain-wrapped.
Use `display_role = "assistant"` for Markdown-rendered reports:

```lua
return {
    display = "## Result\n\n- Rendered as Markdown",
    submit = false,
    display_role = "assistant",
}
```

#### Return Actions

Commands and `before_turn` hooks can return a table with an `action` field to mutate conversation state:

```lua
return {
    action = "conversation.replace",
    messages = {
        { role = "user", content = "Summary of earlier context" },
        { role = "assistant", content = "I understand the summary." },
    },
    display = "Context compacted.",  -- optional UI message
    submit = false,                    -- optional, defaults to true
}
```

**`conversation.replace`** — Replaces the active model-facing transcript with the given `messages` array. Each message must have `role` (`"user"`, `"assistant"`, or `"tool"`) and `content`. Optional fields: `tool_calls`, `name`, `tool_call_id`. Invalid/unknown roles are skipped; if no valid messages remain, the action is ignored. The replacement is stored as a context checkpoint so it survives restart; the complete SQLite message history is retained unchanged for display, search, and export.

When `conversation.replace` is applied:
- The transcript is replaced with the validated messages.
- The context length estimate is recomputed.
- Cumulative token/cost/request counts are unchanged.
- A system message is added to the scrollback noting the replacement.

Multiple return actions from `before_turn` handlers apply in registration order.

**Turn shaping (`before_turn` only).** Independently of `action`, a `before_turn`
handler can return three fields that shape the upcoming provider request:

```lua
return {
    system_prompt_append = "Plan only. Do not edit files; outline the steps.",
    turn_message = "Task list: 2/5 done. Mark items done as you finish them.",
    tool_filter = { "read_file", "grep", "shell" },  -- only these are exposed
}
```

- **`system_prompt_append`** — text appended to the system prompt for this turn
  (stacks after the base prompt; multiple handlers concatenate in order). Use
  only for text that stays constant across the conversation: the system prompt
  renders before the whole history, so turn-to-turn variation here invalidates
  the provider's prefix cache for every request.
- **`turn_message`** — transient guidance inserted at the request tail when the
  handler emits it, then retained at that position for later tool rounds in the
  same user turn (wrapped in `<system-reminder>` tags and never persisted to the
  transcript; multiple handlers concatenate in order). Keeping its position
  stable lets later requests extend the provider's cached prefix. Use it for
  turn-varying nudges like task-list state or iteration counters; unchanged
  guidance is deduplicated within the user turn.
- **`tool_filter`** — a per-turn allow-list of tool names. Only these tools are
  shown to the model this turn; an empty list hides every tool. This filters
  what the model *sees* — it does not change the approval policy. Omit (or
  return `nil`) to expose the full toolset. When several `before_turn` handlers
  return a filter, the last in registration order wins.

Both reset every turn, so a handler that reads a flag (e.g. from `ctx.state`)
can implement a toggled "plan mode" entirely in Lua.

Protected built-ins (`/catalog`, `/clear`, `/config`, `/edit`, `/e`, `/exit`, `/help`, `/model`, `/new`, `/provider`, `/quit`, `/setup`, `/stats`, `/tools`, `/update`) cannot be overridden.

## Event Hooks

Register callbacks for lifecycle events via `bone.on()`:

```lua
-- Block dangerous shell commands
bone.on("tool_call", function(event, ctx)
    if event.name == "shell" and event.arguments.command:find("rm %-rf") then
        return { block = true, reason = "blocked by Lua policy" }
    end
end)

-- Observe tool failures
bone.on("tool_result", function(event, ctx)
    if event.is_error then
        ctx.ui.notify("tool failed: " .. event.name, "warn")
    end
end)
```

### Events

| Event | When | Blockable | Full ctx |
|---|---|---|---|
| `session_start` | New session starts | no | no |
| `session_end` | Session ends | no | no |
| `message` | LLM/user message observed | no | no |
| `tool_call` | Before tool execution | **yes** | no |
| `tool_result` | After tool execution | no | no |
| `mode_change` | Approval mode changes | no | no |
| `turn_start` | A turn begins | no | no |
| `token_usage` | After each provider response, with token counts | no | no |
| `turn_end` | A turn finishes (success or failure) | no | no |
| `before_turn` | After user message, before provider request | no | **yes** |

Handlers run in registration order. First `block` stops the chain. Handler runtime errors do not block (fail-open).

By default, `bone.on` calls inside sub-agents are ignored so sub-agent prompts do not accidentally duplicate host hooks. Pass `{ subagents = true }` as the third argument to register a handler from sub-agent contexts too.

The turn-lifecycle events carry these payloads:

| Event | `event` fields |
|---|---|
| `turn_start` | `task` (user prompt), `model`, `approval` |
| `token_usage` | `sent`, `received`, `context_length` |
| `turn_end` | `ok` (bool); then `content` on success or `error` on failure |

```lua
-- Live stats: keep a status segment in sync with token usage.
bone.on("token_usage", function(event, ctx)
    bone.api.ui.set_statusline("stats", {
        { text = "ctx " .. event.context_length, align = "right" },
    })
end)
```

#### `before_turn`

The `before_turn` event fires after the user message is appended to the transcript and before the provider request is built. It receives a **full ctx** (same as tool `execute` and command `handler`) including `usage`, `state`, `agent`, `tools`, `config`, `session`, and `conversation`.

```lua
bone.on("before_turn", function(event, ctx)
    local snapshot = ctx.usage.snapshot()
    if snapshot and snapshot.context_length >= 8000 then
        -- Summarize older messages and replace the transcript.
        local history = ctx.conversation.history()
        -- ... build summary via ctx.agent.run() ...
        return {
            action = "conversation.replace",
            messages = new_messages,
        }
    end
    -- Return nil to do nothing.
end)
```

Key differences from other events:
- **Full ctx** — Access to `ctx.agent.run()`, `ctx.tools`, `ctx.usage`, `ctx.conversation`, etc.
- **Return actions** — Can return `action = "conversation.replace"` to mutate the transcript before the provider sees it.
- **Not blockable** — Unlike `tool_call`, `before_turn` cannot block the turn (only mutate it).
- **Multiple handlers** — All handlers run; return actions apply in registration order.

This is the mechanism used by the optional catalog `compact.lua` extension. Its
`before_turn` handler summarizes older messages when context exceeds a threshold,
preserves complete tool-call chains, and drops incomplete chains at the
compaction boundary so provider history stays valid.

## Runtime APIs

Most user-facing operations live directly under a purpose-specific `bone`
namespace. `bone.api.ui` is reserved for low-level drawing primitives.

```lua
bone.on(event, handler)             -- register an event handler
bone.api.emit(event, payload?)      -- fire handlers synchronously
bone.submit(text)                   -- queue a prompt as if typed
bone.keymap.set(key, rhs)           -- declare a runtime keymap
bone.settings.get/set/reset(...)    -- canonical persistent settings
bone.theme.list()                   -- list lua/themes/*.lua
bone.theme.load(name)               -- load and persist a theme
```

### `bone.submit(text)`

Queues `text` for the frontend to submit like typed input. When the app is idle
it submits immediately; mid-turn it waits in the input queue (shown as `Q:` in
the status bar) and drains when the active turn ends. Works from any Lua context
— a tool, a command, or an autocmd — so a plugin can steer the agent without a
frontend handle. The text follows normal input rules (a leading `/` runs a
command). This is the primitive behind a `/btw`-style steering command.

### `bone.api.ui` — drawing UI from Lua

Lua draws UI by emitting view updates. Floats render as panes; a status line
appends to the native status bar; highlights recolor the live theme.

```lua
bone.api.ui.open_float({
    id = "help",                 -- required
    title = "Help", lines = { "text" },
    width = 40, height = 10,
    anchor = "center",           -- top_left | top_right | bottom_left | bottom_right | center
    col = 0, row = 0,             -- signed offsets from the anchor
    z = 0, border = false,
})
bone.api.ui.set_lines(id, lines)         -- replace a float's lines
bone.api.ui.close(id)
bone.api.ui.set_statusline(id, segments) -- segments: { {text, fg?, align?}, ... }
bone.api.ui.set_highlight(name, color)   -- color string, or nil to reset
bone.api.ui.term_width()                  -- terminal columns (80 when headless)
```

**`set_statusline(id, segments)`** — each segment is `{ text, fg?, align? }`,
where `fg` is a color string and `align` is `"left"` (default), `"center"`, or
`"right"`. Right-aligned segments are drawn at the right edge of the status
row; left and center segments extend the native bar. Replacing the same `id`
updates it; the segments persist until changed.

**`set_highlight(name, color)`** — recolors a named highlight group live. The
group `name` is one of the [theme](#theme) field names (`user_msg`,
`user_msg_bg`, `input_border`, `status_text`, `approval_safe`, …); `color` is a
hex/named string, or `nil` to reset that group to its default. This is the
runtime counterpart to the boot-time `bone.theme` table — use it to color the
input border, user messages, or status text in response to events.

```lua
-- Tint the input border while a turn is running.
bone.on("turn_start", function() bone.api.ui.set_highlight("input_border", "#e0a050") end)
bone.on("turn_end",   function() bone.api.ui.set_highlight("input_border", nil) end)
```

## Config, Theme, and Keymaps

`~/.bone-rust/config.yaml` is the canonical source for declarative settings. Use
`/config` for supported scalar changes, `bone.settings.get/set/reset` from Lua,
or edit YAML directly. The daemon validates and resolves the file, then sends
one complete settings snapshot to each frontend. `init.lua` may wire runtime
keymaps and select themes, but it must not define competing settings tables.

The file is created automatically on first boot. Its default shape is:

```yaml
version: 1

general:
  approval: safe                 # safe | danger
  show_reasoning: false

ui:
  input:
    preset: null                 # custom | lines | box | filled
    prefix: null
    show_prefix: true
    horizontal_padding: null
    vertical_padding: null
    fill: null
    border:
      horizontal: null
      vertical: null
      top_left: null
      top_right: null
      bottom_left: null
      bottom_right: null
  status_show_model: true
  status_show_approval: true
  status_show_tokens_curr: true
  status_show_tokens_in: true
  status_show_tokens_out: true
  status_show_tokens_total: true
  status_show_queue: true
  status_show_spinner: true
  status_show_timer: true
  spinner_style: braille
  spinner_text: thinking
  spinner_custom: ""
  spinner_speed: 0
  spinner_text_rotate: true
  spinner_text_speed: 0

theme:
  palette: {}
  shell: {}
  syntax: {}
  highlights: {}

keymaps:
  bindings: []
```

Null or omitted input fields use the selected preset's defaults. Input layout is
rendered natively, so Unicode wrapping, cursor placement, autocomplete, viewport
sizing, and small-terminal clipping remain consistent.

### Theme

Theme modules live at `lua/themes/<name>.lua`, return a settings table, and are
listed/loaded with `bone.theme.list()` and `bone.theme.load(name)`. Loading a
theme validates it, records its name and resolved values in `config.yaml`, and
reloads the module on the next boot.

```lua
-- lua/themes/nord.lua
return {
  palette = { accent = "#88c0d0", good = "#a3be8c", error = "#bf616a" },
  thinking = "accent",
}
```

Most users only need palette values. Shell, syntax, and exact UI-role overrides
use separate maps:

```yaml
theme:
  palette:
    accent: "#8cdcdc"
    good: "#78b373"
    warn: "#d7ba7d"
    error: "#e05050"
    selection: "#303030"
  shell:
    program: "#b4c896"
    flag: "#96b4dc"
    string: "#c8aa78"
  syntax:
    comment: "#6a9955"
    keyword: "#569cd6"
    function_name: "#dcdcaa"
  highlights:
    user_msg: { fg: fg, bg: selection }
    input_border: border
    tool_error: error
```

Colors may be hex (`#RRGGBB`) or named (`white`, `black`, `red`, `green`,
`yellow`, `blue`, `magenta`, `cyan`, `gray`, `darkgray`, `lightred`,
`lightgreen`, `lightyellow`, `lightblue`, `lightmagenta`, `lightcyan`). Missing
keys use native defaults.

### Keymaps

Bindings are a single ordered list. YAML bindings persist string actions:

```yaml
keymaps:
  bindings:
    - { key: "<C-p>", action: toggle_panes }
    - { key: "<S-Tab>", action: cycle_approval_mode }
    - { key: "<C-a>", action: cursor_to_start }
    - { key: "<C-e>", action: cursor_to_end }
    - { key: "<C-v>", action: paste_image }
```

`bone.keymap.set(key, rhs)` adds runtime bindings from `init.lua` or a plugin. A
string `rhs` may be a built-in action, a slash command, or prompt text; a
function may return any of those strings or `nil` for no action.

```lua
bone.keymap.set("<C-p>", "toggle_panes")
bone.keymap.set("<C-r>", "/review")
bone.keymap.set("<C-g>", "summarize these changes")
bone.keymap.set("<C-b>", function() return "/usage" end)
```

Keys and actions must not be empty, and each key may be bound only once.

Built-in actions:
  - `toggle_panes` — show/hide the bottom pane area
  - `cycle_approval_mode` — rotate through approval modes
  - `cursor_to_start` — move cursor to start of line
  - `cursor_to_end` — move cursor to end of line
  - `paste_image` — paste clipboard image as attachment (hardcoded to <C-v>, <A-v>, and <C-S-v> when no Lua binding is set)

## Plugin System

Plugins live in `lua/plugins/<name>/init.lua`. They must be loaded explicitly from `init.lua`:

```lua
bone.plugin.load("tokyonight")
bone.plugin.install("user/repo")      -- git clone
bone.plugin.install("/local/path")    -- symlink or copy
bone.plugin.remove("tokyonight")
bone.plugin.list()
bone.plugin.update("tokyonight")
```

Plugins do not auto-run. Repeated `load` is a no-op.

## File Layout

```
<config-dir>/
  config.yaml                  -- canonical approval/UI/theme/keymap settings
  init.lua                     -- optional Lua behavior and orchestration
  command-policy.yaml          -- shell command safety tiers
  AGENTS.md                    -- Bone-owned reference; refreshed by each build
  .setup.json                  -- onboarding selection/marker
  config/
    general.yaml               -- `/config` page schema and compaction settings
    status.yaml                -- `/config` page schema for canonical status settings
    providers.yaml             -- LLM provider entries
    tools.yaml                 -- tool enable/disable toggles
    commands.yaml              -- command enable/disable toggles
  memory/                       -- optional catalog /memory extension data
    global.md                   -- global user preferences
    projects/<cwd-key>.md       -- project-scoped preferences
    inbox.jsonl                 -- queued explicit preference signals
    state.json                  -- processing checkpoint
  lua/
    tools/
      my_custom_tool.lua       -- user-created or installed via /catalog
    commands/
      config.lua               -- seeded default
      compact.lua              -- optional catalog command
      usage.lua                -- optional catalog command
      memory.lua               -- optional catalog command
      my_custom_command.lua    -- user-created
    lib/
      banner.lua               -- seeded startup banner implementation
      history.lua              -- seeded default
      ui/                      -- seeded default UI helpers
    themes/
      my_theme.lua             -- returns a theme settings table
    plugins/
      tokyonight/
        init.lua
```

Legacy root files `memory.md` and `memory.last_run` may remain after the
one-time catalog migration; they are not deleted. Bone refreshes `AGENTS.md`
from the bundled reference at startup. Other seeded Lua files are created on
first launch and do not overwrite existing files. Catalog tools/commands are
installed only when selected during onboarding or via `/catalog`.

## Tool vs Command

- **Tool** — The LLM calls as a function with typed args. Returns a string result. Good for integrations, searches, state management, TUI panes.
- **Command** — User invokes `/name [args]`. Returns a string injected as prompt. Good for workflows, reviews, templates, content generation.
