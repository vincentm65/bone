# Authoritative Message Timestamps Implementation Plan

## 1. Carry timestamps through protocol and SQLite

- Add to `protocol/src/message.rs::ChatMessage`:

  ```rust
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub created_at: Option<String>
  ```

- Add `created_at` to `StoredMessage`.
- Include `messages.created_at` in `query_messages` and `query_messages_after`, updating row indexes and `stored_message_from_row`.
- In `stored_to_chat_message`:
  - Preserve a timestamp already present in `payload_json`.
  - If absent, set it from the row’s `created_at`.
  - Set it directly when reconstructing legacy normalized rows.
- Keep schema version 10. No migration, backfill, or protocol-version bump.
- Change high-level persistence paths to use each message’s supplied timestamp instead of generating an insertion-time or batch-wide value. Missing timestamps on non-display tool/system rows may receive a per-row fallback solely for the non-null database column.

## 2. Stamp user messages once in the daemon

- At `SubmitPrompt` acceptance, generate one UTC timestamp using the existing core formatter.
- Construct one authoritative user `ChatMessage` containing that timestamp.
- Use that same object/value for:
  - Runtime transcript state.
  - Any conversation/state event.
  - `append_user_to_db`.
  - SQLite `created_at` and serialized payload.
- Remove the separate user-message construction and timestamp generation in `RuntimeSession::append_user_to_db`.
- Publish acceptance only after the daemon has accepted the message and enabled persistence has succeeded; preserve incognito behavior when persistence is disabled.

## 3. Timestamp assistant stream attempts at their start

- For each model stream attempt, allocate a fresh attempt/message identity and timestamp before forwarding that attempt’s first delta.
- Emit an ordered assistant-start event before any delta for that identity.
- Retain the timestamp in driver attempt state and attach it to the final assistant `ChatMessage`.
- Persist that exact timestamp through `append_turn_with_checkpoint`; do not replace it with turn-completion time.
- If an attempt is retried:
  - Discard its identity and timestamp.
  - Give the next attempt a fresh identity and timestamp.
  - If the discarded attempt was already exposed to clients, emit an explicit discard/reset event before starting the replacement so its content and separator can be removed.
- Tool/system rows must not affect message timestamp grouping, and batch persistence must no longer assign one shared timestamp to every row.

## 4. Add authoritative live-event ordering

- Add a user acknowledgment such as:

  ```text
  UserMessageAccepted { submission_id, message: ChatMessage }
  ```

  The acknowledged message contains the authoritative `created_at`.

- Add assistant lifecycle events such as:

  ```text
  AssistantMessageStarted {
      submission_id/turn_id,
      message_id,
      attempt,
      created_at
  }
  AssistantMessageDiscarded { message_id }
  ```

- Reuse existing request/turn correlation identifiers where available; otherwise add a backward-compatible submission identifier to `SubmitPrompt`.
- Define stream ordering explicitly:
  1. User acknowledgment before user scrollback rendering.
  2. Assistant start before the first delta.
  3. Deltas belong to the active message identity.
  4. A discarded identity can never be finalized or reused.
- In the TUI, retain submitted user content as pending but do not push or flush it to scrollback until its acknowledgment arrives.
- On assistant start, initialize the streaming message with the supplied timestamp before handling deltas.
- Preserve existing event encodings and incognito-related changes; add new variants/optional fields without changing old variant serialization.

## 5. Render timestamp separators in the TUI

- Add `created_at: Option<String>` to `core/src/chat.rs::Message` and update user/assistant constructors or builders.
- During transcript rebuild, copy `ChatMessage.created_at` into user and nonempty assistant display messages. Tool rows remain timestamp-ineligible.
- Add a strict helper for the core-generated UTC format, `YYYY-MM-DDTHH:MM:SSZ`, rather than adding an unrelated date dependency. Invalid values return `None`.
- Keep renderer-level state:

  ```text
  last_displayed_timestamp: Option<unix_seconds>
  ```

- Before rendering an eligible user/assistant message:
  - Emit a separator for the first valid timestamp.
  - Thereafter emit one only when `current >= last_displayed + 300`.
  - Update the anchor only when a separator is emitted.
  - Missing, malformed, earlier, tool, and system timestamps neither render nor advance the anchor.
- Route normal messages and streamed assistants through one “begin message” timestamp path. Track whether the active streamed message has already passed that path so incremental flushes and finalization cannot duplicate its separator.
- Clear timestamp and active-stream rendering state in `Renderer::reset_scrollback_state`. Resize/replay must rebuild separators deterministically from transcript timestamps.
- When a visible stream attempt is discarded, remove it and replay authoritative scrollback so its separator does not remain as the grouping anchor.

## 6. Tests and validation

- **Protocol:** optional-field omission and missing-field deserialization; acknowledgment/start/discard round trips; unchanged existing event serialization.
- **SQLite:** both queries return `created_at`; modern payload preservation; V2 payload fallback from the DB column; legacy normalized-row reconstruction.
- **Core runtime:** user transcript/event/DB timestamps are identical; assistant start precedes first delta; final transcript and DB reuse the start timestamp; retries receive fresh timestamps and discarded attempts are not persisted.
- **TUI app:** users remain pending until acknowledgment; assistant start initializes timing before deltas; loaded transcripts preserve timestamps; discard/retry handling resets the active stream correctly.
- **Renderer:** first separator, 299 seconds, exactly 300 seconds, malformed/missing timestamps, ignored tool/system rows, incremental flushes, streamed finalization without duplication, discarded attempts, and resize/reset/replay.
- Run focused affected-package tests first, then:

  ```bash
  cargo fmt --all -- --check
  cargo test --workspace
  ```

All edits should remain narrowly scoped and preserve the 12 existing incognito-feature modifications.
