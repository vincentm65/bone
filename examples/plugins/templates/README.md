# templates

Prompt templates: reusable prompts with arguments, expanded in the core so
every client gets the same text, each one a `/name` command in the TUI. The
core has no notion of templates; this plugin adds it.

```sh
cp -r examples/plugins/templates ~/.bone/plugins/
```

```lua
-- ~/.bone/core.lua
bone.config.template_dirs = { "~/.bone/prompts" }
```

A template is a markdown file, say `~/.bone/prompts/review.md`:

```markdown
---
description: Review a file
args: path, focus
---
Review {{path}}, paying attention to {{focus}}. List problems by severity.
```

`/review src/main.rs "error handling"` puts the expanded text in the prompt,
to edit and send. Templates registered in `core.lua` with Lua bodies work the
same way. Commands are recreated when the core reloads; names that are
already commands are skipped.

In `core.lua` (or another plugin) the registry is available directly:

```lua
bone.template.register { name = "fix", description = "Fix an issue", args = { "issue" },
                         body = "Fix issue $1. Notes: $@" }
bone.template.register { name = "status", body = function(args, ctx)
  return "The working tree:\n" .. bone.system("git status --short", { cwd = ctx.cwd }).stdout
end }
```

In a string body `$1`…`$9` are the words of the argument text (`"double
quotes"` keep words together), `$@` and `$ARGUMENTS` all of it, and `{{name}}`
the word in that position of `args`. A function body gets `{ raw, argv,
<name> = ... }` and `ctx = { session_id, cwd }`, and may wait. Also
`bone.template.expand(name, text, ctx)`, `list()` and `unregister(name)`. The
TUI half uses `bone.rpc.call("templates.list")` and `"templates.expand"`.

