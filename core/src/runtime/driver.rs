//! The `Driver` — the one agent loop, extracted from `agent::run_agent`.
//!
//! Previously the loop body lived inline in `run_agent` (headless) and was
//! reimplemented again in the TUI (`ui::app::stream`). The `Driver` is the
//! single owner of that logic so it lives in exactly one place, in core,
//! unit-testable without a terminal. `run_agent` now builds a `Driver` with an
//! [`AutoApprovalGate`] and calls [`Driver::run`].

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use futures_util::StreamExt;

use crate::agent::{
    AgentResponse, AgentRunEvent, emit_event, estimate_context_chars, estimate_tokens,
    summarize_call_args, touch_activity,
};
use crate::chat::{build_chat_history, model_facing_message};
use crate::ext::ExtensionManager;
use crate::llm::provider::{LlmProvider, ProviderRequestContext};
use crate::llm::{ChatEvent, ChatMessage, ChatRole, LlmErrorKind, TokenStats};
use crate::runtime::RuntimeEvent;
use crate::session_sink::SessionSink;
use crate::tools::registry::ToolHandler;
use crate::tools::{ApprovalGate, ApprovalMode, CallOutcome, ToolCall, ToolResult};

/// Maximum turns a sub-agent (agent_depth > 0) may take before the driver
/// breaks the loop with an error. This is a hard backstop against tool-looping;
/// the top-level agent (depth 0) is uncapped.
const SUBAGENT_MAX_TURNS: usize = 30;

fn is_retryable_stream_error(kind: &LlmErrorKind) -> bool {
    matches!(
        kind,
        LlmErrorKind::Connection
            | LlmErrorKind::Timeout
            | LlmErrorKind::Server(_)
            | LlmErrorKind::RateLimit
    )
}

fn append_turn_messages(request_history: &mut Vec<ChatMessage>, turn_messages: &[String]) {
    if turn_messages.is_empty() {
        return;
    }

    let reminder = format!(
        "<system-reminder>\n{}\n</system-reminder>",
        turn_messages.join("\n\n")
    );
    match request_history.last_mut() {
        // Mid-loop: keep reminders inside the final tool result. Adding a fresh
        // trailing user message here makes Qwen-family chat templates discard
        // echoed reasoning from the in-progress turn and reads like a new user
        // prompt, causing models to re-plan and repeat themselves.
        Some(last) if last.role == ChatRole::Tool => {
            last.content.push_str("\n\n");
            last.content.push_str(&reminder);
        }
        _ => request_history.push(ChatMessage::new(ChatRole::User, reminder)),
    }
}

fn restore_ephemeral_image_relays(request_history: &mut Vec<ChatMessage>, relays: &[ChatMessage]) {
    request_history.extend(relays.iter().cloned());
}

fn record_hook_usage(
    usage: Vec<crate::ext::ctx::PrivateLlmUsage>,
    token_stats: &mut TokenStats,
    session: &dyn SessionSink,
    usage_records: &mut Vec<UsageRecord>,
    provider: &str,
    model: &str,
    per_record: Option<&(dyn Fn(&TokenStats) + Send + Sync)>,
) {
    for usage in usage {
        token_stats.record_request(
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cached_tokens,
            usage.cost,
        );
        session.record_usage(
            provider,
            model,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.cached_tokens,
            usage.cost,
            usage.is_estimated,
        );
        usage_records.push(UsageRecord {
            provider: provider.to_string(),
            model: model.to_string(),
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cached_tokens: usage.cached_tokens,
            cost: usage.cost,
            is_estimated: usage.is_estimated,
        });
        if let Some(callback) = per_record {
            callback(token_stats);
        }
    }
}

fn apply_hook_operations(
    operations: Vec<crate::ext::ctx::ConversationOperation>,
    transcript: &mut Vec<ChatMessage>,
    history: &mut Vec<ChatMessage>,
    request_history: &mut Vec<ChatMessage>,
    persist_messages: &mut Vec<ChatMessage>,
    session: &dyn SessionSink,
    session_seq: &mut i64,
) {
    for operation in operations {
        match operation {
            crate::ext::ctx::ConversationOperation::Append(messages) => {
                for mut message in messages {
                    if message.created_at.is_none() {
                        message.created_at = Some(crate::util::utc_now());
                    }
                    *session_seq += 1;
                    session.append_chat_message(&message, *session_seq);
                    history.push(model_facing_message(&message, None));
                    request_history.push(model_facing_message(&message, None));
                    transcript.push(message.clone());
                    persist_messages.push(message);
                }
            }
            crate::ext::ctx::ConversationOperation::Load(_) => {
                crate::ext::ctx::runtime_warn(
                    "bone-lua warn: conversation.load is unavailable during a turn",
                );
            }
        }
    }
}

fn forward_key_request(
    request: crate::pane_content::KeyRequest,
    key_registry: &crate::runtime::KeyReplyRegistry,
    events_out: &tokio::sync::mpsc::UnboundedSender<RuntimeEvent>,
) {
    let id = key_registry.register(request);
    if events_out.send(RuntimeEvent::KeyRequest { id }).is_err() {
        key_registry.remove(id);
    }
}

struct DriverHookRuntime<'a> {
    extensions: &'a ExtensionManager,
    gate: &'a Arc<dyn ApprovalGate>,
    cancel: &'a Option<Arc<std::sync::atomic::AtomicBool>>,
    runtime_events: &'a Option<tokio::sync::mpsc::UnboundedSender<RuntimeEvent>>,
    key_reply_registry: &'a Option<crate::runtime::KeyReplyRegistry>,
    config_store: &'a crate::config::store::ConfigStore,
    config_schema: &'a bone_protocol::ConfigSchema,
    llm: &'a Arc<dyn LlmProvider>,
    system_prompt_override: &'a Option<String>,
    conversation_id: Option<i64>,
    background_scope: Option<i64>,
    agent_depth: usize,
    cache_scope: &'a str,
    turn_state: &'a Arc<OnceLock<String>>,
}

struct DriverHookState<'a> {
    name: &'a str,
    payload: serde_json::Value,
    blockable: bool,
    tools: &'a ToolHandler,
    token_stats: &'a mut TokenStats,
    approval_mode: &'a ApprovalMode,
    transcript: &'a mut Vec<ChatMessage>,
    history: &'a mut Vec<ChatMessage>,
    request_history: &'a mut Vec<ChatMessage>,
    persist_messages: &'a mut Vec<ChatMessage>,
    session: &'a dyn SessionSink,
    session_seq: &'a mut i64,
    usage_records: &'a mut Vec<UsageRecord>,
}

#[derive(Default)]
struct BeforeTurnExtras {
    replacement: Option<Vec<ChatMessage>>,
    system_prompt_appends: Vec<String>,
    turn_messages: Vec<String>,
    tool_filter: Option<Vec<String>>,
    deferred_operations: Vec<crate::ext::ctx::ConversationOperation>,
}

enum DriverHookMode<'a> {
    Generic,
    BeforeTurn {
        report_usage: &'a (dyn Fn(&TokenStats) + Send + Sync),
        extras: &'a mut BeforeTurnExtras,
    },
}

fn context_system_prompt(history: &[ChatMessage], configured: &Option<String>) -> Option<String> {
    history
        .first()
        .filter(|message| message.role == ChatRole::System)
        .map(|message| message.content.clone())
        .or_else(|| configured.clone())
}

impl DriverHookRuntime<'_> {
    async fn run(
        &self,
        state: DriverHookState<'_>,
    ) -> (crate::ext::types::ManagedHookResult, bool) {
        self.run_mode(state, DriverHookMode::Generic).await
    }

    async fn run_mode(
        &self,
        state: DriverHookState<'_>,
        mode: DriverHookMode<'_>,
    ) -> (crate::ext::types::ManagedHookResult, bool) {
        let (
            system_prompt,
            per_record_usage,
            set_handler_cancel,
            forward_keys,
            before_extras,
        ) = match mode {
            DriverHookMode::Generic => (
                context_system_prompt(state.history, self.system_prompt_override),
                None,
                true,
                true,
                None,
            ),
            // before_turn historically receives the raw override, even when the
            // current history already has a system message. Its tool handler also
            // intentionally does not receive the generic run cancel token.
            DriverHookMode::BeforeTurn {
                report_usage,
                extras,
            } => (
                self.system_prompt_override.clone(),
                Some(report_usage),
                false,
                false,
                Some(extras),
            ),
        };
        let mut app_state = crate::ext::ctx::AppCtxState::new(
            state.tools,
            state.token_stats,
            state.approval_mode,
            self.conversation_id,
            self.llm.id(),
            self.llm.model(),
            self.llm.context_window_tokens(),
            system_prompt,
            Vec::new(),
            state.transcript.clone(),
            self.config_store.clone(),
            self.config_schema.clone(),
        );
        app_state.background_scope = self.background_scope;
        let mut ctx_cfg = crate::ext::ctx::build_before_turn_config(&app_state);
        ctx_cfg.runtime_status = self.runtime_events.clone();
        ctx_cfg.cancelled = self.cancel.clone();
        ctx_cfg.approval_gate = Some(crate::tools::SharedGate(self.gate.clone()));
        ctx_cfg.agent_depth = self.agent_depth;
        if let Some(handler) = ctx_cfg.tool_handler.as_mut() {
            handler.approval_gate = ctx_cfg.approval_gate.clone();
            if set_handler_cancel {
                handler.cancel_token = self.cancel.clone();
            }
        }
        let private_usage = Arc::new(Mutex::new(Vec::new()));
        ctx_cfg.private_llm = Some(crate::ext::ctx::PrivateLlmContext {
            provider: Arc::clone(self.llm),
            request_context: ProviderRequestContext {
                conversation_id: self.conversation_id,
                cache_scope: Some(self.cache_scope.to_string()),
                turn_state: Some(Arc::clone(self.turn_state)),
                max_tokens: None,
            },
            usage_records: Arc::clone(&private_usage),
        });
        let forwarder = if forward_keys {
            if let (Some(events_out), Some(key_registry)) =
                (self.runtime_events.clone(), self.key_reply_registry.clone())
            {
                let (live_tx, mut live_rx) = tokio::sync::mpsc::unbounded_channel();
                let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
                ctx_cfg.key_sender = Some(live_tx);
                let handle = tokio::spawn(async move {
                    loop {
                        let request = tokio::select! {
                            request = live_rx.recv() => request,
                            _ = &mut stop_rx => {
                                while let Ok(request) = live_rx.try_recv() {
                                    forward_key_request(request, &key_registry, &events_out);
                                }
                                break;
                            }
                        };
                        let Some(request) = request else { break };
                        forward_key_request(request, &key_registry, &events_out);
                    }
                });
                Some((stop_tx, handle))
            } else {
                None
            }
        } else {
            None
        };
        let ext = self.extensions.clone();
        let name = state.name.to_string();
        let mut hook = tokio::task::spawn_blocking(move || {
            ext.dispatch_managed(&name, state.payload, ctx_cfg, state.blockable)
        });
        let (mut result, cancelled) = if self.cancel.is_some() {
            tokio::select! {
                biased;
                _ = async {
                    while !self.cancel.as_ref().is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                    }
                } => {
                    let result = tokio::time::timeout(std::time::Duration::from_millis(100), &mut hook)
                        .await.ok().and_then(Result::ok).unwrap_or_default();
                    (result, true)
                }
                result = &mut hook => (result.unwrap_or_default(), false),
            }
        } else {
            (hook.await.unwrap_or_default(), false)
        };
        if let Some((stop, forwarder)) = forwarder {
            let _ = stop.send(());
            let _ = forwarder.await;
        }
        record_hook_usage(
            std::mem::take(&mut *private_usage.lock().unwrap_or_else(|p| p.into_inner())),
            state.token_stats,
            state.session,
            state.usage_records,
            self.llm.id(),
            self.llm.model(),
            per_record_usage,
        );
        if let Some(extras) = before_extras {
            for action in std::mem::take(&mut result.actions) {
                if let Some(messages) = action.conversation_replace {
                    extras.replacement = Some(messages);
                }
                if let Some(message) = action.system_prompt_append {
                    extras.system_prompt_appends.push(message);
                }
                if let Some(message) = action.turn_message {
                    extras.turn_messages.push(message);
                }
                if let Some(filter) = action.tool_filter {
                    extras.tool_filter = Some(filter);
                }
            }
            for operation in std::mem::take(&mut result.operations) {
                match operation {
                    crate::ext::ctx::ConversationOperation::Append(messages) => {
                        extras.deferred_operations.push(
                            crate::ext::ctx::ConversationOperation::Append(messages),
                        );
                    }
                    crate::ext::ctx::ConversationOperation::Load(_) => {
                        crate::ext::ctx::runtime_warn(
                            "bone-lua warn: conversation.load is unavailable during a turn",
                        );
                    }
                }
            }
        } else {
            apply_hook_operations(
                std::mem::take(&mut result.operations),
                state.transcript,
                state.history,
                state.request_history,
                state.persist_messages,
                state.session,
                state.session_seq,
            );
        }
        (result, cancelled)
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod driver_tests;

/// The runtime engine: owns everything a turn needs and runs the agent loop.
///
/// Construct it from the pieces produced by `agent::agent_setup` (provider,
/// extensions, tools, session sink, initial history/transcript), choose an
/// [`ApprovalGate`], then call [`Driver::run`].
pub struct Driver {
    pub llm: Arc<dyn LlmProvider>,
    pub extensions: ExtensionManager,
    pub config_store: crate::config::store::ConfigStore,
    pub tools: ToolHandler,
    pub session: Arc<dyn SessionSink>,
    /// Resolves tool-call approval. Headless uses [`crate::tools::AutoApprovalGate`];
    /// interactive frontends supply a gate that prompts the user.
    pub gate: Arc<dyn ApprovalGate>,
    pub approval_mode: crate::tools::SharedApprovalMode,
    pub agent_depth: usize,
    pub activity: Option<Arc<AtomicU64>>,
    pub on_token_usage: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    /// Emit JSONL events to stdout (headless `--events`).
    pub events: bool,
    pub event_sender: Option<tokio::sync::mpsc::UnboundedSender<AgentRunEvent>>,
    /// Rich, frontend-facing event stream (`TextDelta`, `ReasoningDelta`, tool
    /// lifecycle, token usage, finished/failed). The interactive frontend (the
    /// TUI, or a remote client) consumes this to render a turn. `None` for the
    /// headless JSONL path, which only needs `event_sender`.
    pub runtime_events: Option<tokio::sync::mpsc::UnboundedSender<RuntimeEvent>>,
    /// Routes `ctx.ui.key` replies back to blocked tools when a frontend is
    /// attached. Required for live tool key input; `None` headless.
    pub key_reply_registry: Option<crate::runtime::KeyReplyRegistry>,
    /// Cooperative cancel flag. When set true mid-turn, the loop stops after the
    /// current stream chunk / tool batch and ends the turn with whatever content
    /// was produced. Also wired into `tools.cancel_token` so running tools abort.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub history: Vec<ChatMessage>,
    pub transcript: Vec<ChatMessage>,
    pub token_stats: TokenStats,
    pub system_prompt_override: Option<String>,
    /// Stable conversation id for this turn, independent of the session sink.
    /// Frontends that persist out-of-band run with a [`NullSessionSink`] (whose
    /// `conv_id` is `None`), so the id is threaded in directly — it drives the
    /// provider cache key (`prompt_cache_key`) and the `ctx` conversation id.
    pub conversation_id: Option<i64>,
    /// Owner for jobs and managed processes. Unlike `conversation_id`, this is
    /// still present for an incognito actor.
    pub background_scope: Option<i64>,
    /// Shared steer nudge. `LocalConn::send(Steer)` sets it; the driver
    /// loop checks and consumes it at the top of each iteration.
    pub turn_nudge: Arc<Mutex<Option<String>>>,
}

/// What [`Driver::run`] hands back so a stateful frontend (the TUI) can reabsorb
/// the turn's results. The provider and session sink are shared via `Arc` and
/// the Lua VM via the cloned `ExtensionManager`, so those need no return; the
/// transcript, token stats, and tool state (which the Driver owns by value) do.
pub struct DriverOutcome {
    pub result: Result<AgentResponse, String>,
    pub tools: ToolHandler,
    pub transcript: Vec<ChatMessage>,
    pub token_stats: TokenStats,
    /// Messages produced during this turn that still need durable persistence.
    /// Kept separately because a model-facing transcript replacement can shorten
    /// or reshape `transcript`, making a pre-turn transcript index invalid.
    pub persist_messages: Vec<ChatMessage>,
    /// True when `conversation.replace` changed the model-facing transcript and
    /// the resulting view needs a durable checkpoint.
    pub transcript_replaced: bool,
    /// Per-request usage captured during the turn. The Driver also reports these
    /// to its `session` sink, but a frontend that runs with a `NullSessionSink`
    /// (the TUI persists with its own continuous `session_seq`) reads them from
    /// here to write usage events itself. Empty for headless runs that discard
    /// the outcome.
    pub usage: Vec<UsageRecord>,
}

/// One provider-reported (or estimated) usage record captured during a turn.
#[derive(Clone, Debug)]
pub struct UsageRecord {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub cost: Option<f64>,
    pub is_estimated: bool,
}

impl Driver {
    /// Convenience for the headless path: run and return just the result,
    /// discarding the reclaimable state.
    pub async fn run(self, prompt: &str) -> Result<AgentResponse, String> {
        self.run_to_outcome(prompt).await.result
    }

    /// Box the turn into an owned, `'static` future so a frontend connection
    /// (`LocalConn`) can store and poll it on its own task without borrowing the
    /// caller's prompt buffer. The future captures `self` and `prompt` by value.
    ///
    /// `Send`, so a `LocalConn` (and therefore the daemon that owns it) can be
    /// driven on any tokio task — the turn never holds the Lua VM lock across an
    /// `await` (the `before_turn` hook hops to `spawn_blocking`).
    pub fn into_turn_future(
        self,
        prompt: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DriverOutcome> + Send>> {
        Box::pin(async move { self.run_to_outcome(&prompt).await })
    }

    /// Drive the conversation to a final assistant message, returning the
    /// reclaimable [`DriverOutcome`]. `prompt` is the initiating user turn
    /// (already present in `history`/`transcript` from setup; passed here for
    /// event/session bookkeeping).
    ///
    /// Wraps [`run_to_outcome_inner`] in [`catch_unwind`] so a panic during the
    /// turn (e.g. an unexpected `unwrap` on a malformed provider/tool response)
    /// is caught and surfaced as `result: Err(...)` instead of crashing the
    /// process. The reclaimable state (transcript, token stats, tools) is
    /// snapshotted from `self` *before* the turn starts so a panicking turn
    /// returns the pre-turn conversation — the user keeps their history and can
    /// continue, rather than losing everything to a crash.
    ///
    /// Note: SQLite rows written before the panic are not rolled back, so the
    /// DB may contain more entries than the returned transcript.
    ///
    /// [`catch_unwind`]: futures_util::FutureExt::catch_unwind
    pub async fn run_to_outcome(self, prompt: &str) -> DriverOutcome {
        // Snapshot the reclaimable state now, before ownership moves into the
        // inner future. On panic the inner locals are lost to unwinding, so
        // without this the TUI would receive empty state and wipe the
        // conversation transcript.
        let transcript = self.transcript.clone();
        let token_stats = self.token_stats.clone();
        let tools = self.tools.clone();

        use futures_util::FutureExt;
        use std::panic::AssertUnwindSafe;
        match AssertUnwindSafe(self.run_to_outcome_inner(prompt))
            .catch_unwind()
            .await
        {
            Ok(outcome) => outcome,
            Err(payload) => {
                let msg = super::panic_message(&*payload);
                crate::ext::ctx::runtime_warn(format!("bone: agent turn panicked: {msg}"));
                DriverOutcome {
                    result: Err(format!("agent turn panicked: {msg}")),
                    tools,
                    transcript,
                    token_stats,
                    persist_messages: Vec::new(),
                    transcript_replaced: false,
                    usage: Vec::new(),
                }
            }
        }
    }

    async fn run_to_outcome_inner(self, prompt: &str) -> DriverOutcome {
        let Driver {
            llm,
            extensions,
            config_store,
            mut tools,
            session,
            gate,
            approval_mode,
            agent_depth,
            activity,
            on_token_usage,
            events,
            event_sender,
            runtime_events,
            key_reply_registry,
            cancel,
            mut history,
            mut transcript,
            mut token_stats,
            system_prompt_override,
            conversation_id,
            background_scope,
            turn_nudge,
        } = self;
        let tool_names = tools
            .all_definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        let command_names = extensions
            .commands()
            .iter()
            .map(|command| command.name.clone())
            .collect::<Vec<_>>();
        let config_schema = config_store.schema_for(&tool_names, &command_names);
        let is_cancelled = || {
            cancel
                .as_ref()
                .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        };
        // Awaitable form of `is_cancelled`: resolves once the shared cancel flag
        // flips, so a `select!` can interrupt an in-flight `stream.next()` the
        // instant Esc lands rather than only at the next chunk boundary. Without
        // this the turn sits parked on the provider's body stream while the model
        // is slow/thinking, and cancel is observed only when the next token (or a
        // dropped connection) finally arrives — the "Ctrl+C isn't instant" lag.
        // The flag is a plain `AtomicBool`, not awaitable, so we poll it; 25ms is
        // below human perception but cheap. With no flag (headless) it never
        // resolves, so the `select!` always takes the stream branch.
        let await_cancel = || {
            let cancel = cancel.clone();
            async move {
                match cancel {
                    Some(flag) => {
                        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                        }
                    }
                    None => std::future::pending::<()>().await,
                }
            }
        };

        let tool_defs = tools.definitions();
        let tool_defs_json_chars = serde_json::to_string(&tool_defs)
            .map(|j| j.chars().count())
            .unwrap_or(0);

        // Reported once at turn start; the live value is re-read each round so a
        // frontend can toggle Safe/Danger mid-turn (see `SharedApprovalMode`).
        let approval_label = approval_mode.get().mode_str();

        let mut session_seq = 0i64;
        let mut usage_records: Vec<UsageRecord> = Vec::new();
        let mut persist_messages: Vec<ChatMessage> = Vec::new();
        let mut transcript_replaced = false;
        // Shared provider routing/cache state for normal requests and private
        // completions made by any lifecycle hook in this turn.
        let turn_state = Arc::new(OnceLock::new());
        // Durable conversations keep a deterministic cross-turn scope; incognito
        // runs pin to the stable per-actor fallback so the provider sees the
        // same cache/routing identity on every turn instead of a fresh one.
        let cache_scope =
            crate::llm::provider::new_cache_scope(conversation_id, background_scope);
        let hook_runtime = DriverHookRuntime {
            extensions: &extensions,
            gate: &gate,
            cancel: &cancel,
            runtime_events: &runtime_events,
            key_reply_registry: &key_reply_registry,
            config_store: &config_store,
            config_schema: &config_schema,
            llm: &llm,
            system_prompt_override: &system_prompt_override,
            conversation_id,
            background_scope,
            agent_depth,
            cache_scope: &cache_scope,
            turn_state: &turn_state,
        };
        // The initiating user turn is already present in history/transcript:
        // headless `agent_setup` seeds it, and the TUI pushes it before
        // building the driver. Only insert when it is NOT already the last
        // message — otherwise we duplicate it in both the model context
        // (history) and the persisted transcript (the TUI writes the turn's
        // new messages from `persist_from` on, which would include the dup).
        // `session.append_message` runs unconditionally so the headless sink
        // still persists the user turn (the TUI uses a NullSessionSink, so it
        // is a no-op there).
        let prompt_already_last = transcript
            .last()
            .is_some_and(|m| m.role == crate::llm::ChatRole::User && m.content == prompt);
        if !prompt_already_last {
            let mut message = ChatMessage::new(crate::llm::ChatRole::User, prompt);
            message.created_at = Some(crate::util::utc_now());
            session.append_chat_message(&message, session_seq);
            history.push(model_facing_message(&message, None));
            transcript.push(message.clone());
            persist_messages.push(message);
        } else {
            let message = transcript
                .last_mut()
                .expect("prompt_already_last requires a final message");
            if message.created_at.is_none() {
                message.created_at = Some(crate::util::utc_now());
            }
            session.append_chat_message(message, session_seq);
            if let Some(history_message) = history.last_mut()
                && history_message.role == ChatRole::User
                && history_message.content == prompt
            {
                *history_message = model_facing_message(message, None);
            }
        }
        // Request-only history must be cloned after the initiating user's
        // timestamp is normalized so provider requests retain that metadata.
        let mut request_history = history.clone();

        // Rich frontend event stream (best-effort; ignored if no consumer).
        let remit = |event: RuntimeEvent| {
            if let Some(tx) = runtime_events.as_ref() {
                let _ = tx.send(event);
            }
        };
        let emit_runtime = |event: RuntimeEvent| {
            emit_event(events, event_sender.as_ref(), &event);
            remit(event);
        };

        let report_usage = |token_stats: &TokenStats| {
            if let Some(cb) = &on_token_usage {
                cb(token_stats.sent, token_stats.received);
            }
            emit_runtime(RuntimeEvent::TokenUsage {
                sent: token_stats.sent,
                received: token_stats.received,
                context_length: token_stats.context_length,
            });
        };

        emit_runtime(RuntimeEvent::Started {
            request_id: None,
            approval: approval_label.to_string(),
            task: prompt.to_string(),
            model: llm.model().to_string(),
            display: None,
        });
        let hook_mode = approval_mode.get();
        let (_, message_hook_cancelled) = hook_runtime
            .run(DriverHookState {
                name: "message",
                payload: serde_json::json!({ "role": "user", "content": prompt }),
                blockable: false,
                tools: &tools,
                token_stats: &mut token_stats,
                approval_mode: &hook_mode,
                transcript: &mut transcript,
                history: &mut history,
                request_history: &mut request_history,
                persist_messages: &mut persist_messages,
                session: session.as_ref(),
                session_seq: &mut session_seq,
                usage_records: &mut usage_records,
            })
            .await;
        if message_hook_cancelled {
            return DriverOutcome {
                result: Ok(AgentResponse {
                    content: String::new(),
                    transcript: transcript.clone(),
                }),
                tools,
                transcript,
                token_stats,
                persist_messages,
                transcript_replaced,
                usage: usage_records,
            };
        }
        let (_hook, hook_cancelled) = hook_runtime
            .run(DriverHookState {
                name: "session_start",
                payload: serde_json::json!({}),
                blockable: false,
                tools: &tools,
                token_stats: &mut token_stats,
                approval_mode: &hook_mode,
                transcript: &mut transcript,
                history: &mut history,
                request_history: &mut request_history,
                persist_messages: &mut persist_messages,
                session: session.as_ref(),
                session_seq: &mut session_seq,
                usage_records: &mut usage_records,
            })
            .await;
        if hook_cancelled {
            return DriverOutcome {
                result: Ok(AgentResponse {
                    content: String::new(),
                    transcript: transcript.clone(),
                }),
                tools,
                transcript,
                token_stats,
                persist_messages,
                transcript_replaced,
                usage: usage_records,
            };
        }
        let (_hook, hook_cancelled) = hook_runtime
            .run(DriverHookState {
                name: "turn_start",
                payload: serde_json::json!({
                    "task": prompt,
                    "model": llm.model(),
                    "approval": approval_label,
                }),
                blockable: false,
                tools: &tools,
                token_stats: &mut token_stats,
                approval_mode: &hook_mode,
                transcript: &mut transcript,
                history: &mut history,
                request_history: &mut request_history,
                persist_messages: &mut persist_messages,
                session: session.as_ref(),
                session_seq: &mut session_seq,
                usage_records: &mut usage_records,
            })
            .await;
        if hook_cancelled {
            return DriverOutcome {
                result: Ok(AgentResponse {
                    content: String::new(),
                    transcript: transcript.clone(),
                }),
                tools,
                transcript,
                token_stats,
                persist_messages,
                transcript_replaced,
                usage: usage_records,
            };
        }
        emit_runtime(RuntimeEvent::Status {
            message: "thinking".to_string(),
        });

        let mut consecutive_errors = 0u32;
        // Runaway brake: local models often spew the *same* failing tool call
        // (a search that doesn't match, empty/truncated args) many times per
        // second because each error result is just fed back with no throttle.
        // Track a signature of the failing calls each round; when the identical
        // set repeats, steer the model and back off, then abort if it keeps
        // ignoring the steer. `None` = last round had no tool errors.
        let mut last_error_signature: Option<String> = None;
        let mut repeated_error_rounds = 0u32;
        // Injected as a turn_message on the round *after* a repeat is detected,
        // so the model sees a firm reminder before it tries again.
        let mut error_loop_steer: Option<String> = None;
        const MAX_REPEATED_ERROR_ROUNDS: u32 = 3;
        let mut turns: usize = 0;
        // Request-only relays for ephemeral tool images. Preserve every relay
        // in insertion order while keeping them out of persistence.
        let mut ephemeral_image_relays: Vec<ChatMessage> = Vec::new();
        let mut last_turn_messages: Vec<String> = Vec::new();
        let result: Result<String, String> = 'turn: loop {
            if is_cancelled() {
                break Ok(String::new());
            }
            // Defensive sub-agent turn cap: a tool-looping sub-agent must not
            // run forever. The top-level agent is uncapped.
            if agent_depth > 0 && turns >= SUBAGENT_MAX_TURNS {
                break Err(format!("sub-agent exceeded {SUBAGENT_MAX_TURNS} turns"));
            }
            turns += 1;
            // Transient per-turn messages from `before_turn` handlers. Sent as
            // the *last* input item of this turn's requests and never persisted
            // to the transcript: at the prompt tail, turn-varying content costs
            // only its own tokens instead of invalidating the provider's prefix
            // cache for the entire history (as a mutating system prompt would).
            let mut turn_messages: Vec<String> = Vec::new();
            // Consume any mid-turn steer injected via Ctrl+Enter in a single
            // lock acquisition: a separate clone-then-take would be a TOCTOU (a
            // steer landing between the two locks could hand `before_turn` one
            // value and `turn_messages` another). The same text feeds both the
            // request (as a turn_message) and the `before_turn` hook.
            let nudge = turn_nudge.lock().unwrap().take();
            if let Some(nudge) = &nudge {
                turn_messages.push(nudge.clone());
            }
            // A repeated-error steer queued by the previous round takes effect
            // here, as the last input item, so the model reads it right before
            // it would otherwise retry the same failing call.
            if let Some(steer) = error_loop_steer.take() {
                turn_messages.push(steer);
            }
            // Dispatch before_turn hooks before constructing the provider request.
            // `turn_tool_defs` defaults to the full set and is narrowed only when
            // a handler returns a `tool_filter`.
            let mut turn_tool_defs = tool_defs.clone();
            {
                // Refresh context_length from the current pending history so
                // the before_turn snapshot reflects what this request will
                // actually send, including tool results appended mid-loop.
                // Anchor the estimate to the last provider-reported prompt size
                // plus a character estimate of subsequent growth so it tracks
                // provider accounting more closely than a raw whole-history
                // estimate. The next provider usage event replaces this estimate.
                token_stats.context_length = token_stats.anchored_context_estimate(
                    estimate_context_chars(&history, tool_defs_json_chars),
                );
                let mut extras = BeforeTurnExtras::default();
                let hook_mode = approval_mode.get();
                let (_hook_result, hook_cancelled) = hook_runtime
                    .run_mode(
                        DriverHookState {
                            name: "before_turn",
                            payload: serde_json::json!({}),
                            blockable: false,
                            tools: &tools,
                            token_stats: &mut token_stats,
                            approval_mode: &hook_mode,
                            transcript: &mut transcript,
                            history: &mut history,
                            request_history: &mut request_history,
                            persist_messages: &mut persist_messages,
                            session: session.as_ref(),
                            session_seq: &mut session_seq,
                            usage_records: &mut usage_records,
                        },
                        DriverHookMode::BeforeTurn {
                            report_usage: &report_usage,
                            extras: &mut extras,
                        },
                    )
                    .await;
                if hook_cancelled {
                    break 'turn Ok(String::new());
                }
                let replacement = extras.replacement;
                let sys_appends = extras.system_prompt_appends;
                turn_messages.extend(extras.turn_messages);
                let tool_filter = extras.tool_filter;
                let pending_operations = extras.deferred_operations;

                // Finalize the active system prompt only after every hook has
                // run. A replacement uses the same history rebuild path.
                let base_system_prompt = system_prompt_override
                    .clone()
                    .or_else(|| history.first().map(|message| message.content.clone()))
                    .expect("driver history must contain an effective system prompt");
                let active_system_prompt = if sys_appends.is_empty() {
                    base_system_prompt
                } else {
                    format!("{base_system_prompt}\n\n{}", sys_appends.join("\n\n"))
                };
                let had_replacement = replacement.is_some();
                let history_rebuilt = had_replacement || !sys_appends.is_empty();
                if let Some(new_messages) = replacement {
                    // A replacement wins over appends queued by this dispatch,
                    // so superseded messages never reach the authoritative sink.
                    transcript = new_messages;
                    transcript_replaced = true;
                    history = build_chat_history(&transcript, &active_system_prompt);
                    request_history = history.clone();
                    restore_ephemeral_image_relays(&mut request_history, &ephemeral_image_relays);
                    last_turn_messages.clear();
                    token_stats.clear_context_anchor();
                } else {
                    if !pending_operations.is_empty() {
                        apply_hook_operations(
                            pending_operations,
                            &mut transcript,
                            &mut history,
                            &mut request_history,
                            &mut persist_messages,
                            session.as_ref(),
                            &mut session_seq,
                        );
                    }
                    if !sys_appends.is_empty() {
                        history = build_chat_history(&transcript, &active_system_prompt);
                        request_history = history.clone();
                        restore_ephemeral_image_relays(
                            &mut request_history,
                            &ephemeral_image_relays,
                        );
                        last_turn_messages.clear();
                    }
                }

                if history_rebuilt {
                    let prompt_chars = estimate_context_chars(&history, tool_defs_json_chars);
                    token_stats.context_length =
                        token_stats.anchored_context_estimate(prompt_chars);
                }

                // Narrow the normal request's exposed tools after hooks finish.
                // Private completions use the tool list supplied by Lua.
                if let Some(allow) = tool_filter {
                    turn_tool_defs.retain(|d| allow.iter().any(|n| n == &d.name));
                    if turn_tool_defs.is_empty() {
                        crate::ext::ctx::runtime_warn_once(
                            "bone-lua warn: before_turn tool_filter hid every tool this turn",
                        );
                    }
                }
            }

            // Request stream with retry. Both the request itself and the
            // backoff sleep race the cancel flag: establishing the connection
            // (and waiting on the provider's response headers) can park for
            // seconds while the model "thinks" server-side, and a Ctrl+C in
            // that window must return control now rather than waiting out the
            // request or the 2s backoff.
            let mut stream = None;
            // Add changed transient guidance to the request-only history. It
            // stays at this insertion point for later tool rounds, preserving
            // an append-only provider-cache prefix without entering transcript.
            if turn_messages != last_turn_messages {
                append_turn_messages(&mut request_history, &turn_messages);
                last_turn_messages = turn_messages;
            }
            'request: for attempt in 1..=3 {
                let send = llm.chat_stream_with_context(
                    request_history.clone(),
                    turn_tool_defs.clone(),
                    ProviderRequestContext {
                        conversation_id,
                        cache_scope: Some(cache_scope.clone()),
                        turn_state: Some(Arc::clone(&turn_state)),
                        max_tokens: None,
                    },
                );
                let result = tokio::select! {
                    biased;
                    _ = await_cancel() => break 'request,
                    result = send => result,
                };
                match result {
                    Ok(s) => {
                        stream = Some(s);
                        break;
                    }
                    Err(e) if attempt < 3 => {
                        emit_runtime(RuntimeEvent::Status {
                            message: format!("retry {attempt}/3: {e}"),
                        });
                        tokio::select! {
                            biased;
                            _ = await_cancel() => break 'request,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                        }
                    }
                    Err(e) => {
                        emit_runtime(RuntimeEvent::Failed {
                            message: e.to_string(),
                        });
                        break 'turn Err(format!("provider error after 3 attempts: {e}"));
                    }
                }
            }
            // Cancelled while connecting/backing off: discard this turn.
            let Some(mut stream) = stream else {
                break 'turn Ok(String::new());
            };

            // Consume stream.
            let mut assistant_text = String::new();
            let mut reasoning_text = String::new();
            let mut reasoning_echo_field: Option<String> = None;
            let mut reasoning_items: Vec<crate::llm::ReasoningItem> = Vec::new();
            let mut tool_calls = Vec::new();
            // Ordered output items as the provider emits them, so Codex/Responses
            // can replay reasoning + text + tool calls verbatim and in order.
            let mut output_sequence: Vec<crate::llm::OutputItem> = Vec::new();
            // Index of the (single) accumulating text item in `output_sequence`,
            // so streamed deltas land in their original position relative to
            // reasoning items and tool calls rather than always sorting first.
            let mut text_item_index: Option<usize> = None;
            let mut stream_error = false;
            let mut had_usage = false;

            // `biased` so the cancel branch is polled first each iteration: it
            // re-checks the flag immediately (no wait) before yielding to the
            // stream, so a cancel that landed between chunks wins promptly. A
            // `None` here means cancelled (or the stream ended); the
            // `is_cancelled()` check just below discards the partial turn.
            while let Some(chunk) = tokio::select! {
                biased;
                _ = await_cancel() => None,
                chunk = stream.next() => chunk,
            } {
                touch_activity(&activity);
                if is_cancelled() {
                    break;
                }
                match chunk {
                    Ok(ChatEvent::TextDelta(text)) => {
                        remit(RuntimeEvent::TextDelta { text: text.clone() });
                        assistant_text.push_str(&text);
                        match text_item_index {
                            Some(i) => {
                                if let Some(crate::llm::OutputItem::Text(s)) =
                                    output_sequence.get_mut(i)
                                {
                                    s.push_str(&text);
                                }
                            }
                            None => {
                                text_item_index = Some(output_sequence.len());
                                output_sequence.push(crate::llm::OutputItem::Text(text.clone()));
                            }
                        }
                    }
                    Ok(ChatEvent::ReasoningDelta { text, echo_field }) => {
                        remit(RuntimeEvent::ReasoningDelta { text: text.clone() });
                        reasoning_text.push_str(&text);
                        if reasoning_echo_field.is_none() {
                            reasoning_echo_field = echo_field;
                        }
                    }
                    Ok(ChatEvent::EncryptedReasoning {
                        id,
                        encrypted_content,
                    }) => {
                        // Captured for verbatim replay on the next request
                        // (Codex/Responses). Not surfaced to the UI — it is an
                        // opaque blob the model must see again, not text to show.
                        let item = crate::llm::ReasoningItem {
                            id,
                            encrypted_content,
                        };
                        output_sequence.push(crate::llm::OutputItem::Reasoning(item.clone()));
                        reasoning_items.push(item);
                    }
                    Ok(ChatEvent::ToolCall(call)) => {
                        let summary = format!("{}: {}", call.name, summarize_call_args(&call));
                        emit_runtime(RuntimeEvent::ToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            summary: summary.clone(),
                            arguments: call.arguments.clone(),
                        });
                        output_sequence.push(crate::llm::OutputItem::ToolCall(call.clone()));
                        tool_calls.push(call);
                    }
                    Ok(ChatEvent::TokenUsage {
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        cost,
                    }) => {
                        token_stats.record_request(
                            prompt_tokens,
                            completion_tokens,
                            cached_tokens,
                            cost,
                        );
                        // Calibrate the estimator: pair the provider-reported
                        // prompt size with the char count of what we sent, so
                        // pre-request refreshes track real growth instead of
                        // re-guessing the whole history.
                        token_stats.set_context_anchor(
                            prompt_tokens as u64,
                            estimate_context_chars(&request_history, tool_defs_json_chars),
                        );
                        had_usage = true;
                        session.record_usage(
                            llm.id(),
                            llm.model(),
                            prompt_tokens,
                            completion_tokens,
                            cached_tokens,
                            cost,
                            false,
                        );
                        usage_records.push(UsageRecord {
                            provider: llm.id().to_string(),
                            model: llm.model().to_string(),
                            prompt_tokens,
                            completion_tokens,
                            cached_tokens,
                            cost,
                            is_estimated: false,
                        });
                        report_usage(&token_stats);
                    }
                    Err(e) => {
                        if !is_retryable_stream_error(&e.kind) {
                            emit_runtime(RuntimeEvent::Failed {
                                message: e.to_string(),
                            });
                            break 'turn Err(e.to_string());
                        }
                        emit_runtime(RuntimeEvent::Status {
                            message: format!("stream error, will retry: {e}"),
                        });
                        stream_error = true;
                        break;
                    }
                }
            }

            if !had_usage && !stream_error {
                let prompt_chars = estimate_context_chars(&request_history, tool_defs_json_chars);
                let completion_chars = assistant_text.chars().count()
                    + reasoning_text.chars().count()
                    + tool_calls
                        .iter()
                        .map(|call| call.arguments.to_string().chars().count())
                        .sum::<usize>();
                let prompt_tokens = estimate_tokens(prompt_chars);
                let completion_tokens = estimate_tokens(completion_chars);
                token_stats.record_estimate(prompt_chars, completion_chars);
                // Keep the displayed context on the anchored scale so a
                // usage-less response doesn't make the meter jump to the raw
                // char guess and back on the next real report.
                token_stats.context_length = token_stats.anchored_context_estimate(prompt_chars);
                session.record_usage(
                    llm.id(),
                    llm.model(),
                    prompt_tokens,
                    completion_tokens,
                    None,
                    None,
                    true,
                );
                usage_records.push(UsageRecord {
                    provider: llm.id().to_string(),
                    model: llm.model().to_string(),
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens: None,
                    cost: None,
                    is_estimated: true,
                });
                report_usage(&token_stats);
            }

            if !stream_error {
                let hook_mode = approval_mode.get();
                let _ = hook_runtime
                    .run(DriverHookState {
                        name: "token_usage",
                        payload: serde_json::json!({
                            "sent": token_stats.sent,
                            "received": token_stats.received,
                            "context_length": token_stats.context_length,
                        }),
                        blockable: false,
                        tools: &tools,
                        token_stats: &mut token_stats,
                        approval_mode: &hook_mode,
                        transcript: &mut transcript,
                        history: &mut history,
                        request_history: &mut request_history,
                        persist_messages: &mut persist_messages,
                        session: session.as_ref(),
                        session_seq: &mut session_seq,
                        usage_records: &mut usage_records,
                    })
                    .await;
            }

            if stream_error {
                consecutive_errors += 1;
                if consecutive_errors >= 5 {
                    emit_runtime(RuntimeEvent::Failed {
                        message: "too many stream errors".to_string(),
                    });
                    break Err("aborted after 5 consecutive stream errors".to_string());
                }
                tokio::select! {
                    biased;
                    _ = await_cancel() => break Ok(String::new()),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                }
                continue;
            }
            consecutive_errors = 0;

            // Cancelled mid-stream: discard partial text — the stream was
            // interrupted, so what we accumulated is incomplete.
            if is_cancelled() {
                break Ok(String::new());
            }

            // No tool calls -> done. Record the final assistant message in the
            // transcript (so the returned transcript is complete — the TUI
            // reabsorbs it for context and DB persistence, and the next turn's
            // history needs it).
            if tool_calls.is_empty() {
                let mut assistant = ChatMessage::assistant_with_tools(&assistant_text, Vec::new());
                assistant.created_at = Some(crate::util::utc_now());
                if !reasoning_text.is_empty() {
                    assistant.reasoning = Some(crate::llm::Reasoning {
                        text: std::mem::take(&mut reasoning_text),
                        echo_field: reasoning_echo_field.take(),
                    });
                }
                if !reasoning_items.is_empty() {
                    assistant.reasoning_items = std::mem::take(&mut reasoning_items);
                }
                assistant.output_sequence = std::mem::take(&mut output_sequence);
                session_seq += 1;
                session.append_chat_message(&assistant, session_seq);
                transcript.push(assistant.clone());
                persist_messages.push(assistant);
                break Ok(assistant_text);
            }

            // Keep any streamed prose on the assistant/tool-call message.
            // Dropping it loses the model's record of its own plan and
            // progress, making it re-derive context every round.
            let mut assistant =
                ChatMessage::assistant_with_tools(&assistant_text, tool_calls.clone());
            assistant.created_at = Some(crate::util::utc_now());
            if !reasoning_text.is_empty() {
                assistant.reasoning = Some(crate::llm::Reasoning {
                    text: std::mem::take(&mut reasoning_text),
                    echo_field: reasoning_echo_field.take(),
                });
            }
            if !reasoning_items.is_empty() {
                assistant.reasoning_items = std::mem::take(&mut reasoning_items);
            }
            assistant.output_sequence = std::mem::take(&mut output_sequence);
            let requested_at = assistant.created_at.clone();
            let provider_assistant = model_facing_message(&assistant, None);
            session_seq += 1;
            session.append_chat_message(&assistant, session_seq);
            history.push(provider_assistant.clone());
            request_history.push(provider_assistant);
            transcript.push(assistant.clone());
            persist_messages.push(assistant);

            // Execute tool calls.
            for call in &tool_calls {
                emit_runtime(RuntimeEvent::Status {
                    message: format!("running {}: {}", call.name, summarize_call_args(call)),
                });
            }

            // Let running tools observe cancellation.
            tools.cancel_token = cancel.clone();
            tools.approval_gate = Some(crate::tools::SharedGate(gate.clone()));
            // Lua tools need the same live conversation context as before_turn
            // and slash commands. Drop the previous snapshot first so cloning
            // the handler into AppCtxState cannot build a recursive chain.
            tools.app_state = None;
            let mut app_state = crate::ext::ctx::AppCtxState::new(
                &tools,
                &token_stats,
                &approval_mode.get(),
                conversation_id,
                llm.id(),
                llm.model(),
                llm.context_window_tokens(),
                context_system_prompt(&history, &system_prompt_override),
                Vec::new(),
                transcript.clone(),
                config_store.clone(),
                config_schema.clone(),
            );
            app_state.background_scope = background_scope;
            tools.app_state = Some(app_state);
            // Re-read each round so a mid-turn Safe/Danger toggle takes effect
            // on the very next tool batch. Managed tool_call hooks run before
            // native approval and preserve the first blocking result per call.
            let hook_mode = approval_mode.get();
            let mut hook_blocks = Vec::with_capacity(tool_calls.len());
            let mut hook_cancelled = false;
            for call in &tool_calls {
                let safety = tools.safety_for_call(call);
                let safety = match safety {
                    crate::tools::command_policy::CommandSafety::ReadOnly => "read_only",
                    crate::tools::command_policy::CommandSafety::Danger => "danger",
                };
                let (hook, cancelled) = hook_runtime
                    .run(DriverHookState {
                        name: "tool_call",
                        payload: serde_json::json!({
                            "name": call.name,
                            "call_id": call.id,
                            "arguments": call.arguments,
                            "safety": safety,
                        }),
                        blockable: true,
                        tools: &tools,
                        token_stats: &mut token_stats,
                        approval_mode: &hook_mode,
                        transcript: &mut transcript,
                        history: &mut history,
                        request_history: &mut request_history,
                        persist_messages: &mut persist_messages,
                        session: session.as_ref(),
                        session_seq: &mut session_seq,
                        usage_records: &mut usage_records,
                    })
                    .await;
                hook_blocks.push(hook.blocked);
                if cancelled {
                    hook_cancelled = true;
                    break;
                }
            }
            if hook_cancelled {
                break 'turn Ok(String::new());
            }
            let results = execute_tool_calls(
                &tools,
                &hook_mode,
                gate.as_ref(),
                tool_calls,
                hook_blocks,
                agent_depth,
                runtime_events.clone(),
                key_reply_registry.clone(),
            )
            .await;
            for result in &results {
                let _ = hook_runtime
                    .run(DriverHookState {
                        name: "tool_result",
                        payload: serde_json::json!({
                            "name": result.name,
                            "call_id": result.call_id,
                            "is_error": result.is_error,
                        }),
                        blockable: false,
                        tools: &tools,
                        token_stats: &mut token_stats,
                        approval_mode: &hook_mode,
                        transcript: &mut transcript,
                        history: &mut history,
                        request_history: &mut request_history,
                        persist_messages: &mut persist_messages,
                        session: session.as_ref(),
                        session_seq: &mut session_seq,
                        usage_records: &mut usage_records,
                    })
                    .await;
            }
            touch_activity(&activity);

            // Persist stateful tool state across rounds.
            for result in &results {
                if let Some(ref state) = result.state {
                    let source = result
                        .pane_page
                        .as_ref()
                        .map(|p| p.source.as_str())
                        .unwrap_or(&result.name);
                    tools.state_map.set(source, "default", state.clone());
                }
                if let Some(page) = &result.pane_page
                    && page.is_empty()
                {
                    tools.state_map.remove(&page.source, "default");
                }
            }

            for result in &results {
                // Live frontends receive this from ToolHandler as each call
                // completes. Keep the legacy/headless event sinks populated
                // without sending a duplicate to `runtime_events`.
                let event = RuntimeEvent::ToolResult {
                    name: result.name.clone(),
                    call_id: result.call_id.clone(),
                    is_error: result.is_error,
                    content: result.content.clone(),
                };
                emit_event(events, event_sender.as_ref(), &event);
                session_seq += 1;
                let mut message = ChatMessage::tool(result.clone());
                message.created_at = Some(crate::util::utc_now());
                if result.ephemeral_images {
                    message.images.clear();
                }
                session.append_chat_message(&message, session_seq);
                let provider_message = model_facing_message(&message, requested_at.as_deref());
                history.push(provider_message.clone());
                request_history.push(provider_message);
                transcript.push(message.clone());
                persist_messages.push(message);

                // The OpenAI wire format cannot carry images in a tool-role
                // message, so relay tool-returned images to vision-capable
                // models as a follow-up user message. Ephemeral relays live only
                // in request history, and each new one replaces the previous
                // screenshot from this assistant turn.
                if !result.images.is_empty() {
                    let note = format!("Image output from {}:", result.name);
                    let mut relay = ChatMessage::user_with_images(note, result.images.clone());
                    relay.created_at = Some(crate::util::utc_now());
                    let provider_relay = model_facing_message(&relay, None);
                    if result.ephemeral_images {
                        request_history.push(provider_relay.clone());
                        ephemeral_image_relays.push(provider_relay);
                    } else {
                        session_seq += 1;
                        session.append_chat_message(&relay, session_seq);
                        history.push(provider_relay.clone());
                        request_history.push(provider_relay);
                        transcript.push(relay.clone());
                        persist_messages.push(relay);
                    }
                }
            }

            // Runaway brake. Build a signature from this round's *failing*
            // calls, keyed on tool name + error text: a bad edit produces a
            // deterministic error ("search matched 0 times", "truncated…"), so
            // repeating the same broken call yields the same signature. Any
            // successful call, or a genuinely new error, changes it and resets
            // the counter — legitimate long-running work is never throttled.
            let error_signature: Option<String> = {
                let mut errs: Vec<(&str, &str)> = results
                    .iter()
                    .filter(|r| r.is_error)
                    .map(|r| (r.name.as_str(), r.content.as_str()))
                    .collect();
                if errs.is_empty() {
                    None
                } else {
                    errs.sort_unstable();
                    Some(
                        errs.iter()
                            .map(|(name, content)| format!("{name}\0{content}"))
                            .collect::<Vec<_>>()
                            .join("\u{1}"),
                    )
                }
            };

            if error_signature.is_some() && error_signature == last_error_signature {
                repeated_error_rounds += 1;
                if repeated_error_rounds >= MAX_REPEATED_ERROR_ROUNDS {
                    emit_runtime(RuntimeEvent::Failed {
                        message: format!(
                            "aborted after {MAX_REPEATED_ERROR_ROUNDS} identical failing tool calls in a row"
                        ),
                    });
                    break Err(format!(
                        "aborted: the same tool call failed {} times in a row without progress",
                        repeated_error_rounds + 1
                    ));
                }
                // Firm steer, injected on the next round. Also back off briefly:
                // a fast local model can otherwise spin these rounds in
                // milliseconds, and the pause gives a mid-turn cancel a window.
                error_loop_steer = Some(
                    "The last tool call failed with the same error as the previous attempt. \
                     Do not resend the identical call. Stop and reconsider: re-read the file \
                     to get the exact current text, fix the arguments, or take a different \
                     approach. If you cannot make progress, explain the problem instead of retrying."
                        .to_string(),
                );
                emit_runtime(RuntimeEvent::Status {
                    message: "repeated tool error — steering the model to change approach"
                        .to_string(),
                });
                tokio::select! {
                    biased;
                    _ = await_cancel() => break Ok(String::new()),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(750)) => {}
                }
            } else {
                repeated_error_rounds = 0;
            }
            last_error_signature = error_signature;
        };

        // Emit Finished only on success (Failed was already emitted at the
        // break point for error paths).
        if let Ok(content) = &result {
            emit_runtime(RuntimeEvent::Finished {
                content: content.clone(),
            });
        }
        let hook_mode = approval_mode.get();
        let _ = hook_runtime
            .run(DriverHookState {
                name: "turn_end",
                payload: match &result {
                    Ok(content) => serde_json::json!({ "ok": true, "content": content }),
                    Err(message) => serde_json::json!({ "ok": false, "error": message }),
                },
                blockable: false,
                tools: &tools,
                token_stats: &mut token_stats,
                approval_mode: &hook_mode,
                transcript: &mut transcript,
                history: &mut history,
                request_history: &mut request_history,
                persist_messages: &mut persist_messages,
                session: session.as_ref(),
                session_seq: &mut session_seq,
                usage_records: &mut usage_records,
            })
            .await;
        session.end();
        let _ = hook_runtime
            .run(DriverHookState {
                name: "session_end",
                payload: serde_json::json!({}),
                blockable: false,
                tools: &tools,
                token_stats: &mut token_stats,
                approval_mode: &hook_mode,
                transcript: &mut transcript,
                history: &mut history,
                request_history: &mut request_history,
                persist_messages: &mut persist_messages,
                session: session.as_ref(),
                session_seq: &mut session_seq,
                usage_records: &mut usage_records,
            })
            .await;

        DriverOutcome {
            result: result.map(|content| AgentResponse {
                content,
                transcript: transcript.clone(),
            }),
            tools,
            transcript,
            token_stats,
            persist_messages,
            transcript_replaced,
            usage: usage_records,
        }
    }
}

/// Execute tool calls respecting the approval gate.
///
/// For each call: use the already-computed managed hook block result, compute
/// the policy allow-decision from the approval mode, then let the
/// [`ApprovalGate`] resolve the [`CallOutcome`]. Approved calls are dispatched
/// concurrently via `ToolHandler::execute_all`; blocked/denied calls get an error
/// result.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_tool_calls(
    tools: &ToolHandler,
    mode: &ApprovalMode,
    gate: &dyn ApprovalGate,
    calls: Vec<ToolCall>,
    hook_blocks: Vec<Option<String>>,
    agent_depth: usize,
    runtime_events: Option<tokio::sync::mpsc::UnboundedSender<RuntimeEvent>>,
    key_reply_registry: Option<crate::runtime::KeyReplyRegistry>,
) -> Vec<ToolResult> {
    // Track original index to preserve call order in output.
    let mut out: Vec<(usize, ToolResult)> = Vec::with_capacity(calls.len());
    let mut approved: Vec<(usize, ToolCall)> = Vec::new();
    let emit_result = |result: &ToolResult| {
        if let Some(events) = &runtime_events {
            let _ = events.send(RuntimeEvent::ToolResult {
                name: result.name.clone(),
                call_id: result.call_id.clone(),
                is_error: result.is_error,
                content: result.content.clone(),
            });
        }
    };

    for (i, call) in calls.into_iter().enumerate() {
        let safety = tools.safety_for_call(&call);
        let blocked = hook_blocks.get(i).cloned().flatten();
        let auto_allows = mode.allows_safety(safety);

        match gate.decide(blocked, auto_allows, &call).await {
            CallOutcome::Approve => approved.push((i, call)),
            CallOutcome::Blocked(reason) => {
                let result = ToolResult::error(call.id.clone(), call.name.clone(), reason);
                emit_result(&result);
                out.push((i, result));
            }
            CallOutcome::Denied => {
                let result = ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    crate::tools::denied_message(*mode, safety),
                );
                emit_result(&result);
                out.push((i, result));
            }
        }
    }

    // Execute all approved calls concurrently. When a frontend is attached
    // (`runtime_events`), use the live path and forward each `KeyRequest`
    // (key requests) as a `RuntimeEvent` so the frontend can answer
    // `ctx.ui.key` mid-turn. Pane updates now flow through the standalone
    // `UiState` handle (drained by the TUI directly), not this channel.
    // Headless, there's no consumer, so we use the plain (non-live) path.
    if !approved.is_empty() {
        let approved_calls: Vec<ToolCall> = approved.iter().map(|(_, c)| c.clone()).collect();
        let results = if let Some(events_out) = runtime_events.clone() {
            let (live_tx, mut live_rx) =
                tokio::sync::mpsc::unbounded_channel::<crate::pane_content::KeyRequest>();
            // Forward live tool events to the frontend event stream.
            let forwarder = tokio::spawn(async move {
                while let Some(req) = live_rx.recv().await {
                    // Pane diffs go through the shared UiState handle.
                    if let Some(registry) = &key_reply_registry {
                        let id = registry.register(req);
                        if events_out.send(RuntimeEvent::KeyRequest { id }).is_err() {
                            registry.remove(id);
                        }
                    }
                }
            });
            let results = tools
                .execute_all_live(
                    approved_calls,
                    Some(live_tx),
                    agent_depth,
                    0,
                    runtime_events.clone(),
                )
                .await;
            // All sender handles are now owned by the live tool executions.
            // When they finish, the channel closes and the forwarder exits.
            // Do not pass an extra clone into execute_all_live: if the root
            // future holds a sender across its own await, live_rx never closes
            // and the Driver wedges after ctx.ui.key replies.
            let _ = forwarder.await;
            results
        } else {
            tools.execute_all(approved_calls, agent_depth).await
        };
        for ((orig_idx, _call), result) in approved.into_iter().zip(results) {
            out.push((orig_idx, result));
        }
    }

    // Restore original call order.
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}
