# bone-desktop GUI — Computer-Use Test Findings (v2.4.5)

Date: 2026-09-20
Method: live GUI testing via computer use (not just the test suite), on Hyprland (monitor DP-3).
Environment: eframe/egui client (debug build, `target/debug/bone-desktop`), daemon `bone serve` at 127.0.0.1:7878, local llama.cpp provider (Qwen3.8-27B-Q4_K_M) at 127.0.0.1:8081.

## Step results — 8/8 PASS

| # | Step | Result | Notes |
|---|------|--------|-------|
| 1 | Build + CLI smoke | PASS | `cargo build -p bone-desktop` 17.8s; `--version` → 2.4.5; `--help` ok; `--connect 10.0.0.5:7878` rejected with SSH-tunnel guidance (rc=2); `--badflag` → unknown-argument + usage (rc=2) |
| 2 | Launch & connect | PASS | Window renders, green "Connected", conversation sidebar, `local` provider badge in composer |
| 3 | Conversation round-trip | PASS | "Reply with exactly: OK" → model replied "OK"; stats line updated ("curr 3,155 · in 3,155 · out 23 · total 3,178 · worked for 0:10"); busy composer correctly showed Send now / Queue next / Stop with key hint |
| 4 | Tool card | PASS | `read_file` card with file header, expandable args JSON + 5-line output (model's "hash bone" quote was a model hallucination, not a UI bug) |
| 5 | Palette / keys | PASS | Ctrl+K palette lists all 10 commands with descriptions; filtering works; Enter executes (opened Settings) |
| 6 | Dialogs | PASS | Settings side-pane (Approval mode=safe, Show reasoning=Off, System prompt) opens/closes; Usage modal with real data (13,311 tokens, 4 requests, 49% cache hit, bar chart, Models table); Plugins tab: Browse 16 / Installed 1 / Updates 0, full catalog with Install buttons, web_search shows Disable; Setup wizard opens as 5-step "Provider setup" modal with Start/Cancel, cancelled without saving |
| 7 | Multi-tab | PASS | Ctrl+T created tab 2 (welcome screen w/ quick-start buttons); Ctrl+1/Ctrl+2 switch with per-tab state intact; Ctrl+W closed empty tab gracefully — focus fell back to remaining tab and sidebar updated |
| 8 | Error & shutdown | PASS | Second instance vs dead port 9999: red "Daemon offline" badge + red banner "no daemon responding at 127.0.0.1:9999 … Custom ports never auto-start a daemon", "Waiting for the daemon…" sidebar, disabled "Model …" composer — no crash; instance killed cleanly; main GUI SIGTERM-exited promptly with no log errors; daemon still alive |

No crashes, no hangs, no panics in the GUI log.

## UX findings (bad/broken list)

1. **Tool card clipping (minor):** args JSON and long output lines hard-clip at the right edge — no wrap, no horizontal scroll. Long paths/lines are silently truncated with no "…" affordance.
2. **Tools menu description clipping (minor):** option descriptions cut off at right edge ("appears in the transcrip…").
3. **Plugins pane footer clipped (minor):** the "Enable or disable plugins; install, update, or remove them" footer text is cut at the bottom edge of the pane.
4. **Focus edge case (minor):** first Ctrl+T right after launch/menu use was silently dropped until the window was clicked.
5. **Composer not auto-focused after restart (minor):** after app restart, the first typed text silently went nowhere; had to click the composer. Auto-focus composer on window restore would fix this.
6. **Tool-permissions toggle is under-protected (safety-relevant):** "Ask" vs "Auto-approve" in the Tools menu is an instant no-confirmation toggle that affects all tasks on the server. A confirm step or visible scope warning is warranted.
7. **Maximize didn't work (unresolved, likely environment):** Super+Up and title-bar maximize both left the window at ~half width. Could not isolate whether app or Hyprland is at fault; worth a quick repro on a plain session.
8. **Wizard Cancel click target small (nit):** first Cancel click 35px right of the button center missed; the button hit-area feels tighter than its visual extent (egui rounding) — harmless but noticeable under precise input.

## Verified keybindings (all worked as documented)

Ctrl+K palette, Ctrl+P switch-task, Ctrl+T new tab, Ctrl+W close tab, Ctrl+1–9 select,
Ctrl+PageUp/Down cycle, Ctrl+Shift+N new window, Ctrl+\ / Ctrl+Shift+\ split,
Enter send, Shift+Enter newline, Ctrl/Cmd+Enter send-now/steer, Esc closes menus/modals,
Ctrl+Up/Down prompt history.

Also in source: `BONE_DESKTOP_DEMO` env enables demo mode.

## Cleanup

Both GUI test instances exited; daemon (127.0.0.1:7878) and llama.cpp server left running.
Test conversations persisted to daemon SQLite and appeared in the sidebar ("Reply with exactly OK" under "Today").
