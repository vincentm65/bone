# Extension API

Bone embeds Lua 5.4 for tools, commands, settings, themes, keymaps, event hooks,
and UI components. Startup wiring lives in `init.lua`: Bone reads it from the
config root and, when present, from `lua/init.lua`, running the root file first.
If neither exists, the runtime behaves as before and a blank root `init.lua` is
created. Errors in startup Lua are warnings; core continues without that wiring.

Use the namespaced APIs below. Keep `init.lua` as wiring and put implementations
in `lua/tools/`, `lua/commands/`, `lua/themes/`, `lua/plugins/<name>/`, or
`lua/lib/` as appropriate. `require` resolves against `lua/lib/` as the shared
module root, so `require("ui.menu")` loads `lua/lib/ui/menu.lua`; inside a
plugin, `require` additionally resolves against that plugin's own package
directory (see [Plugins](#plugins)). Native helper binaries
belong in the never-auto-loaded `lua/helpers/` directory, exposed as
`bone.helpers_dir`.

## Reloading

Bone fingerprints the init files plus every lowercase `*.lua` file recursively
under `lua/`. An uppercase stem such as `Foo.lua` is ignored, matching tool and
command discovery. It checks that fingerprint at interaction boundaries,
immediately before the next prompt or slash command, rather than polling or
watching the filesystem.
In a multi-conversation daemon, one actor claims the changed fingerprint and
notifies its peers only after accepting the replacement.

Reload builds a fresh Lua VM and tool registry before replacing the active one.
If startup, tool, or command source fails, Bone keeps the previous VM and does not
retry that exact fingerprint; save another Lua change to retry. Installing an
extension through `/catalog` (or toggling one in `/config`) still forces a reload
even when the fingerprint is unchanged, while retaining the previous VM if the
candidate is broken.

## Registration

```lua
bone.tool.register({ name = "...", description = "...", parameters = schema,
    safety = "read_only", execute = function(params, ctx) ... end })
bone.command.register("name", { description = "...", handler = function(args, ctx) ... end })
bone.keymap.set("<C-p>", "toggle_panes")
bone.theme.load("name")
bone.settings.define("namespace", { title = "...", fields = { ... } })
bone.on("event_name", function(event, ctx) ... end)
```

The global metadata includes `bone.version`, `bone.cwd`, `bone.config_dir`,
`bone.helpers_dir`, `bone.agent_depth`, `bone.headless`, `bone.model`, and
`bone.provider`. Logging is available through `bone.log.info`, `bone.log.warn`,
and `bone.log.error`.

## Plugins

A plugin is an installable package: a directory `lua/plugins/<name>/init.lua`
(the entry point) that registers capabilities — tools, commands, keymaps,
themes, settings, hooks — through the same namespaced APIs. Plugins are
distributed through the catalog as `kind = "plugin"` entries, installed under
`lua/plugins/<name>/`, and discovered at VM build. No hand-editing of a shared
`init.lua` is required; the plugin's own `init.lua` runs automatically.

Boot runs plugins after tools and commands, in sorted directory order, each with
the same `_settings_owner` rollback behavior as other startup Lua: a failing
plugin is reported and skipped without taking down the runtime. Capabilities
inherit their plugin's state — disabling a plugin skips its `init.lua`, so none
of its tools or commands register.

A plugin may `require` its own submodules. While its `init.lua` runs, the
package directory is appended to `package.path`, so `require("x")` resolves
against `<pkg>/x.lua`, `<pkg>/lib/x.lua`, and `<pkg>/lib/x/init.lua`. The global
`lua/lib` keeps name priority, so a plugin's `ui.menu` can never shadow a seeded
helper.

A plugin can also ship theme files under `lua/plugins/<name>/themes/`. They
appear in `bone.theme.list()` alongside the user's `lua/themes/` entries, and a
user theme with the same name wins.

Remove a plugin through the catalog screen (or by deleting its directory). Enable
or disable it without deleting files through the `plugins.<name>` setting in the
canonical settings YAML; a disabled plugin stays installed (checksums and updates
are still tracked) but its `init.lua` is not run, and a reload applies the change.

> **Trust.** Bone Lua is not sandboxed. A plugin runs with the same authority as
> your own `init.lua`, including filesystem, `ctx.fs`, and shell access through
> registered tools. Installing or updating a plugin therefore asks for explicit
> consent, and every file's `sha256` is verified before anything is written. Only
> install plugins you trust.

## Submitting turns

`bone.submit(text, options)` and `ctx.conversation.submit(text, options)` queue a
prompt for the owning conversation's next idle turn. By default the frontend
shows the submitted text as an automated user row. Set `display` to a string to
show a shorter label, or to `false` to suppress that live row:

```lua
bone.submit("Continue internal work.", { display = false })
```

Presentation does not change the model-facing or persisted prompt text.

## Context

Tool `execute(params, ctx)` and command `handler(args, ctx)` receive a context
with the following core groups:

- `ctx.config_dir`, `ctx.cwd`, `ctx.call_id`, and `ctx.log.*`;
- read-only filesystem queries in `ctx.fs.*`, plus `ctx.read_file` and
  `ctx.create_file`;
- approved shell through `ctx.shell` and `ctx.shell_streaming`;
- bounded waits and binary-safe codecs under `ctx.time.*` and `ctx.codec.*`;
- `ctx.ui.*` for notifications, panes, view diffs, key input, and terminal width;
- `ctx.runtime.info()` and request-scoped model metadata;
- session/conversation inspection through `ctx.session.*`, `ctx.conversation.*`,
  and read-only `ctx.db.query`;
- session-scoped state in `ctx.state.*`;
- typed tool calls through `ctx.tools.definitions()` and `ctx.tools.call()`;
- delegated work through `ctx.agent.*`, and managed processes through
  `ctx.process.*`; and
- daemon-owned settings/configuration access through `ctx.settings.*` and
  `ctx.config.*`.

`ctx.agent.run_stream(prompt, opts)` keeps the blocking run contract while
forwarding live callbacks. In addition to `on_started`, `on_status`, tool, usage,
finish, and failure callbacks, `on_text_delta(text)` and
`on_reasoning_delta(text)` receive each streamed chunk in order. A plugin can
use these callbacks to replace the lines of a stable shared panel with
`bone.api.ui.set_lines`; use `bone.on("panel_action", ...)` for semantic
`focus`, `expand`, or `close` handling. The existing native activity panel
remains authoritative for `ctx.agent.spawn` jobs; extensions should not create a
second panel for the same background job.

Lifecycle event handlers receive the same complete bounded context. They run as
managed daemon work rather than inline in a frontend event loop: handlers execute
sequentially by descending `priority`, preserving registration order among equal
priorities. Priority defaults to `0`; set it with
`bone.on(name, handler, { priority = 100 })`. Handlers may block on bounded APIs
and native approval requests, observe cancellation, and have a per-registration
timeout. The default is 30 seconds; override it with
`bone.on(name, handler, { timeout_ms = 5000 })`. Values are clamped to 100 ms–1
hour. A timeout or handler error fails open and the next registration runs, except
that the first successful `{ block = true }` result still stops a blockable
`tool_call`.

All file writes, shell commands, and nested tool calls still pass through Bone's
native approval and policy path. This includes `ctx.tools.call` issued by a
lifecycle handler. The Lua API grants no unrestricted OS access or approval
bypass. `ctx.ui.key` is available only when the hook has both a live runtime
event channel and a key-reply registry; headless hooks otherwise retain the
non-interactive UI APIs.

## Tools

A tool's `name`, description, JSON-Schema `parameters`, safety level, and
`execute` function define its agent contract. Native tools cannot be overridden.
Use `read_only` for inspection and safe external reads; use `danger` only when
the operation genuinely mutates or executes untrusted-side effects.

Tools return a string to the agent. A JSON return envelope may additionally
contain `content`, serialized `state`, and a `pane` with `source`, `title`,
`lines`, `visible_rows`, and `scroll`. A pane with the same source replaces the
previous pane; empty lines remove it. `ctx.ui.pane` can upsert panes while the
tool runs. Clean up panes when work completes; cancellation automatically clears
host-owned panes.

`ctx.tools.call(name, args, { approval = "safe" | "read_only" | "danger" })`
uses the normal registry and approval pipeline. Lua tool nesting is bounded.

### Native `read_file`

Use the native `read_file` tool for source and text inspection instead of shell
commands such as `cat`, `head`, `tail`, or `sed`. A single literal `path` keeps
the normal line-window behavior: `start_line` is 1-based and `max_lines`
defaults to 1,000. Images with `png`, `jpg`, `jpeg`, `gif`, or `webp`
extensions are returned as image attachments.

Bulk reads are additive. Keep the required scalar `path` and use `paths` for
additional literals or globs; `exclude` accepts one pattern or an array of
patterns to omit. A glob in `path`, a glob in `paths`, or either additive field
selects bulk mode. Bulk reads do not accept `start_line` or `max_lines`; read a
specific literal separately when a line range is needed.

Bulk expansion honors `.gitignore`, skips hidden paths and `.git`, and applies
`exclude` patterns after matching. Results are canonicalized and de-duplicated.
At most 50 matched files are considered, and the aggregate returned text is
capped at 100 KiB between complete files; the result reports returned, matched,
and skipped counts. Text output is still bounded per file at 50 KiB. Matching
image files remain attachments. Missing literal paths get bounded spelling
repair and did-you-mean suggestions, while special files such as devices,
streams, directories, and other non-regular files are rejected safely.

When the same unchanged literal window is read twice in the live session, the
second result may be a one-use `Unchanged` stub because the content is already
in context. The stub is consumed on use, so the next identical read returns the
content again. Deduplication is per file for bulk reads and does not replace
image, empty, out-of-range, or uneditable-only responses.

## Commands and return actions

Commands are invoked as `/name args`. A command can return:

- `nil` to handle the command without submitting a prompt;
- a string to inject as the next prompt/output;
- a display table with `display`, `reply`, or `content` and `submit = false`; or
- an action table to request a supported daemon state mutation.

The `conversation.replace` action replaces the model-facing transcript with
validated `user`, `assistant`, and `tool` messages. Core recomputes the context
estimate, persists a checkpoint, and keeps complete SQLite display history.
`ctx.conversation.append(messages)` validates the same message roles, queues a
persistent append, and returns `(true, nil)` when an authoritative Driver/session
owner accepts the request. It returns `(false, reason)` when unavailable or no
valid messages were supplied. Lua never writes transcript storage directly; the
Driver or daemon applies each queued append once, updates model-facing state, and
skips durable writes while the session is incognito.
Commands and `before_turn` handlers may also return `system_prompt_append`, a
transient `turn_message`, and a per-turn `tool_filter` allow-list. These fields
shape the request; they do not mutate the global configuration.

Core command names remain protected and cannot be overridden. A command with
settings should register its namespace through `bone.settings.define` rather
than writing YAML directly.

## before_turn return values

`before_turn` may return the same validated request-shaping values as commands:

- `{ action = "conversation.replace", messages = {...} }` replaces the model-facing transcript;
- `system_prompt_append` and `turn_message` are transient per-turn values; and
- `tool_filter` is a per-turn allow-list of tool names.

Handlers run in effective priority order, preserving registration order for equal
priorities. Their `system_prompt_append` values accumulate; other fields use the
normal hook merge rules.

Compaction is implemented in catalog Lua rather than as a dedicated Rust action.
Lua owns thresholds, history selection, prompts, repair, checkpoint formatting,
continuation wording, notices, and replacement policy. It supplies explicit messages,
tools, and an optional positive `max_tokens` to `ctx.llm.complete`, which performs
exactly one private provider request with no agent/tool loop. During `before_turn`,
`ctx.conversation.system_prompt()` starts with the normal request's base system prompt;
a successful `system_prompt_append` is folded into it before the next registered handler
runs. Other Driver hooks reflect the system message at the front of current provider
history, including appends already applied to that history. Messages returned by
`ctx.conversation.history()` include their `created_at` metadata, and private requests
apply the same provider-only message/tool timing context as normal requests. Private
text is not surfaced, and returned tool calls are
exposed to Lua without execution. Usage and cancellation are accounted by the
authoritative Driver turn or daemon command path. Transcript mutation occurs only
when the validated `conversation.replace` result is applied and persisted by the
daemon.

Private completion is intentionally unavailable during `bone run` slash-command
expansion: that path has no durable conversation or command usage owner. It remains
available to `before_turn` hooks during the headless agent turn itself.

## Events

```lua
bone.on("tool_call", function(event, ctx)
    if event.name == "shell" then
        -- return { block = true, reason = "..." } to stop this call
    end
end)
```

Core events are `session_start`, `session_end`, `message`, `tool_call`,
`tool_result`, `mode_change`, `turn_start`, `token_usage`, `turn_end`, and
`before_turn`. Higher-priority handlers run first; equal priorities retain
registration order. The first blocking result stops a `tool_call`; handler errors
fail open and do not block. Ordinary lifecycle return actions are ignored;
`tool_call` blocking and the documented `before_turn` request-shaping values are
the exceptions. Hook-originated bounded API work is treated as nested work and is
not recursively redispatched through the same lifecycle chain. Registration in a
subagent is ignored by default; pass `{ subagents = true }` when that is
intentional.

`bone.api.emit(name, payload)` is the synchronous compatibility event path, not
daemon-managed lifecycle dispatch. Managed callbacks suppress emits of core
lifecycle names to prevent recursion, while custom compatibility events remain
emittable.

## UI API

Lua UI is declarative. `ctx.ui.pane` and `ctx.ui.apply` emit shared view updates;
`bone.api.ui` provides lower-level floats, status-line segments, and live
highlights. Stable component ids make updates idempotent. A frontend renders the
same protocol `ViewDiff` whether the update came from Rust or Lua.

```lua
bone.api.ui.open_float({ id = "help", title = "Help", lines = { "text" },
    width = 40, height = 10, anchor = "center",
    placement = {
        slot = "right", order = 0, size_hint = 32,
        pinned = false, closable = true,
    } })
bone.api.ui.set_lines("help", { "updated" })
bone.api.ui.set_placement("help", { slot = "bottom", order = 0 })
bone.api.ui.set_statusline("stats", { { text = "ready", align = "right" } })
bone.api.ui.set_highlight("input_border", "#e0a050")
bone.api.ui.close("help")
```

`open_float` accepts the optional semantic `placement` fields `slot`
(`left`, `right`, `top`, `bottom`, or `overlay`), `order`, `size_hint`,
`pinned`, and `closable`. `owner` may also be supplied for compatibility, but
managed plugin code is stamped with its registered plugin owner instead. A
plugin's declarative `ctx.ui.pane` and `ctx.ui.apply` float updates receive the
same ownership stamp. Owned panels are removed when that plugin is successfully
reloaded or disabled; owner-less legacy panels are preserved. `set_placement`
updates only an existing float and returns `true` on success. Placement is
semantic protocol data: native clients may dock or float the panel, while exact
window geometry and layout preferences remain client-local. The TUI continues
to use its existing pane layout and ignores placement-only updates.

Plugins can receive native or other frontend panel actions through the managed
`panel_action` hook. The daemon sends a payload of the following shape:

```lua
bone.on("panel_action", function(event, ctx)
    -- event.panel_id, event.action, event.payload, event.request_id
end)
```

Panel actions are semantic and fire-and-forget; a handler can update shared UI
or conversation state, but must not assume a frontend-specific drawing API.

For debugging, use `bone.log.*`, inspect `ctx.runtime.info()`, enumerate tool
and subagent definitions, and use the protocol/event stream rather than relying
on frontend-local state. See [Agents](agents.md) for delegated execution and
[UI](ui.md) for frontend behavior.
