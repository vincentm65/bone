# testrun

Run the tests in the background and watch the output stream into a panel
under the chat.

```sh
cp -r examples/plugins/testrun ~/.bone/plugins/
```

- `/test` runs `bone.o.test_command` (default `cargo test`; set it in
  `tui.lua` with `bone.o.test_command = "pytest -q"`), `/test some command`
  runs that instead, and `/test cancel` stops it (the whole process group).
- The panel follows the output; scroll back with the usual keys once it has
  the keyboard (click it). `c` cancels, `f` puts the failing lines in the
  prompt for the model, `esc` goes back. The title shows running / passed /
  failed and the time.

It uses `bone.job` (a streamed, cancellable process), `bone.ui.panel` with
`follow`, a dynamic option and `bone.prompt`.
