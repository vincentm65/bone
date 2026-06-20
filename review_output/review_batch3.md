# Review Batch 3: `/src/llm/`

Reviewed 7 files, 1562 total lines.

---

## /home/vincent/projects/bone/src/llm/mod.rs
- **Lines:** 10
- **Assessment:** mostly good
- **Notes:** Minimal module declarations and re-exports. No fat to trim. The `pub use` lines re-export from `provider` and `token_tracker` — this is normal facade practice. Nothing to simplify.

---

## /home/vincent/projects/bone/src/llm/prompts.rs
- **Lines:** 56
- **Assessment:** mostly good
- **Notes:** Two prompt-builders (`system_prompt`, `subagent_system_prompt`) that share the same `cwd` + `bone_dir()` boilerplate. Could extract a small helper like `env_context()` to avoid the duplicated `current_dir()` / `bone_dir()` calls, but that is a minor polish, not over-engineering. The `SYSTEM_PROMPT` static is clean. No feature loss concerns.

---

## /home/vincent/projects/bone/src/llm/provider.rs
- **Lines:** 234
- **Assessment:** can be simplified
- **Notes:**
  - `ChatRole::as_str()` is a manual method for something `#[derive(strum::AsRefStr)]` or even simple `match` in callers would cover; it is used only once in the codebase (in `openai_compat`). The entire `ChatRole` enum plus `as_str()` could be collapsed.
  - `ChatMessage::new()` vs `ChatMessage::assistant_with_tools()` vs `ChatMessage::tool()` — three constructors for one struct. `tool()` takes a `ToolResult` while the others take `impl Into<String>`; this inconsistency is minor but adds to surface area. Could reduce to just `new()` with builder-pattern setters.
  - `http_status_to_error_kind(&str)` duplicates the logic already inside `From<reqwest::Error> for LlmError`. Callers currently call both code paths. Either remove the free function or refactor `From<reqwest::Error>` to delegate to it.
  - `LlmErrorKind` is `#[non_exhaustive]` and has 7 variants, including `Server(u16)`. Only `Server`, `Auth`, and `RateLimit` are actually matched on downstream. Consider trimming to the needed subset.
  - `LlmProvider::validate()` defaults to `Ok(())` — both `CodexProvider` and `OpenAiCompatProvider` accept the default. If no provider overrides it, remove from the trait entirely or make it non-required.
  - The `Reasoning` struct with an opaque `echo_field` is necessary for provider round-tripping but adds mental overhead. Could be a simple `Option<String>` on `ChatMessage` instead of a nested struct.
  - `impl Error for LlmError {}` is a no-op derive. If no code does `source()` or `downcast_ref()` on it, this is dead weight.

---

## /home/vincent/projects/bone/src/llm/token_tracker.rs
- **Lines:** 99
- **Assessment:** mostly good
- **Notes:**
  - `TokenStats::new()` is redundant — `#[derive(Default)]` already provides `TokenStats::default()` with identical behaviour. Could drop `new()` to reduce API surface.
  - `format_tokens()` is a thin wrapper around `num_format::ToFormattedString`. The dependency (`num-format`) is pulled in for a single call. Could be replaced with a manual thousands-separator implementation to remove the dependency, but the current approach is clear and unlikely to be a maintenance burden.
  - Otherwise clean and focused. No over-engineering.

---

## /home/vincent/projects/bone/src/llm/providers/mod.rs
- **Lines:** 56
- **Assessment:** mostly good
- **Notes:** Straightforward factory function dispatching on `entry.handler`. Error messages are helpful. The test exercises the bundled defaults. Nothing to simplify.

---

## /home/vincent/projects/bone/src/llm/providers/codex.rs
- **Lines:** 570
- **Assessment:** over-engineered
- **Notes:**
  - **Largest file** in the module at 570 lines, exceeding openai_compat's 537 lines even though Codex is a less common provider.
  - Reuses `PartialToolCall`, `flush_partial_tool_calls` from `openai_compat` (via `use super::openai_compat::...`). This creates a cross-provider coupling — if openai_compat changes those internals, codex breaks. Should extract a shared SSE tool-call-accumulation module instead.
  - Manages three mutable tracking structures in the stream loop (`partial_tool_calls`, `emitted_tool_call_ids`, `last_usage`) plus a `BTreeSet` to de-duplicate tool call IDs across `response.output_item.done` and `response.completed`. This dedup complexity exists because the Codex Responses API can emit the same tool call via both streaming and final-response paths.
  - `resolve_codex_api_key()` reads `~/.codex/auth.json` — a side-effect hidden inside `chat_stream()`. This breaks the principle that config comes from the `ProviderEntry`. Consider resolving at construction time instead.
  - `CodexInputItem` has three enum-like variants (Message, FunctionCall, FunctionCallOutput) but is `#[serde(untagged)]` — the raw JSON shape is re-embodied in struct fields rather than using a true `#[serde(tag = "type")]` enum. This makes the serde contract harder to reason about.
  - `CodexContent` has two variants (InputText, OutputText) with similar structure — could be collapsed to a single `Text { text: String, kind: &'static str }` or a simpler representation.
  - `extract_response_events` has `#[allow(clippy::type_complexity)]` — a clear smell. Returns `(Vec<ChatEvent>, Option<(u32, u32, Option<u32>)>)` which is hard to read. Could use a named struct.
  - `build_instructions()` produces a hardcoded fallback "You are a helpful assistant." when no system messages exist — duplicates what prompts.rs already does. Could be removed if the caller always ensures a system message.
  - `build_codex_messages()` silently drops `ChatRole::System` messages (they go into `instructions` instead). This split is non-obvious and easy to get wrong when adding features.

---

## /home/vincent/projects/bone/src/llm/providers/openai_compat/mod.rs
- **Lines:** 537
- **Assessment:** can be simplified
- **Notes:**
  - **Large, but well-structured.** The extraction of `process_sse_chunk()`, `ThinkParser`, `flush_partial_tool_calls()`, and `delta_has_reasoning_field()` into named functions/structs is good practice.
  - **`ThinkParser`** is ~70 lines of careful streaming logic for stripping inline `<think>…</think>` tags. This is necessary for providers that emit reasoning inline (Qwen, MiniMax), but adds significant complexity for an edge case that some users may never hit. Consider gating behind a config flag.
  - **`delta_has_reasoning_field()`** re-parses the SSE data JSON that was already parsed by `process_sse_chunk()`. The data is deserialized twice per chunk: once in `process_sse_chunk()` and once in `delta_has_reasoning_field()`. Could be merged to avoid double-parsing.
  - **`stream_options`** logic uses URL sniffing (`self.base_url.contains("api.openai.com")` etc.) to decide whether to request usage data. This is fragile — a self-hosted OpenAI-compatible server at a custom domain won't get usage. Should be a config-level flag instead.
  - **Reasoning echo-back** uses `#[serde(flatten)]` on a `BTreeMap<String, String>` in `OpenAiMessage`, keyed by an opaque wire key. This is flexible but opaque — any type can flow through unchecked. If only two keys (`reasoning_content`, `thoughts`) are used in practice, a simpler enum + string field would be more maintainable.
  - **`ChatRequest`** always sets `stream: true` — this is hardcoded; the field could be removed from the struct entirely.
  - **Error construction** in `chat_stream()` duplicates the "capped body" pattern from `codex.rs` with slightly different formatting. Both should share a helper.

---

## Summary

| File | Lines | Assessment |
|---|---|---|
| `src/llm/mod.rs` | 10 | mostly good |
| `src/llm/prompts.rs` | 56 | mostly good |
| `src/llm/provider.rs` | 234 | can be simplified |
| `src/llm/token_tracker.rs` | 99 | mostly good |
| `src/llm/providers/mod.rs` | 56 | mostly good |
| `src/llm/providers/codex.rs` | 570 | **over-engineered** |
| `src/llm/providers/openai_compat/mod.rs` | 537 | can be simplified |

**Key simplification opportunities:**

1. **Cross-provider coupling:** `codex.rs` imports `PartialToolCall` and `flush_partial_tool_calls` from `openai_compat`. Extract a shared `sse_utils` module.
2. **Duplicate error-body handling:** Both providers cap/surface error bodies identically. Extract a helper.
3. **Double JSON parse in openai_compat:** `delta_has_reasoning_field()` re-parses data already parsed in `process_sse_chunk()`.
4. **URL-based feature sniffing in openai_compat:** Replace `base_url.contains(...)` gating with a config field.
5. **`provider.rs` fat:** `http_status_to_error_kind` duplicates `From<reqwest::Error>`; `ChatRole::as_str()` is unused outside the module; dead `impl Error` trait.
6. **`token_tracker.rs`:** Drop `TokenStats::new()` in favour of `Default`.
