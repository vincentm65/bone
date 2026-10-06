# How the chat looks: views, tool rendering, custom items

The transcript is a list of items. Each kind has a *view* — a Lua function
that returns the item's lines; return `nil` to hide the item. Rust keeps the
session data, wraps text, caches, scrolls and paints; you only decide what
each item's lines are.

Reference: `docs/lua.md` "Views: how each item looks" and "Chat data".
Source: `crates/bone-tui/src/chat.rs` (the items), `render.rs` (drawing and
caching), `lua.rs` (the API). The standard views are
`runtime/lua/bone/ui/views.lua`; `examples/plugins/style/` is a complete
alternative look built only from this API.

## Views

```lua
bone.ui.views.reasoning = function(item, ctx)
  return { { { "∴ ", "Dim" }, { item.text, "Reasoning" } } }
end
bone.ui.views.user = function(item, ctx)
  return bone.text.wrap({ { item.text, "UserMessage" } }, ctx.width, { first = "› ", pad = "UserMessage" })
end
```

| Kind | Item fields |
|---|---|
| `user` | `text` |
| `reasoning`, `assistant` | `text`, `streaming`; `assistant` also `usage = { input, output }` |
| `tool` | `id`, `name`, `arguments` (decoded), `raw_arguments`, `output` (nil while running), `is_error`, `done`, `live` (output so far while it runs), `started_at` (ms), `duration_ms`, `usage` |
| `notice` | `text`, `error` |
| `queued` | `id`, `text`, `mode` (`"steer"` or `"next"`), `position` |

Every item also has `kind`, `index` (its place in the chat now, from 1) and
`key`: a name that stays the same as items come and go around it, for keeping
your own state per item (which ones are expanded). `ctx` is
`{ width, region, prev = { kind } }` — `region` is `"chat"` in the transcript,
and `prev` lets a view decide spacing.

Assigning a view redraws the chat; a view that errors is reported once and its
items fall back to plain text until it is redefined.

## Refreshing

Chat views are cached: an item is drawn again when it changes, when the width,
options, views or colors change, and when chat data the view read through
`bone.chat.items`, `bone.chat.item`, `bone.chat.count` or `bone.chat.turns`
changes. For time, call `bone.chat.refresh_in(ms)` inside the view (a clock, a
spinner; `bone.now()` is milliseconds). Otherwise `bone.chat.redraw(index)`
draws one item again and `bone.chat.redraw()` all of them;
`bone.ui.refresh()` also redraws regions and windows.

## Tool calls

`views.tool` draws the frame (status marker, label rows, output), and
`bone.ui.tool_views[name](item, ctx)` can supply the content for one tool as
`{ title = line, lines = { line, ... } }` (`title` may also be a list of
lines; `lines` sit indented under it). Return `nil` for the built-in content
(`bone.ui.tool_content`).

```lua
-- a compact one-line summary for a specific tool
bone.ui.tool_views.word_count = function(item, ctx)
  return { title = { { "word_count", "ToolName" }, { " -> " .. item.output, "ToolOutput" } } }
end
```

The standard UI draws tools like: a label (`shell cmd`, `read_file path
(lines 1-20, 20 read)`, `edit_file path (-1 | +2)`), shell output and errors
in a `│ ╰` gutter cut to its first and last two rows, edits as a numbered
diff. `bone.o.tool_detail` (`ctrl+t`) picks how much: `"summary"` (the
default) folds each stretch of calls between edits and failures into one line
such as `Read 3 files, ran 2 shell commands`, `"rows"` is the above, `"full"`
shows every output in full.

## Your own items

```lua
local id = bone.chat.add("build", { text = "building…" })   -- an item of your own
bone.chat.update(id, { text = "built", ok = true })         -- merge fields, redraw it
bone.chat.remove(id)
```

`kind` is your name for it (lowercase, not a built-in kind) and
`bone.ui.views[kind]` draws it, getting `fields` plus `kind`, `index` and
`key`; without a view its `text` shows. It is listed by `items`, can be
clicked and scrolled to, and stays where it was put while the answer streams
after it. It belongs to the TUI: the core never sees it, and reloading the
chat drops it, so keep anything lasting in `bone.state`.

## Reading chat data

```lua
bone.chat.items({ kind = "tool", name = "shell", turn = 2 })  -- items as views see them, plus `turn`
bone.chat.item(4)                       -- by index, or nil
bone.chat.count({ running = true })     -- tools still running, text still streaming
bone.chat.turns()                       -- one entry per user message
bone.chat.session()                     -- the chat on screen
bone.chat.messages(function(messages, err) ... end)  -- the stored transcript, from the core
```

`items(opts)` filters: `kind` (one, or a list), `around` (an item index: only
the unbroken stretch of items of `kind` around it), `name` (tool name),
`turn`, `running`, `error`, `from`/`to` (item indexes, inclusive), then
`first`/`last` keep the first or last N matches. `turns(opts)` entries:
`{ index, text, first, last, items, tools, tool_errors, running, outcome,
error }`.

## Clicking

The `mouse` event gets `index` and `line` when it is over a chat item, plus
`region`, `row` and `col` for the layout leaf under it:

```lua
local expanded = {}
bone.on("mouse", function(ev)
  if ev.action == "down" and ev.button == "left" and ev.index then
    local item = bone.chat.item(ev.index)
    if item and item.kind == "tool" then
      expanded[item.key] = not expanded[item.key]
      bone.chat.redraw(ev.index)
    end
    return true
  end
end)
```

With `bone.chat.redraw(index)` and your own state per item (`item.key`), that
is enough for click-to-expand.

## Moving the chat

```lua
bone.chat.view()                        -- { top, height, rows, follow, first, last }
bone.chat.scroll_to(4, "center")        -- "top" (default), "center" or "bottom"
bone.chat.scroll(-5)                    -- rows (negative is up), or "top" / "bottom"
bone.chat.at(x, y)                      -- the item at a screen cell: { index, line } or nil
```