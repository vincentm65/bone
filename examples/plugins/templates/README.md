# templates

Registers prompt templates from folders and turns each into a `/name`
command in the TUI.

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
