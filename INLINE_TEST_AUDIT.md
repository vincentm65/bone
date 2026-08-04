# Inline Test Audit

## Summary

| Metric | Count |
|---|---|
| Files with inline `mod tests {` | 36 |
| Total inline test LOC | 3,065 |
| Total `#[test]` + `#[tokio::test]` functions | 114 |
| Project pattern (`#[path = "..."]` re-exports) | 35 separate `*_tests.rs` files |

All 36 files below use `#[cfg(test)]` + inline `mod tests {` instead of the project's established `#[path = "..."]` re-export pattern.

---

## Large (>100 LOC)

| LOC | Tests | File |
|---|---|---|
| 608 | 12 | `core/src/config/store.rs` |
| 365 | 14 | `core/src/config/mod.rs` |
| 231 | 7 | `tui/src/ui/selectable_pane.rs` |
| 230 | 9 | `tui/src/ui/catalog.rs` |
| 136 | 8 | `core/src/llm/providers/codex.rs` |

## Medium (20–100 LOC)

| LOC | Tests | File |
|---|---|---|
| 216 | 8 | `core/src/config/settings.rs` |
| 122 | 3 | `core/src/ext/settings_registry.rs` |
| 103 | 4 | `core/src/processes.rs` |
| 69 | 2 | `tui/src/ui/setup.rs` |
| 68 | 4 | `tui/src/ui/stats.rs` |
| 66 | 2 | `core/src/tools/command_policy/mod.rs` |
| 63 | 3 | `core/src/ext/lua_tool.rs` |
| 61 | 4 | `core/src/ext/provider_slots.rs` |
| 58 | 1 | `tui/src/ui/picker.rs` |
| 58 | 1 | `core/src/config/domains.rs` |
| 48 | 2 | `tui/src/ui/process_view.rs` |
| 45 | 3 | `core/src/config/theme.rs` |
| 44 | 2 | `core/src/ext/ops_plugins.rs` |
| 42 | 2 | `protocol/src/config.rs` |
| 42 | 2 | `core/src/tools/write_atomic.rs` |
| 41 | 2 | `tui/src/ui/processes_pane.rs` |
| 38 | 1 | `core/src/commands.rs` |
| 37 | 2 | `protocol/src/message.rs` |
| 36 | 1 | `tui/src/ui/render/backend.rs` |
| 32 | 2 | `core/src/tools/approval.rs` |
| 22 | 1 | `tui/src/ui/commands/mod.rs` |
| 21 | 2 | `core/src/llm/prompts.rs` |
| 20 | 1 | `core/src/llm/provider.rs` |

## Small (<20 LOC)

| LOC | Tests | File |
|---|---|---|
| 19 | 1 | `core/src/ext/ops_commands.rs` |
| 17 | 1 | `tui/src/ui/app/keymap.rs` |
| 16 | 1 | `core/src/config/providers_config.rs` |
| 15 | 1 | `core/src/build_info.rs` |
| 14 | 1 | `core/src/config/error.rs` |
| 12 | 1 | `core/src/ext/snapshots.rs` |
| 12 | 1 | `tui/src/ui/color.rs` |

---

## Notes

- `tui/src/ui/render/backend.rs` uses `#[cfg(all(test, not(windows)))]` instead of plain `#[cfg(test)]`.
- The 35 existing `*_tests.rs` files follow the pattern `#[cfg(test)]\n#[path = "..."]\nmod ...;` — this is the target pattern for migration.
