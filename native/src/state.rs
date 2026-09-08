//! Small, renderer-independent projection of the authoritative daemon stream.
use std::collections::{HashMap, HashSet};

use bone_protocol::{ChatMessage, ChatRole, RuntimeCommand, RuntimeEvent, SessionSnapshot};

#[derive(Default)]
pub struct State {
    pub rows: Vec<(String, String)>,
    /// Parallel to `rows`: tool-card overlay state for tool rows (`None` for
    /// ordinary text rows). Every row append goes through [`Self::push_row`]
    /// so the two vectors stay index-aligned.
    pub toolcards: Vec<Option<ToolCard>>,
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
    /// A fresh socket always replays the default actor's `conversation_loaded`
    /// first. A New tab skips exactly that one and lets its own conversation
    /// (created via `NewConversation`) load next.
    pub ignore_first_load: bool,
    assistant: Option<usize>,
    reasoning: Option<usize>,
    tools: HashMap<String, usize>,
    answered: HashSet<u64>,
    sync_id: Option<u64>,
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
    pub args: Option<String>,
}

pub struct Approval {
    pub id: u64,
    pub name: String,
    pub summary: String,
    pub preview: Option<String>,
    pub blocked: Option<String>,
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
            ..Self::default()
        };
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id = self.next_id.wrapping_add(1);
        self.next_id
    }

    pub fn synchronize(&mut self) -> RuntimeCommand {
        let request_id = self.next_id();
        self.sync_id = Some(request_id);
        RuntimeCommand::Synchronize {
            request_id,
            include_messages: true,
        }
    }

    pub fn answered(&mut self, id: u64) {
        self.answered.insert(id);
        self.approvals.retain(|a| a.id != id);
    }

    pub fn needs_approval(&self) -> bool {
        !self.approvals.is_empty()
    }

    /// Best tab label: first user prompt, whitespace-collapsed and truncated;
    /// falls back to the conversation id, then "New conversation".
    pub fn short_title(&self) -> String {
        let mut title = String::new();
        for (role, text) in &self.rows {
            if role != "user" {
                continue;
            }
            title = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !title.is_empty() {
                break;
            }
        }
        if title.is_empty() {
            return match self.snapshot.conversation_id {
                Some(id) => format!("Conversation {id}"),
                None => "New conversation".into(),
            };
        }
        let mut shortened: String = title.chars().take(46).collect();
        if shortened != title {
            shortened.push('…');
        }
        shortened
    }

    /// Every append to `rows` must go through here so `toolcards` (parallel to
    /// `rows`) keeps its indexes aligned with `rows`.
    pub(crate) fn push_row(&mut self, role: impl Into<String>, text: impl Into<String>) -> usize {
        let i = self.rows.len();
        self.rows.push((role.into(), text.into()));
        self.toolcards.push(None);
        self.changed_rows.push(i);
        i
    }

    fn replace(&mut self, messages: Vec<ChatMessage>) {
        self.rows.clear();
        self.toolcards.clear();
        self.assistant = None;
        self.reasoning = None;
        self.tools.clear();
        for message in messages {
            if message.role == ChatRole::System {
                continue;
            }
            if let Some(reasoning) = message.reasoning {
                self.push_row("reasoning", reasoning.text);
            }
            if !message.content.is_empty() {
                self.push_row(message.role.as_str(), message.content);
            }
            for call in message.tool_calls {
                let args = (!call.arguments.is_null()).then(|| call.arguments.to_string());
                let i = self.push_row(format!("tool: {}", call.name), call.arguments.to_string());
                // History replays finished calls: card is Done with its args.
                self.toolcards[i] = Some(ToolCard {
                    name: call.name,
                    state: ToolState::Done,
                    args,
                });
            }
            if !message.images.is_empty() {
                self.push_row(
                    "attachments",
                    format!(
                        "{} image(s); image display is not implemented yet",
                        message.images.len()
                    ),
                );
            }
        }
    }

    fn delta(&mut self, text: String, reasoning: bool) {
        let existing = if reasoning {
            self.reasoning
        } else {
            self.assistant
        };
        let (row, created) = match existing {
            Some(row) => (row, false),
            None => {
                let row = self.push_row(
                    if reasoning { "reasoning" } else { "assistant" },
                    String::new(),
                );
                if reasoning {
                    self.reasoning = Some(row);
                } else {
                    self.assistant = Some(row);
                }
                (row, true)
            }
        };
        self.rows[row].1.push_str(&text);
        if !created {
            // push_row already recorded a brand-new row; only appending to an
            // existing row is a mutation that needs a fresh record here.
            self.changed_rows.push(row);
        }
    }

    /// Returns the tool row's index and whether this call created it. The tool
    /// events share one row per call id so a stream stays in place.
    fn tool_row(&mut self, id: String, name: String) -> (usize, bool) {
        if let Some(&row) = self.tools.get(&id) {
            return (row, false);
        }
        let row = self.push_row(format!("tool: {name}"), String::new());
        self.tools.insert(id, row);
        (row, true)
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
                    self.status = format!(
                        "Load failed: {message}. Disconnect to choose another conversation."
                    );
                    self.last_error = Some(message.clone());
                    return None;
                }
                _ => return None,
            }
        }
        match event {
            RuntimeEvent::ConversationLoaded {
                messages,
                snapshot,
                busy,
            } => {
                self.replace(messages);
                self.snapshot = snapshot;
                self.busy = busy;
                self.ready = true;
                self.approvals.clear();
                self.answered.clear();
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
                ..
            } if self.sync_id == Some(request_id) => {
                self.sync_id = None;
                self.last_error = None;
                if let Some(messages) = messages {
                    self.replace(messages);
                }
                self.snapshot = snapshot;
                self.busy = busy;
                self.repairing = busy;
                self.approvals.clear(); // authoritative replay follows this event
                if !busy {
                    self.status = "Ready".into();
                }
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
                self.reasoning = None;
                self.tools.clear();
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
                self.reasoning = None;
                let (i, created) = self.tool_row(id, name.clone());
                self.rows[i].1 = summary;
                if !created {
                    self.changed_rows.push(i);
                }
                let args = (!arguments.is_null()).then(|| arguments.to_string());
                self.toolcards[i] = Some(ToolCard {
                    name,
                    state: ToolState::Running,
                    args,
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
                let (i, created) = self.tool_row(call_id, name.clone());
                self.rows[i] = (
                    format!("tool: {name}{}", if is_error { " (error)" } else { "" }),
                    content,
                );
                if !created {
                    self.changed_rows.push(i);
                }
                let args = self.toolcards[i]
                    .as_ref()
                    .and_then(|card| card.args.clone());
                self.toolcards[i] = Some(ToolCard {
                    name,
                    state: if is_error {
                        ToolState::Error
                    } else {
                        ToolState::Done
                    },
                    args,
                });
            }
            RuntimeEvent::Finished { content } => {
                // Finished is the final full response, not another delta.
                if let Some(i) = self.assistant {
                    self.rows[i].1 = content;
                    self.changed_rows.push(i);
                } else if !content.is_empty() {
                    self.push_row("assistant", content);
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
                return Some(self.synchronize());
            }
            RuntimeEvent::Failed { message } => {
                self.status = format!("Failed: {message}");
                self.last_error = Some(message);
                self.approvals.clear();
                self.busy = false;
            }
            RuntimeEvent::ApprovalRequest {
                id,
                name,
                summary,
                preview,
                blocked,
                ..
            } => {
                if !self.answered.contains(&id) && !self.approvals.iter().any(|a| a.id == id) {
                    self.approvals.push(Approval {
                        id,
                        name,
                        summary,
                        preview,
                        blocked,
                    });
                }
            }
            RuntimeEvent::KeyRequest { .. } => {
                self.status =
                    "Interactive key input is not supported yet; use Cancel or another frontend"
                        .into();
                self.busy = true;
            }
            RuntimeEvent::StreamLagged { .. } => {
                self.repairing = true;
                self.status = "Repairing missed events…".into();
                return Some(self.synchronize());
            }
            RuntimeEvent::Status { message } | RuntimeEvent::Notice { message } => {
                self.status = message
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
    fn snapshot_replaces_history_and_ignores_other_sync_ids() {
        let mut s = loaded();
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id: 999,
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: Some(vec![]),
        });
        assert_eq!(s.rows.len(), 1);
        s.reduce(RuntimeEvent::StateSynchronized {
            request_id: s.sync_id.unwrap(),
            busy: false,
            snapshot: SessionSnapshot::default(),
            view: None,
            messages: Some(vec![]),
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
        assert_eq!(s.short_title(), "first prompt");
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
        assert!(fresh.status.contains("no such conversation"));
    }
}
