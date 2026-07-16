# Customization Simplification Plan

## Status

Proposed.

## Executive summary

Bone currently exposes customization through `/config`, several YAML files,
`init.lua`, runtime Lua APIs, extension directories, `AGENTS.local.md`, and
memory. Some settings have more than one owner, so users must understand
precedence and startup behavior before they can predict the result.

The target model is:

> Settings live in one canonical declarative configuration; executable behavior
> lives in purpose-specific Lua files.

For most users, customization should mean:

1. Run `/config` to change settings.
2. Run `/catalog` to install capabilities.
3. Edit `AGENTS.local.md` to provide agent instructions.
4. Use a lightweight `init.lua` to wire keymaps, subagents, plugins, themes, and
   shared Lua modules.

`init.lua` is the startup wiring layer, not an implementation dumping ground.
Substantial logic belongs in `lua/tools/`, `lua/commands/`, `lua/themes/`,
`lua/plugins/*/init.lua`, or shared `lua/lib/` modules.

## Problem

Bone's customization primitives are individually useful, but their ownership is
unclear:

- General settings are defined in `core/src/config/pages/general.yaml`.
- Status settings are defined in `core/src/config/pages/status.yaml`.
- `bone.config` overlaps approval, status, and input settings in
  `core/defaults/AGENTS.md:910`; some fields use different granularity, so they
  cannot be merged mechanically.
- Static themes and keymaps only exist through `init.lua` at
  `core/defaults/AGENTS.md:956` and `core/defaults/AGENTS.md:1035`.
- Plugins are installed and loaded imperatively from `init.lua` at
  `core/defaults/AGENTS.md:1063`.
- Tools and commands already have purpose-specific directories, while hooks and
  subagents generally depend on `init.lua` registration.
- Lua configuration is snapshotted at boot, while `/config` writes YAML at
  runtime. A value changed interactively can therefore conflict with a Lua
  startup value on the next launch.

This creates four user-facing problems:

1. There is no single obvious place to configure Bone.
2. Similar settings use different formats and APIs.
3. Conflicting values can appear not to persist.
4. `init.lua` is both a settings file and an arbitrary-code entry point, so its
   purpose is difficult to explain.

## Goals

- Make `/config` the primary customization interface.
- Give every declarative setting one canonical owner.
- Preserve Lua as a powerful extension and automation language.
- Make extension locations predictable by capability type.
- Ensure the daemon owns resolved runtime configuration for all frontends.
- Support deterministic reload behavior where technically safe.
- Perform one verified local cutover without preserving permanent migration code.
- Reduce documentation to a small, stable mental model.

## Non-goals

- Reimplement extension policy in Rust.
- Remove Lua or prevent arbitrary advanced startup code.
- Force all users to store their configuration identically internally.
- Make provider credentials or shell policy hot-reloadable before it is safe.
- Redesign tools, commands, hooks, plugins, and subagents into one generic
  extension primitive.
- Copy Neovim's configuration model exactly.

## Design principles

### 1. Values versus behavior

- Static values belong to canonical declarative configuration.
- Executable or conditional behavior belongs to Lua.
- If a setting can be represented in `/config`, it must not require Lua.

Examples:

- A blue input border is configuration.
- A border that changes color while a turn runs is a Lua hook.
- A fixed reviewer agent definition is configuration.
- Agents generated dynamically from workspace metadata are Lua behavior.

### 2. One owner per setting

There must be one persisted value and one resolved runtime value for each
setting. The one-time local cutover may translate old values before the new
runtime is activated, but production code never retains competing sources.

### 3. The daemon is authoritative

The core runtime resolves configuration once and sends the resolved state to the
TUI, web UI, and headless clients. Frontends must not independently merge YAML,
Lua snapshots, and local defaults.

### 4. Lua remains the extension language

The config editor, extension discovery, and high-level customization policy
should remain Lua/config-first where possible. Rust should provide storage,
validation, protocol updates, lifecycle primitives, and native TUI/widget
rendering; config-editor behavior remains in Lua.

### 5. One clean cutover

There is one known user installation. Before switching formats, record its
currently effective values, back up the config directory, translate those values
to `config.yaml`, and verify startup behavior. A temporary local script is
acceptable if it saves time, but it is deleted after the cutover. Bone does not
ship a general migration framework, mixed-format resolver, or permanent legacy
readers.

### 6. Prefer the final model over compatibility machinery

Preserve `init.lua` as an advanced behavior hook, but manually update this known
installation where old static APIs are used. Do not retain aliases, precedence
rules, diagnostics, or schema complexity solely for hypothetical external users.

## Target user experience

### Ordinary users

```text
/config                 Change models, UI, keybindings, safety, agents, and extensions
/catalog                Browse and install tools, commands, and plugins
AGENTS.local.md          Add persistent instructions for the agent
/memory                 View or update learned global/project preferences
```

Ordinary setup may include a short top-level `init.lua` for wiring. A plugin's
own `lua/plugins/<name>/init.lua` is its implementation entry point and follows
the same rule: substantial plugin logic stays within its plugin directory.

### Advanced users

```text
lua/tools/*.lua          Model-callable typed tools
lua/commands/*.lua       User-invoked slash commands
lua/hooks/*.lua          Event handlers and turn-shaping behavior
lua/plugins/*/init.lua   Packaged extensions
lua/lib/*.lua            Shared Lua modules
init.lua                 Optional dynamic startup orchestration
```

### Proposed `/config` hierarchy

```text
General
Models
Appearance
  Input
  Theme
  Status
  Spinner
Keybindings
Extensions
  Tools
  Commands
  Plugins
  Agents
Context & Memory
Safety
```

The editor should support search, reset-to-default, conflict warnings, and a
"show config key" action for users who prefer file editing.

## Target storage model

Use one clean, versioned, values-only `config.yaml` as the canonical v1
configuration. The daemon exposes one logical configuration API; a future schema
version may add explicit includes if the file becomes unwieldy.

```yaml
version: 1

general:
  approval: safe
  show_reasoning: false

model:
  provider: anthropic
  name: claude-sonnet-4-6

ui:
  input:
    style: box
    prefix: "> "
  status:
    model: true
    approval: true
    tokens: true
    timer: true
  spinner:
    style: braille
    text: thinking

theme:
  palette:
    accent: "#8cdcdc"
    good: "#78b373"
    error: "#e05050"

keymaps:
  normal:
    "<C-p>": toggle_panes
  insert:
    "<C-a>": cursor_to_start

extensions:
  tools:
    web_search: true
  commands:
    compact: true
  plugins:
    tokyonight: true

agents:
  reviewer:
    description: Review changes for regressions
    provider: anthropic
    model: claude-sonnet-4-6
    approval: safe
    prompt_file: prompts/reviewer.md
```

The example is illustrative, not a reduced feature contract. The approved schema
must cover the complete existing theme surface, including palette, shell, syntax,
and highlights, and every current keymap mode and action. Phase 0 defines explicit
mappings between canonical names such as `normal` and legacy names such as `n`.
No existing static customization may require `init.lua` merely because it was
omitted from this example.

Schemas and defaults remain bundled with Bone and may continue to be authored as
YAML. User-owned files contain values only; they do not duplicate labels, field
types, option lists, and defaults.

Keep specialized content separate where merging it would reduce clarity:

- `command-policy.yaml` remains the shell safety policy.
- `AGENTS.local.md` remains user-authored agent instructions.
- `memory/` remains managed scoped memory.
- Provider secrets may continue to support environment variables or a dedicated
  secret mechanism.

## Role of `init.lua`

Document `init.lua` as:

> Lightweight startup wiring for keymaps, subagents, plugins, themes, and shared
> Lua modules. Put substantial implementations in purpose-specific `lua/`
> files.

Valid uses include:

- `bone.keymap.set(...)` declarations.
- `bone.subagent.register(...)` declarations.
- `bone.theme.load(...)` and `bone.plugin.load(...)` calls.
- `require(...)` calls for startup hooks or shared modules.
- Small conditional setup based on `bone.cwd` or environment metadata.

Keep substantial tools, commands, themes, plugin implementations, and shared
logic out of `init.lua`. Persistent scalar values remain in canonical YAML.

New installations may seed this small wiring file. Its presence is a normal
steady state; it should stay short and readable.

## Runtime configuration API

Replace the distinction between a mutable Lua snapshot and persisted config with
one API backed by the canonical store:

```lua
bone.settings.get("general.approval")
bone.settings.set("general.approval", "danger")
bone.settings.set_session("general.approval", "danger")
```

Requirements:

- `get` reads the resolved canonical value.
- `set` validates, atomically persists, updates every runtime consumer, and emits
  a protocol update, or leaves the old value active and returns an error.
- `set_session` creates an explicit non-persistent override with a documented
  lifetime and precedence.
- Frontends receive updates from the daemon rather than rereading files.
- Unsupported hot reloads persist only after explicit confirmation and return a
  clear restart-required result; they must not leave disk and runtime appearing
  to agree when they do not.
- Every setting defines its persisted owner, runtime owner, consumers, apply and
  rollback behavior, reload class, redaction rules, and protocol visibility.

`bone.settings` becomes the only settings API. Remove the overlapping static
configuration meanings of `bone.config`, `bone.api.config`, and `ctx.config` in
the cutover rather than maintaining compatibility aliases. Update the known
`init.lua` manually: persistent values move to `config.yaml`, while dynamic
per-session choices use `bone.settings.set_session`.

## Loading order

Use one documented startup sequence:

1. Seed bundled schemas and extension libraries.
2. Load and validate canonical declarative configuration.
3. Initialize providers, command policy, and runtime state.
4. Initialize Lua with read access to resolved settings and runtime metadata.
5. Run optional `init.lua`; setup performed here is available to subsequently
   discovered files.
6. Load declaratively enabled plugins not already loaded by startup code.
7. Auto-load `lua/tools/*.lua`.
8. Auto-load `lua/commands/*.lua`.
9. Auto-load `lua/hooks/*.lua`.
10. Register declarative subagents not already registered by startup code.
11. Publish one resolved frontend state snapshot.

Keep `init.lua` before discovered extension files because dynamic startup code may
initialize globals, modules, plugins, hooks, tools, or commands that those files
consume. If a future post-load startup hook is useful, introduce it under a
distinct name and contract rather than changing `init.lua` ordering.

Duplicate registrations and invalid values produce actionable diagnostics naming
both sources. The first valid owner remains active; no registration silently
shadows another.

## Implementation phases

### Phase 0: Confirm contracts and capture the current installation

Before changing storage:

- Inventory every field read from YAML, `bone.config`, theme snapshots, keymap
  snapshots, CLI overrides, and runtime state.
- Record the currently effective value of every setting in the known installation,
  including intentional dynamic behavior in `init.lua`.
- For every setting, define its canonical key, runtime owner, consumers, apply and
  rollback behavior, reload class, redaction rules, and protocol visibility.
- Define the complete versioned schema, including exhaustive theme/keymap mappings
  and handling for extension-owned schema fragments.
- Confirm `config.yaml` atomic replacement, locking/concurrent-write behavior,
  backup behavior, and forward-compatible unknown-key handling.
- Decide which old `init.lua` behavior remains dynamic and rewrite it against the
  final API; all static values move to canonical configuration.

Deliverable: approved schema, consumer matrix, reload classification, a snapshot
of the current effective configuration, and a concrete local cutover checklist.

### Phase 1: Establish canonical storage and runtime ownership

- Introduce the versioned, values-only `config.yaml` format and generic validated,
  atomic read/write operations.
- Introduce a daemon-owned resolved settings store that reads only canonical
  configuration.
- Route `/config` reads and writes through the store.
- Update every runtime consumer transactionally or return a restart-required
  result without presenting stale runtime state as applied.
- Publish the initial resolved state and incremental setting updates to attached
  frontends over protocol.
- Remove frontend-side precedence merging.

Deliverable: one persisted owner and one runtime owner for every setting. The
production code has no mixed-format resolver or per-key legacy ownership.

Validation:

- Approval, input, and status changes persist and do not revert after restart.
- TUI, web, and headless sessions resolve the same values.
- Enforcement consumers, including approval gating, observe the same committed
  value shown by clients.
- A failed apply rolls back cleanly or remains explicitly pending restart.
- Concurrent writes cannot truncate, lose, or partially apply configuration.
- No startup flicker from applying a second Lua configuration snapshot.

### Phase 2: Expand `/config`

Add declarative pages for:

- Theme and highlights.
- Keybindings.
- Plugins and catalog enablement.
- Fixed subagent definitions.
- Remaining ordinary UI settings.

These pages define bundled schema/UI metadata, while user values are written only
to canonical `config.yaml`. Do not create additional page-format user files.
Keep the editor implementation in Lua and expose only generic validated config
operations from Rust.

Deliverable: ordinary settings can be represented without adding new static
assignments to `init.lua`; declarative plugin and subagent activation becomes
complete in Phase 3.

### Phase 3: Separate executable behavior

- Add automatic loading for `lua/hooks/*.lua`.
- Keep automatic loading for tools and commands.
- Load enabled plugins from canonical configuration.
- Load fixed subagents from canonical configuration.
- Add diagnostics identifying the source file of duplicate or invalid
  registrations.

Deliverable: each extension type has one predictable location, and ordinary
static customization uses a top-level `init.lua` only for lightweight wiring.

Validation:

- `init.lua` setup executes before discovered extension files.
- Declaratively enabled plugins and fixed subagents load without editing
  `init.lua`.
- Duplicate registration diagnostics identify both owners and preserve the first
  valid owner.

### Phase 4: One-time local cutover and cleanup

- Stop Bone and make a timestamped backup of the existing config directory.
- Translate the Phase 0 effective-value snapshot into canonical `config.yaml`.
  Do this manually or with a temporary repository-local script.
- Rewrite the known `init.lua`: move static values to `config.yaml`, retain only
  dynamic behavior, and use `bone.settings.set_session` where appropriate.
- Start Bone and compare resolved settings, providers, approval behavior, theme,
  keymaps, extensions, and agents against the Phase 0 snapshot.
- Fix the canonical file or rewritten Lua until the comparison passes.
- Remove page-format value readers, Lua static-config snapshots, legacy settings
  APIs, compatibility branches, and any temporary migration script.
- Stop creating `init.lua` during onboarding and runtime startup. Track onboarding
  completion independently and verify that a missing file remains absent.
- Keep the backup until the new system has been used successfully; rollback means
  restoring the backup and the pre-cutover binary or commit.

Deliverable: the known installation runs entirely on the final model, and the
repository contains no permanent migration or legacy-config machinery.

### Phase 5: Reload and diagnostics

Build on the existing all-or-nothing extension reload command to add a unified
`/reload` flow with scoped actions:

```text
/reload config
/reload extensions
/reload all
```

Phase 5 depends on the per-setting reload classification approved in Phase 0;
no unclassified setting is eligible for live reload.

- Reload declarative values classified as live or next-turn and notify all
  clients; report next-session and restart-required values without applying them.
- Rebuild extension registrations deterministically.
- Clearly identify provider or policy changes that still require restart.
- Show source paths and validation failures without silently falling back.

Deliverable: predictable customization iteration with daemon-authoritative,
deterministic reload semantics.

## Cutover policy

- The cutover is a local maintenance operation, not a user-facing feature.
- Back up the full config directory before changing it.
- Use the recorded effective values rather than trying to infer arbitrary Lua.
- A temporary script may transform repetitive values, but it is not shipped or
  retained after verification.
- Remove old readers and settings APIs in the same change that activates the new
  canonical path.
- Preserve tool and command file locations, native override restrictions, and
  `init.lua` ordering; only static configuration ownership changes.
- Catalog installation must not edit `init.lua`.
- Roll back by restoring both the backup and the pre-cutover binary or commit.

## Testing plan

### Unit tests

- Schema/default/value resolution.
- Canonical-key validation and unknown-key handling.
- Values-only serialization and round trips.
- Settings API validation, persistence, rollback, and reload classification.
- Initial and incremental protocol state.
- Extension discovery and deterministic loading order.

### Integration tests

- `/config` changes are visible to the daemon, every enforcement/runtime consumer,
  and all attached frontends.
- Restart preserves the selected approval, input, theme, and status settings.
- A failed live apply rolls back or is reported as pending restart without a false
  success state.
- `init.lua` setup remains available before discovered tool and command files
  execute.
- A fresh installation may seed a lightweight `init.lua` wiring file.
- Legacy static settings APIs are absent after the cutover.
- Plugin, tool, command, hook, and subagent enablement survives restart.
- Invalid configuration fails visibly without corrupting the previous file.
- Concurrent writes are serialized without lost updates or partial files.
- Remote clients receive the same resolved state as local clients.

### Local cutover verification

Before deleting the backup and temporary conversion script, compare the converted
installation against the Phase 0 snapshot:

- Provider/model selection and credentials resolve correctly.
- Approval enforcement, input, status, spinner, theme, and keymaps match.
- Enabled tools, commands, plugins, hooks, and subagents load exactly once.
- Dynamic `init.lua` behavior still runs in the expected order.
- Restart and `/reload` retain the expected state.
- The runtime reads no legacy page values or static Lua snapshots.

## Success criteria

The project is successful when:

- A new user can discover all ordinary settings through `/config`.
- Documentation can explain customization with the values-versus-behavior rule.
- Every setting has one canonical persisted owner.
- Changing a setting does not unexpectedly revert after restart.
- TUI, web, and headless clients use the same resolved configuration.
- Installing a catalog item never requires editing `init.lua`.
- Configuring static themes, keymaps, plugin enablement, and fixed agents
  uses top-level `init.lua` only for lightweight wiring; plugin implementations remain Lua.
- Advanced users retain arbitrary Lua startup and extension capabilities.
- The one-time cutover is verified against the captured installation and can be
  rolled back from its backup.

## Risks and mitigations

### Scope expansion

Moving every setting at once could become a broad rewrite.

Mitigation: define the complete canonical schema first, implement one runtime path,
and cut over the known installation only after the core settings path is verified.

### Breaking existing Lua configuration

The known `init.lua` may contain dynamic construction or depend on startup
ordering.

Mitigation: capture its current behavior, move static values manually, preserve
the pre-discovery ordering contract, and verify the rewritten file during the
local cutover. Do not build a compatibility layer.

### Recreating configuration policy in Rust

A unified store could tempt implementation of all page behavior in Rust.

Mitigation: retain bundled YAML schemas and the Lua `/config` UI. Rust owns only
validated storage, runtime synchronization, and protocol transport.

### Unsafe hot reload

Providers, credentials, command policy, and running extensions may not be safe
to replace during active work.

Mitigation: classify settings as live, next-turn, next-session, or
restart-required and expose that classification in `/config`.

### One large file becoming difficult to maintain

A single physical file may become unwieldy as extensions add settings.

Mitigation: use one atomic, values-only `config.yaml` for v1 and keep the schema
organized by stable top-level sections. If real-world size becomes a problem, a
future schema version may add explicit includes without changing the logical API.

## Phase 0 decision gates

The Phase 0 deliverable must resolve these before Phase 1 begins:

1. How provider secrets are represented, redacted, and excluded from protocol
   snapshots and backups where appropriate.
2. Whether declarative subagents use an `agents` section or one file per agent.
3. How plugin-specific schemas register namespaced `/config` pages without taking
   ownership of core keys.
4. Which protocol messages carry the initial resolved settings snapshot,
   incremental updates, apply failures, and restart-required state.

`init.lua` retains its current name and pre-discovery ordering. A future post-load
hook, if needed, receives a distinct name and contract.

## Recommended first milestone

Implement Phases 0 and 1 only:

1. Capture the known installation's effective settings and dynamic Lua behavior.
2. Approve canonical keys, reload classes, and Phase 0 decision gates.
3. Add the versioned canonical storage primitive.
4. Add a daemon-owned resolved settings store and transactional apply path.
5. Route `/config`, runtime consumers, and frontend state through it.
6. Validate approval, status, and input behavior against canonical test fixtures.

This establishes the final runtime path before expanding the editor or performing
the one-time local cutover.
