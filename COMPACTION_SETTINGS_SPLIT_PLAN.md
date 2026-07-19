# Extension Settings and Compaction Split Plan

## Goal

Give every Lua command the same small settings contract:

1. Declare a namespaced page once at module load.
2. Read resolved values from `ctx.settings` in handlers and hooks.
3. Let the daemon own validation, persistence, protocol updates, and UI snapshots.

`/compact` is the first consumer, not a special case. Bone must contain no command-specific settings code except isolated legacy migration.

## Command author experience

A future catalog command should need only this:

```lua
bone.settings.register({
    namespace = "example",
    title = "Example",
    fields = {
        { key = "enabled", label = "Enabled", type = "bool", default = true },
        { key = "limit", label = "Limit", type = "number", integer = true, default = 10, min = 1, max = 100 },
    },
})

bone.command.register({
    name = "example",
    handler = function(ctx)
        local enabled = ctx.settings.get("example.enabled")
        local limit = ctx.settings.get("example.limit")
        -- command behavior
    end,
})
```

That registration automatically creates the TUI and web settings page. Commands do not read page YAML, parse defaults, write `config.yaml`, or add Rust/UI branches.

Initial scope is deliberately small:

- Field types: `string`, `number`, `bool`, and `enum`; `number` may require an integer.
- Scalar defaults and values only.
- `ctx.settings.get(path)` is read-only.
- UI writes use the daemon protocol. Add command-side mutation later only when a real command needs it, through the same daemon service.

## Ownership

### Bone

- A daemon-global settings service shared by all conversation actors.
- Schema registration, validation, collision detection, and reload staging.
- Canonical value persistence in `config.yaml`.
- One resolved page snapshot used by Lua, TUI, and web.
- Protocol-authoritative mutation and broadcasts.
- Provider context-window metadata and legacy compaction migration.

### Catalog commands

- Their namespace, labels, fields, and defaults.
- Their policy and internal constants.
- Reads through `ctx.settings.get` only.

### `/compact`

Expose only:

- `compact.auto`, default `true`.
- `compact.trigger_percentage`, default `80`, bounds `50..95`.
- `compact.context_window_tokens`, default `100000`, minimum `10000`.

Keep budgets, prompts, retries, reserves, and compression targets as constants in `compact.lua`.

## Registration and lifecycle

`bone.settings.register` runs only while a Lua module is loading. Bone records the canonical module path automatically; command authors do not pass it.

The loader builds a fresh staged registry on every extension reload:

1. Start an empty registry.
2. Stage registrations separately for each module.
3. Commit that module only if it finishes successfully.
4. Reject duplicate namespaces from different modules and built-in namespace collisions.
5. After discovery completes, atomically publish the completed registry. A fatal loader failure keeps the previous registry active.

This makes update, failure, and uninstall behavior deterministic. Removing a module removes its page on reload, but never deletes its persisted values.

Validate namespace and key syntax, supported types, enum options, defaults, and numeric bounds. Add optional `min` and `max` to the shared page field representation. The daemon repeats value validation on every write; client validation is only immediate feedback.

## Daemon-global settings service

Do not put the authoritative store only inside a per-conversation `ExtensionManager`. `bone serve` has multiple conversation actors, so they must share one service containing:

- The current registry snapshot.
- Canonical `BoneSettings` values.
- The resolved settings pages.
- Atomic load/update/save operations.
- A revision number and broadcast channel.

Every actor and client reads the same revision. A successful mutation broadcasts a fresh resolved settings snapshot to all attached actors/clients, not only the conversation that initiated it.

`ctx.settings.get(path)` reads from this service. Lua handlers never cache settings.

## Persistence

Persist values, not schemas, in canonical `config.yaml`:

```yaml
extensions:
  compact:
    auto: true
    trigger_percentage: 80
```

Add a declared `extensions` scalar map to `BoneSettings` and retain top-level `deny_unknown_fields`. Preserve unknown extension namespaces and keys across load/save so uninstall/reinstall restores values.

Resolution rules:

- A valid persisted value wins.
- A missing value resolves to the registered default.
- An invalid persisted value resolves to the default and emits a warning, but is not silently rewritten.
- A namespace without an active schema remains stored but is absent from settings pages.

No catalog-owned `config/<command>.yaml`, uninstall hook, or lifecycle database is needed.

## One settings-page pipeline

Define one resolved protocol page model containing namespace, title, fields, resolved values, bounds, and validation metadata.

- Registry pages are produced from Lua declarations plus `config.yaml` values.
- Existing YAML pages are adapted into the same resolved model during migration.
- `ctx.config.get_pages()`, TUI, and web consume the daemon snapshot instead of rereading page files in a render loop.
- TUI and web render fields generically.
- The web bridge stops parsing or writing `general.yaml` for represented fields.

Add `RuntimeCommand::SetSetting { path, value }`. Its daemon handler validates against the active schema, atomically updates the latest `config.yaml`, updates the shared service, increments the revision, and broadcasts the new snapshot. Remote and in-process clients use this identical path.

## Compaction prerequisites

Percentage triggering needs maximum context capacity, not current token use. Add context-window capacity to provider/model metadata and resolve it for each request. Expose the result read-only as `ctx.model.context_window_tokens` (or `nil` when unknown).

`compact.lua` prefers provider metadata and falls back to its user-configurable `context_window_tokens` setting, which defaults to `100000`.

## Legacy migration

Bone owns one isolated, idempotent migration because Bone seeded the old fields. Run it before publishing registered defaults to Lua:

1. Lock and reload the latest `config.yaml`.
2. For each missing destination, derive `extensions.compact.auto`, `extensions.compact.trigger_percentage`, and `extensions.compact.context_window_tokens` from `config/general.yaml`.
3. Never overwrite a destination key that is already present, even when invalid.
4. Atomically save `config.yaml`.
5. Only after that succeeds, remove all legacy compaction field blocks from `general.yaml` atomically.
6. If cleanup fails, retry it at the next startup; the existing destination values make the migration safe to repeat.

Migration mapping:

- `auto = true` for legacy percentage mode, or for absolute mode with a positive `auto_compact_tokens`; otherwise `false`.
- Copy an in-range numeric `compact_trigger_percentage`; otherwise use `80`.
- Copy a legacy `compact_context_window_tokens` value of at least `10000`; otherwise use `100000`.
- Treat a page default as its value when no explicit value exists, matching current behavior.
- Preserve unrelated General settings.
- Drop old implementation budgets; they have no public destination.

Remove the old fields from the built-in General seed in the same release so backfill cannot restore them. Keep the migration reader permanently as upgrade-only code for users who skip releases; do not expose legacy aliases to commands.

This ordering removes the need for a catalog fallback and avoids the ambiguity between “missing persisted value” and “resolved registered default.”

## Delivery order

### 1. Build the generic settings foundation

- Add the extension scalar map and resolved page protocol types.
- Build the daemon-global store, staged registry, generic `SetSetting`, revisions, and broadcasts.
- Add `bone.settings.register` and read-only `ctx.settings.get`.
- Discover schema from every installed module, including disabled commands. Command enablement controls dispatch, not schema discovery.
- Adapt existing YAML pages to the resolved page model.
- Render all supported fields generically in TUI and web.

### 2. Add compaction prerequisites and migration

- Add provider/model context-window metadata and the request-scoped Lua value.
- Run the durable migration before extension boot.
- Remove compaction fields from the General seed and user pages.
- Remove compaction-specific web and bridge branches.

### 3. Publish catalog `/compact`

- Register its three fields.
- Read only `ctx.settings` and prefer provider capacity over the configured fallback.
- Move private controls into Lua constants.
- Publish and verify catalog manifest/checksum changes.

## Required tests

### Registry and persistence

- Valid registration succeeds; invalid schema, collisions, and failed-module partial registrations do not enter the live registry.
- Failed full reload leaves the previous registry active.
- Disabled installed commands retain their pages; uninstall hides a page; reinstall restores its values.
- Scalar types, integer constraints, enums, and numeric bounds are enforced by the daemon.
- Unknown namespaces and keys survive load/save.
- Invalid dormant values resolve to defaults with a visible warning and are not rewritten.

### Shared runtime and protocol

- A mutation from one conversation is immediately visible to another conversation and to every connected client.
- Rejected writes change neither disk, revision, nor live snapshot.
- In-process and remote clients receive equivalent pages and errors.
- TUI and web render a new test command's page without command-specific code.

### Migration and compaction

- Existing destination keys win; missing keys migrate independently.
- Migration handles page defaults and invalid legacy values, preserves unrelated fields, and survives failure between the two file writes.
- Legacy-disabled users remain disabled after migration.
- `/compact` exposes exactly three settings and has no General-page fallback.
- Manual compaction works independently of automatic compaction settings.
- Missing provider capacity uses the configured compaction fallback.

## Completion criteria

- A new command can declare a settings page entirely in Lua and read it through `ctx.settings` with no Rust, TUI, web, or YAML-page change.
- All actors and clients use one daemon-owned settings revision and mutation path.
- Bone has no compaction policy, schema, defaults, or UI branches outside isolated migration and generic provider metadata.
- `config.yaml` is the sole durable store for extension values.
- The built-in General page no longer contains or restores compaction fields.
