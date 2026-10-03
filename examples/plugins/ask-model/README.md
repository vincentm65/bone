# ask-model

`/ask question` asks a model on the side, outside the session, and streams
the answer into a pager. `/ask` alone asks it to explain the latest answer in
the session more simply.

```sh
cp -r examples/plugins/ask-model ~/.bone/plugins/
```

`bone.o.ask_provider = "cheap"` sends questions to another
`bone.config.providers` entry. It uses `bone.model.complete` (the core's
`model/complete`), `bone.chat.items` and `bone.ui.pager`.
