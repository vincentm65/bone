# bone desktop GUI — Computer-Use Test Findings (3.0.10)

Date: 2026-11-10
Method: live GUI testing through computer use (screenshot + coordinate input), as an end user.
Build: `bone 3.0.10 (bf51fadf-dirty)` — `/home/vincent/projects/bone3/target/release/bone`, symlinked as `bone` and `bone3`.
Window: `0x55836b761250`, class `bone`, workspace 2, monitor **DP-2**, geometry `2572,4` size `2536x1432` (logical px; DP-2 is 3840x2160@160 with scale 1.5, screenshots 1280x720).
Session under test: `01a1c364-25ef-71f9-a281-4ac32fccdbce`, provider `local` (`http://localhost:8081/v1`, Qwen3.8-27B-exl3-3.8bpw).

The GUI is custom-rendered (`gpui 0.2.2`): there is no accessibility tree, so every observation comes from screenshots. No installs, updates, removals, provider switches or setting writes were performed.

## Executive result

- The core user journey (composer round-trip, slash commands, catalog, settings, tool cards, tray, sessions sidebar, resize) **works**.
- All four layout defects from the 2.4.5 report that were re-testable (**F-1, F-4, F-5**) are **fixed**; **F-3** is largely fixed with one residual gap.
- Eight new defects were found (G-1 … G-8), the most substantive being **G-1** (the catalog silently swallows Tab and pill clicks while it is scanning) and **G-6** (the sessions sidebar is hard to close).

## Retest of the 2.4.5 findings

| Old ID | 2.4.5 finding | Result in 3.0.10 |
|---|---|---|
| F-1 | Esc does not dismiss the slash-command autocomplete popup | **FIXED.** `/co` → popup (`/cooder-usage, /compact, /config, /context` + descriptions + hint bar `↑↓ move  Enter run  Tab complete  Esc close`); Esc closes the popup and leaves the draft `/co` intact. |
| F-2 | System-prompt value clips in the narrow configuration pane | **NOT REPRODUCED.** The General tab has no System-prompt row (searching `prompt` there yields only `Prompt max height`); values render in a bordered box with an ellipsis. At 700x600 logical px nothing clipped. The System prompt's current home (Providers tab) was not inspected — follow up. |
| F-3 | Tool-card arguments and output clip at the right edge | **LARGELY FIXED, residual gap.** See G-8 notes below: expanded sub-rows wrap the full command and output; collapsed sub-rows truncate with an explicit `…` and no cue that they are expandable. |
| F-4 | Tools-menu descriptions clip at the right edge | **FIXED.** Catalog, settings rows and the slash popup all end truncated text with an explicit `…`. |
| F-5 | Plugins-pane footer clips at the bottom edge | **FIXED.** The full hint bar is visible in the Plugins tab (`↑↓ move  / search  Enter settings  Space on/off … Esc close`) and in a plugin's own settings page (`↑↓ move  / search  Enter change  z reset … Esc back`). |
| F-6 | Approval-mode change has no confirmation | **N/A in this configuration** — no approval controls were present (tool approval is off in `~/.bone/core.lua`; the `approve` example plugin is not installed). Not tested. |
| F-7 | Provider switch stuck on "Switching…" | **NOT TESTED** — deliberately avoided, to prevent changing the live provider. |
| F-8 | Composer focus after restart is intermittent | **PASS.** The composer accepted typing at the start of every test segment. |
| F-9 | Maximize leaves the window at half width | **N/A** — resizing is compositor-driven; see "Resize" below, which reflows correctly. |
| F-10 | Setup Cancel hit area smaller than the button | Not tested (the setup wizard was not opened). |

## New findings

### G-1 — The catalog silently swallows Tab and clicks on the "GUIs" pill while it is scanning

- **Severity:** Medium / P2 UX defect
- **Reproduction:**
  1. `/catalog` (or sidebar → Catalog). The modal opens showing `Loading…` and `0 updates · 0 selected`.
  2. Press Tab, or click the `GUIs` pill in the modal header.
  3. Nothing changes; the Plugins list is still shown and the counter is unchanged. There is no cue of any kind.
  4. Wait for the scan to finish (≈4 s) and press Tab again: the page switches to GUIs ("No GUIs in the catalog yet (its libraries are for the terminal: /tui)").
- **Cause in source:** `runtime/lua/bone/catalog.lua` gates the key with `elseif k == "tab" and not st.busy then`; the desktop view wires the pill as `pick = function() page.on_key("tab") end`, so a click goes through the same gated key handler and is discarded.
- **Expected:** While busy, the control should be visibly disabled, or the press should be queued and applied when the scan finishes.
- **Fix:** Route the pill click through a page-switch function that works during loading (as `settings.lua` does with `pick = function(i) page.switch_tab(i - st.tab) refresh() end`), or grey the pill and show a busy state.

### G-2 — The header "…" menu is not dismissed by Esc

- **Severity:** Low / P3 UX defect
- **Reproduction:** Click the `…` at the top-right of the chat (window-local ≈ `1256,17`; a click at `1256,22` misses). The menu opens with **Chat actions → Rename / Compact / Archive** (Archive in red). Press Esc: the menu stays open. Clicking anywhere else dismisses it.
- **Expected:** Esc closes the menu, consistent with every other transient surface in the app (slash popup, catalog, settings, tray edit).

### G-3 — Settings rows scroll *under* the search field

- **Severity:** Low / P4 visual defect
- **Reproduction:** `/plugins`, scroll the list to the bottom (mouse wheel). A row is left half-visible directly beneath the search box — its toggle is drawn clipped against the search field's border. Same in a plugin's own page (`Settings › Plugins › Files`).
- **Expected:** The list viewport should clip rows cleanly inside its own bounds, with a gap under the search field.

### G-4 — "Enter settings" is advertised for plugins that have no settings page

- **Severity:** Low / P3 UX defect
- **Reproduction:** `/plugins`, select `Shell` (no `›` on the right), press Enter — nothing happens. Press Enter a second time — still nothing. Rows that do declare settings (`Files`, `Markdown`, `MCP`, …) show a `›` and open a real sub-page.
- **Cause in source:** `runtime/lua/bone/settings_page.lua:568-571` — `elseif not space and row.page then st.tab, st.sel = row.page, 1 end`. There is no `else` branch, and the footer hint bar always advertises `Enter settings`.
- **Expected:** Either omit the hint for rows with no page, or show a short "no settings" note.

### G-5 — The sessions sidebar cannot be closed with Esc, and ctrl+o only works twice

- **Severity:** Low / P3 UX defect
- **Reproduction:**
  1. `ctrl+o` opens the sidebar; type a query (e.g. `table`) — the list filters correctly and groups by date.
  2. Press Esc: the caret leaves the search field, but the sidebar stays open with the filter still applied.
  3. Press Esc again: nothing.
  4. Press `ctrl+o`: nothing visible (the caret simply returns to the search field).
  5. Click into the chat area, then press `ctrl+o` — the caret returns to the search field again; a **second** `ctrl+o` finally closes it.
- **Cause in source:** `runtime/lua/bone/desktop/sidebar.lua` — `esc = function() if st.menu then … else bone.gui.focus(nil) end` (leaves the keyboard, keeps the panel), `["ctrl+o"] = close`, and `M.open()` closes only `if has_keys() then close() else take_keys() end`.
- **Expected:** Esc should close the sidebar (or at least clear the query first, so the panel visibly returns to the full list). A single `ctrl+o` should close it regardless of who currently holds the keyboard.

### G-6 — Focus returns to the composer after a modal opened from the sidebar

- **Severity:** Low / P3 UX defect
- **Reproduction:** `ctrl+o` (sidebar open) → click `Catalog` → the catalog modal opens → Esc closes it. The sidebar is still open, but keyboard focus is back on the chat composer: typing `table` produced a draft in the composer, not a sidebar search.
- **Expected:** Focus should return to the surface the user came from (the sidebar), or the sidebar should close with the modal it launched.

### G-7 — Clicking a chat row switches the whole GUI instantly, with an empty transcript while it loads

- **Severity:** Low / P3 UX defect
- **Reproduction:** In the sidebar, a click intended for the `Catalog` nav item landed on a chat row (`please open bone desktop a d test thro…`). The window title changed to that chat, the chat area rendered **empty** (only the composer, showing `Message bone (queued)…`, `Working 0s` and `96.7k / 200k`), and the transcript appeared a moment later. There is no confirmation and no spinner.
- **Expected:** Either confirm switching away from a chat, or show a loading state in the chat area instead of a blank one.
- **Note:** the sidebar's row hover affordances (pin and rename replacing the age) work, and switching back works.

### G-8 — The same provider reports two different context limits in two chats

- **Severity:** Low / P3, ownership unresolved (core vs. desktop)
- **Evidence:** The composer's context pill showed `6.4k / 100k` (later `8.1k / 100k`) for session `01a1c364…` and `96.7k / 200k` (later `95k / 200k`) for session `01a1c519…`. Both sessions report `"provider":"local","model":"local"` in `~/.bone/sessions/<id>.jsonl`, and `~/.bone/core.lua` sets no context limit for `local`.
- **What is *not* wrong:** the used figures match the usage records exactly (`6417 → 8149` input tokens for the first session), and the counter only updates when a turn finishes. The earlier 2.4.5 note about a large counter drop is explained by the idle-compaction event the transcript itself prints (`— compacted 223 messages ~90.3k tokens saved (95.8k → 5.5k)`), and the earlier "180k" reading was an OCR error for "100k".
- **Expected:** one limit per provider/model unless a per-session override exists, and the override should be visible.

### G-8b — Residual F-3 gap in tool cards

- **Severity:** Low / P4
- Long single-line values in a **collapsed** tool-card sub-row are truncated with `…` and give no affordance that the row is expandable (e.g. `Run hyperctl layers 2>/dev/null | head -40; echo "=== waybar proc ==="; ps -o pid,ppid,etime,cmd -p 1309; echo "=== journal tail ==="; j…`). Clicking the row expands it and the full command wraps over two lines, with the output beneath.
- Expanding a card whose header is near the top of the viewport leaves the header scrolled off-screen; only the tail of the output is visible until the user scrolls up.
- Long outputs are truncated with an explicit marker (`… 29 more lines`), which is the correct behaviour.

## Verified functional behavior

| Area | Result | Evidence |
|---|---|---|
| Composer round-trip | PASS | Draft → `Message bone (queued)…` + footer `Working 0s`; reply streamed; footer `Worked for 1s · Finished at 06:06`; placeholder restored. |
| Unknown slash command | PASS | `/tools` → `Unknown command /tools (start with // to send it as a message)` in red above the composer, draft preserved. (There is no `/tools` command in 3.0.10; the tools view is the transcript's tool-card renderer, `runtime/lua/bone/desktop/tools.lua`.) |
| Slash autocomplete | PASS | `/co` filtering, bare `/` full list, Down highlight, Tab completing the draft to `/catalog`, Enter running it, Esc closing the popup with the draft intact. |
| Catalog page | PASS | Modal with `Plugins \| GUIs` tabs, `1 update · 0 selected`, Updates/Installed sections, `…`-ellipsized descriptions, full hint bar, Down moves selection, footer action label is context-sensitive (`Enter update` → `Enter reinstall` with the row's detail line), `a` selects all (`1 selected`, hint bar becomes `Space toggle · Enter install selected · a all · n clear · u scan · x remove · Tab page`), `n` clears, Esc closes. No install/update/remove was performed. |
| Settings page | PASS | `/config` → General (Appearance/Input/More, collapsed Compaction); tabs switch by click; Providers lists all eight providers with model + host and an `In use` pill; `/plugins` deep-links to the Plugins tab; `/` focuses search, filtering works, `×` clears, Esc clears. |
| Plugin settings sub-page | PASS | `Settings › Plugins › Files` breadcrumb, full descriptions, segmented controls (`built-in / nvim / vim / custom`, `drawn / elements`), numeric row with `50`, switches, footer `↑↓ move  / search  Enter change  z reset  Esc back`. No value was changed. |
| Tool cards | PASS | Collapsed summary + duration + chevron; expands to sub-rows (`computer`, `Run <cmd>`, `Edited <path> +N -N` with red/green diff lines); error output in red (`(700, 690) is outside the 1280x360 screenshot`); sub-rows expand again for full args + output. |
| Background tray | PASS | Appears only when there is queued/agent/shell work: `Queue 1  Agents 0  Shells 0 … ↓ to manage`, row `↳ Second message while you work.  joins this turn`. Down on an empty composer enters the tray (footer → `enter open · c cancel · esc`), Enter pulls the queued message into the composer with the tooltip `editing a queued message · enter saves · esc cancels`, Esc cancels the edit and leaves the queue intact, and the queued message is delivered as its own turn (`Worked for 25s · Finished at 06:08`). |
| Prompt suggestions | PASS | Dim ghost text in the empty composer (`Now count backward from 40 to 1, one number per line.`); Tab accepts it into the draft; `Ctrl+A` + `Delete` clears. |
| Composer `+` (attach) | PASS | Opens the native XDG/GTK `Attach files` chooser in its own window; Escape closes it and returns focus to bone. No file was attached. |
| Model pill `local ▾` | PASS | Opens Settings → Providers, with the tooltip `Change the model (/provider)`. No provider was changed. |
| Sessions sidebar | PASS | `ctrl+o` docks it left: search field with `Ctrl ⇧` keycap, `Chats / Processes / Catalog`, chats grouped `Recent · last 1h`, `Yesterday`, `This week`, `Earlier` with ages and `now` for live chats; hover swaps the age for pin/rename icons; Tab cycles Chats → Processes (`Nothing running in the background`); Catalog opens the same catalog modal; typing in the field filters and keeps date groups. |
| Header `…` menu | PASS (see G-2) | Chat actions → Rename / Compact / Archive. Nothing was selected. |
| Resize | PASS | 2536x1432 → 1600x1000 → 700x600 logical px and back (`hyprctl dispatch resizeactive`, `togglefloating`). At every size the header, transcript bubbles, tool cards and composer reflow correctly; the title truncates with `…`; the `+`, model pill, counter and send button stay in place. At 700x600 the sessions sidebar renders as a card over the chat, as designed. |

## Recommended priority

1. **G-1** — make the catalog's page switch work (or visibly disable) during a scan.
2. **G-5 / G-6** — sidebar close and focus-return rules; a single Esc should close the panel.
3. **G-2** — Esc on the header menu.
4. **G-4** — stop advertising `Enter settings` where there is none.
5. **G-7** — loading state (and optional confirmation) when switching chats.
6. **G-3, G-8b** — cosmetic: list clipping under the search field, expandable-row affordance in collapsed tool sub-rows.
7. **G-8** — decide where the context limit comes from and make it consistent per provider.

## State left behind

Nothing was installed, updated, removed or reconfigured. Three messages were sent to the `local` provider in the GUI session (`testing the desktop GUI…`, `Count from 1 to 40…`, `Count backward from 400 to 1…`, plus one queued message `Second message while you work.`), and one draft was typed and cleared. The catalog selection was toggled with `a` and cleared with `n`. The window was returned to its original tiled geometry (`2572,4`, `2536x1432`).
