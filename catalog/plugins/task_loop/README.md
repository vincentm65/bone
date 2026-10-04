# tasks

A task list in a panel beside the chat, kept between sessions.

```sh
cp -r examples/plugins/tasks ~/.bone/plugins/
```

- `/task fix the parser` adds a task, `/tasks` shows or hides the panel
  (it reopens at startup if it was open), `/task` gives it the keyboard.
- In the panel: `up`/`down` select, `enter` or `space` toggles done, `s` puts
  the task in the prompt, `d` deletes it, `x` clears done ones, `esc` goes
  back to the prompt.

It is a small example of `bone.ui.panel` (a docked panel with its own keys),
`bone.plugin.state` (a table saved in `~/.bone/state/tui/tasks.json`) and
`bone.prompt`.
