# bone-desktop GUI — Computer-Use Test Findings (v2.4.5)

Date: 2026-09-29
Method: live GUI testing through computer use, with screenshot/frame evidence, on Hyprland monitor DP-3.
Builds: `target/debug/bone-desktop` 2.4.5; `target/release/bone` 2.4.5.

## Test configurations and evidence rules

The original desktop/daemon pair was not stopped or restarted. The resumed tests used an isolated pair:

```text
Daemon:  ./target/release/bone serve --listen 127.0.0.1:17878
Desktop: target/debug/bone-desktop --connect 127.0.0.1:17878
Desktop window: 0x557ef5d52870
Monitor: DP-3
Provider: deepseek
```

Both physical monitors were inspected before GUI interaction; DP-2 and DP-3 were visible and DPMS-on. Inputs below were sent to the exact isolated window on DP-3. Every recorded input in the autocomplete run reported `interrupted: false`. Frame IDs refer to the live computer-use trace.

The earlier 2026-09-20 baseline used port 7878 and is retained where its findings were not repeated in the isolated run. Where the old screenshot ID was not retained in the resumed checkpoint, that limitation is stated explicitly rather than inventing a frame reference.

## Executive result

- The existing functional workflow remains **8/8 passing**.
- The isolated live-menu retest found one reproducible desktop UX defect: **Esc does not dismiss the slash-command autocomplete popup**.
- Slash-command prefix filtering, arrow selection, and Enter activation worked.
- No confirmed daemon or protocol failure was established. Socket/request evidence showed the daemon responding to loading commands.
- Several historical layout and interaction findings remain valid as qualifications, but some were not revalidated in this isolated segment.

## Findings requiring fixes or follow-up

### F-1 — Slash-command autocomplete cannot be dismissed with Esc

- **Severity:** Low / P3 UX defect
- **Ownership:** Desktop
- **Reproducibility:** Reproduced in one controlled run; both Esc attempts were ineffective.
- **Exact reproduction:**
  1. In the isolated chat, type `/co`.
  2. Observe frame `frame-86`: the composer contains `/co`; autocomplete shows `/compact` and `/config`, with `/compact` highlighted.
  3. Press Down and observe `frame-87`: the highlight moves to `/config`.
  4. Press Enter and observe `frame-88`: `/config` opens the configuration page.
  5. Press Esc and observe `frame-89`: the configuration page closes and chat returns.
  6. Type `/co` again and observe `frame-90`.
  7. Press Esc and observe `frame-91`; press Esc once more and observe `frame-92`.
- **Observed evidence:** Frames `frame-91` and `frame-92` still show `/co` and the same autocomplete rows. The input results for both Esc presses reported success with `interrupted: false`; there was no visible state transition.
- **Expected behavior:** Esc should close the autocomplete popup (normally leaving the draft intact), without opening a command or changing page state. The current source path also clears `session.autocomplete` on Esc (`native/src/lib.rs:3203–3208`), although source/binary correspondence is not assumed as proof.
- **Recommended fix:** Ensure the focused composer receives Esc before any other input path and clears autocomplete state; add a GUI regression test for `/co` + Esc and a second Esc.

### F-2 — System-prompt value clips in the narrow configuration pane

- **Severity:** Low / P3 visual defect
- **Ownership:** Desktop
- **Reproducibility:** Reproduced in the observed narrow pane; layout-dependent.
- **Exact reproduction:** Open `/config`, using the autocomplete flow above, and inspect the General section in `frame-88`.
- **Observed evidence:** The System prompt row displays the beginning of the configured prompt and cuts off at the right edge of the pane without a visible full-value affordance.
- **Expected behavior:** The value should wrap, scroll, or provide a clear ellipsis/secondary view so the user can inspect the complete prompt.
- **Recommended fix:** Use a wrapping/scrollable value widget or expose the complete prompt in an editor/details view; test at the current narrow pane width.

### F-3 — Tool-card arguments and output clip at the right edge (historical)

- **Severity:** Low / P3 visual defect
- **Ownership:** Desktop
- **Reproducibility:** Observed in the 2026-09-20 baseline; not re-run in the final isolated segment.
- **Exact reproduction:** Trigger a tool call with a long path or long output, expand the tool card, and inspect its arguments and output lines.
- **Observed evidence:** The retained baseline reported hard right-edge clipping with no wrapping, horizontal scrolling, or ellipsis. A frame ID was not retained in the resumed checkpoint.
- **Expected behavior:** Long JSON and output should wrap, scroll horizontally, or visibly indicate truncation.
- **Recommended fix:** Add bounded wrapping/scrolling and an explicit truncation affordance; test both long keys/values and long single-line output.

### F-4 — Tools-menu descriptions clip at the right edge (historical)

- **Severity:** Low / P3 visual defect
- **Ownership:** Desktop
- **Reproducibility:** Observed in the 2026-09-20 baseline; current isolated revalidation pending.
- **Exact reproduction:** Open the Tools menu and inspect entries with long descriptions.
- **Observed evidence:** The baseline showed descriptions ending abruptly, including text similar to `appears in the transcrip…`; no retained frame ID is available.
- **Expected behavior:** Descriptions should wrap, scroll, or have an intentional ellipsis that does not obscure whether more information is available.
- **Recommended fix:** Allocate a wrapping description column or add a tooltip/details view.

### F-5 — Plugins-pane footer clips at the bottom edge (historical)

- **Severity:** Low / P3 visual defect
- **Ownership:** Desktop
- **Reproducibility:** Observed in the 2026-09-20 baseline; current isolated revalidation pending.
- **Exact reproduction:** Open the Plugins pane and inspect the explanatory footer below the plugin list.
- **Observed evidence:** The footer text was cut at the bottom of the pane. No retained frame ID is available.
- **Expected behavior:** The complete footer should remain visible or be reachable by scrolling.
- **Recommended fix:** Reserve footer height or make the pane content scrollable.

### F-6 — Tool-permission mode change has insufficient protection (historical safety concern)

- **Severity:** Medium / P2 safety UX concern
- **Ownership:** Desktop for confirmation/warning UX; server-wide scope remains unverified
- **Reproducibility:** Observed in the 2026-09-20 baseline; exact current toggle behavior was not repeated.
- **Exact reproduction:** Open the Tools/configuration controls and switch between Ask and Auto-approve while no task is running.
- **Observed evidence:** The baseline described an immediate toggle with no confirmation and a scope that appeared to affect all tasks on the server. The isolated config view in `frame-88` visibly showed the current Approval mode as `danger`, but did not by itself test the toggle.
- **Expected behavior:** A mode that can authorize tool use broadly should show explicit scope and risk, and preferably require confirmation before changing.
- **Recommended fix:** Add a confirmation dialog or persistent, unambiguous scope warning; verify whether the setting is per-tab, per-session, desktop-wide, or daemon-wide.

### F-7 — Provider switch can remain on “Switching…”; ownership unresolved

- **Severity:** Low / P3 intermittent UX concern
- **Ownership:** Unresolved — desktop, daemon, or protocol cannot be separated from the retained evidence
- **Reproducibility:** Observed once in the isolated run; not sufficient to establish a backend failure.
- **Exact reproduction:**
  1. Open `/config` and navigate to Providers; frame `frame-69` showed `claude`, `codex`, and `deepseek`, with `deepseek` current.
  2. Click the already-current `deepseek` row at approximately normalized `(0.558, 0.855)` on window `0x557ef5d52870` / DP-3.
  3. The click reported `interrupted: false`; frame `frame-70` showed `Switching provider to deepseek…`.
  4. Frame `frame-71` still showed the switching state approximately 29 seconds later; `frame-72` still showed it after additional waiting.
- **Observed evidence:** Persistent UI state only. No raw provider-switch response or request-level failure was captured, so this is not classified as daemon/protocol latency or failure.
- **Expected behavior:** Selecting the current provider should be a no-op or complete promptly with a stable selected state.
- **Recommended fix:** Capture the request/response and timeout/error state; make repeated selection idempotent and surface a bounded, actionable error if switching cannot complete.

### F-8 — Composer focus after restart is intermittent

- **Severity:** Low / P3 UX defect when reproduced
- **Ownership:** Desktop
- **Reproducibility:** Inconsistent. The old baseline reported the first typed text being lost after restart, while the isolated run positively verified startup focus in `frame-75`/`frame-76`.
- **Exact reproduction:** Restart or restore the desktop window, type a short draft without clicking the composer, and inspect whether the draft appears. Repeat after menu/page use.
- **Observed evidence:** Conflicting historical and isolated results; no current defect is asserted from the passing isolated run.
- **Expected behavior:** The composer should receive focus on a new/returned chat unless focus was deliberately moved elsewhere.
- **Recommended fix:** Preserve or explicitly restore composer focus after window activation; add a repeatable focus regression test.

### F-9 — Maximize behavior remains unresolved

- **Severity:** Low / P3, pending ownership
- **Ownership:** Likely compositor; unresolved without an isolated compositor comparison
- **Reproducibility:** Observed in the 2026-09-20 baseline; not re-run in the final isolated segment.
- **Exact reproduction:** Send Super+Up and use the title-bar maximize control while the desktop window is visible.
- **Observed evidence:** The old run left the window at approximately half width. No retained frame ID is available, and the app/compositor boundary was not isolated.
- **Expected behavior:** The window should maximize when the compositor accepts the request.
- **Recommended fix:** Reproduce in a plain/non-Hyprland session and compare with another eframe window before assigning this to Desktop.

### F-10 — Setup Cancel hit area feels smaller than its visual button (historical)

- **Severity:** Informational / P4
- **Ownership:** Desktop
- **Reproducibility:** Observed once in the 2026-09-20 baseline; not re-run.
- **Exact reproduction:** Open the provider setup wizard and click near, but inside the apparent visual extent of, Cancel.
- **Observed evidence:** A click approximately 35 px from the button center missed on the first attempt. No retained frame ID is available.
- **Expected behavior:** The hit target should cover the visible button with normal pointer tolerance.
- **Recommended fix:** Verify egui button layout/hit rectangle and add padding or a larger interaction area.

## Expected behavior / explicitly not defects

- `/help` remaining on the Commands page after Esc matches the current page behavior and is not counted as a defect.
- Earlier unchanged pages in the original client were confounded by shared daemon/session state and sparse screenshot timing; they are not daemon findings.
- Android emulator focus, typing, and display behavior were not attributed to Desktop.
- Screenshot gaps and TCP byte-counter changes were not interpreted as backend latency measurements.
- Ctrl+T ultimately passed in isolated testing: it created a second connection to `:17878` (original local port `52306`, new local port `47228`), with both desktop and daemon send/receive queues at `0,0`. Earlier inconsistency is retained only as an intermittent observation, not a confirmed defect.
- Ctrl+W passed in the earlier clean multi-tab run (`frame-14`), although the provider-switch sequence (`frame-72`–`frame-74`) did not make its close effect conclusive.
- Ctrl+K has conflicting historical observations and remains unresolved rather than being classified as a failure.

## Verified functional behavior

| Area | Result | Evidence |
|---|---|---|
| Conversation round-trip and second-prompt queueing | PASS | Exact queued response `QUEUED-SECOND`; draft typing and Ctrl+A/Backspace clearing also passed. |
| Long-response cancellation | PASS eventually | Cancellation completed; immediate visual confirmation remains a low-confidence UX concern. |
| Multi-tab shortcuts | PASS | Ctrl+T `frame-10`, Ctrl+1 `frame-12`, Ctrl+2 `frame-13`, Ctrl+W `frame-14`; socket isolation independently confirmed. |
| `/stats` loading | PASS | Fresh chat `frame-37`; command `frame-38`; loading `frame-39`; dashboard `frame-40`; isolated raw request returned in 0.489 s with 105343 bytes. |
| Catalog loading and close | PASS | Catalog `frame-48`; Esc returned to chat `frame-49`; the current autocomplete-selected catalog was visible in `frame-84` and closed in `frame-85`. |
| Config loading/navigation | PASS | `/config` autocomplete `frame-50`, `frame-53`, `frame-64`; menu open `frame-65`; Down/Up selection `frame-66`/`frame-67`; Providers view `frame-69`; Esc close `frame-89`. |
| Autocomplete filtering and selection | PASS except Esc dismissal | `/co` filtered to `/compact` and `/config` in `frame-86`/`frame-90`; Down moved selection in `frame-87`; Enter opened config in `frame-88`. |
| Startup composer focus | PASS in isolated verification | `frame-75`/`frame-76`. |
| Error/shutdown workflow | PASS in baseline | No GUI crash, panic, or log error; dead-port state showed the expected offline UI. |

## Recommended priority

1. Fix and regression-test autocomplete Esc dismissal (F-1).
2. Fix narrow-pane clipping for the System prompt and recheck tool cards, tool descriptions, and Plugins footer (F-2–F-5).
3. Add explicit scope/risk confirmation for Auto-approve (F-6).
4. Instrument provider switching with request/response IDs and a timeout/error state before assigning ownership (F-7).
5. Re-run focus, Cancel hit-area, maximize, Ctrl+K, and immediate-cancellation checks in a clean isolated session; separate desktop behavior from compositor behavior.

## Cleanup

The isolated GUI and daemon were used only for this test segment; the original daemon/client pair was left untouched. Test conversations may remain in the isolated daemon’s SQLite state.
