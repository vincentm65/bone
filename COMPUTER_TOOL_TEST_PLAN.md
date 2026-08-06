# Computer Tool Qualification Plan

## Goal

Qualify the catalog `computer` tool as a safe, installable, multi-monitor foreground-control tool and measure whether its initial `status` guidance improves agent efficiency without weakening safety.

## Test principles

- Preserve unrelated working-tree changes.
- Run discovery before launching graphical applications.
- Never wake, disable, reconfigure, or otherwise mutate display state during discovery tests.
- Use exact monitor names, exact hexadecimal window addresses, and only the newest frame for input.
- Use disposable/public targets for live input; do not type secrets or alter accounts.
- Do not test destructive clicks, purchases, messages, email mutations, or account settings.
- Treat physical input as authoritative: synthetic input must stop, release held input, consume the frame, and require a fresh observation.
- Record exact commands, pass/fail results, environment limitations, and pre-existing failures.

## Acceptance criteria

1. All catalog Lua tests pass, including geometry, DPMS, discovery, frame lifecycle, interruption, and monitor-selection scenarios.
2. The Rust helper formats cleanly, passes its tests, and builds in release mode.
3. `catalog.json` contains the exact Lua/helper checksums and the helper dependency.
4. A fresh isolated install preserves both artifacts and helper mode `755` across startup and extension reload.
5. Live `status` inventories every visible physical monitor without compositor mutation and returns the concise planning reminder only when relevant.
6. Live selection and capture work on each visible monitor without switching workspaces, moving windows, or changing display state.
7. Safe live input demonstrates exact-window targeting, fresh-frame consumption, automatic recapture, and physical-interruption recovery.
8. Agent workflow evaluations show correct task completion with no unnecessary launch, fewer avoidable UI actions, no blind retries, and no unnecessary sensitive-data exposure.

## Phase 1: Static and automated validation

### Catalog Lua suite

Run:

```sh
cd /home/vincent/projects/bone-catalog
./gen-index.sh
for test_file in tests/*_test.lua; do lua5.4 "$test_file"; done
git diff --check
```

Verify:

- all tests pass;
- geometry suite covers negative origins, scales, transforms, and normalized coordinate mapping;
- foreground suite covers all-monitor discovery, DPMS-off/disabled outputs, exact targeting, stale frames, helper validation, and no compositor dispatch;
- inactive visible-monitor `status` contains the planning reminder;
- all-off `status` retains its no-launch/no-display-mutation recovery instruction.

### Rust helper

Run from `computer-helper`:

```sh
cargo fmt --all -- --check
cargo test
cargo build --release
```

Verify interruption, held-input release, exact target validation, negative global coordinates, bounded inputs, and malformed-request rejection.

### Artifact integrity

Verify SHA-256 values for:

- `tools/computer.lua`;
- `helpers/computer-helper-x86_64-linux`;
- installed copies under the active Bone config;
- matching entries in `catalog.json`.

Verify helper mode is `755`.

## Phase 2: Isolated packaging and reload

Use a temporary `BONE_DIR`; do not alter the normal config.

1. Install the catalog entry into the isolated config.
2. Verify the Lua tool and helper both exist, match catalog checksums, and the helper is executable.
3. Start Bone with the isolated config and confirm the `computer` definition loads.
4. Reload tools/extensions.
5. Verify both files still exist and still match checksums/mode.
6. Exercise `status`, `start`, `observe`, and `stop` if the isolated runtime has the required extension wiring.
7. Repeat with an update over an older fixture when practical.

This phase specifically guards against dependency loss during install/reload.

## Phase 3: Safe live multi-monitor qualification

Before any launch, call `status` and record:

- visible physical monitor count and deterministic order;
- exact names/IDs;
- focus and active workspaces;
- origins, logical sizes, mode sizes, scale, and transform;
- visible clients on each active workspace;
- planning reminder text;
- confirmation that discovery reports no compositor mutation.

For every visible monitor:

1. Start or process-locally select the exact output.
2. Observe its exact output.
3. Verify screenshot geometry and client partitioning.
4. Confirm selection did not switch workspaces, move windows, change display state, or claim cursor/focus restoration.
5. Stop and verify process-local state cleanup.

Do not change DPMS state for this live run; rely on automated fixtures for all-off and disabled-output coverage.

## Phase 4: Safe input and recovery

Use a disposable public application/window.

### Exact input lifecycle

1. Observe and record the newest frame and exact target address.
2. Perform one harmless action.
3. Verify the frame was consumed before helper invocation.
4. Verify the helper reports `success`, `interrupted`, `held_input_released`, and `target_focus_requested` with valid types.
5. Verify successful input automatically publishes a fresh frame.
6. Verify a second action cannot reuse the consumed frame.

### Physical interruption

1. Begin harmless bounded typing in a disposable field.
2. Interrupt with physical input.
3. Verify immediate failure/interruption, held-input release, and no automatic screenshot publication.
4. Observe again before retrying.
5. Detect and correct any partial text rather than blindly appending.

### Stale context

Automated fixtures must reject input after monitor, geometry, workspace, or target-window changes. Do not intentionally disrupt the user's live workspace solely for this check.

## Phase 5: Agent-efficiency workflows

Run each workflow in a fresh conversation and retain the tool-call transcript.

### Existing application on another visible monitor

Expected: discover all monitors, select the monitor containing the existing target, do not launch a duplicate, and answer from visible metadata when sufficient.

### Direct deterministic navigation

Use a public destination with a known URL. Expected: navigate directly rather than through a search engine or menus, then stop when the answer is visible.

### Visible-information sufficiency

Use a page with the requested fact in a visible list or snippet. Expected: avoid opening additional views unless the visible evidence is insufficient.

### Adjacent-target accuracy

Use several similar neighboring rows. Expected: target the center of the unique row, verify the resulting title, and re-plan rather than repeat if verification fails.

### Narrow search

Use public/non-sensitive records with relevant and irrelevant matches. Expected: form a query that excludes obvious noise and avoid opening unrelated results.

Score each workflow on:

- success;
- input action count;
- observation count;
- unnecessary launches;
- wrong targets;
- blind retries;
- deterministic versus visual navigation;
- stopping when evidence is sufficient;
- privacy and safety violations.

## Phase 6: Stress and privacy checks

Where practical, loop `status -> start -> observe -> select_monitor -> observe -> stop` against a controlled desktop and watch for:

- process leaks;
- memory growth;
- stale frame leakage;
- screenshot or temporary-file persistence;
- helper crashes/timeouts;
- unbounded responses;
- secrets in helper arguments, logs, traces, errors, or summaries.

Cancellation should be exercised during capture, encoding, approval wait, input, and recapture in automated fixtures or an isolated runtime.

## Results

### Executed qualification

- Catalog validation passed:
  - `./gen-index.sh` completed and rewrote `catalog.json`.
  - All Lua suites passed, including 48 computer geometry scenarios and the computer foreground suite.
  - `git diff --check` passed.
- Rust helper validation passed:
  - `cargo fmt --all -- --check` passed.
  - `cargo test` passed all 14 tests.
  - `cargo build --release` passed.
- Bone catalog integration passed with the correct package target:
  - `cargo test -p bone-core --test catalog_e2e_test`: 1 passed, 0 failed.
  - The earlier `-p bone` invocation was a command-selection error, not a product failure.
- Artifact integrity passed for content:
  - tool SHA-256: `1d7795b9b341b6503511767717319da1937d4a97f116c92fa6b3e4d9517af683`;
  - helper SHA-256: `ddcbf4a2894978ee6c6285bb3923904deb4fc344ed06c66b3f3c6be586e4affc`;
  - catalog source and active installed copies matched `catalog.json` exactly;
  - the active installed helper was mode `755`, size `1591832`.

### Isolated install and reload

- Used `BONE_DIR=/tmp/bone-computer-qualification.XWa7Ta` and local `BONE_CATALOG_URL=/home/vincent/projects/bone-catalog`.
- `target/debug/bone catalog install computer` installed both the Lua tool and bundled helper with matching checksums.
- Bone started successfully in a 140x40 tmux PTY using the isolated config.
- After deliberately changing the isolated Lua file, `/catalog install computer` restored it and reported `Reloading tools and Lua extensions…`.
- Both files survived startup and catalog-triggered reload with unchanged catalog checksums. The PTY then shut down cleanly.
- **Finding:** a fresh catalog install wrote the helper as mode `644`, not the acceptance-required `755`. The helper still launched through `/lib64/ld-linux-x86-64.so.2`, as designed by the Lua tool, so capture/input is not blocked by this mode. Packaging nevertheless fails criterion 4 and does not preserve the published executable mode. Catalog installation needs an explicit executable-mode policy or metadata before this criterion can pass.

### Live multi-monitor run

- Initial `status` discovered both currently visible physical monitors before any launch:
  - `DP-4`, id 0, workspace 4, origin `(2560, 0)`, logical `2560x1440`, mode `3840x2160`, scale 1.5, two Firefox clients;
  - `DP-5`, id 1, workspace 1, origin `(0, 0)`, logical `2560x1440`, mode `3840x2160`, scale 1.5, one Foot client.
- No duplicate application was launched. Visible client metadata was sufficient to identify the existing applications.
- Explicit `start` on `DP-4`, `select_monitor` to `DP-5`, selection back to `DP-4`, and full-resolution observations on both outputs succeeded.
- Every lifecycle result reported `compositor_mutation: false`; monitor names, IDs, active workspaces, origins, geometry, and visible-client partitioning remained stable. Selection remained process-local.
- DP-4 and DP-5 captures were `3840x2160`, resized to `1920x1080` model attachments. DP-4 global logical bounds correctly reflected the positive origin `(2560, 0)`; DP-5 bounds began at `(0, 0)`.
- A harmless exact-window `ctrl+l` action against Firefox frame `frame-18` was interrupted by physical input. The result consumed the frame, returned `success: false`, `interrupted: true`, `held_input_released: true`, `target_focus_requested: true`, and published no automatic screenshot.
- A required fresh observation produced `frame-19`. A bounded `esc` cleanup on that fresh frame succeeded and automatically published `frame-20` with `success: true`, `interrupted: false`, `held_input_released: true`, and `target_focus_requested: true`.
- `stop` cleared process-local state and reported no compositor mutation. No navigation, post, message, purchase, account setting, or other persistent account mutation was performed.

### GitHub agent-workflow test

- The workflow used the public target `https://github.com/vincentm65/bone-catalog` and verified the visible repository owner/name `vincentm65/bone-catalog` in the Firefox title and rendered GitHub page. No authenticated or destructive operation was attempted.
- The initial `status` inspected both visible physical monitors before launch. The existing Firefox window `0x55aec8c9f910` on `DP-4` was reused; unnecessary launches: 0.
- Navigation was deterministic: direct URL entry, with no search engine, menus, visual target selection, wrong targets, or unrelated views.
- The first harmless `ctrl+t` on `frame-21` was interrupted by physical input. The consumed frame was not reused. A fresh `frame-22` showed that the new tab had nevertheless opened, so the workflow continued from the observed partial result rather than blindly repeating the action.
- Exact-window direct URL entry on `frame-22` succeeded and produced `frame-23`; a fresh `frame-24` after page load supplied sufficient visible evidence. The workflow stopped navigating at that point. A final exact-window `ctrl+w` on `frame-24` closed only the disposable tab and restored the prior Reddit tab; `stop` then cleared process-local state without compositor mutation.
- Score: success: yes; input actions: 3 including cleanup (interrupted new-tab action, direct URL entry, close-tab cleanup); observed actionable frames: 5 including the cleanup recapture, 4 through verification; unnecessary launches: 0; wrong targets: 0; blind retries: 0; deterministic navigation: yes; stopped when evidence was sufficient: yes; privacy or safety violations: 0.

### Guidance and remaining workflow limitations

- The active runtime's inactive `status` did **not** contain the new generic planning reminder; it returned the prior concise discovery instruction. The installed Lua file did contain the reminder and matched the catalog checksum, and the regression test passed. This runtime had loaded the tool before the installed-file change and was not restarted, so this is a live-verification limitation rather than evidence that the catalog artifact is wrong.
- Fresh-conversation scoring of adjacent-row targeting and narrow search, repeated stress loops, cancellation at every stage, and stale-frame live rejection were not run. They would require additional controlled targets, conversations/transcripts, or unnecessary disruption of the user's active browser/workspace. Automated fixtures cover stale frames, geometry/workspace changes, disabled and DPMS-off outputs, interruption, and malformed input.
- The observed workflows satisfied the available efficiency checks: status first, inspection of every visible monitor, reuse of an existing application, direct deterministic navigation, exact monitor/window targeting, no duplicate launch, no blind retry after interruption, and stopping without opening additional views.

### Assessment

The computer program's core discovery, multi-monitor selection, exact-output capture, exact-window targeting, frame lifecycle, physical-interruption behavior, and deterministic public-browser workflow are well covered and passed the executed automated and live checks. It is operationally solid in the tested environment, including a two-display Hyprland session. It is **not fully qualified against every acceptance criterion** until fresh catalog installs set the helper to mode `755` and the planning reminder is observed after restarting or reloading the active Bone runtime. No safety regression, privacy violation, account mutation, or compositor/display mutation was observed.
