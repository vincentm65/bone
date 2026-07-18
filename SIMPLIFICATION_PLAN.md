# Bone simplification plan

## Goal

Reduce product surface, maintenance cost, and duplicated UX while preserving Bone's core: a protocol-driven coding agent with a strong TUI client.

## Principles

- Keep one authoritative implementation path.
- Prefer protocol-owned runtime state over frontend filesystem/database access.
- Keep generic provider mechanisms; remove branded compatibility presets and bespoke integrations unless usage justifies them.
- Keep essential behavior, but remove dashboards, panes, settings, and controls that expose internal complexity.
- Do not retain indefinite compatibility shims for removed experimental features.

## Status

- [ ] 1. Remove the stats dashboard
- [x] 2. Move `/memory` to `bone-catalog`
- [ ] 3. Remove Grok Build
- [ ] 4. Remove line-oriented `bone connect`
- [ ] 5. Reduce compaction settings and provider presets
- [ ] 6. Simplify queue and process UX
- [ ] 7. Decide whether web is core and execute that decision

---

## 1. Remove the stats dashboard

**Target:** Delete the full-screen historical token dashboard while retaining lightweight current-session usage reporting through `/usage` and normal token accounting.

### Work

- [ ] Delete `tui/src/ui/stats.rs` and remove its module export.
- [ ] Remove the `/stats` built-in command registration and dispatch path.
- [ ] Remove the hidden `stats-popup` CLI entry point and tmux popup wiring.
- [ ] Remove dashboard-only historical query types and methods from `core/src/session_db.rs`.
- [ ] Remove dashboard-only tests from `core/src/session_db_tests.rs`.
- [ ] Update help text, command lists, comments, and docs that mention `/stats` or `stats-popup`.
- [ ] Confirm `/usage` still reports current-session requests, input/output/cache tokens, context, and cost.

### Preserve

- `TokenStats` and runtime token accounting.
- Persisted usage events that serve a purpose outside the deleted dashboard.
- `/usage` as the single simple usage surface.

### Acceptance

- [ ] No `stats-popup` CLI mode or `/stats` command remains.
- [ ] No full-screen stats renderer or dashboard-specific database API remains.
- [ ] `/usage` works in local and remote TUI sessions.

---

## 2. Move `/memory` to `bone-catalog`

**Target:** Make automated long-term memory an optional catalog extension. Persistent user and project preferences use the scoped memory files managed by that extension.

### Work

- [x] Add `memory.lua` to the `bone-catalog` repository with metadata, installation instructions, and ownership there.
- [x] Verify the catalog-installed command works against the current Lua API.
- [x] Delete `core/defaults/lua/commands/memory.lua` from bundled defaults.
- [x] Remove `memory.lua` from default command selection/seeding and refresh logic.
- [x] Remove bundled-memory documentation and replace it with a short catalog pointer.
- [x] Remove or update tests that assume `/memory` is bundled.
- [x] Document scoped memory files as the persistent preference mechanism.

### Migration

- [x] Legacy seeded `memory.lua` files are deleted; existing memory data is preserved.
- [x] New installs and refreshed defaults no longer seed or enable `/memory`.
- [x] Catalog installation must not silently overwrite existing memory data.

### Acceptance

- [x] A clean Bone install has no `/memory` command.
- [x] Installing the catalog extension restores `/memory` without Rust changes.
- [x] No bundled code path performs automatic long-term memory capture.

---

## 3. Remove Grok Build

**Target:** Remove the bespoke Grok Build subscription/OAuth provider. Keep the generic OpenAI-compatible provider path for ordinary endpoint/API-key configurations.

### Work

- [ ] Delete `core/src/llm/providers/grok_build.rs` and `grok_build_tests.rs`.
- [ ] Remove the module, provider factory branch, and supported-handler references.
- [ ] Remove Grok-specific cached-auth detection and configuration branches.
- [ ] Remove the `grok_build` preset from `core/src/config/pages/providers.yaml`.
- [ ] Remove Grok-specific setup, credential persistence, environment variables, help, and docs.
- [ ] Remove tests and fixtures that exist only for Grok Build behavior.
- [ ] Verify generic OpenAI compatibility tests do not accidentally depend on Grok-specific semantics.

### Migration

- Fail clearly when an existing config selects `handler: grok_build`; do not silently choose another provider.
- Users with a standard OpenAI-compatible Grok endpoint can configure it through the generic provider mechanism.

### Acceptance

- [ ] No Grok-specific OAuth, credential refresh, persistence, or handler code remains.
- [ ] `grok_build` is not offered by setup or `/config`.
- [ ] OpenAI-compatible custom providers continue to work.

---

## 4. Remove line-oriented `bone connect`

**Target:** Delete only the line-oriented reference client. Preserve `bone serve` and the full TUI remote mode, `bone --connect <addr>`.

### Work

- [ ] Delete `run_connect` from `tui/src/main.rs`.
- [ ] Remove the `bone connect` subcommand dispatch.
- [ ] Remove `bone connect` from CLI usage/help text.
- [ ] Update comments that treat it as the reference remote client.
- [ ] Remove tests and docs specific to line-oriented prompt input/output.
- [ ] Keep protocol codec and remote runtime code required by `bone serve`, `bone --connect`, and web if retained.

### Acceptance

- [ ] `bone connect` is no longer recognized or documented.
- [ ] `bone --connect <addr>` still runs the complete TUI against `bone serve`.
- [ ] Removing the client does not remove shared daemon/protocol transport.

---

## 5. Reduce compaction settings and provider presets

### 5a. Compaction settings

**Target:** Expose at most two user-facing compaction controls. Keep implementation budgets as internal constants with tested defaults.

**Recommended public settings:**

1. `auto_compact` — enable or disable automatic compaction.
2. `compact_trigger_percentage` — optional advanced threshold, defaulting to a safe value.

**Remove from public configuration:**

- `auto_compact_tokens`
- `compact_trigger_mode`
- `compact_context_window_tokens`
- `compact_keep_tokens`
- `compact_input_tokens`
- `compact_checkpoint_tokens`
- `compact_generation_tokens`
- `compact_safety_tokens`
- `auto_compact_keep_messages`
- Deprecated `compact_summary_tokens` fallback

### Work

- [ ] Confirm the two-setting target above or choose a one-setting alternative.
- [ ] Require provider context-window metadata for percentage-based automatic compaction; disable with a clear reason when unavailable.
- [ ] Move keep/input/checkpoint/generation/safety budgets to constants in `compact.lua`.
- [ ] Remove legacy setting parsing and fallback branches.
- [ ] Reduce `core/src/config/pages/general.yaml` to the approved public controls.
- [ ] Update compaction docs and tests around behavior rather than configuration combinations.
- [ ] Decide whether removed keys are ignored with one release note or rejected immediately; do not maintain permanent aliases.

### Acceptance

- [ ] General configuration exposes no more than two compaction fields.
- [ ] `compact.lua` has one automatic-trigger path, not absolute/percentage/legacy branches.
- [ ] Manual `/compact` remains available and reliable.
- [ ] Missing provider capacity produces an explicit disabled state, not guessed behavior.

### 5b. Provider presets

**Target:** Keep protocol/handler capabilities, but remove most branded OpenAI-compatible presets.

**Recommended bundled set:**

- OpenAI
- Anthropic
- Codex
- llama.cpp/local

**Recommended preset removals:**

- Gemini
- DeepSeek
- OpenRouter
- GLM
- GLM Plan
- Kimi
- MiniMax
- MiniMax Plan
- Grok Build, handled separately above

### Work

- [ ] Confirm the recommended bundled set.
- [ ] Remove the approved presets from `core/src/config/pages/providers.yaml`.
- [ ] Preserve a documented generic custom OpenAI-compatible configuration path.
- [ ] Ensure removing presets does not remove generic handler behavior used by user-defined providers.
- [ ] Update setup, model-picker, docs, and tests that assume branded presets exist.

### Acceptance

- [ ] Bundled presets represent distinct handlers or essential local setup, not a compatibility directory.
- [ ] Users can still add arbitrary OpenAI-compatible providers through configuration.
- [ ] Setup and provider selection remain understandable with the smaller list.

---

## 6. Simplify queue and process UX

### 6a. Queue

**Target:** Keep FIFO submission while a turn is running, but remove the queue-management pane and editor.

### Preserve

- Enter while busy queues a prompt.
- `Ctrl+Enter` steers the active turn.
- A small status indicator shows the queued count.
- One clear-queue action remains available.

### Remove

- `tui/src/ui/queue_pane.rs`.
- Queue selection state.
- Reordering, per-item editing, “run next,” and per-item deletion controls.
- Duplicated key handling for idle and streaming queue-pane navigation.

### Acceptance

- [ ] Queue behavior is FIFO and has no pane-specific mode.
- [ ] The user can see the count and clear queued prompts.
- [ ] Typed input is never overwritten when the queue advances.

### 6b. Processes

**Target:** Keep background-process execution only if required by tools/extensions, but remove its dedicated live pane. Surface concise lifecycle status through the existing shared activity/status UI.

### Work

- [ ] Delete `tui/src/ui/processes_pane.rs` and its refresh/polling integration.
- [ ] Show only a running-process count or latest lifecycle event in existing status UI.
- [ ] Keep process IDs and explicit stop/list operations only where they are needed for control.
- [ ] Audit `core/src/processes.rs` after pane removal; delete registry fields and snapshots used only for rendering.
- [ ] Keep process failures visible; do not replace the pane with silent background failure.
- [ ] Decide whether native background processes and Lua jobs should share one activity model; avoid adding a second replacement pane.

### Acceptance

- [ ] No dedicated queue or process pane remains.
- [ ] Normal chat has one compact activity/status surface.
- [ ] Queued prompts and running processes remain observable and controllable at a basic level.

---

## 7. Decide explicitly whether web is core

**Decision required before implementation:**

- [ ] **Yes — web is a core client.**
- [ ] **No — web is not core.**

Do not continue maintaining the current middle state, where the web UI is protocol-based for chat but directly owns database/config access and a separate canvas product.

### Option A: Yes — retain a reduced protocol-driven chat client

**Target:** A thin browser chat client over the Bone protocol.

#### Keep

- Streaming chat and reasoning.
- Tool calls/results and approvals.
- Conversation selection only if daemon-owned through protocol.
- Provider selection only if daemon-owned through protocol.
- Basic attachment support only if represented cleanly in protocol.

#### Remove

- Canvas, file tabs, diff/document rendering, workspace search, downloads, and editor launching.
- Direct reads of `conversations.db`.
- Direct reads/writes of `providers.yaml`, `general.yaml`, and `tools.yaml`.
- Frontend-specific ownership of runtime configuration.
- Bridge endpoints that bypass the daemon's authoritative state.

#### Work

- [ ] Define the minimum supported web feature set.
- [ ] Add only the protocol commands/events needed for that minimum set.
- [ ] Reduce `webui/bridge.mjs` to transport/static serving and daemon lifecycle.
- [ ] Delete canvas code and associated styling/tests.
- [ ] Delete direct config/database bridge routes.
- [ ] Remove UI controls whose behavior is not available through protocol rather than adding filesystem fallbacks.
- [ ] Update `webui/README.md` to describe a thin client, not “bone studio.”

#### Acceptance

- [ ] Browser state comes from protocol events/commands or browser-local display preferences.
- [ ] The bridge does not parse Bone's database or configuration files.
- [ ] The web client has no canvas/workspace product surface.
- [ ] TUI and web exercise the same daemon-owned semantics.

### Option B: No — extract or remove web

**Target:** The Rust workspace ships and maintains the TUI/daemon/protocol, not a separate web product.

#### Work

- [ ] Decide between moving `webui/` to its own repository and deleting it outright.
- [ ] If extracted, define the protocol compatibility boundary and independent release ownership.
- [ ] Remove `webui/` from this repository.
- [ ] Remove web-specific docs, scripts, CI, tests, and daemon assumptions.
- [ ] Keep only protocol behavior justified by TUI/daemon clients or the public protocol contract.

#### Acceptance

- [ ] No web frontend code or web-specific bridge remains in this repository.
- [ ] Core does not read or shape runtime behavior for a removed frontend.
- [ ] Any extracted client depends only on the documented protocol.

---

## Suggested execution order

1. Remove stats dashboard.
2. Move `/memory` to catalog, then stop bundling it.
3. Remove Grok Build.
4. Remove line-oriented `bone connect`.
5. Reduce provider presets.
6. Simplify queue and process UX.
7. Reduce compaction settings.
8. Execute the explicit web decision.

Keep each numbered item as a separate reviewable commit where practical. Run formatting, workspace tests, and frontend tests relevant to the retained web decision after each slice.

## Decisions to record

| Decision | Choice | Notes |
|---|---|---|
| Public compaction controls | Pending | Recommended: `auto_compact` + `compact_trigger_percentage` |
| Bundled provider presets | Pending | Recommended: OpenAI, Anthropic, Codex, llama.cpp |
| Process backend | Pending | Keep backend and remove pane, then audit actual callers |
| Web is core | Pending | Must choose Yes or No before web work starts |
| If web is not core | Pending | Extract vs delete |
