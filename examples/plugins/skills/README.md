# skills

Skills: instructions for particular kinds of work (cutting a release,
writing a migration) that the model loads only when a task calls for them.
The core has no notion of skills; this plugin adds it.

```sh
cp -r examples/plugins/skills ~/.bone/plugins/
```

```lua
-- ~/.bone/core.lua
bone.config.skill_dirs = { "~/.bone/skills" }   -- each <dir>/<name>/SKILL.md
-- or register them yourself:
bone.skill.register { name = "style", description = "Our code style", content = "Use tabs. ..." }
bone.skill.register { name = "deploy", path = "~/work/deploy",            -- a folder with SKILL.md, or a .md file
                      enabled = function(ctx) return ctx.cwd:find("/work/") ~= nil end }
```

`SKILL.md` files start with front matter (`name`, `description`). While a
session has at least one skill (`enabled(ctx)` decides per session), the
system prompt gets a short "Skills" list (a `system` hook with priority 1000,
so your own `system` hooks run after it) and a `skill` tool returns a skill's
full text plus the paths of the other files in its folder. With no skills,
nothing changes. `bone.config.skills = { prompt = false }` or `{ tool = false }`
turns either part off. Also `bone.skill.load_dir(dir)`, `unregister(name)` and
`list()`.

In the TUI: `/skills` lists them and `/skill name task` writes "Use the name
skill: task" into the prompt (with completion of names). The TUI half gets the
list through `bone.rpc.call("skills.list")`.
