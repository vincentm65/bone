# `ask_user` swallows the last agent line — tmux repro & mechanism

Deliverable: a **reproduction** (not a fix) of the visual bug where, when
`ask_user` opens its prompt, the last visible line of the agent message is
visually swallowed — hidden but **not deleted** (it still lives in app
message state and reappears after a resize re-flush).

Reproduced in tmux 3.7b, pane 100x24 (status line inside the 24), by replaying
the exact TUI render sequence (ratatui 0.29.0, same viewport /
`insert_before` / resize calls) through an `example` in the `tui` crate — no
live agent or daemon involved.

**3/3 runs consistent**: Phase A & A2 sentinel visible → Phase B sentinel
gone → Phase C sentinel restored.

## Repro artifacts

| Artifact | Purpose |
|---|---|
| `tui/examples/ask_user_swallow.rs` | Replays the exact ratatui render sequence (A → A2 → B → C) with marker files for capture |
| `tui/examples/ask_user_swallow_repro.sh` | Drives the whole thing in tmux 100x24, captures each phase, greps the sentinel |
| `tui/examples/decstbm_probe.sh` | Standalone probe: verifies tmux 3.7b clamps CUD-at-bottom when a DECSTBM region excludes the bottom rows |
| `tui/examples/captures/phase_{a,a2,b,c}.txt` | `tmux capture-pane -S -100` per phase (history included) |
| `tui/examples/captures/markers/` | Per-phase marker files written by the example |

Build & run:

```sh
cd tui && cargo build --example ask_user_swallow
cd tui/examples && ./ask_user_swallow_repro.sh
```

## What the example replays (exact app path)

- `init_terminal(3)` — idle 3-row inline viewport, same as the app.
- **Phase A** (idle): assistant message rendered, 22 lines ending in
  `SENTINEL-LAST-AGENT-LINE-4f9c` (no trailing newline). Sizing via
  `ensure_viewport_height`, `draw_bottom_pane`, `flush_new_to_scrollback`
  (`tui/src/ui/render/mod.rs:294, 334`). Sentinel ends up at capture line 28
  (screen row 5), above the viewport.
- **Phase A2**: injects `ESC[1;20r` (DECSTBM region rows 1..20, 1-based —
  everything above the idle viewport top row 21) between A and B. See
  "What the injection stands in for" below.
- **Phase B** (ask_user opens): `PanePage{source:"interact", title:"ask_user",
  visible_rows:7}` + `ensure_viewport_height` (viewport grows 4 → 13, the real
  app path through `Renderer::resize_viewport`, `tui/src/ui/render/mod.rs:207`)
  + `draw_bottom_pane` → `insert_before_scrolling_regions`
  (ratatui `terminal.rs:695-762`).
- **Phase C** (resize re-flush): `hard_reset_viewport`
  (`mod.rs:232` — `\x1b[2J\x1b[3J\x1b[H` + `replace_terminal`) +
  `reset_scrollback_state` (`mod.rs:247`) + re-flush + draw.

## Result (identical across all 3 runs)

- **A / A2**: sentinel visible at capture line 28.
- **B**: sentinel **gone from the screen**, and scrollback did **not** grow
  (same 9 lines as A) — the swallowed rows are not in history; they were
  overwritten in place. The row now reads:

  ```
  SEdarwin/arm64AGENT-LINE-4f9c
  ```

  i.e. the menu option `  darwin/arm64` painted over
  `SENTINEL-LAST-AGENT-LINE-4f9c`; the leading `SE` and the tail
  `AGENT-LINE-4f9c` survive because the menu text is shorter. Other rows show
  the same in-place overwrite: `Question 1 of 1-off binary for local testing.
  The`, `Which deployment target should bone use?ching the`,
  `>  er the current git short SHA.`.
- **C**: sentinel restored at capture line 24 — proving the data was never
  deleted from app state; a resize re-flush re-paints it. This matches the
  reported "reappears after a resize" behavior exactly.

## Mechanism

### Phase B geometry (0-based rows, tmux 100x24)

After the grow 4 → 13, the new inline viewport sits at rows **11..23**:

```
row  11-12  input area
row  14     blank line — the empty line paints nothing, so old assistant
            text ("You can also pass...") stays visible
row  15-20  menu (6 rows painted; row 17 is the bg row, fully painted)
row  21-22  blank
row  23     status
```

The sentinel sits at screen row **18** — which is exactly menu row
`  darwin/arm64`. The first draw after the grow is a **fresh empty ratatui
buffer**, so it paints rows 11..19 **in place**, overwriting the assistant
text without any scroll. The sentinel is visually swallowed but still in
message state → restored by the Phase C re-flush.

### Why it requires CUD-at-bottom to be a no-op clamp

`compute_inline_size` (ratatui `terminal.rs:824-857`) computes
`missing_lines = lines_after_cursor - available_lines` and then
`row -= missing_lines` (lines 841-845), **assuming the cursor is now at
screen bottom** after a CUD. Two terminal behaviors:

1. **tmux 3.7b default (no region)**: CUD at the bottom **scrolls**
   k = nh − oh = 9 lines into history. The sentinel ends up at row 9, above
   the new viewport at rows 11..23 — **visible, bug masked**. (Verified 3×.)
2. **Clamping terminal / DECSTBM region excluding bottom rows**: CUD at the
   bottom is a **no-op clamp** — the cursor stays put. `missing_lines` math
   then places the new viewport at rows 11..23 **with no scroll**, and the
   fresh-buffer first draw overwrites rows 11..19 in place → **bug
   reproduced**.

The pure app path therefore does **not** reproduce in stock tmux 3.7b:
tmux's CUD-scroll masks the bug.

### What the `ESC[1;20r` injection stands in for

- A probe (`decstbm_probe.sh`) verified tmux 3.7b **clamps** CUD when the
  cursor is at row 20 inside region `1;20` (sentinel stayed at row 19, no
  scroll), while the control without the region scrolls. The injected region
  (rows 1..20 = everything above the idle viewport top, row 21) makes the
  app's own CUD clamp, reproducing the clamping-terminal behavior.
- The app itself never leaves a DECSTBM region active: ratatui's
  `ScrollUpInRegion::write_ansi` (`crossterm.rs:536-551`) always ends with
  `ESC[r` (region reset). So the "leftover region from the app" theory is
  dead — the injection is a deliberate stand-in for terminals that clamp
  CUD-at-bottom for their own reasons (the class of terminal where the user
  observed the bug).
- crossterm 0.28.1 doesn't expose `SetScrollingRegion` publicly, so the
  example writes raw `ESC[1;20r` via `crossterm::queue!` and resets with
  `ESC[r` at the end.

### Phase C geometry

`ESC[2J ESC[3J ESC[H` wipes screen + scrollback; the viewport rebuilds at the
top (rows 0..12), then `scroll_region_down` + 2× `scroll_region_up(0..11, 11)`
pushes it to rows 11..23 with 22 lines of history. Block = user 1 + blank 1 +
assistant 22 (sentinel is the last line, no trailing `\n`) + trailing blank 1
= 25 lines → sentinel is block line 23 → screen row 1 → capture line 24.

## Candidate fix direction (report only — no fix implemented)

- On the **grow path**, rows that the new viewport will cover must be moved
  into scrollback before the overwrite: e.g. an explicit `scroll_up(nh - oh)`
  or `insert_before` of the covered rows before `replace_terminal`, rather
  than relying on CUD having scrolled.
- Alternatively, make `compute_inline_size` **verify the cursor actually
  reached the expected position** (e.g. `get_cursor_position`) before
  applying the `row -= missing_lines` adjustment, instead of assuming CUD
  scrolled.

## Assumptions (unverified)

1. The user observed the bug in a **clamping-terminal environment** — their
   actual terminal or a different tmux configuration (tmux versions/configs
   with different CUD-at-bottom behavior, or a terminal with its own scroll
   region semantics).
2. The DECSTBM-clamped tmux 3.7b run **faithfully models** that environment —
   verified only that tmux 3.7b clamps CUD under the injected region and that
   the resulting paint sequence matches the reported symptoms (line hidden in
   place, not in scrollback, restored after a resize re-flush).
