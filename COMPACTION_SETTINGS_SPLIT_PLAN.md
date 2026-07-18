# Catalog Extension Settings Split Plan

## Goal

Make `/compact` a fully catalog-owned extension while Bone owns only generic settings registration, persistence, protocol transport, and UI rendering.

## Target ownership

### Bone

- Generic namespaced settings schema registration for Lua extensions.
- Schema validation and namespace collision checks.
- Canonical value persistence in `config.yaml`.
- Daemon-authoritative settings snapshots and mutations.
- Generic TUI and web rendering of registered fields.
- One-time migration of legacy built-in compaction values.

### `bone-catalog/commands/compact.lua`

- Compaction settings schema and defaults.
- Manual and automatic compaction policy.
- Prompts, internal budgets, validation, retries, and failure messages.
- Temporary reads of legacy settings during rollout only.

## Public compaction settings

Expose only:

1. `compact.auto` — enable automatic compaction; manual `/compact` remains available.
2. `compact.trigger_percentage` — context-capacity threshold, default `80`.

Keep these as internal constants in `compact.lua`:

- Recent-context keep budget.
- Summarizer input budget.
- Checkpoint budget.
- Generation allowance.
- Context safety reserve.
- Retry count and compression targets.

Require provider context-window metadata for automatic percentage triggering. If capacity is unavailable, disable automatic compaction with a clear reason rather than falling back to a guessed absolute threshold.

## Lua API

Add a generic registration API:

```lua
bone.settings.register({
    namespace = "compact",
    title = "Compaction",
    fields = {
        {
            key = "auto",
            label = "Automatic compaction",
            type = "bool",
            default = true,
        },
        {
            key = "trigger_percentage",
            label = "Compact at context capacity",
            type = "number",
            default = 80,
            min = 50,
            max = 95,
        },
    },
})
```

Extend the existing `bone.settings.{get,set,reset}` API with registration, and add request-scoped `ctx.settings.{get,set,reset}` access backed by the same daemon-owned store. Catalog handlers should read `ctx.settings.get("compact.auto")`; clients and Lua must not maintain separate copies.

Registration requirements:

- Validate namespace, keys, field types, defaults, options, and numeric bounds.
- Add optional numeric `min` and `max` to the shared `ConfigField`/protocol field representation; absent bounds remain valid for existing YAML pages.
- Bind each namespace to the canonical path of the Lua module that first registered it. Reject a second module claiming that namespace; allow idempotent re-registration by the same module during reload.
- Reserve built-in namespaces and reject extension collisions with them.
- Enforce bounds and field types in the daemon, not only in clients.
- Keep schema runtime-owned; do not persist extension schemas into user configuration.

Registration is transactional per module: if a module fails after registering, discard that module's schema so a broken extension cannot leave a stale settings page.

`bone.settings.register`, `ctx.settings`, and the registry do not exist yet; only `bone.settings.{get,set,reset}` exists. Implement registration during installed Lua module boot and keep one daemon-owned in-memory registry. Do not create a second client-side source of truth.

## Persistence

Persist only values in canonical `config.yaml`:

```yaml
extensions:
  compact:
    auto: true
    trigger_percentage: 80
```

Add a declared `extensions` field to `BoneSettings`, for example a nested map of namespace to key to generic scalar value. Keep `BoneSettings`'s top-level `deny_unknown_fields`: `extensions` becomes a known top-level key while misspelled peers remain errors. Preserve every unknown namespace and key across load/save even when no active schema recognizes it.

Registered extension fields are limited to the declared scalar field types, so persistence does not need to preserve arbitrary YAML tags or formatting.

Behavior:

- Installing the Lua file causes it to register its schema on the next runtime reload, making the settings page visible.
- Disabling the command keeps settings visible because the module remains installed and loaded.
- Uninstalling removes the Lua file; the next reload omits its schema but leaves its value map untouched.
- Reinstalling reloads the module, re-registers the schema, validates the dormant values, and restores them.
- Invalid dormant values fall back to registered defaults with a warning; they are not silently rewritten.

No catalog uninstall hook or separate lifecycle database is needed: schema presence follows successful Lua registration, while durable values are independent of schema presence.

Do not introduce a catalog-owned `config/compact.yaml`; that would continue coupling schema and values and complicate uninstall behavior.

## Protocol and clients

The daemon resolves built-in and extension schemas, combines them with persisted values, and exposes one protocol-authoritative settings snapshot and mutation API.

Extend the resolved frontend state with generic settings pages containing namespace, title, field schema, resolved value, and validation metadata. Add protocol types for those pages, a namespaced `SetSetting { path, value }` runtime command, and a refreshed settings event/snapshot. The daemon handler validates against the active registry, persists `config.yaml` atomically, and broadcasts the refreshed resolved state. Do not let clients edit YAML directly.

The existing `CustomConfigPage` and `ctx.config.get_pages()` path is the current generic page mechanism, but it reloads page YAML from disk on each call. Reuse its field model where practical, add bounds, and initially return a merged daemon-resolved view of legacy YAML pages plus registry pages. Remove disk reloads from the settings UI hot path. Do not leave two long-term generic settings systems: built-in legacy pages must either be adapted into the same resolved-page representation or explicitly remain a temporary adapter until canonical migration is complete.

- TUI renders all pages generically from the resolved frontend state instead of only deserializing fixed `BoneSettings` fields.
- Web UI renders all pages generically from the same daemon snapshot.
- Replace the web bridge's direct `general.yaml` parser/writer with protocol calls.
- Remove all compaction-specific field names and rendering branches from the web client and bridge.
- Remote and in-process clients must have identical settings behavior.

## Legacy migration

Bone owns migration because Bone originally seeded the legacy fields.

Migrate values from the user's `config/general.yaml` page into `config.yaml`:

- Never overwrite an already-present `extensions.compact.auto` or `extensions.compact.trigger_percentage`; migrate each missing destination independently.
- Infer `extensions.compact.auto` as `true` when legacy mode is `percentage`, or when legacy mode is `absolute` and `auto_compact_tokens` is a positive integer. Otherwise preserve the old disabled behavior with `false`.
- Copy `compact_trigger_percentage` only when it is numeric and within the new schema bounds; otherwise use `80`.
- Treat page defaults as legacy values when no explicit `value` exists, matching current `ctx.config.get` behavior.
- Preserve unrelated General fields and values.
- Drop implementation-only values with no public destination. In particular, the context-window override is intentionally removed because automatic compaction now requires provider metadata, and `auto_compact_keep_messages` is superseded by the internal recent-context budget.
- Stop backfilling legacy fields from the built-in seed, remove them from the built-in page, and remove their field blocks from existing user pages after their supported values have been persisted.
- Make migration crash-safe and idempotent: atomically write `config.yaml` first, then atomically clean `general.yaml`; never delete source fields if the destination write fails. A single atomic transaction across both files is not required. If cleanup fails after the destination is durable, retry cleanup on later starts whenever legacy blocks remain.
- Retain the isolated idempotent Bone migration reader so users can skip the compatibility release; remove catalog runtime fallbacks after the compatibility window.

Legacy keys recognized only by migration/cleanup:

- `auto_compact_tokens`
- `compact_trigger_mode`
- `compact_trigger_percentage`
- `compact_context_window_tokens`
- `compact_keep_tokens`
- `compact_input_tokens`
- `compact_checkpoint_tokens`
- `compact_generation_tokens`
- `compact_safety_tokens`
- `auto_compact_keep_messages`
- `compact_summary_tokens` (deprecated, not in the current seed page, but accepted by the catalog fallback)

Do not create permanent runtime aliases for removed implementation settings. The isolated upgrade migration may continue recognizing their names.

## Rollout

### Phase 1: Generic Bone support

- Add a typed generic `extensions` map to `BoneSettings` while retaining strict validation for other top-level fields.
- Extend the shared field schema and protocol representation with numeric bounds and build the daemon-owned schema registry.
- Add `bone.settings.register` plus request-scoped `ctx.settings` reads and mutations backed by the same store.
- Persist namespaced extension values in `config.yaml` without dropping unknown dormant namespaces or keys.
- Add generic settings pages to resolved frontend state, `SetSetting` to `RuntimeCommand`, and a refreshed settings event/snapshot.
- Adapt `ctx.config.get_pages()` and `/config` to a merged registry view so the old and new paths do not diverge; stop reloading YAML on every UI iteration.
- Add generic TUI and web field rendering; remove direct web YAML mutation for fields represented by the registry.
- Keep legacy General compaction fields temporarily.

### Phase 2: Catalog adoption

- Update `compact.lua` to register the two public settings.
- Read namespaced settings first.
- Temporarily fall back to the exact legacy `ctx.config.get("general", key)` values when a namespaced value is absent.
- Move all implementation budgets to constants and keep the deprecated `compact_summary_tokens` read only inside the temporary fallback.
- Publish the catalog manifest/checksum and test install/update behavior.

### Phase 3: Bone migration and cleanup

- Migrate supported legacy values without overwriting existing namespaced values.
- Remove compaction fields from the built-in General page and stop `backfill_fields` from restoring them.
- Clean migrated compaction field blocks from existing user `general.yaml` files only after `config.yaml` is durable.
- Remove compaction-specific web UI and bridge code, including the current mismatch where the web UI writes deprecated `compact_summary_tokens` while the seeded page defines `compact_checkpoint_tokens`.
- Preserve dormant extension values when `/compact` is not installed.

### Phase 4: Remove catalog fallback

After one compatibility release in which Bone performs the durable migration:

- Remove reads from `ctx.config.get("general", ...)` in `compact.lua`.
- Remove deprecated key parsing and compatibility branches from the catalog extension.
- Keep Bone's isolated idempotent upgrade migration for users who skip releases; it must not expose aliases to runtime code.

## Validation

### Core

- Schema registration accepts valid namespaced fields and rejects collisions, invalid definitions, out-of-range defaults, and partial registrations from failed modules.
- Extension values round-trip through `config.yaml`.
- Unknown extension namespaces and keys survive load/save.
- Invalid active values resolve to defaults with a warning but are not silently rewritten.
- Uninstalled extension schemas are absent while values remain.
- `SetSetting` rejects unknown paths, wrong types, and out-of-range values without changing disk or the live snapshot.
- In-process and daemon-connected clients receive identical resolved settings and updates.
- Migration preserves unrelated General settings, preserves existing namespaced values, handles page defaults, and is crash-safe and idempotent.

### Catalog

- Installing `/compact` exposes exactly two settings.
- Disabling it does not erase values.
- Uninstalling hides settings; reinstalling restores values.
- Manual compaction works when automatic compaction is disabled.
- Automatic compaction clearly reports unavailable context-window metadata.
- Legacy fallback works during the compatibility phase and is later removed.

### UI

- TUI and web render the registered fields without compaction-specific code.
- Both clients validate numeric bounds and display daemon errors consistently.
- No compaction controls appear when the extension is absent.

## Completion criteria

- Bone has no compaction schema, defaults, policy, or rendering branches outside the isolated legacy migration.
- `core/src/config/pages/general.yaml` contains only core General settings, and existing user pages are cleaned after migration.
- Web and TUI have no compaction-specific settings branches.
- `compact.lua` declares its own two-field schema and owns all implementation constants.
- `config.yaml` is the sole durable store for compaction values.
- Legacy fields are migrated once per installation, catalog fallbacks are removed, and no permanent runtime compatibility aliases remain.
