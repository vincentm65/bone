# Section 9 — TUI rendering, panes, transcript, and terminal compatibility

**Review goal:** production-ready cleanup — correctness of presentation as a deterministic projection of protocol/view state, terminal-dimension safety, streaming/scrollback invariants, and unnecessary dual paths or fragile render couplings.  
**Scope:** `tui/src/ui/render/**`; `*pane*.rs`, `fullscreen.rs`, `stats.rs`, `theme.rs`, `tool_display.rs`, `transcript_view.rs`, `color.rs`; rendering/backend/wrap/messages/tool-display/diff-preview/Unicode/bottom-pane tests. `webui/` excluded.  
**Mode:** initial investigation followed by targeted remediation of findings 1–3.

> Working-tree note: untracked section reports 1–2, 4–8 predate this report. Findings describe the tree as read during this pass. An earlier partial draft of this section overstated table overflow and backend EL edge cases; those are corrected below.
>
> **Remediation status:** Findings 1–2 were fixed in `messages.rs` with regression coverage in `messages_test.rs`. Finding 3 was downgraded to Low and its test helper was aligned with production. The remaining Low items are accepted limitations or optional cleanup, not merge blockers.

---

## Scope and checks

- **Files reviewed (LOC approx.):**
  - Render: `mod.rs` (636), `backend.rs` (210), `wrap.rs` (98), `messages.rs` (1036), `markdown.rs` (821), `bottom_pane.rs` (1183), `render_tests.rs` (50)
  - Panes / viewers: `jobs_pane.rs` (315), `queue_pane.rs` (89), `processes_pane.rs` (47), `pane_page.rs` (122), `transcript_view.rs` (157), `fullscreen.rs` (75)
  - Display / theme: `tool_display.rs` (446), `theme.rs` (633) + `theme_tests.rs`, `stats.rs` (984), `color.rs` (46)
  - Tests: `render_test` (392), `wrap_test` (48), `messages_test` (210), `unicode_stress_test` (82), `backend_test` (36), `shell_render_test` (285), `bottom_pane_test` (597), `diff_preview_test` (120), `tool_display_test` (341); unit modules under jobs/pane_page/queue
- **Tests/commands run:** targeted unit tests for jobs/pane_page/queue and markdown render helpers (passed during this pass). No production edits.
- **Unreviewed / uncertain:** full `stats.rs` chart arithmetic beyond entry/loop/layout; every theme highlight group beyond set/reset tests; Windows-specific backend paths (`#[cfg(not(windows))]` EL path); live multi-emulator resize matrix.

---

## Architecture and invariants

### Entry points

```text
App redraw / stream tick
  ├─ Renderer::ensure_viewport_height → resize_viewport / replace_terminal
  ├─ Renderer::flush_new_to_scrollback / flush_streaming_message / finalize_streaming_message
  │     └─ insert_lines_to_scrollback → dedup_scrollback_blanks → term.insert_before
  │           └─ messages::msg_to_lines* / assistant_markdown_to_lines
  │                 ├─ markdown::render_markdown (+ syntect highlight)
  │                 ├─ wrap_user_line / wrap_text*
  │                 └─ tool content styling (split_content_into_links_and_text, shell lex)
  └─ Renderer::draw_bottom_pane* → bottom_pane layout (input, pages, approval, autocomplete)

Fullscreen takeovers
  fullscreen::run  → RawModeGuard + EnterAlternateScreen + BoneBackend Terminal
    ├─ transcript_view::run / run_collapsed  (+ MouseCaptureGuard)
    ├─ stats::run
    └─ setup / catalog (owned by Section 8 orchestration; same scaffolding)

Pane projections (pure; App owns upsert into PanePage list)
  jobs_pane::{render, render_selected}
  queue_pane::render
  processes_pane::render
  PanePage::from_content (wire PaneContent → ratatui Lines)
```

### Owned state

| Owner | State |
| --- | --- |
| Daemon / protocol | messages, tool results, pane content payloads, token stats source |
| `Renderer` | theme snapshot, viewport height, scrollback cursor / last-blank, streaming source offset |
| `App` (view-model) | `messages`, pane pages + active index, input, approval UI, streaming flags (Section 8) |
| Terminal / OS | native scrollback buffer, raw mode, alt-screen, mouse capture |

### Trust boundaries

- Render is a **projection**: no tool execution, no protocol authority.
- Pane modules only format snapshots; cancel/selection logic lives in App (Section 8 dual-path notes).
- `stats` loads via caller-provided closure (local `session_db` ownership called out in Section 8).
- OSC/raw/alt-screen/mouse must restore on all exit paths (Drop guards + `prepare_exit` / `shutdown_terminal`).

### Required invariants

1. Streaming fragments flushed only at block-safe boundaries (`safe_markdown_prefix_end`); finalize emits the remainder; stream render ≡ bulk render for complete content.
2. Scrollback inserts measure width against the **viewport** width, not a lagging live size (avoids ratatui buffer OOB).
3. Inline viewport height ≤ `max_viewport_height(terminal_height)` = `height.saturating_sub(1).max(1)` (reserves one row for `insert_before` safety).
4. Pane visible rows clamped to `1..=MAX_PANE_ROWS` (24); layout uses saturating arithmetic; tiny viewports must not panic.
5. Fullscreen / mouse / raw mode restore via RAII even on body error.
6. Unicode width wrapping must not panic on multi-byte/ZWJ/CJK; final terminal column of styled user rows left unpainted when inserting into scrollback.
7. Table layout must fit terminal width (boxed when possible, pipe fallback when not).
8. Tool summary rows are labels only; shell content retained for expanded transcript viewer.

### Rendering invariant checklist (exit condition)

| Case | Status |
| --- | --- |
| Narrow terminal / tiny viewport | Exercised: `bottom_pane_test` tiny presets; wrap/messages leave last column free; remaining gap at width 1–3 user markers |
| Wide content / long shell | Exercised: shell_render + tool_display truncation/heredoc |
| Unicode / emoji / CJK / RTL | Exercised: `unicode_stress_test`, shell non-ASCII lexer |
| Streaming vs bulk markdown | Exercised: `render_test` stream identity; seam blank reinsert |
| Pane-heavy layout | Exercised: jobs/queue unit tests + bottom_pane page height; processes projection untested in isolation |
| Resize / hard reset | Code path reviewed; no automated cascade test |
| Fullscreen teardown | Code path reviewed (RAII); no panic-inject test |

---

## Findings

### 1. [High, resolved] Tool-content link splitter drops incomplete trailing URL schemes

**Confidence:** verified  
**Evidence:**  
- `messages.rs:276-303` `split_content_into_links_and_text`  
- When `find_link_start` matches a scheme but `find_link_end` returns `None`, branch sets `start = len` without pushing `text[link_start..]` (`messages.rs:287-291`).

**Scenario:** Tool content beginning with `"file://"` (or bare `http://` with no host chars): the scheme bytes were neither `Text` nor `Link`, causing silent content loss in the rendered tool row. A scheme after ordinary text was preserved before remediation because Finding 2 prevented it from being recognized.

**Root cause:** Error path treats “matched scheme, no link body” as consume-to-end rather than emit-as-text.

**Impact:** User-visible omission of trailing characters in tool output (rare, but correctness bug).

**Recommended fix:** On `find_link_end == None`, push `ContentPart::Text(text[link_start..].to_string())` then break (or advance by one byte and continue).

**Regression test:** `tool_content_preserves_incomplete_url_scheme` verifies that `"file://"` is preserved as text.

**Resolution:** The no-body branch now emits the unmatched remainder as `Text` and stops scanning.

---

### 2. [Medium, resolved] Tool-content link detection never scans forward from mid-string

**Confidence:** verified  
**Evidence:**  
- `find_link_start` (`messages.rs:305-329`) only inspects the byte at `start` (scheme windows from that offset, or `/` `$` `~/` `./` `../` at that byte).  
- On `None`, the loop pushes the **entire remainder** as one `Text` part and breaks (`messages.rs:293-295`).

**Scenario:** `"see /tmp/foo for details"` or `"visit https://example.com later"` — no path/URL highlight; whole string is plain tool color.

**Root cause:** API shape implies a scanner (`find_link_start(bytes, start) → Option<(link_start, link_end)>`) but implementation is a point check, not a forward search.

**Impact:** Cosmetic only (no content loss). Affects `style_tool_content_line` / `render_tool_content`, not assistant markdown links.

**Recommended fix:** Scan remaining bytes for next trigger (`/`, `$`, `~`, `.`, `h`/`f` for schemes) then validate; or reuse a small `memchr`-style loop.

**Regression test:** `tool_content_highlights_links_after_plain_text` verifies distinct path and URL spans with surrounding text preserved.

**Resolution:** `find_link_start` now scans each remaining byte for a validated scheme or path trigger.

---

### 3. [Low, resolved] Streaming test helper passes `from=0` into `safe_markdown_prefix_end`, unlike production

**Confidence:** verified  
**Evidence:**  
- Production: `flush_streaming_message` → `safe_markdown_prefix_end(content, self.streaming_source_flushed)` (`mod.rs:392-401`).  
- Test helper: `safe_markdown_prefix_end(&content, 0)` always (`tests/render_test.rs:113-114`), while it already tracks `stable_source` for slice rendering.

**Scenario:** Once the helper flushes an initial complete block, subsequent calls still rescan the entire accumulated source instead of exercising production's non-zero offset. Safe flush boundaries mean an open fence/table should not actually straddle that offset, so this was test-fidelity drift rather than a demonstrated production defect.

**Root cause:** Helper mirrors fragment splicing but not the production `from` argument.

**Impact:** The stream-identity test did not exercise production-style non-zero source offsets.

**Recommended fix:** Call `safe_markdown_prefix_end(&content, stable_source)`.

**Regression test:** The existing mixed-block streaming identity fixture now exercises non-zero offsets before fenced code and tables.

**Resolution:** The helper now passes its tracked `stable_source`, matching `Renderer::flush_streaming_message`.

---

### 4. [Low] `wrap_user_line` degrades markers at extreme narrow widths

**Confidence:** verified  
**Evidence:** `messages.rs:895-908` — `width = width.saturating_sub(1).max(1)` then `prefix_limit = width.saturating_sub(required_content_width)` and `truncate_to_display_width` on `"> "` / indent.

**Scenario:** Terminal width ≤ ~3–4 columns: `"> "` truncates to `">"` or empty; continuation indent may vanish. No panic.

**Root cause:** Correct “leave final column free” policy without a minimum usable content/prefix budget.

**Impact:** Cosmetic misalignment only at pathological widths.

**Recommended fix:** Optional: if `prefix_limit == 0`, render content-only wrap; document floor width.

**Regression test:** `wrap_user_line("hi", true, 3)` no panic; content `"hi"` present.

---

### 5. [Low] `take_width` returns a wide character that alone exceeds the column budget

**Confidence:** verified  
**Evidence:** `wrap.rs:84-98` — if first char’s width > `width`, returns `idx + ch.len_utf8()` so the line includes that char.

**Scenario:** `wrap_text("你", 1)` → one line of display width 2.

**Root cause:** No zero-width progress alternative without dropping or replacing the character.

**Impact:** Possible terminal auto-wrap on width-1 columns with CJK/emoji. Inherent limitation.

**Recommended fix:** Document; optional ellipsis replacement when `ch_width > width`.

**Regression test:** Capture current behavior for width-1 CJK (already adjacent unicode stress coverage).

---

### 6. [Low] `display_local_link` does CWD I/O during markdown render

**Confidence:** verified  
**Evidence:** `markdown.rs:590-608` — each local link calls `std::env::current_dir()` and `canonicalize`.

**Scenario:** Assistant message with many local links (or re-render of large history into scrollback) pays repeated FS ops on the UI thread.

**Root cause:** Relative display convenience implemented per-link without caching.

**Impact:** Latency / allocation pressure only; errors fall back to raw target.

**Recommended fix:** Resolve CWD once per `render_markdown` (or pass in); cache on `Renderer`.

**Regression test:** n/a (behavior-preserving refactor).

---

### 7. [Low] User-message scrollback background fill inferred from span styles

**Confidence:** verified  
**Evidence:** `mod.rs` `render_scrollback_lines_with_bg` (~502-504): if any span’s `bg == user_msg_bg`, fill the whole row.

**Scenario:** Future change to Paragraph-level bg or separator styling silently drops full-row EL fill for user rows.

**Root cause:** Implicit convention instead of an explicit “user row” flag on the logical line.

**Impact:** Fragile coupling; hard-to-spot visual regression.

**Recommended fix:** Tag lines at `msg_to_lines` time, or pass a parallel bitset.

**Regression test:** Flush user message through scrollback path; assert EL / full-width bg in backend output.

---

### 8. [Low] `backend_test` depends on live terminal width

**Confidence:** verified  
**Evidence:** `tests/backend_test.rs:11` `crossterm::terminal::size().unwrap().0`.

**Scenario:** Headless CI without a TTY may fail `size()` or return 0, breaking the EL assertion.

**Root cause:** Test coupled to host geometry; EL logic only needs a chosen buffer width.

**Impact:** Flaky/non-portable test, not production.

**Recommended fix:** Hardcode width (e.g. 80) for the buffer.

**Regression test:** Run under `script`/`pty` or headless with fixed width.

---

### 9. [Low] Pane summary truncation uses char counts, not display width

**Confidence:** verified  
**Evidence:**  
- `processes_pane.rs:20-25` `.chars().take(72)`  
- `queue_pane.rs:22-23` `chars().count() > 72` / `take(69)`  
- `jobs_pane.rs:70-76` similar 36-char caps  

**Scenario:** CJK/emoji-heavy command or queue text can visually exceed the intended column budget (or under-fill for narrow glyphs).

**Root cause:** Display-width helpers exist in wrap/messages but pane projections use char counts.

**Impact:** Cosmetic overflow in pane rows; no panic (bottom pane still clips by area).

**Recommended fix:** Share a `truncate_to_display_width` helper for pane labels.

**Regression test:** Queue/process line with wide chars stays ≤ N columns.

---

### 10. [Low] `highlight_line` allocates a newline-terminated `String` per code line

**Confidence:** verified  
**Evidence:** `markdown.rs:563-567` `format!("{line}\n")` required by syntect newlines syntax set (comment at 560-562 documents why).

**Scenario:** Large streamed code blocks → one allocation per line per highlight pass.

**Root cause:** Grammar requirement, not accidental.

**Impact:** Allocation pressure only; correctness depends on the newline.

**Recommended fix:** Keep unless a zero-copy terminator API appears; optional stack buffer for short lines.

**Regression test:** Existing `line_comment_scope_does_not_leak_into_next_code_line` must keep passing.

---

## Non-findings / corrected intermediate claims

| Claim (earlier draft) | Resolution |
| --- | --- |
| `fit_table_width` leaves tables wider than terminal | **Incorrect.** After fit, `boxed = table_total_width <= width`; non-boxed uses `table_pipe_row` (`markdown.rs:222-235`). Covered by `table_fallback_fits_width_smaller_than_frame_overhead`. |
| `background_suffix_start` erases content on width-1 lag | **Incorrect / inverted.** Requires `last_x + 1 == width` **and** background fill (`backend.rs:161-165`). Mismatched last column → `None` (paint path). |
| Streaming flush / `max_viewport_height` unsound | **Rejected.** Block-boundary flush + seam blank reinsert is intentional; viewport reserves one row (`mod.rs:41-42`). |
| `replace_terminal` drop without flush | Callers `resize_viewport` / `hard_reset_viewport` flush first. Defensive note only — not filed as a defect. |

---

## LOC reduction and streamline plan (ordered)

| Step | Action | Est. ΔLOC | Depends |
| --- | --- | --- | --- |
| A | Fix trailing-scheme drop + forward scan in `split_content_into_links_and_text` | ~+15–30 net | unit tests |
| B | Align `streamed_text` helper `from=` with production; add straddle case | ~+20 tests | — |
| C | Cache CWD in `render_markdown` / link display | ~0 / −syscalls | — |
| D | Explicit user-row bg tagging for scrollback fill | ~+10–20 | optional |
| E | Display-width truncation for pane labels | ~+10 shared helper | — |
| F | Hardcode backend_test width; optional narrow `wrap_user_line` fixture | tests only | — |

No large dual-path collapse in this layer: panes are already pure projections; markdown/stream is a single path. Main cleanup is correctness (A) and test fidelity (B), not LOC deletion.

---

## Coverage gaps

Closed in remediation: link splitting now has end-to-end regression coverage through expanded tool rendering, and the mixed-block streaming identity fixture now exercises production-style non-zero `safe_markdown_prefix_end` offsets.

Remaining gaps:

1. Full-row user-message background in scrollback (EL path) not integration-tested.  
2. Rapid resize cascade: `resize_viewport` ↔ `hard_reset_viewport` ↔ streaming flush state (`scrollback_cursor`, `streaming_source_flushed`, `viewport_height`) untested.  
3. `processes_pane` has no dedicated unit test (jobs/queue do).  
4. `transcript_view` / `fullscreen` teardown under body error not automated.  
5. Syntax highlight languages beyond those covered in render tests (Haskell/Lua `--` etc.).  
6. Width 0 / 1 full pipeline (`init_terminal` → desired_height → draw) not end-to-end tested (bottom_pane tiny cases help).

---

## Clean areas verified

- **Streaming architecture:** `flush_fragment` + `safe_markdown_prefix_end` + finalize; seam blank + `dedup_scrollback_blanks`; O(N) incremental flush (`mod.rs:415-442`).  
- **Scrollback width safety:** `scrollback_insert_width` / measure-against-viewport comment (`mod.rs:285-294`); underallocated-height regression in `render_tests.rs`.  
- **Viewport height:** `max_viewport_height` reserves one row; `ensure_viewport_height` clamps desired height.  
- **Wrap core:** `take_breakable_width` / indent preservation; unicode stress suite.  
- **Markdown tables:** fit + boxed vs `table_pipe_row` fallback; fence unwrap for ```` ```markdown ```` tables.  
- **Shell render:** lexer UTF-8 safe; 5-line head/marker/tail gutter; non-ASCII shell tests.  
- **Bottom pane:** saturating y/`content_bottom` math; `InputStyle::content_rect` / `input_width` clamps; `clamped_pane_visible_rows` 1..=24; tiny viewport tests.  
- **Pane projections:** jobs/queue/processes pure; `PanePage::upsert`/`remove` index rules; queue selection scroll math.  
- **Fullscreen / transcript:** `RawModeGuard` only disables if it enabled; always leave alt-screen; mouse capture Drop guard; scroll clamps on resize.  
- **Tool display:** shell/heredoc label expansion; `read_file` footer exclusion; template/args display; solid `tool_display_test` coverage.  
- **Theme / color:** set/reset/reject for highlight groups; named + hex parse; shell_* groups covered in `theme_tests`.  
- **Backend EL optimization:** full-width background via erase-to-EOL instead of space padding (when row fills width).

---

## Cross-section notes

- **Section 8 (App lifecycle):** hard-reset on load/resize, quit `prepare_exit`/`shutdown_terminal`, and pane key handling dual paths are orchestration-owned; this section owns draw correctness only. Jobs inject dual-path remains Section 8.  
- **Section 8 stats DB ownership:** `stats::run` is a pure fullscreen viewer of whatever the loader returns; durable ownership fix is not render.  
- **Section 2 / protocol:** `PaneContent` wire shape → `PanePage::from_content` is the single client projection for Lua panes.

---

## Summary

| Severity | Count | Notes |
| --- | --- | --- |
| Critical | 0 | |
| High | 0 open / 1 resolved | Incomplete-scheme content loss fixed with regression coverage |
| Medium | 0 open / 1 resolved | Mid-string tool-link scan fixed with regression coverage |
| Low | 6 observations + 1 documented intentional cost / 1 resolved | Streaming helper aligned; remaining items are optional cleanup or accepted limitations |
| Coverage gaps | 6 | Link and non-zero-offset coverage added; integration/terminal edge gaps remain |

The render stack is structurally sound: single markdown/stream path, deliberate viewport and scrollback guards, pure pane projections, and RAII terminal cleanup. The tool-content link splitter and streaming test-helper drift have been remediated. Remaining findings are non-blocking Low observations. Table overflow and backend EL “races” from the partial draft are **not** defects.

---

## Exit condition checklist

- [x] Rendering invariant checklist across narrow, wide, Unicode, streaming, pane-heavy cases  
- [x] Chronological/incremental scrollback and resize behavior reviewed  
- [x] Pane layout arithmetic / clamps at tiny dimensions reviewed  
- [x] Terminal cleanup (raw, alt-screen, mouse, prepare_exit) reviewed  
- [x] Dual-path / overstated claims corrected with source evidence  
- [x] Fixes applied and regression-covered
