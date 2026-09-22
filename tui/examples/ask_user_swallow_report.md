# `ask_user` viewport overwrite — verified mechanism and regression fix

## Scope

This is a **renderer fault-injection regression**, not an end-to-end reproduction
of the original user's environment. The example builds a representative menu
`PanePage` and uses the real Renderer/ratatui pipeline; it does not run Lua, a live
agent, or the daemon.

Before the fix, injecting a restricted ANSI scrolling region between transcript
insertion and viewport growth caused menu text to overwrite the transcript. The
message data survived and could be replayed after a hard reset. Without that
injection, the same sequence did not reproduce the bug in tmux 3.7b.

The fix resets scrolling margins before clearing/rebuilding the inline viewport
on non-Windows terminals. It fixes the demonstrated failure without assuming an
unverified terminal class or adding an extra scroll to the normal path.

## Run the checks

From the repository root, with Bash, tmux, and the standard POSIX text tools:

```sh
cargo build -p bone --example ask_user_swallow
bash tui/examples/ask_user_swallow_repro.sh
bash tui/examples/decstbm_probe.sh
```

The scripts use fresh private tmux sockets with `-f /dev/null`, 100x24 panes,
bounded marker handshakes, and exit traps. They never kill user sessions or delete
the committed captures. New captures and geometry records are retained in unique
`target/ask-user-swallow.*` and `target/decstbm-probe.*` directories. Assertions
fail with a nonzero exit status. `REPRO_BIN` can select an alternate example binary.

## Verified mechanism

`Renderer::resize_viewport` clears the old inline viewport, flushes output, and
constructs a replacement `Terminal::with_options(Viewport::Inline(new_height))`.
Ratatui 0.29's `compute_inline_size` calls `Backend::append_lines`; Bone delegates
to `CrosstermBackend`, which writes repeated **LF (`\n`) bytes**, not CUD (`CSI B`).

At 24 rows, the old four-row viewport starts at zero-based row 20. Clearing it
positions the cursor there. Growing to 13 rows appends 12 newlines: three reach
the bottom and nine normally scroll the screen. Ratatui subtracts those nine
missing rows from the viewport top, yielding rows 11..23.

With injected `ESC[1;20r`, the cursor at row 20 is below the scrolling region
(rows 0..19). Those newlines reach the screen bottom without scrolling. Ratatui
still places the new viewport at rows 11..23. Drawing into a fresh empty buffer
then overwrites transcript rows in place, with remnants surviving where unchanged
blank cells are not painted. The sentinel at row 18 becomes:

```text
SEdarwin/arm64AGENT-LINE-4f9c
```

Phase B only resizes and draws; it does **not** call `insert_before`. Transcript
insertion in phases A and C uses ratatui's scrolling-region insertion path.

The actual scrolling-region commands in ratatui reset their margins with `ESC[r`
after use. The injection therefore demonstrates a vulnerability to restricted
margins, not evidence that normal application insertion leaves them active.

### Terminal probe

The repaired probe emits bytes directly to terminal output in raw mode. It sets
the cursor **after** DECSTBM, which otherwise homes it. A cursor-report round trip
ensures output was processed before capturing; it is a synchronization barrier,
not a test that scrolling occurred.

For 12 downward movements from zero-based row 20 in a 24-row pane, tmux 3.7b gives:

| Output | Full-screen region: history growth | Restricted region: history growth |
|---|---:|---:|
| `CSI 12 B` (CUD) | 0 | 0 |
| 12 LF bytes | 9 | 0 |

The cursor ends at row 23 in **all four cases**. Querying its final position alone
cannot distinguish successful scrolling from the failure. The old claim that
"tmux's CUD scrolls and masks the bug" was incorrect. The old `cat`-based probe
also left canonical input enabled, buffering its unterminated escape sequences;
counting sentinel occurrences could not distinguish scrolling from clamping.

## Fix

Before `term.clear()`, `Renderer::resize_viewport` now emits `ESC[r` on non-Windows
platforms. Ordering matters: resetting margins homes the cursor, then `clear()`
repositions it to the tracked viewport top and erases the old UI. Replacement
allocation can then scroll normally through its existing newline path.

No unconditional extra scroll is added, avoiding double-scrolling in the healthy
case. The Windows backend path is unchanged. This is defensive normalization for
the verified restricted-margin case, not a claim to fix every possible terminal
or streaming/event-ordering cause of a disappearing line.

## Regression checks and results

The driver runs both a normal-region control (`REPRO_SCROLL_REGION=full`) and the
fault injection (`restricted`, the example's default). It checks exact transcript
preservation, one complete sentinel in screen+history, sentinel visibility on
screen, viewport height, history size, scroll-region bounds, and successful exit.

| Phase | Action | Viewport | Expected history (clean direct launch) |
|---|---|---:|---:|
| A | Flush user and assistant messages | 4 | 5 |
| A2 | Optionally inject region rows 1..20 | 4 | 5 |
| B | Open menu | 13 | 14 |
| C | Hard reset + transcript replay | 13 | 14 |
| D | Close menu | 4 | 14 |
| E | Reopen menu | 13 | 14 |

Before the renderer change, the new check passed all control phases and failed
at restricted/B with a missing sentinel. After the change, both cases pass all
six phases. The history delta at B is exactly nine, not eighteen; shrink/reopen
does not duplicate transcript rows. The standalone probe passes all four cases.

### Historical captures

`tui/examples/captures/phase_{a,a2,b,c}.txt` and its marker files are preserved
**pre-fix artifacts**, not outputs from the current regression driver. They include
shell startup text, so their initial history count differs from a direct launch.

In those files, A/A2/B contain 33 lines: 9 history rows plus 24 screen rows. The
sentinel at capture line 28 is screen row 18 (zero-based), not row 5. C contains
38 lines: **14** history rows plus 24 screen rows, with the sentinel at capture
line 24 / screen row **9**, not 22 history rows / screen row 1.

## Remaining uncertainty

No evidence establishes that the original user had restricted scrolling margins.
The normal tmux path passed even before this fix. Reproducing any remaining live
`ask_user` failure still requires the actual terminal and event sequence; the
fault injection must not be presented as proof of that original root cause.
