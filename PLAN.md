# Subagent Tool with Fresh Chat Manager

## Goal

Add a tool that spawns subagents — isolated chat sessions with their own message history, running concurrently with the parent agent. Subagents share the same LLM provider and tool registry but operate on a fresh transcript.

## Architecture

### Current State

- `App` (src/ui/app/mod.rs) is monolithic: terminal rendering + event loop + state + chat loop
- Chat loop lives in `stream.rs` (~800 lines), tightly coupled to `App` and `BoneTerminal`
- No subagent or fresh chat manager exists today

### Target State

```
App (parent)
  │
  ├── User: "/subagent analyze this codebase"
  │
  ├── subagent tool.execute(query="analyze this codebase")
  │     │
  │     ├── FreshChatManager (fresh transcript, same LLM + tools)
  │     │     ├── chat_stream(messages) → tool calls?
  │     │     │     ├── yes → execute tools → chat_stream again
  │     │     │     └── no → return final text
  │     │     └── returns "The codebase has X structure..."
  │     │
  │     └── returns result to parent
  │
  └── Parent shows subagent's answer in chat
```

## Design

### 1. FreshChatManager (src/chat_manager.rs)

A UI-independent struct that encapsulates the chat loop. No terminal, no UI.

```rust
pub struct FreshChatManager {
    llm: Box<dyn LlmProvider>,
    tools: ToolHandler,
    transcript: Vec<ChatMessage>,
    cancel: Arc<AtomicBool>,
    system_prompt: String,
}

pub enum ChatResult {
    Completed { text: String, tool_calls_count: usize },
    Cancelled,
}

impl FreshChatManager {
    pub fn new(llm: Box<dyn LlmProvider>, tools: ToolHandler) -> Self { ... }
    pub fn with_system_prompt(mut self, prompt: String) -> Self { ... }
    pub async fn send_message(&mut self, text: &str) -> ChatResult { ... }
    pub async fn stream_messages(
        &mut self,
        text: &str,
        callback: impl FnMut(&StreamEvent),
    ) -> ChatResult { ... }
    pub fn cancel(&self) { self.cancel.store(true, Ordering::SeqCst); }
    pub fn transcript(&self) -> &[ChatMessage] { &self.transcript }
}
```

**Key methods:**

- `send_message(text)` — runs the full tool-call loop (request LLM → handle tool calls → loop) and returns the final assistant text
- `stream_messages(text, callback)` — yields `StreamEvent` variants as each step completes (for streaming partial results back to parent)
- `cancel()` — signals abort via shared `Arc<AtomicBool>`

**Events:**

```rust
pub enum StreamEvent {
    AssistantText(String),        // incremental text chunk
    ToolCall(ToolCall),           // tool call emitted
    ToolResult(ToolResult),       // tool result received
    RoundComplete,                // one full round finished
    Finished(ChatResult),         // final result
}
```

### 2. Chat Loop Logic (extracted from stream.rs)

The loop handles:
- LLM streaming with 90s initial timeout, 90s idle timeout, up to 2 retries
- Tool call collection and execution
- Round limiting (64 rounds max)
- Cancellation at every await point

### 3. Subagent Tool (src/tools/subagent.rs)

Builtin tool registered in `builtin_tools()`:

```yaml
name: subagent
description: Spawn a subagent with a fresh chat session to answer a question or complete a task
```

**Arguments:**

| Arg | Type | Required | Description |
|-----|------|----------|-------------|
| query | string | yes | The question or task for the subagent |
| system_prompt | string | no | Optional custom system prompt |

**Execution flow:**

1. Extract `query` and optional `system_prompt`
2. Reconstruct LLM provider from config (base_url, model, api_key)
3. Clone/create ToolHandler with same tool registry
4. Create `FreshChatManager` with fresh transcript
5. Call `manager.send_message(query)`
6. Return the subagent's final response

### 4. Provider Reconstruction

`App` owns `llm: Box<dyn LlmProvider>`. Subagent tool needs its own instance.

**Approach:** Store `ProviderConfig` in `UserConfig`:

```rust
pub struct ProviderConfig {
    pub provider_id: String,
    pub model: String,
    pub api_key_env: String,  // or base_url + other fields as needed
}
```

The subagent tool reads `ProviderConfig` from disk/config and reconstructs the provider. This avoids sharing `Box<dyn LlmProvider>` across tool boundaries.

### 5. Approval Routing

Subagent tool calls go through the same `ToolHandler.execute_all()` path:
- If parent is in `Danger` mode: all subagent tool calls auto-approved
- If parent is in `Safe`/`Edits` mode: subagent tool calls need approval (auto-approve within subagent context since user already approved the subagent invocation)

**Decision:** Subagent tool calls are auto-approved. The user approved the subagent by invoking it.

### 6. Cancellation

`Arc<AtomicBool>` shared between parent and subagent:
- Ctrl+C on parent → sets cancel flag → subagent sees it at next await point → aborts

### 7. Nesting

Subagents can spawn subagents. Each `FreshChatManager` is independent with its own transcript. No recursion limit beyond the 64-round-per-agent cap.

## Files

| File | Action | Description |
|------|--------|-------------|
| `src/chat_manager.rs` | **Create** | FreshChatManager struct + chat loop logic |
| `src/tools/subagent.rs` | **Create** | SubagentTool implementation |
| `src/tools/mod.rs` | **Modify** | Register subagent in `builtin_tools()` |
| `src/config/mod.rs` | **Modify** | Add `ProviderConfig` to `UserConfig` |
| `src/ui/app/stream.rs` | **Modify** | Refactor to use `FreshChatManager` internally |
| `src/ui/app/mod.rs` | **Modify** | Pass config to subagent tool |
| `tests/chat_manager_test.rs` | **Create** | Unit tests for FreshChatManager |

## Implementation Order

1. **FreshChatManager core** — `send_message` with full loop, no streaming yet
2. **Subagent tool skeleton** — register tool, accept args, call manager
3. **Provider reconstruction** — `ProviderConfig` in config, rebuild provider in tool
4. **Cancellation** — `Arc<AtomicBool>` wiring
5. **Streaming** — `stream_messages` with `StreamEvent` callback
6. **Refactor stream.rs** — extract loop into `FreshChatManager`, use it in `App`
7. **Nesting + tests** — recursive subagent support, unit tests
8. **System prompt override** — optional argument

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Provider reconstruction is fragile (provider impls may not expose constructors) | Add a `ProviderFactory` trait or `Box<dyn LlmProvider>::from_config()` |
| Tool approval creates UX friction in subagents | Auto-approve all subagent tool calls |
| Memory — long transcripts in nested subagents | Add token budget / transcript truncation to FreshChatManager |
| Circular dependency — tool needs App config, App needs tool registry | Keep config in `config/` module, both pull from there |
