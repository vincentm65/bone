# Configuration system redesign plan

## Goal

Replace Bone's overlapping configuration files and frontend-specific persistence with one daemon-owned configuration system that remains highly extensible without producing one giant YAML file.

Bone Core must be the only authority for loading, validating, mutating, resolving, and persisting configuration. The TUI, web UI, headless runner, and remote clients must use the same protocol API and receive the same resolved state.

## Principles

- One configuration system does not require one physical file.
- Keep files separated by stable configuration domain, not by frontend page.
- Store user-selected values in YAML; do not store built-in UI labels, field types, or option lists there.
- Define built-in schemas and validation in Rust.
- Let Lua extensions register namespaced schemas for their own settings.
- Keep behavior in Lua and declarative values in YAML.
- Make the daemon the only config writer; frontends must not parse or edit config files.
- Resolve defaults, user values, and runtime overrides once in Core.
- Send complete resolved snapshots and incremental change events through the protocol.
- Preserve unknown extension namespaces so temporarily unavailable extensions do not lose settings.
- Validate before committing and write atomically.
- Make migrations explicit, versioned, idempotent, and covered by fixture tests.
- Do not combine this refactor with unrelated runtime or UI redesigns.

## Problems addressed by this redesign

Before this implementation:

- `config.yaml` is canonical for general, UI, status, theme, keymap, extension, and subagent settings, but not for every domain.
- `config/general.yaml` and `config/status.yaml` contain schemas plus legacy-looking `value` fields even though canonical values are routed through `config.yaml`.
- Legacy `config/providers.yaml`, `config/tools.yaml`, and `config/commands.yaml`, plus root `command-policy.yaml`, remain independent authorities.
- The web bridge reads and writes YAML directly instead of mutating daemon state through the protocol.
- The web bridge can update `general.yaml` values that Core ignores after `config.yaml` exists.
- Defaults and field knowledge are duplicated across Rust, seeded YAML, Lua, TUI code, and JavaScript.
- `~/.bone-rust` mixes configuration, Lua modules, credentials, databases, logs, cache files, memory, and other durable state.
- The phrase "config directory" can mean either the whole Bone root or its nested `config/` directory.

## Target ownership model

### Rust owns

- Built-in setting schemas.
- Defaults and validation.
- Cross-field invariants.
- Config loading and atomic persistence.
- Versioned migrations.
- Secret resolution interfaces.
- The merged, resolved in-memory configuration.
- Protocol commands, snapshots, and change events.
- Security-sensitive policy evaluation.

### Lua owns

- Tools, commands, themes, plugins, hooks, and advanced orchestration.
- Extension-specific setting schema registration.
- Runtime behavior derived from resolved settings.
- Optional custom config-page grouping and descriptions for extension settings.

### YAML owns

- User-selected declarative values.
- Provider definitions and credential references.
- Named subagent definitions.
- Extension values.
- Shell policy customizations.

### Frontends own

- Rendering config schemas received from Core.
- Collecting edits and sending typed mutations to Core.
- Ephemeral, frontend-local presentation state only, such as an open panel or temporary filter.

Frontends do not own persistent agent behavior, provider state, tool enablement, or approval settings.

## Target file layout

Long-term XDG layout:

```text
~/.config/bone/
├── config.yaml          # core settings, UI selections, theme selection, toggles
├── providers.yaml       # providers, models, endpoints, credential references
├── subagents.yaml       # named static subagent definitions and prompts
├── extensions.yaml      # namespaced extension values
├── command-policy.yaml  # shell safety policy and user overrides
├── init.lua             # optional advanced wiring
└── lua/
    ├── tools/
    ├── commands/
    ├── themes/
    ├── plugins/
    └── lib/

~/.local/share/bone/
├── conversations.db
├── memory/
└── goals/

~/.cache/bone/
└── catalog/
```

Do not perform the XDG move in the first implementation phase. First establish one authoritative config service while retaining `~/.bone-rust`; move paths only after configuration semantics and migrations are stable.

Interim layout:

```text
~/.bone-rust/
├── config.yaml
├── providers.yaml
├── subagents.yaml
├── extensions.yaml
├── command-policy.yaml
├── init.lua
├── lua/
├── data/
├── memory/
└── cache/
```

There is no nested user-facing `config/` directory in the target layout. Every configuration document is a peer with a clear domain name.

## Target document responsibilities

### `config.yaml`

Keep this file compact. It contains common settings and active selections:

```yaml
version: 2

general:
  approval: danger
  show_reasoning: false

ui:
  input:
    preset: filled
  status:
    show_model: true
    show_tokens_in: false
    spinner_style: braille

theme:
  name: catppuccin
  overrides: {}

tools:
  disabled: [browser, cron]

commands:
  disabled: []

keymaps:
  bindings: []
```

Do not serialize large resolved defaults or complete theme palettes unless the user explicitly overrides them. A selected theme name plus sparse overrides is sufficient.

### `providers.yaml`

Store provider definitions separately because they are numerous, independently managed, and may reference secrets:

```yaml
version: 1
active: codex
providers:
  codex:
    label: Codex
    handler: codex
    model: gpt-5.6
  local-3090:
    label: Local 3090
    handler: openai
    base_url: http://127.0.0.1:8081
    endpoint: /v1/chat/completions
    model: local
```

Prefer credential references over plaintext values:

```yaml
api_key: ${OPENROUTER_API_KEY}
```

Only a complete `${ENV_VAR}` scalar is resolved from the process environment at
runtime. Other strings, including partial interpolation such as
`prefix-${ENV_VAR}`, are preserved exactly. Plaintext credentials remain
supported.

A future keyring integration may use an explicit reference form, but it is not required for the initial refactor.

### `subagents.yaml`

Keep large prompts and named agent definitions out of common settings:

```yaml
version: 1
subagents:
  reviewer:
    description: Review code for correctness
    provider: codex
    model: gpt-5.6
    approval: safe
    enabled: true
    system_prompt: |
      Review verified correctness and regression risks.
```

### `extensions.yaml`

Store only values, grouped by extension namespace:

```yaml
version: 1
extensions:
  compact:
    auto: true
    trigger_percentage: 80
    context_window_tokens: 100000
```

An unavailable extension's values remain intact but inactive.

### `command-policy.yaml`

Keep security policy separate because it has a specialized structure, validation rules, and restart/reload implications. Core remains its only runtime consumer and mutation authority.

## Schema model

A setting definition should contain enough information for validation and generic frontend rendering:

```rust
SettingDefinition {
    path,
    value_type,
    default,
    label,
    description,
    group,
    enum_options,
    constraints,
    sensitivity,
    reload_behavior,
}
```

Built-in definitions live in Rust. They are not copied into user YAML files.

Lua extensions register definitions through a namespaced API:

```lua
bone.settings.define("compact", {
  title = "Compact",
  fields = {
    auto = {
      type = "bool",
      default = true,
      label = "Automatic compaction",
    },
    trigger_percentage = {
      type = "number",
      default = 80,
      min = 50,
      max = 95,
      label = "Trigger percentage",
    },
  },
})
```

Rules:

- Extension paths are always namespaced.
- Extensions cannot redefine built-in paths.
- Schema registration does not write defaults into YAML.
- Defaults are resolved in memory.
- Setting values are persisted only after the user changes them or migration requires them.
- Removing an extension does not delete its stored values.
- Invalid extension schemas disable that settings page and produce one actionable warning.

## Core configuration service

Introduce one `ConfigStore` owned by the daemon. It loads domain documents into one typed aggregate:

```text
ConfigStore
├── CoreSettings
├── ProvidersConfig
├── SubagentsConfig
├── ExtensionsConfig
└── CommandPolicy
```

Responsibilities:

- Load every document once during daemon startup.
- Apply defaults without expanding them into user files.
- Validate each document and cross-document references.
- Resolve active providers, themes, tools, commands, subagents, and extension values.
- Expose one immutable resolved snapshot to runtime consumers.
- Apply mutations under a write lock against the latest revision.
- Persist only the affected domain document.
- Publish a new revision and change event after a successful write.
- Leave the previous in-memory state active after a failed write.
- Report parse errors with the exact file and field path.

A single config revision covers the aggregate even though values are stored in multiple files. Mutations include an expected revision to prevent lost updates between TUI, web, Lua, and remote clients.

## Protocol design

Add protocol-authoritative configuration operations rather than frontend-specific endpoints.

### Queries

- `GetConfigSchema`
- `GetConfigSnapshot`
- `GetProviders`
- `GetSubagents`

### Mutations

- `SetConfigValue { path, value, expected_revision }`
- `ResetConfigValue { path, expected_revision }`
- `UpsertProvider { provider, expected_revision }`
- `DeleteProvider { id, expected_revision }`
- `SetActiveProvider { id, expected_revision }`
- `UpsertSubagent { subagent, expected_revision }`
- `DeleteSubagent { name, expected_revision }`
- `SetToolEnabled { name, enabled, expected_revision }`
- `SetCommandEnabled { name, enabled, expected_revision }`
- `ReloadConfiguration`

### Events

- `ConfigSnapshot`
- `ConfigChanged { revision, changed_paths, snapshot }`
- `ConfigMutationRejected { current_revision, error }`

The exact protocol representation may combine related operations, but mutations must remain typed and daemon-authoritative. Do not expose an unrestricted "write arbitrary YAML" command.

## Reload behavior

Every setting definition declares one behavior:

- `immediate`: apply and broadcast without restarting.
- `next_turn`: use for new model requests but do not disturb an active turn.
- `reload_extensions`: rebuild extension snapshots safely.
- `restart_required`: persist now and clearly report that the daemon must restart.

Core determines this behavior. Frontends only display the result.

Provider and command-policy changes may initially remain `restart_required`; later work can make them safely reloadable without changing the persistence model.

## Frontend changes

### TUI

- Render built-in and extension config pages from the daemon-provided schema.
- Send typed config mutations instead of writing through `CustomConfigs` locally.
- Update visible values from authoritative snapshots/events.
- Display validation, revision-conflict, and restart-required messages.
- Keep only temporary UI navigation state locally.

### Web UI

- Delete direct parsing and rewriting of `general.yaml`, `tools.yaml`, and `providers.yaml` from `webui/bridge.mjs`.
- Forward protocol config queries and mutations to the daemon.
- Render the same schema and resolved values as the TUI.
- Remove JavaScript copies of built-in defaults and field options.
- Keep browser-only display preferences local only when they cannot affect agent behavior.

### Headless and remote clients

- Consume the same config snapshot.
- Use the same mutations when configuration changes are supported.
- Never infer runtime config by reading local files, since the daemon may be remote.

## Lua API changes

Keep namespaced APIs:

- `bone.settings.get(path)` reads from the daemon-owned resolved snapshot.
- `bone.settings.set(path, value)` uses the same validated mutation path as frontends.
- `bone.settings.reset(path)` removes the persisted override and exposes the default.
- `bone.settings.define(namespace, schema)` registers extension schemas.

Lua must not retain a second authoritative settings table. Runtime overrides that are intentionally ephemeral must use a separate API and must not silently persist.

## Migration strategy

### Migration requirements

- Run only in Core.
- Acquire the config write lock.
- Read all legacy sources before writing anything.
- Validate the complete candidate configuration.
- Write new documents atomically.
- Preserve permissions, especially on provider credentials.
- Create a migration marker only after all writes succeed.
- Keep timestamped backups until the user confirms the new version works.
- Be safe to retry after interruption.
- Never delete or overwrite an invalid legacy file merely to make startup succeed.

### Legacy mapping

- Root `config.yaml` general/UI/theme/keymap values -> new `config.yaml`.
- Root `config.yaml` `subagents` -> `subagents.yaml`.
- Root `config.yaml` `extensions` -> `extensions.yaml`.
- `config/providers.yaml` -> `providers.yaml`.
- `config/tools.yaml` deny list -> `config.yaml.tools.disabled`.
- `config/commands.yaml` deny list -> `config.yaml.commands.disabled`.
- `command-policy.yaml` -> retained initially, then normalized in place or moved during the path migration.
- `config/general.yaml` and `config/status.yaml` contribute values only when no newer canonical value exists.
- Their labels, defaults, field types, and options are never migrated as user values.

### Precedence during migration

From highest to lowest priority:

1. Existing canonical root `config.yaml` values.
2. Current domain-specific values such as providers and deny lists.
3. Legacy General/Status page values.
4. Built-in defaults.

After successful migration, legacy page values are never consulted again.

## Implementation phases

## Status

- [x] 0. Confirm product and compatibility decisions
- [x] 1. Introduce the aggregate Core config service
- [x] 2. Add protocol-authoritative schemas and mutations
- [x] 3. Move TUI configuration writes to the daemon
- [x] 4. Move web configuration reads and writes to the daemon
- [x] 5. Split large domains and migrate legacy files
- [x] 6. Remove seeded config-page YAML and dead compatibility paths
- [x] 7. Add extension-owned schemas and generic config pages
- [ ] 8. Move configuration and state to XDG paths (deferred)
- [x] 9. Complete validation, documentation, and cleanup

### Implementation audit (2026-07-21)

- Phase 1 is complete: the daemon-owned `ConfigStore` is the sole live aggregate for typed core, provider, subagent, extension-value, enablement, and command-policy state. Runtime and Lua consumers use installed snapshots rather than independently reloading legacy pages.
- Phase 2 is complete: attach-time schemas and snapshots, aggregate revisions, typed mutations, change/rejection events, stale-write protection, and correlated mutation responses are protocol-authoritative.
- Phases 3 and 4 are complete: the TUI and web UI render canonical schemas and snapshots and send daemon mutations. Provider editor metadata remains browser-local because the protocol does not expose a provider-field schema; it is not a competing value/default authority.
- Phase 5 is complete: the five peer domain documents and conservative, marker-last migration preserve precedence, exact values, permissions, legacy inputs, and timestamped backups across retries.
- Phase 6 is complete: built-in schemas live in Rust, legacy page files are no longer seeded or consulted after migration, and their parser is isolated to migration compatibility.
- Phase 7 is complete: `bone.settings.define` registers validated namespaced schemas, generic TUI/web pages use the shared schema, values persist in `extensions.yaml`, and unavailable namespaces are preserved.
- Phase 8 is explicitly deferred; the existing Bone root remains in use.
- Phase 9 is complete: migration, failure, conflict, extension, store, protocol, TUI, Lua, remote, and web coverage passes alongside the full workspace test/check, Rust formatting, JavaScript syntax/test, and diff-hygiene validation.

---

## 0. Confirm product and compatibility decisions

### Confirmed scope

- This implementation covers phases 0–7 and 9. The XDG path move in phase 8 is deferred.
- The five domain documents are `config.yaml`, `providers.yaml`, `subagents.yaml`, `extensions.yaml`, and `command-policy.yaml`, all peers under the existing Bone root.
- Plaintext provider credentials remain supported alongside environment references.
- Migration retains timestamped backups and legacy source files indefinitely. Legacy files become read-only migration inputs and never remain live authorities.
- Migration has no downgrade compatibility guarantee; older Bone versions are not kept synchronized with the new documents.
- Command policy remains daemon-owned, file-edited, and restart-required. Generic command-policy UI mutations are deferred.
- Project-local configuration is out of scope and is never implicitly loaded or merged.

### Decisions

- [x] Confirm the five target domain documents and their names.
- [x] Confirm whether plaintext provider credentials remain supported alongside environment references.
- [x] Decide how long migration backups are retained.
- [x] Decide whether command-policy mutations are exposed in the first generic config UI.
- [x] Confirm that project-local config is out of scope; do not implicitly execute or merge configuration from the working directory.
- [x] Confirm the supported downgrade behavior after migration.

### Acceptance

- [x] File ownership and precedence are documented without ambiguous fallback behavior.
- [x] No phase requires frontends to read local config files.

---

## 1. Introduce the aggregate Core config service

### Work

- [x] Add `ConfigStore` around existing typed settings, provider config, subagents, extension values, and command policy.
- [x] Give the daemon sole ownership of the live store.
- [x] Add an aggregate revision and locked atomic mutation path.
- [x] Preserve per-domain writes so one changed field does not rewrite unrelated credentials or prompts.
- [x] Replace runtime `CustomConfigs::load()` calls with reads from daemon-owned snapshots where possible.
- [x] Define parse and validation errors with file and setting paths.

### Primary areas

- `core/src/config/settings.rs`
- `core/src/config/custom.rs`
- `core/src/config/providers_config.rs`
- `core/src/config/mod.rs`
- `core/src/tools/command_policy/`
- `core/src/rpc/mod.rs`

### Acceptance

- [x] Runtime consumers do not independently reload competing configuration sources.
- [x] A failed mutation leaves disk and active runtime state consistent.

---

## 2. Add protocol-authoritative schemas and mutations

### Work

- [x] Add serializable setting definitions and resolved config snapshots to `protocol`.
- [x] Add revision-checked mutation commands.
- [x] Add config snapshot/change/rejection events.
- [x] Send config schema and state during frontend attach.
- [x] Keep provider, subagent, tool, and command operations typed.
- [x] Add protocol round-trip and backward-compatibility tests.

### Acceptance

- [x] A remote frontend can configure Bone without filesystem access.
- [x] Concurrent stale mutations are rejected instead of overwriting newer values.

---

## 3. Move TUI configuration writes to the daemon

### Work

- [x] Build `/config` pages from protocol schemas.
- [x] Replace direct `CustomConfigs` persistence with runtime commands.
- [x] Apply authoritative snapshots after mutation success.
- [x] Surface validation and restart requirements in the UI.
- [x] Preserve immediate feedback while a mutation is pending.

### Acceptance

- [x] In-process and remote TUI paths behave identically.
- [x] The TUI does not write configuration files directly.

---

## 4. Move web configuration reads and writes to the daemon

### Work

- [x] Remove `parseConfigPage`, `setGeneralValue`, and direct general/tools writes from the bridge.
- [x] Remove direct provider YAML CRUD from the bridge after typed provider protocol operations exist.
- [x] Proxy daemon configuration operations through the existing bridge transport.
- [x] Render daemon-provided schemas and values.
- [x] Remove duplicated JavaScript config defaults and field metadata; retain only provider editor metadata until the protocol exposes a provider-field schema.

### Acceptance

- [x] Web and TUI changes produce the same persisted values and runtime behavior.
- [x] Web behavior settings no longer write ignored legacy values.
- [x] The bridge contains no Bone configuration parser.

---

## 5. Split large domains and migrate legacy files

### Work

- [x] Move providers to root `providers.yaml` under the interim Bone root.
- [x] Move subagents out of `config.yaml` into `subagents.yaml`.
- [x] Move extension values out of `config.yaml` into `extensions.yaml`.
- [x] Keep common settings and tool/command deny lists in compact `config.yaml`.
- [x] Implement the versioned migration and backups.
- [x] Preserve provider file permissions and credential values.
- [x] Add fixtures for every supported legacy shape.

### Acceptance

- [x] `config.yaml` remains readable and excludes large prompts, provider entries, and extension payloads.
- [x] Existing installations preserve all effective values.
- [x] Interrupted migration can be rerun safely.

---

## 6. Remove seeded config-page YAML and dead compatibility paths

### Work

- [x] Define General and Status schemas in Rust.
- [x] Stop seeding `config/general.yaml` and `config/status.yaml`.
- [x] Stop seeding `config/tools.yaml` and `config/commands.yaml` after their values migrate.
- [x] Remove legacy page routing and fallback logic from live runtime paths.
- [x] Delete stale comments claiming page YAML is authoritative.
- [x] Keep a read-only migration parser isolated from the live config path.

### Acceptance

- [x] No user-editable YAML file contains UI labels, types, options, and current values together.
- [x] Legacy files cannot override or shadow canonical values after migration.

---

## 7. Add extension-owned schemas and generic config pages

### Work

- [x] Implement `bone.settings.define` with namespace validation.
- [x] Merge valid extension schemas into the daemon config schema snapshot.
- [x] Persist extension values in `extensions.yaml`.
- [x] Generate one config page or section for each extension that declares settings.
- [x] Preserve values when an extension is disabled or unavailable.
- [x] Reject schema collisions and invalid defaults with actionable warnings.

### Acceptance

- [x] An extension can add a config page without Rust, TUI, or web-specific code.
- [x] Extensions without settings do not create empty pages.
- [x] Both frontends render extension settings from the same schema.

---

## 8. Move configuration and state to XDG paths

### Work

- [ ] Resolve config, data, and cache roots independently.
- [ ] Continue honoring explicit `BONE_DIR` for portable/test deployments.
- [ ] Migrate `~/.bone-rust` only after the new config model is stable.
- [ ] Preserve a clear rollback path and avoid duplicate active roots.
- [ ] Update setup, catalog, memory, history, logging, and documentation paths.

### Acceptance

- [ ] Configuration, durable application data, and disposable cache are separated.
- [ ] Startup reports exactly which roots are active.
- [ ] Bone never silently merges two roots.

---

## 9. Complete validation, documentation, and cleanup

### Tests

- [x] Default-only startup creates minimal files rather than serializing every default.
- [x] Every built-in setting accepts valid values and rejects invalid values.
- [x] Cross-document references reject missing providers or invalid active selections.
- [x] TUI, web, Lua, and remote mutations follow the same persistence path.
- [x] Revision conflicts do not lose updates.
- [x] Atomic write failures preserve the prior active and persisted state.
- [x] Provider credentials and permissions survive migration.
- [x] Unknown extension values survive load/save cycles.
- [x] Invalid extension schemas do not break unrelated config pages.
- [x] Restart-required changes are persisted and reported consistently.
- [x] Legacy migration is idempotent across all fixtures.
- [x] No runtime path reads General/Status page values after migration.

Verified coverage includes settings values-only/validation tests; migration precedence,
credential, permission, invalid-input, marker, fixture, and retry tests; store rollback,
revision, mirror, subagent, and inert-legacy tests; RPC provider preflight and daemon
subagent tests; Lua schema owner rollback/collision and snapshot tests; protocol
round trips; TUI schema/pending-response tests; and web canonical-path/correlation tests.

### Documentation

- [x] Document the domain files and who owns them.
- [x] Document schema versus value separation.
- [x] Document provider secret references.
- [x] Document Lua extension setting registration.
- [x] Document reload behavior and restart requirements.
- [x] Update generated `AGENTS.md`, README, setup help, and web documentation together.

### Cleanup

- [x] Remove obsolete page migration branches and dead config methods while retaining live compatibility types.
- [x] Remove direct frontend filesystem access.
- [x] Remove duplicated defaults and option lists.
- [x] Inspect the final diff for unrelated changes and dead code.

## Final acceptance criteria

- Bone Core is the only persistent configuration authority.
- All clients observe and mutate the same revisioned resolved configuration.
- The user sees a small set of domain files with no ambiguous duplicate values.
- `config.yaml` remains compact rather than becoming a mega-file.
- Built-in config pages require no seeded schema YAML.
- Lua extensions can define config pages without frontend-specific implementation.
- Provider, subagent, extension, and policy domains remain independently maintainable.
- Existing users migrate without losing settings, credentials, prompts, or enablement state.
- TUI, web, in-process, headless, and remote behavior remain in parity.
