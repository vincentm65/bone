# compact

Keeps long sessions inside the model's context: the older part of a
transcript is replaced by a summary a model writes, and the latest turns stay
word for word.

```sh
cp -r examples/plugins/compact ~/.bone/plugins/
```

- `/compact` in the TUI compacts the session on screen.
- When a model call fails because the context is too long, it compacts and
  retries by itself (`auto = false` turns that off).
- `bone.config.compact = { keep = 2, provider = "cheap", auto = true }`:
  turns to keep, which provider writes the summary.

The session file keeps everything (compaction writes a checkpoint), so the
full history is still on disk. It uses `bone.session` (messages, compact),
`bone.model`, a `request_error` hook, and `bone.rpc` (the TUI's `/compact`
calls the core half's `compact` function).
