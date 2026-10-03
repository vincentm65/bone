# skills

Registers the skills in folders you name and adds TUI commands for them.

```sh
cp -r examples/plugins/skills ~/.bone/plugins/
```

```lua
-- ~/.bone/core.lua
bone.config.skill_dirs = { "~/.bone/skills" }   -- each <dir>/<name>/SKILL.md
```

- `/skills` lists them (name and description).
- `/skill name task` writes "Use the name skill: task" into the prompt, with
  completion of skill names.

The model already sees the list and loads skills through the `skill` tool
once any are registered (that part is the core's); this plugin only finds
the folders and adds the commands.
