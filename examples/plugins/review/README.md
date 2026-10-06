# review

The files the session on screen has changed, in a panel under the chat, and
quick ways to look at them and to ask the model about them.

```sh
cp -r examples/plugins/review ~/.bone/plugins/
```

- `/review` shows or hides the panel, or briefly notifies if nothing changed.
  It lists each file touched by `edit_file`/`write_file` in this session,
  with edits, turns, and failed edits; it follows the session as it goes.
- In the panel: `up`/`down` select, `enter` writes a review request for that
  file into the prompt (selected, so you can retype it or send it as is),
  `g` shows `git diff` for it in a pager (run in the background), `esc` goes
  back.
- `/review all` writes a review request for every changed file.

It uses `bone.chat.items` (read-only chat data, with each item's turn),
`bone.ui.panel`, `bone.prompt` (setting and selecting text) and `bone.job`.
