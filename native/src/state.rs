//! Small, renderer-independent projection of the authoritative daemon stream.
use std::collections::{HashMap, HashSet};

use bone_protocol::tools::CallOutcome;
use bone_protocol::{
    ChatMessage, ChatRole, Component, ImageData, JobSnapshot, ProcessSnapshot, RuntimeCommand,
    RuntimeEvent, SessionSnapshot, ViewDiff, ViewModel,
};

use bone_protocol::ToolDisplayConfig;

/// Status shown while the daemon waits for a `ctx.ui.key()` reply. Set when the
/// `KeyRequest` arrives; cleared by [`State::finish_command`] once the command
/// settles, so a cancelled menu cannot leave the composer stuck busy.
pub const KEY_WAIT_STATUS: &str = "A tool is waiting for a key press…";

/// Status shown after the composer's Stop button sends `RuntimeCommand::Cancel`.
/// Cleared by [`State::finish_command`] when the daemon confirms the command is
/// over, so a cancel of an idle key wait cannot linger.
pub const CANCEL_STATUS: &str = "Cancelling…";

/// Format a millisecond duration as `m:ss`, mirroring the TUI's turn notice.
pub fn format_elapsed_ms(ms: u64) -> String {
    let total = ms / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Composer preset from `config.yaml` → `ui.input.preset`, mirroring the TUI's
/// `InputPreset` (padding/fill defaults per preset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputPreset {
    #[default]
    Lines,
    Box,
    Filled,
}

/// Native rendering of the daemon's declarative composer style. Parsed from the
/// resolved frontend settings JSON (`/ui/input`); the daemon remains authoritative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputStyle {
    pub preset: InputPreset,
    pub prefix: String,
    pub horizontal_padding: u16,
    pub vertical_padding: u16,
    pub fill: bool,
}

const MAX_INPUT_PADDING: u16 = 8;
/// A `Synchronize` reply older than this is presumed lost and may be re-sent.
const SYNC_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

impl Default for InputStyle {
    fn default() -> Self {
        Self::preset(InputPreset::Lines)
    }
}

impl InputStyle {
    fn preset(preset: InputPreset) -> Self {
        let (horizontal_padding, vertical_padding, fill) = match preset {
            InputPreset::Lines => (0, 0, false),
            InputPreset::Box => (1, 0, false),
            InputPreset::Filled => (1, 1, true),
        };
        Self {
            preset,
            // No prefix by default; the desktop app shows plain input unless
            // `ui.input.prefix` is configured (unlike the TUI's "> " default).
            prefix: String::new(),
            horizontal_padding,
            vertical_padding,
            fill,
        }
    }

    /// Parse `ui.input` from the daemon's resolved frontend settings JSON.
    /// Unknown presets fall back to `Lines` (matching the TUI's behavior).
    pub fn from_settings(settings: &serde_json::Value) -> Self {
        let Some(input) = settings.pointer("/ui/input") else {
            return Self::default();
        };
        let preset = match input.get("preset").and_then(serde_json::Value::as_str) {
            None | Some("lines") => InputPreset::Lines,
            Some("box") => InputPreset::Box,
            Some("filled") => InputPreset::Filled,
            Some(_) => InputPreset::Lines,
        };
        let mut style = Self::preset(preset);
        if input
            .get("show_prefix")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
            style.prefix.clear();
        } else if let Some(prefix) = input.get("prefix").and_then(serde_json::Value::as_str) {
            style.prefix = prefix.to_string();
        }
        if let Some(padding) = input
            .get("horizontal_padding")
            .and_then(serde_json::Value::as_u64)
        {
            style.horizontal_padding = padding.min(MAX_INPUT_PADDING as u64) as u16;
        }
        if let Some(padding) = input
            .get("vertical_padding")
            .and_then(serde_json::Value::as_u64)
        {
            style.vertical_padding = padding.min(MAX_INPUT_PADDING as u64) as u16;
        }
        if let Some(fill) = input.get("fill").and_then(serde_json::Value::as_bool) {
            style.fill = fill;
        }
        style
    }
}

#[derive(Default)]
pub struct State {
    pub rows: Vec<(String, String)>,
    /// Authoritative image attachments from the current transcript. Renderers
    /// must use these payloads rather than fetching by filename or path.
    pub images: Vec<ImageData>,
    /// Parallel to `rows`: tool-card overlay state for tool rows (`None` for
    /// ordinary text rows). Every row append goes through [`Self::push_row`]
    /// so the two vectors stay index-aligned.
    pub toolcards: Vec<Option<ToolCard>>,
    /// Parallel to `rows`: attached thinking that led to each row (`None` for
    /// rows with no reasoning). Every row append goes through [`Self::push_row`]
    /// so `rows`, `toolcards`, and `thinking` stay index-aligned. Reasoning is an
    /// attachment on the row it produced, never a peer row of its own.
    pub thinking: Vec<Option<String>>,
    /// In-progress reasoning for the current segment. Rendered as a single
    /// transient badge until it settles onto the row the segment produces (see
    /// [`Self::settle_reasoning`]). `None` once moved or when idle.
    pub live_reasoning: Option<String>,
    /// Indexes of rows whose content changed since the renderer last synced.
    /// Recorded by every row mutation ([`Self::push_row`], content overwrites,
    /// deltas) and drained by the UI layer once per frame via [`std::mem::take`].
    /// Only indices below the current `rows.len()` are meaningful.
    pub changed_rows: Vec<usize>,
    pub approvals: Vec<Approval>,
    pub status: String,
    pub ready: bool,
    pub busy: bool,
    pub snapshot: SessionSnapshot,
    pub expected_id: Option<i64>,
    pub repairing: bool,
    /// Most recent failure, cleared on the next successful load/sync/turn.
    pub last_error: Option<String>,
    /// Most recent per-turn token accounting.
    pub token_usage: Option<TokenUsage>,
    /// Daemon-owned declarative UI projection (panes, status lines, highlights).
    pub view: ViewModel,
    /// Opaque resolved theme payload from `ViewDiff::SetTheme`, if any.
    pub theme: Option<serde_json::Value>,
    /// Conversation-scoped background processes (newest daemon snapshot wins).
    pub processes: Vec<ProcessSnapshot>,
    pub processes_version: u64,
    /// Conversation-scoped background jobs (newest daemon snapshot wins).
    pub jobs: Vec<JobSnapshot>,
    pub jobs_version: u64,
    /// Boot-time resolved display state mirrored from the daemon.
    pub frontend: Option<FrontendState>,
    /// Parsed `FrontendState.tool_display` (name → config), refreshed on each
    /// `FrontendState`; drives custom tool-row headings and result visibility.
    pub tool_display: HashMap<String, ToolDisplayConfig>,
    /// `general.show_reasoning` from the daemon's resolved settings: when false
    /// (the default) live and historical reasoning are not surfaced, mirroring
    /// the TUI's `show_thinking` gate.
    pub show_reasoning: bool,
    /// Composer style from the daemon's resolved settings (`ui.input`).
    pub input_style: InputStyle,
    /// Request id of a pending `ctx.ui.key()` request awaiting a captured key.
    /// Set by `KeyRequest`; cleared when the client sends the matching
    /// `KeyReply`.
    pub pending_key: Option<u64>,
    /// Last reported turn work duration.
    pub work_elapsed_ms: Option<u64>,
    /// A fresh socket always replays the default actor's `conversation_loaded`
    /// first. A New tab skips exactly that one and lets its own conversation
    /// (created via `NewConversation`) load next.
    pub ignore_first_load: bool,
    /// When set, request only the newest `window` display messages on load and
    /// fetch older pages on demand; `None` keeps the whole transcript (matching
    /// the TUI, which always loads everything). Preserved across resets.
    pub window: Option<u32>,
    /// Whether older messages remain before the currently loaded suffix, so the
    /// "load older" affordance is shown. Set by `ConversationLoaded` (inferred
    /// from the window size) and `OlderMessagesLoaded`; cleared on a fresh load.
    pub has_older: bool,
    /// True while an older-page request is in flight (disables the affordance).
    pub loading_older: bool,
    /// Request id of the in-flight `LoadOlderMessages`, for reply correlation.
    older_request: Option<u64>,
    /// The display messages currently held (the suffix rendered into `rows`),
    /// tracked so older pages can be prepended without re-fetching the suffix
    /// and so `synchronize` can request at least what is already held.
    loaded_messages: Vec<ChatMessage>,
    /// Rows produced by the most recent [`Self::replace`]. Live turn events
    /// append rows beyond it, and since one display message can span several
    /// rows (or none) that growth cannot be recounted from `rows` — until the
    /// next replace rebuilds both, `loaded_messages` no longer counts every
    /// message held and [`Self::synchronize`] must go unbounded.
    replaced_rows: usize,
    /// Rows prepended by the most recent older-page load, pending the renderer's
    /// transcript-cache shift; `None` when no prepend is pending.
    pub pending_prepend: Option<usize>,
    assistant: Option<usize>,
    tools: HashMap<String, usize>,
    answered: HashSet<u64>,
    /// Call IDs whose daemon-computed edit preview already represents the result.
    shown_tool_rows: HashSet<String>,
    /// Daemon-computed edit previews retained for rebuilding the display transcript.
    /// This is deliberately separate from the model-facing persisted tool result.
    edit_previews: HashMap<String, String>,
    // Key and approval registries allocate IDs independently, both starting at 0.
    answered_keys: HashSet<u64>,
    sync_id: Option<u64>,
    /// When the outstanding `Synchronize` was sent; see [`Self::sync_in_flight`].
    sync_sent: Option<std::time::Instant>,
    /// A lag arrived while a sync was in flight: resync once its reply lands
    /// instead of orphaning it with a new request id.
    resync_pending: bool,
    next_id: u64,
}

/// Live overlay state for a tool row in the transcript.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolState {
    Running,
    Done,
    Error,
}

pub struct ToolCard {
    pub name: String,
    pub state: ToolState,
    /// The call's JSON arguments, or the command line for a local shell row.
    pub args: Option<String>,
    /// When a live call started, for the running-shell strip.
    pub started: Option<std::time::Instant>,
}

pub struct Approval {
    pub id: u64,
    /// The call awaiting approval, for the prompt title and command preview.
    pub call: bone_protocol::ToolCall,
    pub blocked: Option<String>,
}

/// Boot-time resolved display state mirrored from the daemon's `FrontendState`
/// event, so the native UI can render user customizations without running Lua.
/// Only the fields the native UI renders are kept; per-event details such as
/// `host_api_version`/`cwd` are applied by the caller from the event itself.
#[derive(Default, Clone)]
pub struct FrontendState {
    pub settings: serde_json::Value,
    pub commands: Vec<(String, String)>,
}

/// Most recent per-turn token accounting from the daemon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TokenUsage {
    pub sent: u64,
    pub received: u64,
    pub context_length: u64,
}

impl State {
    pub fn reset(&mut self, expected_id: Option<i64>) {
        self.reset_into(expected_id, false);
    }

    /// Prepare for a brand-new conversation. `NewConversation` is sent after
    /// attach; the socket's first `conversation_loaded` is the default actor
    /// replay and is skipped via `ignore_first_load`.
    pub fn reset_new(&mut self) {
        self.reset_into(None, true);
    }

    fn reset_into(&mut self, expected_id: Option<i64>, ignore_first_load: bool) {
        *self = Self {
            expected_id,
            ignore_first_load,
            next_id: self.next_id,
            window: self.window,
            ..Self::default()
        };
    }

    /// Clear the conversation transcript for `/clear`/`/new` without tearing
    /// down the connection or display state. The caller sends
    /// `NewConversation`; the daemon's follow-up snapshot replaces `snapshot`
    /// shortly after. Keeps `ready`, `expected_id`, `snapshot`, `frontend`,
    /// `view`, `theme`, `tool_display`, `ignore_first_load`, `sync_id`, and
    /// `next_id`.
    pub fn clear_conversation(&mut self) {
        self.rows.clear();
        self.images.clear();
        self.toolcards.clear();
        self.thinking.clear();
        self.live_reasoning = None;
        self.changed_rows.clear();
        self.approvals.clear();
        self.answered.clear();
        self.shown_tool_rows.clear();
        self.edit_previews.clear();
        self.answered_keys.clear();
        self.assistant = None;
        self.tools.clear();
        self.token_usage = None;
        self.work_elapsed_ms = None;
        self.pending_key = None;
        self.busy = false;
        self.repairing = false;
        self.last_error = None;
        self.status = "Ready".into();
        self.has_older = false;
        self.loading_older = false;
        self.older_request = None;
        self.loaded_messages.clear();
        self.replaced_rows = 0;
        self.pending_prepend = None;
    }

    /// Drop live edit previews when the daemon connection is lost.
    pub(crate) fn clear_edit_previews(&mut self) {
        self.shown_tool_rows.clear();
        self.edit_previews.clear();
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    pub fn synchronize(&mut self) -> RuntimeCommand {
        let request_id = self.next_id();
        self.sync_id = Some(request_id);
        self.sync_sent = Some(std::time::Instant::now());
        self.resync_pending = false;
        RuntimeCommand::Synchronize {
            request_id,
            include_messages: true,
            // Request at least what the client already holds so a repair sync
            // never discards loaded older pages, while staying bounded by the
            // window (not the full transcript). Rows appended live since the
            // last replace break that arithmetic (`loaded_messages` understates
            // what is held), so any such growth makes the sync unbounded rather
            // than risk a reply shorter than the transcript on screen.
            window: if self.rows.len() <= self.replaced_rows {
                self.window
                    .map(|window| window.max(self.loaded_messages.len() as u32))
            } else {
                None
            },
        }
    }

    /// True while a `Synchronize` reply is outstanding and not yet stale.
    /// Re-requesting sooner would give `sync_id` a new id and discard the
    /// in-flight reply, so repeated lag could keep repair from converging.
    pub fn sync_in_flight(&self) -> bool {
        self.sync_id.is_some()
            && self
                .sync_sent
                .is_some_and(|sent| sent.elapsed() < SYNC_STALE_AFTER)
    }

    /// Request the next page of older messages before the currently loaded
    /// suffix. Returns `None` when a turn is running (a rebuild would drop its
    /// streaming rows), a page is already in flight, the transcript is complete,
    /// or windowing is disabled.
    pub fn load_older(&mut self) -> Option<RuntimeCommand> {
        if self.busy || self.loading_older || !self.has_older {
            return None;
        }
        let window = self.window?;
        let request_id = self.next_id();
        self.older_request = Some(request_id);
        self.loading_older = true;
        Some(RuntimeCommand::LoadOlderMessages {
            request_id,
            offset: self.loaded_messages.len() as u32,
            limit: window,
        })
    }

    pub fn answered(&mut self, id: u64) {
        self.answered.insert(id);
        self.approvals.retain(|a| a.id != id);
    }

    /// Record that the client replied to `ctx.ui.key()` request `id`, clearing
    /// the pending capture slot if it still points at that request.
    pub fn answer_key(&mut self, id: u64) {
        self.answered_keys.insert(id);
        if self.pending_key == Some(id) {
            self.pending_key = None;
        }
    }

    /// Settle a daemon-run command that finished without starting a turn (a
    /// cancelled interactive menu, a client-local reply, or an empty result).
    /// The `KeyRequest` that opened the menu set `busy` + the waiting status; a
    /// `CommandComplete { submit: false }` is the only signal the turn is over
    /// for this tab, so clear both here or the composer stays stuck busy.
    pub fn finish_command(&mut self) {
        self.pending_key = None;
        self.busy = false;
        if self.status == KEY_WAIT_STATUS || self.status == CANCEL_STATUS {
            self.status = "Ready".into();
        }
    }

    /// Every append to `rows` must go through here so `toolcards` and `thinking`
    /// (both parallel to `rows`) keep their indexes aligned with `rows`.
    pub(crate) fn push_row(&mut self, role: impl Into<String>, text: impl Into<String>) -> usize {
        let i = self.rows.len();
        self.rows.push((role.into(), text.into()));
        self.toolcards.push(None);
        self.thinking.push(None);
        self.changed_rows.push(i);
        i
    }

    /// Append a settled thought to the row it produced, concatenating when a row
    /// accumulates reasoning from several segments. Keeps `thinking` the same
    /// length as `rows` via the `get_mut` guard.
    fn attach_thinking(&mut self, row: usize, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(slot) = self.thinking.get_mut(row) {
            match slot {
                Some(existing) => existing.push_str(&text),
                None => *slot = Some(text),
            }
        }
    }

    /// Move the in-progress `live_reasoning` onto the row its segment produced,
    /// returning it to `None`. Called when a segment settles onto a new row
    /// (assistant text, tool call, or finished response).
    fn settle_reasoning(&mut self, row: usize) {
        if let Some(live) = self.live_reasoning.take() {
            self.attach_thinking(row, live);
        }
    }

    /// Rebuild the row projection from `messages`. `prefix_len` leading
    /// messages are a freshly prepended older page (0 for a full replace); the
    /// return value is the row index at which the suffix begins, so the caller
    /// can shift its index-keyed caches by exactly that many rows. The count
    /// must come from the rebuild itself: a suffix's leading tool result can
    /// merge into a prefix tool-call row and add no row of its own, so the
    /// number of prepended rows is not `rows.len() - old_rows`.
    fn replace(&mut self, messages: Vec<ChatMessage>, busy: bool, prefix_len: usize) -> usize {
        self.loaded_messages = messages.clone();
        self.rows.clear();
        self.images.clear();
        self.toolcards.clear();
        self.thinking.clear();
        self.live_reasoning = None;
        self.assistant = None;
        self.tools.clear();
        self.shown_tool_rows.clear();
        // Seed previews persisted with each result so a reloaded conversation
        // renders exactly like the live one.
        for message in &messages {
            if message.role == ChatRole::Tool
                && !message.is_error
                && let (Some(id), Some(preview)) = (&message.tool_call_id, &message.edit_preview)
            {
                self.edit_previews.insert(id.clone(), preview.clone());
            }
        }
        let failed_calls: HashSet<String> = messages
            .iter()
            .filter(|message| message.role == ChatRole::Tool && message.is_error)
            .filter_map(|message| message.tool_call_id.clone())
            .collect();
        let total = messages.len();
        let mut prefix_rows = 0;
        for (index, message) in messages.into_iter().enumerate() {
            if index == prefix_len {
                prefix_rows = self.rows.len();
            }
            if message.role == ChatRole::System {
                continue;
            }
            // The row this message's reasoning attaches to. A tool row is
            // preferred so settled reasoning is revealed inside an expanded call;
            // otherwise it is the message's content row. `None` when the message
            // produces no row at all.
            let mut content_target: Option<usize> = None;
            let mut tool_target: Option<usize> = None;
            // Computed before `content`/`reasoning` are moved out below.
            let synthetic_relay = message.is_synthetic_relay();
            let reasoning = message.reasoning.map(|reasoning| reasoning.text);
            if message.role == ChatRole::Tool {
                let id = message.tool_call_id.unwrap_or_default();
                let name = message.name.unwrap_or_else(|| {
                    self.tools
                        .get(&id)
                        .and_then(|&i| self.toolcards[i].as_ref())
                        .map(|card| card.name.clone())
                        .unwrap_or_else(|| "output".into())
                });
                let has_preview = !message.is_error && self.edit_previews.contains_key(&id);
                if message.is_error {
                    self.edit_previews.remove(&id);
                }
                tool_target = Some(self.tool_result(
                    id.clone(),
                    name,
                    (!has_preview).then_some(message.content),
                    message.is_error,
                ));
                if has_preview {
                    self.append_edit_preview(&id);
                }
            } else if !message.content.is_empty() {
                // A runtime relay of tool-returned images reads as ambient text
                // (a system note), not a user prompt, after a history reload.
                let role = if synthetic_relay {
                    "system"
                } else {
                    message.role.as_str()
                };
                content_target = Some(self.push_row(role, message.content));
            }
            for call in message.tool_calls {
                let call_id = call.id.clone();
                let args = (!call.arguments.is_null()).then(|| call.arguments.to_string());
                let (i, _) = self.tool_row(call.id, call.name.clone());
                tool_target.get_or_insert(i);
                // Keep arguments separate from output. A following saved or live
                // result fills this same card and supplies its authoritative state.
                self.toolcards[i] = Some(ToolCard {
                    name: call.name,
                    state: if busy {
                        ToolState::Running
                    } else {
                        ToolState::Done
                    },
                    args,
                    started: None,
                });
                if !failed_calls.contains(&call_id) {
                    self.append_edit_preview(&call_id);
                }
            }
            if !message.images.is_empty() {
                self.images.extend(message.images.iter().cloned());
                self.push_row("attachments", format!("{} image(s)", message.images.len()));
            }
            // Reasoning is surfaced only when configured, matching the live gate.
            // It attaches to the tool row this message produced, if any; a message
            // with no tool row simply keeps no reasoning (there is nowhere to reveal
            // it — reasoning is never a peer row).
            if self.show_reasoning
                && let Some(reasoning) = reasoning
                && let Some(row) = tool_target.or(content_target)
            {
                self.attach_thinking(row, reasoning);
            }
        }
        self.replaced_rows = self.rows.len();
        if prefix_len >= total {
            prefix_rows = self.rows.len();
        }
        prefix_rows
    }

    fn delta(&mut self, text: String, reasoning: bool) {
        if reasoning {
            if !self.show_reasoning {
                // Live reasoning is suppressed unless `general.show_reasoning` is
                // set, mirroring the TUI's `show_thinking` gate.
                return;
            }
            // Accumulate into the transient badge; it settles onto the row the
            // segment produces. No reasoning rows appear in scrollback.
            self.live_reasoning
                .get_or_insert_with(String::new)
                .push_str(&text);
            return;
        }
        let (row, created) = match self.assistant {
            Some(row) => (row, false),
            None => {
                let row = self.push_row("assistant", String::new());
                self.assistant = Some(row);
                (row, true)
            }
        };
        if created {
            // The first assistant text of a segment settles any leading thought.
            self.settle_reasoning(row);
        }
        // Append streamed chunks verbatim. Chunk boundaries are not token
        // boundaries, so inferring separators from character classes would
        // corrupt words that are split across chunks (e.g. "token" + "ization").
        self.rows[row].1.push_str(&text);
        if !created {
            // push_row already recorded a brand-new row; only appending to an
            // existing row is a mutation that needs a fresh record here.
            self.changed_rows.push(row);
        }
    }

    /// Reinsert a cached daemon preview once, using the same marker that
    /// suppresses the successful model-facing tool result.
    fn append_edit_preview(&mut self, call_id: &str) {
        let Some(preview) = self.edit_previews.get(call_id).cloned() else {
            return;
        };
        if self.shown_tool_rows.insert(call_id.to_owned()) {
            self.push_row("system", preview);
        }
    }

    /// Returns the tool row's index and whether this call created it. The tool
    /// events share one row per call id so a stream stays in place.
    fn tool_row(&mut self, id: String, name: String) -> (usize, bool) {
        if let Some(&row) = self.tools.get(&id) {
            return (row, false);
        }
        let row = self.push_row(format!("tool: {name}"), String::new());
        // Older/incomplete history may omit ids; never merge unrelated orphans.
        if !id.is_empty() {
            self.tools.insert(id, row);
        }
        (row, true)
    }

    /// History and live results share card state, labels, arguments, and output.
    /// Returns the tool row's index so callers can attach leading reasoning.
    fn tool_result(
        &mut self,
        call_id: String,
        name: String,
        content: Option<String>,
        is_error: bool,
    ) -> usize {
        let (i, created) = self.tool_row(call_id, name.clone());
        let args = self.toolcards[i]
            .as_ref()
            .and_then(|card| card.args.clone());
        self.rows[i].0 = format!("tool: {name}{}", if is_error { " (error)" } else { "" });
        if let Some(content) = content {
            self.rows[i].1 = content;
        }
        if !created {
            self.changed_rows.push(i);
        }
        self.toolcards[i] = Some(ToolCard {
            name,
            state: if is_error {
                ToolState::Error
            } else {
                ToolState::Done
            },
            args,
            started: None,
        });
        i
    }

    /// Apply a daemon view diff to the local projection. Upserts replace a
    /// component with the same id in place; removes drop it and its highlight.
    fn apply_view_diff(&mut self, diff: ViewDiff) {
        match diff {
            ViewDiff::Upsert { component } => {
                let id = component.id().to_string();
                if let Some(existing) = self.view.components.iter_mut().find(|c| c.id() == id) {
                    *existing = component;
                } else {
                    self.view.components.push(component);
                }
            }
            ViewDiff::Remove { id } => {
                self.view.components.retain(|c| c.id() != id);
                self.view.highlights.remove(&id);
            }
            ViewDiff::UpdatePlacement { id, placement } => {
                if let Some(Component::Float {
                    placement: current, ..
                }) = self.view.components.iter_mut().find(|c| c.id() == id)
                {
                    *current = placement;
                }
            }
            ViewDiff::SetHighlight { name, fg } => match fg {
                Some(fg) => {
                    self.view.highlights.insert(name, fg);
                }
                None => {
                    self.view.highlights.remove(&name);
                }
            },
            ViewDiff::SetTheme { theme } if theme.is_object() => self.theme = Some(theme),
            ViewDiff::SetTheme { .. } => {}
        }
    }

    pub fn reduce(&mut self, event: RuntimeEvent) -> Option<RuntimeCommand> {
        // A fresh socket first replays the default actor. Do not expose or mutate
        // that actor's state while restoring an explicitly selected conversation.
        if !self.ready {
            match &event {
                RuntimeEvent::ConversationLoaded { .. } if self.ignore_first_load => {
                    // A fresh socket always replays the default actor first; a
                    // New tab skips that one load and waits for its own
                    // conversation (created via NewConversation) next.
                    self.ignore_first_load = false;
                    return None;
                }
                RuntimeEvent::ConversationLoaded { snapshot, .. }
                    if self.expected_id.is_none()
                        || snapshot.conversation_id == self.expected_id => {}
                RuntimeEvent::ConversationLoadFailed { id, message }
                    if Some(*id) == self.expected_id =>
                {
                    self.status = "Conversation unavailable".into();
                    // Keep the complete daemon error for the details disclosure,
                    // but do not duplicate a potentially huge path in scrollback.
                    self.last_error = Some(message.clone());
                    return None;
                }
                // FrontendState is the daemon's boot-time display baseline and
                // arrives before ConversationLoaded on a fresh attachment.
                RuntimeEvent::FrontendState { .. } => {}
                _ => return None,
            }
        }
        match event {
            RuntimeEvent::ConversationLoaded {
                messages,
                snapshot,
                busy,
            } => {
                // The daemon returns `min(total, window)` newest messages, so a
                // full page at the window size means older messages remain. (A
                // transcript that exactly fills the window reports one harmless
                // extra page, whose empty reply clears the affordance.)
                let has_older = self
                    .window
                    .is_some_and(|window| messages.len() >= window as usize);
                self.replace(messages, busy, 0);
                self.has_older = has_older;
                self.loading_older = false;
                self.older_request = None;
                self.pending_prepend = None;
                self.snapshot = snapshot;
                self.busy = busy;
                self.ready = true;
                self.approvals.clear();
                self.answered.clear();
                self.answered_keys.clear();
                self.pending_key = None;
                self.ignore_first_load = false;
                // Record the authoritative id so a reconnect replays into this
                // conversation instead of being filtered as a default actor.
                self.expected_id = self.snapshot.conversation_id;
                self.last_error = None;
                self.repairing = busy;
                self.status = if busy {
                    "Joining running turn"
                } else {
                    "Ready"
                }
                .into();
                // Also replay pending interactions for an idle attachment.
                return Some(self.synchronize());
            }
            RuntimeEvent::StateSynchronized {
                request_id,
                busy,
                snapshot,
                messages,
                view,
                theme,
            } if self.sync_id == Some(request_id) => {
                self.sync_id = None;
                self.last_error = None;
                if let Some(messages) = messages {
                    self.replace(messages, busy, 0);
                }
                self.snapshot = snapshot;
                self.busy = busy;
                self.repairing = busy;
                if let Some(theme) = theme.filter(serde_json::Value::is_object) {
                    self.theme = Some(theme);
                }
                self.approvals.clear(); // authoritative replay follows this event
                if let Some(view) = view {
                    self.view = view;
                }
                if !busy {
                    self.status = "Ready".into();
                }
                if std::mem::take(&mut self.resync_pending) {
                    return Some(self.synchronize());
                }
            }
            RuntimeEvent::OlderMessagesLoaded {
                request_id,
                messages,
                has_older,
            } if self.older_request == Some(request_id) => {
                self.older_request = None;
                self.loading_older = false;
                // A turn started while the page was in flight: rebuilding now
                // would discard its streaming rows, so drop the page.
                if self.busy {
                    return None;
                }
                self.has_older = has_older;
                let mut older = messages;
                let prefix_len = older.len();
                let mut combined = std::mem::take(&mut self.loaded_messages);
                older.append(&mut combined);
                let prefix_rows = self.replace(older, false, prefix_len);
                self.pending_prepend = Some(prefix_rows);
            }
            RuntimeEvent::StateSnapshot { snapshot } => self.snapshot = snapshot,
            RuntimeEvent::Started {
                task,
                model,
                display,
                ..
            } => {
                self.push_row("user", display.unwrap_or(task));
                self.assistant = None;
                self.live_reasoning = None;
                self.tools.clear();
                self.shown_tool_rows.clear();
                self.busy = true;
                self.status = format!("Running {model}");
            }
            RuntimeEvent::TextDelta { text } => self.delta(text, false),
            RuntimeEvent::ReasoningDelta { text } => self.delta(text, true),
            RuntimeEvent::ToolCall {
                id,
                name,
                summary,
                arguments,
                ..
            } => {
                self.assistant = None;
                let (i, created) = self.tool_row(id, name.clone());
                if created {
                    // The tool call settles any reasoning that led to it.
                    self.settle_reasoning(i);
                }
                self.rows[i].1 = summary;
                if !created {
                    self.changed_rows.push(i);
                }
                let args = (!arguments.is_null()).then(|| arguments.to_string());
                self.toolcards[i] = Some(ToolCard {
                    name,
                    state: ToolState::Running,
                    args,
                    started: Some(std::time::Instant::now()),
                });
            }
            RuntimeEvent::ToolOutput {
                call_id, content, ..
            } => {
                let (i, created) = self.tool_row(call_id, "output".into());
                self.rows[i].1.push_str(&content);
                if !created {
                    self.changed_rows.push(i);
                }
            }
            RuntimeEvent::ToolResult {
                call_id,
                name,
                content,
                is_error,
            } => {
                let shown_preview = self.shown_tool_rows.remove(&call_id);
                if is_error {
                    self.edit_previews.remove(&call_id);
                    self.tool_result(call_id, name, Some(content), is_error);
                } else if shown_preview || self.edit_previews.contains_key(&call_id) {
                    self.tool_result(call_id, name, None, false);
                } else {
                    self.tool_result(call_id, name, Some(content), is_error);
                }
            }
            RuntimeEvent::Finished { content } => {
                // Finished is the final full response, not another delta.
                if let Some(i) = self.assistant {
                    self.rows[i].1 = content;
                    self.changed_rows.push(i);
                } else if !content.is_empty() {
                    let i = self.push_row("assistant", content);
                    // A response that only ever arrived as `Finished` still
                    // settles any reasoning streamed before it.
                    self.settle_reasoning(i);
                }
                self.approvals.clear();
                self.status = "Finishing…".into();
            }
            RuntimeEvent::TurnCompleted { .. } | RuntimeEvent::TurnComplete => {
                self.busy = false;
                self.repairing = false;
                self.approvals.clear();
                self.last_error = None;
                self.status = "Ready".into();
                // The post-turn sync is unbounded whenever the turn appended
                // rows, so its reply holds the whole transcript and no older
                // page can remain; meanwhile a stale affordance would page
                // against the stale `loaded_messages` offset.
                self.has_older &= self.rows.len() <= self.replaced_rows;
                return Some(self.synchronize());
            }
            RuntimeEvent::Failed { message } => {
                self.status = format!("Failed: {message}");
                self.push_row("system", format!("Failed: {message}"));
                self.last_error = Some(message);
                self.approvals.clear();
                self.busy = false;
            }
            RuntimeEvent::ApprovalRequest {
                id,
                call_id,
                name,
                arguments,
                blocked,
                auto_allows,
                preview,
                ..
            } => {
                if self.answered.contains(&id) || self.approvals.iter().any(|a| a.id == id) {
                    return None;
                }
                if let Some(preview) = preview {
                    self.edit_previews.insert(call_id.clone(), preview);
                    self.append_edit_preview(&call_id);
                }
                if auto_allows {
                    let outcome = match blocked {
                        Some(reason) => CallOutcome::Blocked(reason),
                        None => CallOutcome::Approve,
                    };
                    self.answered(id);
                    return Some(RuntimeCommand::ApprovalReply { id, outcome });
                }
                self.approvals.push(Approval {
                    id,
                    call: bone_protocol::ToolCall {
                        id: call_id,
                        name,
                        arguments,
                    },
                    blocked,
                });
            }
            RuntimeEvent::KeyRequest { id }
                if !self.answered_keys.contains(&id) => {
                    self.pending_key = Some(id);
                    self.status = KEY_WAIT_STATUS.into();
                    self.busy = true;
                }
            RuntimeEvent::StreamLagged { .. } => {
                self.repairing = true;
                self.status = "Repairing missed events…".into();
                // One notice per repair: repeat lags while a resync is
                // already queued stay silent instead of spamming rows.
                if !self.resync_pending {
                    self.push_row("system", "Repairing missed events…");
                }
                if self.sync_in_flight() {
                    self.resync_pending = true;
                    return None;
                }
                return Some(self.synchronize());
            }
            RuntimeEvent::Status { message } | RuntimeEvent::Notice { message } => {
                self.status = message
            }
            RuntimeEvent::TokenUsage {
                sent,
                received,
                context_length,
            } => {
                self.token_usage = Some(TokenUsage {
                    sent,
                    received,
                    context_length,
                });
            }
            RuntimeEvent::WorkElapsed { elapsed_ms } => {
                self.work_elapsed_ms = Some(elapsed_ms);
                // End-of-turn notice in the transcript, matching the TUI.
                self.push_row(
                    "system",
                    format!("worked for {}", format_elapsed_ms(elapsed_ms)),
                );
            }
            RuntimeEvent::ViewSnapshot { view } => self.view = view,
            RuntimeEvent::ViewDiff { diff } => self.apply_view_diff(diff),
            RuntimeEvent::ProcessesSnapshot { version, processes }
                // Snapshots are versioned: ignore an out-of-order older one.
                if version >= self.processes_version => {
                    self.processes_version = version;
                    self.processes = processes;
                }
            RuntimeEvent::JobsSnapshot { version, jobs }
                if version >= self.jobs_version => {
                    self.jobs_version = version;
                    self.jobs = jobs;
                }
            RuntimeEvent::FrontendState {
                settings,
                commands,
                tool_display,
                ..
            } => {
                self.tool_display = serde_json::from_value(tool_display).unwrap_or_default();
                self.show_reasoning = settings
                    .pointer("/general/show_reasoning")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                self.input_style = InputStyle::from_settings(&settings);
                // ResolvedFrontendSettings flattens BoneSettings, so the
                // daemon-resolved semantic theme is `settings.theme`. Keep the
                // prior theme when a legacy or malformed payload omits it.
                if let Some(theme) = settings
                    .get("theme")
                    .filter(|theme| theme.is_object())
                    .cloned()
                {
                    self.theme = Some(theme);
                }
                self.frontend = Some(FrontendState { settings, commands });
                // Rows are relabelled at render time from this map.
                self.changed_rows.extend(0..self.rows.len());
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bone_protocol::ToolCall;
    use serde_json::json;

    fn loaded() -> State {
        let mut state = State::default();
        state.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::new(ChatRole::User, "old")],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        state
    }

    #[test]
    fn windowed_load_requests_and_prepends_older_messages() {
        let mut s = State {
            window: Some(2),
            ..Default::default()
        };
        // A transcript that fills the window implies older history remains.
        let _ = s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![
                ChatMessage::new(ChatRole::User, "u2"),
                ChatMessage::new(ChatRole::Assistant, "a2"),
            ],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert!(s.has_older);
        assert_eq!(s.loaded_messages.len(), 2);

        let command = s.load_older().expect("older page requested");
        let (request_id, offset, limit) = match command {
            RuntimeCommand::LoadOlderMessages {
                request_id,
                offset,
                limit,
            } => (request_id, offset, limit),
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!((offset, limit), (2, 2));
        assert!(s.loading_older);
        // A second page cannot be requested while one is already in flight.
        assert!(s.load_older().is_none());

        let _ = s.reduce(RuntimeEvent::OlderMessagesLoaded {
            request_id,
            messages: vec![
                ChatMessage::new(ChatRole::User, "u1"),
                ChatMessage::new(ChatRole::Assistant, "a1"),
            ],
            has_older: false,
        });
        assert!(!s.has_older);
        assert!(!s.loading_older);
        assert_eq!(s.loaded_messages.len(), 4);
        // Older messages precede the previously loaded suffix.
        assert_eq!(s.loaded_messages.first().unwrap().content, "u1");
        assert_eq!(s.loaded_messages.last().unwrap().content, "a2");
        // The prepend is signalled for the transcript cache to shift.
        assert!(s.pending_prepend.is_some());
    }

    #[test]
    fn older_page_prepend_counts_rows_when_boundary_splits_a_tool_pair() {
        // The page boundary lands between an assistant tool call (older page)
        // and its result (first suffix message). On the fresh rebuild the
        // result merges into the prefix call row and adds no row of its own, so
        // the prepend count must come from the rebuild itself, not
        // `new_rows - old_suffix_rows`.
        let mut s = State {
            window: Some(2),
            ..Default::default()
        };
        // Suffix: an orphaned tool result (its call lives in the older page)
        // followed by a user message. The orphan still gets its own row.
        let _ = s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![
                ChatMessage::tool(bone_protocol::ToolResult {
                    call_id: "t1".into(),
                    name: "shell".into(),
                    content: "ok".into(),
                    ..Default::default()
                }),
                ChatMessage::new(ChatRole::User, "u2"),
            ],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert!(s.has_older);
        assert_eq!(s.rows.len(), 2);

        let command = s.load_older().expect("older page requested");
        let request_id = match command {
            RuntimeCommand::LoadOlderMessages { request_id, .. } => request_id,
            other => panic!("unexpected command: {other:?}"),
        };
        let _ = s.reduce(RuntimeEvent::OlderMessagesLoaded {
            request_id,
            messages: vec![
                ChatMessage::new(ChatRole::User, "u1"),
                ChatMessage::assistant_with_tools(
                    "",
                    vec![ToolCall {
                        id: "t1".into(),
                        name: "shell".into(),
                        arguments: json!({"command": "ls"}),
                    }],
                ),
            ],
            has_older: false,
        });
        // u1, the call row (the result merged into it), and u2: three rows, with
        // the suffix starting at index 2 — not the 2 - 1 = 1 a subtraction gives.
        assert_eq!(s.rows.len(), 3);
        assert_eq!(s.pending_prepend, Some(2));
    }

    #[test]
    fn windowed_load_without_older_history_hides_the_affordance() {
        let mut s = State {
            window: Some(4),
            ..Default::default()
        };
        let _ = s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::new(ChatRole::User, "only")],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert!(!s.has_older);
        assert!(s.load_older().is_none());
    }

    #[test]
    fn startup_frontend_state_supplies_theme_before_conversation_load() {
        let mut s = State::default();
        let theme = json!({
            "name": "startup",
            "palette": { "accent": "#112233" }
        });
        s.reduce(frontend_event(json!({ "theme": theme.clone() })));
        assert!(
            !s.ready,
            "frontend settings arrive before conversation load"
        );
        assert_eq!(s.theme.as_ref(), Some(&theme));

        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert_eq!(s.theme.as_ref(), Some(&theme));
    }

    #[test]
    fn missing_or_malformed_frontend_theme_does_not_replace_last_good_theme() {
        let mut s = loaded();
        let theme = json!({ "palette": { "accent": "#112233" } });
        s.theme = Some(theme.clone());

        for settings in [
            json!({}),
            json!({ "theme": "not-an-object" }),
            json!({ "theme": null }),
        ] {
            s.reduce(frontend_event(settings));
            assert_eq!(s.theme.as_ref(), Some(&theme));
        }
    }

    #[test]
    fn reconnect_reset_clears_stale_theme_from_previous_attachment() {
        let mut s = loaded();
        s.theme = Some(json!({ "palette": { "accent": "#112233" } }));
        s.frontend = Some(FrontendState::default());
        s.reset(Some(42));
        assert!(!s.ready);
        assert!(s.theme.is_none());
        assert!(s.frontend.is_none());
    }
    #[test]
    fn input_style_defaults_and_parsing() {
        // No `ui.input` → the Lines preset with no prefix.
        let style = InputStyle::from_settings(&json!({}));
        assert_eq!(style, InputStyle::default());
        assert_eq!(style.preset, InputPreset::Lines);
        assert!(style.prefix.is_empty());
        assert_eq!((style.horizontal_padding, style.vertical_padding), (0, 0));
        assert!(!style.fill);

        // Named presets carry their own padding/fill defaults.
        let boxed = InputStyle::from_settings(&json!({ "ui": { "input": { "preset": "box" } } }));
        assert_eq!(boxed.preset, InputPreset::Box);
        assert_eq!((boxed.horizontal_padding, boxed.vertical_padding), (1, 0));
        assert!(!boxed.fill);

        let filled =
            InputStyle::from_settings(&json!({ "ui": { "input": { "preset": "filled" } } }));
        assert_eq!(filled.preset, InputPreset::Filled);
        assert!(filled.fill);

        // Unknown presets fall back to Lines.
        let unknown =
            InputStyle::from_settings(&json!({ "ui": { "input": { "preset": "zigzag" } } }));
        assert_eq!(unknown.preset, InputPreset::Lines);

        // Explicit prefix/padding/fill; padding clamps to MAX_INPUT_PADDING.
        let custom = InputStyle::from_settings(&json!({
            "ui": { "input": {
                "preset": "box",
                "prefix": "$ ",
                "horizontal_padding": 99,
                "vertical_padding": 3,
                "fill": true,
            } }
        }));
        assert_eq!(custom.prefix, "$ ");
        assert_eq!(custom.horizontal_padding, MAX_INPUT_PADDING);
        assert_eq!(custom.vertical_padding, 3);
        assert!(custom.fill);

        // `show_prefix = false` clears the prefix even when one is configured.
        let hidden = InputStyle::from_settings(&json!({
            "ui": { "input": { "prefix": "$ ", "show_prefix": false } }
        }));
        assert!(hidden.prefix.is_empty());
    }

    #[test]
    fn failed_and_lagged_events_append_system_rows() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::Failed {
            message: "boom".into(),
        });
        assert_eq!(
            s.rows.last().unwrap(),
            &("system".to_string(), "Failed: boom".to_string())
        );
        assert_eq!(s.last_error.as_deref(), Some("boom"));

        let mut s = loaded();
        s.reduce(RuntimeEvent::StreamLagged { skipped: 5 });
        assert_eq!(
            s.rows.last().unwrap(),
            &("system".to_string(), "Repairing missed events…".to_string())
        );
    }

    #[test]
    fn lag_during_an_inflight_sync_defers_one_resync() {
        let mut s = loaded();
        let first = s.sync_id.expect("load requests a sync");
        // Lag while the reply is outstanding must not orphan it with a new id.
        assert!(
            s.reduce(RuntimeEvent::StreamLagged { skipped: 1 })
                .is_none()
        );
        assert!(
            s.reduce(RuntimeEvent::StreamLagged { skipped: 1 })
                .is_none()
        );
        assert_eq!(s.sync_id, Some(first));
        let lag_rows = s
            .rows
            .iter()
            .filter(|(_, text)| text == "Repairing missed events…")
            .count();
        assert_eq!(lag_rows, 1);

        let synced = |request_id| RuntimeEvent::StateSynchronized {
            request_id,
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: None,
            theme: None,
        };
        let Some(RuntimeCommand::Synchronize {
            request_id: second, ..
        }) = s.reduce(synced(first))
        else {
            panic!("expected one follow-up synchronization");
        };
        assert_ne!(second, first);
        assert!(s.reduce(synced(second)).is_none());
        assert!(!s.repairing);
        assert!(!s.sync_in_flight());
    }

    #[test]
    fn finished_replaces_delta_instead_of_duplicating() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::TextDelta {
            text: "hello".into(),
        });
        s.reduce(RuntimeEvent::Finished {
            content: "hello".into(),
        });
        assert_eq!(s.rows.last().unwrap().1, "hello");
    }

    #[test]
    fn streamed_chunks_concatenate_verbatim() {
        // Provider chunks are not token-aligned, so a word can be split across
        // events. Chunks must join exactly, with no inferred separators.
        let mut s = loaded();
        for chunk in ["The token", "ization", " process"] {
            s.reduce(RuntimeEvent::TextDelta { text: chunk.into() });
        }
        assert_eq!(s.rows.last().unwrap().1, "The tokenization process");
    }

    #[test]
    fn clear_conversation_keeps_display_state_and_drops_transcript() {
        let mut s = loaded();
        s.ready = true;
        s.status = "Working…".into();
        s.token_usage = Some(TokenUsage {
            sent: 1,
            received: 2,
            context_length: 3,
        });
        s.work_elapsed_ms = Some(1234);
        s.last_error = Some("boom".into());
        s.busy = true;
        s.frontend = Some(FrontendState::default());
        s.view = ViewModel::default();
        s.theme = Some(json!({ "palette": {} }));

        s.clear_conversation();

        assert!(s.rows.is_empty());
        assert!(s.toolcards.is_empty());
        assert!(s.token_usage.is_none());
        assert!(s.work_elapsed_ms.is_none());
        assert!(s.last_error.is_none());
        assert!(!s.busy);
        assert_eq!(s.status, "Ready");
        assert!(s.ready, "ready is preserved");
        assert!(s.frontend.is_some(), "frontend is preserved");
        assert!(s.theme.is_some(), "theme is preserved");
    }

    #[test]
    fn snapshot_replaces_history_and_ignores_other_sync_ids() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id: 999,
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: Some(vec![]),
            theme: None,
        });
        assert_eq!(s.rows.len(), 1);
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id: s.sync_id.unwrap(),
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: Some(vec![]),
            theme: None,
        });
        assert!(s.rows.is_empty());
    }

    #[test]
    fn restore_ignores_default_conversation() {
        let mut s = State::default();
        s.reset(Some(42));
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert!(!s.ready);
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![],
            snapshot: SessionSnapshot {
                conversation_id: Some(42),
                ..Default::default()
            },
            busy: true,
        });
        assert!(s.ready && s.repairing && s.busy);
    }

    #[test]
    fn tool_results_replace_output_by_id() {
        let mut s = loaded();
        for id in ["a", "b"] {
            s.reduce(RuntimeEvent::ToolOutput {
                call_id: id.into(),
                content: id.into(),
                stderr: false,
            });
        }
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "a".into(),
            name: "shell".into(),
            content: "final".into(),
            is_error: false,
        });
        assert_eq!(s.rows[1].1, "final");
        assert_eq!(s.rows[2].1, "b");
    }

    #[test]
    fn new_conversation_skips_default_replay_then_loads() {
        let mut s = State::default();
        s.reset_new();
        assert!(s.ignore_first_load);
        // Fresh-socket default actor replay must not surface in a New tab.
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::new(ChatRole::User, "default actor")],
            snapshot: SessionSnapshot {
                conversation_id: Some(3),
                ..Default::default()
            },
            busy: false,
        });
        assert!(!s.ready);
        assert!(s.rows.is_empty());
        assert!(!s.ignore_first_load, "exactly one replay is skipped");
        // The tab's own conversation loads next and pins the expected id.
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::new(ChatRole::User, "first prompt")],
            snapshot: SessionSnapshot {
                conversation_id: Some(4),
                ..Default::default()
            },
            busy: false,
        });
        assert!(s.ready);
        assert_eq!(s.expected_id, Some(4));
        // A reconnect must filter the default replay by expected id.
        s.reset(Some(4));
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert!(!s.ready);
    }

    #[test]
    fn tool_cards_track_live_lifecycle_and_history_is_done() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            summary: "checking…".into(),
            arguments: json!({"cmd": "ls"}),
        });
        let i = s.rows.len() - 1;
        assert!(s.rows[i].0.starts_with("tool: shell"));
        let card = s.toolcards[i].as_ref().unwrap();
        assert_eq!(card.state, ToolState::Running);
        assert_eq!(card.name, "shell");
        assert!(card.args.as_deref().unwrap().contains("cmd"));
        s.reduce(RuntimeEvent::ToolOutput {
            call_id: "c1".into(),
            content: "partial".into(),
            stderr: false,
        });
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "c1".into(),
            name: "shell".into(),
            content: "done".into(),
            is_error: false,
        });
        let i = s.rows.len() - 1;
        assert_eq!(s.rows[i].1, "done");
        assert_eq!(s.toolcards[i].as_ref().unwrap().state, ToolState::Done);
        // Error results flip the card and the role label.
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "c2".into(),
            name: "read_file".into(),
            content: "no such file".into(),
            is_error: true,
        });
        let i = s.rows.len() - 1;
        assert_eq!(s.toolcards[i].as_ref().unwrap().state, ToolState::Error);
        assert!(s.rows[i].0.ends_with("(error)"));

        // History replay produces Done cards carrying their arguments.
        let mut state = State::default();
        let mut message = ChatMessage::new(ChatRole::Assistant, "");
        message.tool_calls.push(ToolCall {
            id: "c9".into(),
            name: "edit_file".into(),
            arguments: json!({"path": "a.txt"}),
        });
        state.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![message],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        let i = state.rows.len() - 1;
        assert_eq!(state.rows[i].0, "tool: edit_file");
        let card = state.toolcards[i].as_ref().unwrap();
        assert_eq!(card.state, ToolState::Done);
        assert!(card.args.as_deref().unwrap().contains("a.txt"));
    }

    #[test]
    fn history_pairs_tool_results_by_id_and_preserves_card_details() {
        let mut s = loaded();
        s.reduce(frontend_state(json!({
            "shell": { "args": ["command"], "show_result": true, "eager": true }
        })));
        let calls = ["a", "b"].map(|id| ToolCall {
            id: id.into(),
            name: "shell".into(),
            arguments: json!({"command": id}),
        });
        let messages = vec![
            ChatMessage::assistant_with_tools("Checking", calls.to_vec()),
            ChatMessage::tool(bone_protocol::ToolResult::error("b", "shell", "failed")),
            ChatMessage::tool(bone_protocol::ToolResult {
                call_id: "a".into(),
                name: "shell".into(),
                content: "success".into(),
                ..Default::default()
            }),
        ];
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: messages.clone(),
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert_eq!(s.rows.len(), 3, "results belong inside the original cards");
        assert_eq!(s.tool_display["shell"].show_result, Some(true));
        assert_eq!(s.rows[0], ("assistant".into(), "Checking".into()));
        for (i, id, body, state) in [
            (1, "a", "success", ToolState::Done),
            (2, "b", "failed", ToolState::Error),
        ] {
            assert_eq!(s.rows[i].1, body);
            assert_eq!(s.tools[id], i);
            let card = s.toolcards[i].as_ref().unwrap();
            assert_eq!(card.state, state);
            assert_eq!(card.args, Some(json!({"command": id}).to_string()));
        }
        let rows = s.rows.clone();
        let RuntimeCommand::Synchronize { request_id, .. } = s.synchronize() else {
            panic!("expected synchronization");
        };
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id,
            messages: Some(messages),
            snapshot: SessionSnapshot::default(),
            busy: false,
            view: None,
            theme: None,
        });
        assert_eq!(
            s.rows, rows,
            "turn-end synchronization uses the same replay"
        );
        assert_eq!(s.toolcards[2].as_ref().unwrap().state, ToolState::Error);
    }

    #[test]
    fn history_orphan_tool_results_keep_empty_bodies_errors_and_images() {
        let image = ImageData {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
            ..Default::default()
        };
        let mut s = State::default();
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![
                ChatMessage::tool(bone_protocol::ToolResult {
                    call_id: "orphan".into(),
                    name: "computer".into(),
                    images: vec![image.clone()],
                    ..Default::default()
                }),
                ChatMessage::new(ChatRole::Tool, "legacy result one"),
                ChatMessage {
                    is_error: true,
                    ..ChatMessage::new(ChatRole::Tool, "legacy result two")
                },
            ],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert_eq!(s.rows.len(), 4);
        assert!(s.rows[0].1.is_empty());
        assert_eq!(s.toolcards[0].as_ref().unwrap().name, "computer");
        assert!(s.toolcards[0].as_ref().unwrap().args.is_none());
        assert_eq!(s.rows[1].0, "attachments");
        assert_eq!(s.images, vec![image]);
        assert_eq!(s.rows[2].1, "legacy result one");
        assert_eq!(s.toolcards[2].as_ref().unwrap().state, ToolState::Done);
        assert_eq!(s.rows[3].1, "legacy result two");
        assert_eq!(s.toolcards[3].as_ref().unwrap().state, ToolState::Error);
    }

    #[test]
    fn history_pending_call_accepts_live_result_in_place() {
        let mut s = State::default();
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "pending".into(),
                    name: "shell".into(),
                    arguments: json!({"command": "sleep 2"}),
                }],
            )],
            snapshot: SessionSnapshot::default(),
            busy: true,
        });
        assert_eq!(s.rows.len(), 1);
        assert!(s.rows[0].1.is_empty(), "arguments are not result output");
        assert_eq!(s.toolcards[0].as_ref().unwrap().state, ToolState::Running);
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "pending".into(),
            name: "shell".into(),
            content: "finished".into(),
            is_error: false,
        });
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.rows[0].1, "finished");
        let card = s.toolcards[0].as_ref().unwrap();
        assert_eq!(card.state, ToolState::Done);
        assert_eq!(card.args, Some(json!({"command": "sleep 2"}).to_string()));
    }

    fn frontend_event(settings: serde_json::Value) -> RuntimeEvent {
        RuntimeEvent::FrontendState {
            banner: String::new(),
            settings,
            commands: vec![],
            tool_defs: vec![],
            tool_display: json!({}),
            subagents: vec![],
            host_api_version: 1,
            catalog_updates: 0,
            cwd: None,
        }
    }

    fn frontend_state(tool_display: serde_json::Value) -> RuntimeEvent {
        let mut event = frontend_event(json!({}));
        if let RuntimeEvent::FrontendState {
            tool_display: current,
            ..
        } = &mut event
        {
            *current = tool_display;
        }
        event
    }

    #[test]
    fn frontend_state_updates_the_display_map_and_marks_rows_for_relabel() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::ToolCall {
            id: "c1".into(),
            name: "grep".into(),
            summary: "searching".into(),
            arguments: json!({"query": "foo"}),
        });
        let _ = std::mem::take(&mut s.changed_rows);
        s.reduce(frontend_state(json!({
            "grep": { "template": "search {query}" }
        })));
        assert_eq!(
            s.tool_display["grep"].template.as_deref(),
            Some("search {query}")
        );
        let i = s.rows.len() - 1;
        assert!(s.changed_rows.contains(&i), "rendered rows relabel");
        let card = s.toolcards[i].as_ref().unwrap();
        assert_eq!(card.args, Some(json!({"query": "foo"}).to_string()));
    }

    #[test]
    fn reasoning_is_gated_by_show_reasoning() {
        let mut s = loaded();

        // Default (config off): live reasoning is suppressed entirely.
        s.reduce(RuntimeEvent::ReasoningDelta {
            text: "hidden".into(),
        });
        assert!(
            s.live_reasoning.is_none(),
            "live reasoning hidden by default"
        );

        let with_reasoning = ChatMessage {
            reasoning: Some(bone_protocol::Reasoning {
                text: "old thought".into(),
                echo_field: None,
            }),
            ..ChatMessage::new(ChatRole::Assistant, "answer")
        };
        // Historical reasoning is dropped too.
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![with_reasoning.clone()],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert!(
            s.thinking.iter().all(Option::is_none),
            "historical reasoning hidden by default"
        );

        // Enabling surfaces both historical and live reasoning.
        s.reduce(RuntimeEvent::FrontendState {
            banner: String::new(),
            settings: json!({ "general": { "show_reasoning": true } }),
            commands: vec![],
            tool_defs: vec![],
            tool_display: json!({}),
            subagents: vec![],
            host_api_version: 1,
            catalog_updates: 0,
            cwd: None,
        });
        assert!(s.show_reasoning, "FrontendState drives the reasoning gate");
        s.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![with_reasoning],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        // Historical reasoning attaches to the row its message produced.
        let answer = s
            .rows
            .iter()
            .position(|row| row.0 == "assistant" && row.1 == "answer")
            .expect("assistant row produced by the message");
        assert_eq!(s.thinking[answer].as_deref(), Some("old thought"));

        // Live reasoning accumulates in the transient badge until the segment
        // settles it onto the produced row.
        s.reduce(RuntimeEvent::ReasoningDelta {
            text: "live thought".into(),
        });
        assert_eq!(s.live_reasoning.as_deref(), Some("live thought"));
        s.reduce(RuntimeEvent::TextDelta {
            text: "reply".into(),
        });
        assert!(s.live_reasoning.is_none(), "reasoning settles on the row");
        let reply = s
            .rows
            .iter()
            .position(|row| row.0 == "assistant" && row.1 == "reply")
            .expect("streamed assistant row");
        assert_eq!(s.thinking[reply].as_deref(), Some("live thought"));
    }

    #[test]
    fn changed_rows_tracks_mutations_and_is_drained() {
        let mut s = loaded(); // replace() touches every row it rebuilds
        let all: Vec<usize> = (0..s.rows.len()).collect();
        assert_eq!(s.changed_rows, all);
        // mem::take is how the renderer drains per frame; afterwards only new
        // mutations are recorded.
        let _ = std::mem::take(&mut s.changed_rows);
        assert!(s.changed_rows.is_empty());
        s.reduce(RuntimeEvent::TextDelta {
            text: " tail".into(),
        });
        let last = s.rows.len() - 1;
        assert_eq!(s.changed_rows, vec![last]);
        assert!(s.rows[last].1.ends_with(" tail"));
        // Streaming into a running tool row records that row each time.
        let _ = std::mem::take(&mut s.changed_rows);
        s.reduce(RuntimeEvent::ToolOutput {
            call_id: "t1".into(),
            content: "out".into(),
            stderr: false,
        });
        assert_eq!(s.changed_rows, vec![s.rows.len() - 1]);
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "t1".into(),
            name: "shell".into(),
            content: "final".into(),
            is_error: false,
        });
        assert_eq!(s.changed_rows.last(), Some(&(s.rows.len() - 1)));
        // reset_into wipes the log along with the rest of the state.
        s.reset(Some(9));
        assert!(s.changed_rows.is_empty());
        assert!(s.rows.is_empty());
    }

    #[test]
    fn history_reconstructs_authoritative_images_and_replaces_them() {
        let mut state = State::default();
        let image = ImageData {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
            width: Some(1),
            height: Some(1),
            sha256: None,
        };
        state.reduce(RuntimeEvent::ConversationLoaded {
            messages: vec![ChatMessage::user_with_images(
                "with image",
                vec![image.clone()],
            )],
            snapshot: SessionSnapshot::default(),
            busy: false,
        });
        assert_eq!(state.images, vec![image]);
        assert_eq!(state.rows.last().unwrap().1, "1 image(s)");
        state.reduce(RuntimeEvent::StateSynchronized {
            request_id: state.sync_id.unwrap(),
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: Some(vec![ChatMessage::new(ChatRole::User, "without image")]),
            theme: None,
        });
        assert!(state.images.is_empty());
    }

    #[test]
    fn last_error_tracks_failures_and_clears_on_recovery() {
        let mut s = loaded();
        assert!(s.last_error.is_none());
        s.reduce(RuntimeEvent::Failed {
            message: "boom".into(),
        });
        assert_eq!(s.last_error.as_deref(), Some("boom"));
        // Successful turn completion clears it.
        s.reduce(RuntimeEvent::TurnComplete);
        assert!(s.last_error.is_none());
        s.reduce(RuntimeEvent::Failed {
            message: "boom again".into(),
        });
        assert!(s.last_error.is_some());
        // A load failure surfaces while restoring an explicit conversation.
        let mut fresh = State::default();
        fresh.reset(Some(7));
        fresh.reduce(RuntimeEvent::ConversationLoadFailed {
            id: 7,
            message: "no such conversation".into(),
        });
        assert_eq!(fresh.last_error.as_deref(), Some("no such conversation"));
        assert_eq!(fresh.status, "Conversation unavailable");
        assert!(
            fresh.rows.is_empty(),
            "technical load errors stay out of the transcript"
        );
    }

    #[test]
    fn token_usage_and_work_elapsed_are_recorded() {
        let mut s = loaded();
        assert!(s.token_usage.is_none());
        s.reduce(RuntimeEvent::TokenUsage {
            sent: 10,
            received: 20,
            context_length: 4096,
        });
        assert_eq!(
            s.token_usage,
            Some(TokenUsage {
                sent: 10,
                received: 20,
                context_length: 4096
            })
        );
        s.reduce(RuntimeEvent::WorkElapsed { elapsed_ms: 1500 });
        assert_eq!(s.work_elapsed_ms, Some(1500));
        assert_eq!(
            s.rows.last().unwrap().1,
            "worked for 0:01",
            "end-of-turn elapsed notice"
        );
    }

    #[test]
    fn format_elapsed_ms_renders_minutes_and_seconds() {
        assert_eq!(format_elapsed_ms(0), "0:00");
        assert_eq!(format_elapsed_ms(1_500), "0:01");
        assert_eq!(format_elapsed_ms(65_000), "1:05");
        assert_eq!(format_elapsed_ms(3_600_000), "60:00");
    }

    #[test]
    fn view_snapshot_replaces_and_diffs_apply_in_place() {
        use bone_protocol::{Component, FloatRect, PaneLineSpec, StatusSegment, ViewDiff};
        let mut s = loaded();
        let status = |id: &str, text: &str| Component::StatusLine {
            id: id.into(),
            segments: vec![StatusSegment {
                text: text.into(),
                fg: None,
                align: Default::default(),
            }],
        };
        s.reduce(RuntimeEvent::ViewSnapshot {
            view: ViewModel {
                components: vec![status("a", "one")],
                highlights: HashMap::new(),
            },
        });
        assert_eq!(s.view.components.len(), 1);
        // Upsert replaces the component with the same id rather than appending.
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::Upsert {
                component: status("a", "two"),
            },
        });
        assert_eq!(s.view.components.len(), 1);
        // A new id appends.
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::Upsert {
                component: Component::Float {
                    presentation: bone_protocol::PanePresentation::Overlay,
                    id: "pane".into(),
                    title: "Pane".into(),
                    lines: vec![PaneLineSpec::Plain("hi".into())],
                    rect: FloatRect {
                        anchor: Default::default(),
                        width: 10,
                        height: 3,
                        col: 0,
                        row: 0,
                    },
                    z: 0,
                    border: true,
                    scroll: 0,
                    placement: None,
                    owner: None,
                },
            },
        });
        assert_eq!(s.view.components.len(), 2);
        // Highlight set/clear.
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::SetHighlight {
                name: "assistant".into(),
                fg: Some("red".into()),
            },
        });
        assert_eq!(s.view.highlights.get("assistant"), Some(&"red".into()));
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::SetHighlight {
                name: "assistant".into(),
                fg: None,
            },
        });
        assert!(!s.view.highlights.contains_key("assistant"));
        // Remove drops the component and its highlight.
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::Remove { id: "pane".into() },
        });
        assert_eq!(s.view.components.len(), 1);
        // Theme is stored opaquely.
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::SetTheme {
                theme: json!({"accent": "blue"}),
            },
        });
        assert_eq!(s.theme, Some(json!({"accent": "blue"})));
        // A malformed live payload cannot erase the last successfully decoded
        // theme.
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::SetTheme {
                theme: json!("not-an-object"),
            },
        });
        assert_eq!(s.theme, Some(json!({"accent": "blue"})));
    }

    #[test]
    fn synchronized_theme_restores_persisted_theme_and_legacy_payload_retains_it() {
        let mut s = loaded();
        let preview = json!({ "name": "ocean-preview" });
        let persisted = json!({ "name": "configured", "palette": { "accent": "#445566" } });
        s.reduce(RuntimeEvent::ViewDiff {
            diff: ViewDiff::SetTheme {
                theme: preview.clone(),
            },
        });
        assert_eq!(s.theme, Some(preview));

        let RuntimeCommand::Synchronize { request_id, .. } = s.synchronize() else {
            panic!("expected synchronization request");
        };
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id,
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: None,
            // This is the payload a `preview(nil)` cancellation restores.
            theme: Some(persisted.clone()),
        });
        assert_eq!(s.theme, Some(persisted.clone()));

        let RuntimeCommand::Synchronize { request_id, .. } = s.synchronize() else {
            panic!("expected second synchronization request");
        };
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id,
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: None,
            theme: None,
        });
        assert_eq!(s.theme, Some(persisted));
    }

    #[test]
    fn processes_and_jobs_ignore_stale_snapshots() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::ProcessesSnapshot {
            version: 2,
            processes: vec![],
        });
        assert_eq!(s.processes_version, 2);
        // Older version is ignored.
        s.reduce(RuntimeEvent::ProcessesSnapshot {
            version: 1,
            processes: vec![],
        });
        assert_eq!(s.processes_version, 2);
        s.reduce(RuntimeEvent::JobsSnapshot {
            version: 5,
            jobs: vec![],
        });
        assert_eq!(s.jobs_version, 5);
        s.reduce(RuntimeEvent::JobsSnapshot {
            version: 3,
            jobs: vec![],
        });
        assert_eq!(s.jobs_version, 5);
    }

    #[test]
    fn frontend_state_is_captured() {
        let mut s = loaded();

        s.reduce(RuntimeEvent::FrontendState {
            banner: "hi".into(),
            settings: json!({"a": 1}),
            commands: vec![("help".into(), "Show help".into())],
            tool_defs: vec![],
            tool_display: json!({}),
            subagents: vec![],
            host_api_version: 1,
            catalog_updates: 3,
            cwd: Some("/tmp".into()),
        });
        let fe = s.frontend.as_ref().unwrap();
        assert_eq!(
            fe.commands,
            vec![("help".to_string(), "Show help".to_string())]
        );
    }

    #[test]
    fn edit_preview_is_rendered_once_and_success_is_suppressed() {
        let mut s = loaded();
        let preview = "\nedit_file a.txt (-1 | +1)\n    1 - old\n    1 + new";
        let approval = RuntimeEvent::ApprovalRequest {
            id: 44,
            call_id: "edit-1".into(),
            name: "edit_file".into(),
            summary: "edit a file".into(),
            arguments: json!({"path": "a.txt"}),
            blocked: None,
            auto_allows: false,
            preview: Some(preview.into()),
        };

        s.reduce(RuntimeEvent::ToolCall {
            id: "edit-1".into(),
            name: "edit_file".into(),
            summary: "edit a file".into(),
            arguments: json!({"path": "a.txt"}),
        });

        assert!(s.reduce(approval.clone()).is_none());
        assert_eq!(
            s.rows.last(),
            Some(&("system".into(), preview.into())),
            "the daemon preview is a system diff row"
        );
        assert!(s.shown_tool_rows.contains("edit-1"));
        let row_count = s.rows.len();

        // Synchronization clears the pending-approval registry before replaying gates.
        s.approvals.clear();

        // Replayed approval events must not duplicate the preview.
        assert!(s.reduce(approval.clone()).is_none());
        assert_eq!(s.rows.len(), row_count);

        s.reduce(RuntimeEvent::ToolResult {
            call_id: "edit-1".into(),
            name: "edit_file".into(),
            content: "1#deadbe|new".into(),
            is_error: false,
        });
        assert_eq!(s.rows.len(), row_count);
        assert!(!s.rows.iter().any(|(_, content)| content == "1#deadbe|new"));
        assert!(!s.shown_tool_rows.contains("edit-1"));
        let history = vec![
            ChatMessage::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "edit-1".into(),
                    name: "edit_file".into(),
                    arguments: json!({"path": "a.txt"}),
                }],
            ),
            ChatMessage::tool(bone_protocol::ToolResult {
                call_id: "edit-1".into(),
                name: "edit_file".into(),
                content: "1#deadbe|new".into(),
                ..Default::default()
            }),
        ];
        let RuntimeCommand::Synchronize { request_id, .. } = s.synchronize() else {
            panic!("expected synchronization request");
        };
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id,
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: Some(history),
            theme: None,
        });
        assert_eq!(
            s.rows
                .iter()
                .filter(|(role, content)| role == "system" && content == preview)
                .count(),
            1,
            "the cached preview survives transcript rebuilding"
        );
        assert!(
            !s.rows.iter().any(|(_, content)| content == "1#deadbe|new"),
            "the persisted hashline result stays hidden"
        );
        let tool_row = s
            .rows
            .iter()
            .position(|(role, _)| role == "tool: edit_file")
            .expect("the tool call remains represented");
        assert_eq!(
            s.toolcards[tool_row].as_ref().unwrap().state,
            ToolState::Done
        );

        // A replayed approval after synchronization must not add a second row.
        let row_count = s.rows.len();
        assert!(s.reduce(approval).is_none());
        assert_eq!(s.rows.len(), row_count);
        assert_eq!(
            s.rows
                .iter()
                .filter(|(role, content)| role == "system" && content == preview)
                .count(),
            1
        );
    }

    #[test]
    fn failed_edit_and_non_preview_results_keep_tool_rows() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::ApprovalRequest {
            id: 45,
            call_id: "edit-error".into(),
            name: "edit_file".into(),
            summary: "edit a file".into(),
            arguments: json!({"path": "a.txt"}),
            blocked: None,
            auto_allows: false,
            preview: Some("\nedit_file a.txt (-1 | +1)".into()),
        });
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "edit-error".into(),
            name: "edit_file".into(),
            content: "permission denied".into(),
            is_error: true,
        });
        assert_eq!(s.rows.last().unwrap().0, "tool: edit_file (error)");
        assert_eq!(s.rows.last().unwrap().1, "permission denied");
        assert!(!s.shown_tool_rows.contains("edit-error"));

        s.reduce(RuntimeEvent::ToolCall {
            id: "shell-1".into(),
            name: "shell".into(),
            summary: "running".into(),
            arguments: json!({"command": "true"}),
        });
        s.reduce(RuntimeEvent::ToolResult {
            call_id: "shell-1".into(),
            name: "shell".into(),
            content: "exit code: 0".into(),
            is_error: false,
        });
        assert_eq!(s.rows.last().unwrap().0, "tool: shell");
        assert_eq!(s.rows.last().unwrap().1, "exit code: 0");
    }

    #[test]
    fn auto_allowed_approval_replies_once_without_becoming_pending() {
        let mut s = loaded();
        let approval = RuntimeEvent::ApprovalRequest {
            id: 41,
            call_id: "call-1".into(),
            name: "read_file".into(),
            summary: "read a file".into(),
            arguments: json!({"path": "README.md"}),
            blocked: None,
            auto_allows: true,
            preview: None,
        };

        let Some(RuntimeCommand::ApprovalReply { id, outcome }) = s.reduce(approval.clone()) else {
            panic!("expected automatic approval reply");
        };
        assert_eq!(id, 41);
        assert_eq!(outcome, CallOutcome::Approve);
        assert!(s.approvals.is_empty());
        assert!(s.reduce(approval).is_none(), "replay must not reply twice");
        assert!(s.approvals.is_empty());
    }

    #[test]
    fn approval_requiring_consent_remains_pending() {
        let mut s = loaded();
        assert!(
            s.reduce(RuntimeEvent::ApprovalRequest {
                id: 42,
                call_id: "call-2".into(),
                name: "shell".into(),
                summary: "run a command".into(),
                arguments: json!({"command": "touch marker"}),
                blocked: None,
                auto_allows: false,
                preview: None,
            })
            .is_none()
        );
        assert_eq!(s.approvals.len(), 1);
        assert_eq!(s.approvals[0].id, 42);
    }

    #[test]
    fn auto_allowed_but_hook_blocked_call_replies_blocked() {
        let mut s = loaded();
        let Some(RuntimeCommand::ApprovalReply { id, outcome }) =
            s.reduce(RuntimeEvent::ApprovalRequest {
                id: 43,
                call_id: "call-3".into(),
                name: "shell".into(),
                summary: "run a blocked command".into(),
                arguments: json!({"command": "forbidden"}),
                blocked: Some("blocked by hook".into()),
                auto_allows: true,
                preview: None,
            })
        else {
            panic!("expected automatic blocked reply");
        };
        assert_eq!(id, 43);
        assert_eq!(outcome, CallOutcome::Blocked("blocked by hook".into()));
        assert!(s.approvals.is_empty());
    }

    #[test]
    fn approval_reply_does_not_suppress_key_request_with_same_id() {
        let mut s = loaded();
        s.answered(0);
        s.reduce(RuntimeEvent::KeyRequest { id: 0 });
        assert_eq!(s.pending_key, Some(0));
        s.answer_key(0);
        s.reduce(RuntimeEvent::KeyRequest { id: 0 });
        assert!(s.pending_key.is_none(), "answered keys still ignore replay");
    }

    #[test]
    fn key_reply_does_not_mark_approval_with_same_id_answered() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::KeyRequest { id: 0 });
        s.answer_key(0);
        let approval = RuntimeEvent::ApprovalRequest {
            id: 0,
            call_id: "call-1".into(),
            name: "shell".into(),
            summary: "test approval".into(),
            arguments: serde_json::json!({}),
            blocked: None,
            auto_allows: false,
            preview: None,
        };
        s.reduce(approval.clone());
        assert_eq!(s.approvals.len(), 1);
        s.answered(0);
        s.reduce(approval);
        assert!(
            s.approvals.is_empty(),
            "answered approvals still ignore replay"
        );
    }

    #[test]
    fn conversation_reset_releases_pending_and_answered_keys() {
        for clear in [false, true] {
            let mut s = loaded();
            s.answer_key(0);
            s.reduce(RuntimeEvent::KeyRequest { id: 1 });
            if clear {
                s.clear_conversation();
            } else {
                s.reduce(RuntimeEvent::ConversationLoaded {
                    messages: Vec::new(),
                    snapshot: s.snapshot.clone(),
                    busy: false,
                });
            }
            assert!(s.pending_key.is_none());
            s.reduce(RuntimeEvent::KeyRequest { id: 0 });
            assert_eq!(s.pending_key, Some(0));
        }
    }

    #[test]
    fn key_request_sets_pending_and_answer_clears_it() {
        let mut s = loaded();
        assert!(s.pending_key.is_none());
        s.reduce(RuntimeEvent::KeyRequest { id: 41 });
        assert_eq!(s.pending_key, Some(41));
        assert!(s.busy);
        // A duplicate request for the same id must not re-arm it.
        s.reduce(RuntimeEvent::KeyRequest { id: 41 });
        assert_eq!(s.pending_key, Some(41));
        // Replying clears the slot and records the id so a replayed request is
        // ignored.
        s.answer_key(41);
        assert!(s.pending_key.is_none());
        s.reduce(RuntimeEvent::KeyRequest { id: 41 });
        assert!(s.pending_key.is_none());
    }
}
