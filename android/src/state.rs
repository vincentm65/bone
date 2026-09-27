//! Folds daemon events into what the phone UI shows: the conversation's rows,
//! turn/approval state, and the conversation list.
//!
//! Deliberately smaller than the desktop reducer (`native/src/state.rs`): text,
//! tool rows, and approvals only. Every method that needs to talk back to the
//! daemon returns the commands to send, so the reducer stays pure and testable.

use bone_protocol::tools::CallOutcome;
use bone_protocol::{
    ChatMessage, ChatRole, ConversationMeta, HostRequest, HostResponse, KeyEvent, RuntimeCommand,
    RuntimeEvent,
};

/// Newest display messages requested when a conversation opens.
pub const LOAD_WINDOW: u32 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    User,
    Assistant,
    Tool,
    Note,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub text: String,
    /// Tool call id, to attach the result's outcome.
    pub call_id: Option<String>,
    /// A tool row whose result was an error.
    pub failed: bool,
}

impl Row {
    fn new(kind: RowKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            call_id: None,
            failed: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub id: u64,
    pub name: String,
    pub summary: String,
    /// Set when policy blocks the call; it can only be denied.
    pub blocked: Option<String>,
}

#[derive(Debug, Default)]
pub struct State {
    pub rows: Vec<Row>,
    pub busy: bool,
    /// Pending tool approvals, oldest first.
    pub approvals: Vec<Approval>,
    pub conversations: Vec<ConversationMeta>,
    pub conversation_id: Option<i64>,
    pub model: String,
    /// Latest one-line daemon status.
    pub status: String,
    /// Row index of the assistant reply still being streamed.
    assistant: Option<usize>,
    next_request_id: u64,
    conversations_request: Option<u64>,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.next_request_id += 1;
        self.next_request_id
    }

    /// Ask the daemon for the recent-conversations list.
    pub fn request_conversations(&mut self) -> RuntimeCommand {
        let request_id = self.next_id();
        self.conversations_request = Some(request_id);
        RuntimeCommand::HostRequest {
            request_id,
            request: HostRequest::Conversations { limit: 0 },
        }
    }

    /// Open a stored conversation; the rows arrive as `ConversationLoaded`.
    pub fn open(&mut self, id: i64) -> RuntimeCommand {
        RuntimeCommand::LoadConversation {
            id,
            window: Some(LOAD_WINDOW),
        }
    }

    /// Send a prompt. The user row appears when the daemon echoes `Started`.
    pub fn submit(&mut self, text: &str) -> Option<RuntimeCommand> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        Some(RuntimeCommand::SubmitPrompt {
            request_id: Some(self.next_id()),
            text: text.to_string(),
            images: Vec::new(),
        })
    }

    /// Answer the oldest pending approval.
    pub fn answer(&mut self, approve: bool) -> Option<RuntimeCommand> {
        if self.approvals.is_empty() {
            return None;
        }
        let approval = self.approvals.remove(0);
        let outcome = if approve && approval.blocked.is_none() {
            CallOutcome::Approve
        } else {
            CallOutcome::Denied
        };
        Some(RuntimeCommand::ApprovalReply {
            id: approval.id,
            outcome,
        })
    }

    /// Fold one daemon event; returns commands to send in reply.
    pub fn apply(&mut self, event: RuntimeEvent) -> Vec<RuntimeCommand> {
        match event {
            RuntimeEvent::ConversationLoaded {
                messages,
                snapshot,
                busy,
            } => {
                self.rows = rows_from_messages(&messages);
                self.assistant = None;
                self.approvals.clear();
                self.busy = busy;
                self.conversation_id = snapshot.conversation_id;
                self.model = snapshot.provider_model;
                return vec![self.request_conversations()];
            }
            RuntimeEvent::ConversationLoadFailed { message, .. } => {
                self.rows.push(Row::new(RowKind::Error, message));
            }
            RuntimeEvent::StateSnapshot { snapshot } => {
                self.conversation_id = snapshot.conversation_id;
                if !snapshot.provider_model.is_empty() {
                    self.model = snapshot.provider_model;
                }
            }
            RuntimeEvent::Started {
                task,
                display,
                model,
                ..
            } => {
                self.rows
                    .push(Row::new(RowKind::User, display.unwrap_or(task)));
                self.assistant = None;
                self.busy = true;
                if !model.is_empty() {
                    self.model = model;
                }
            }
            RuntimeEvent::TextDelta { text } => match self.assistant {
                Some(index) => self.rows[index].text.push_str(&text),
                None => {
                    self.rows.push(Row::new(RowKind::Assistant, text));
                    self.assistant = Some(self.rows.len() - 1);
                }
            },
            RuntimeEvent::ToolCall {
                id, name, summary, ..
            } => {
                self.assistant = None;
                let mut row = Row::new(
                    RowKind::Tool,
                    format!("{name} {summary}").trim().to_string(),
                );
                row.call_id = Some(id);
                self.rows.push(row);
            }
            RuntimeEvent::ToolResult {
                call_id, is_error, ..
            } => {
                if let Some(row) = self
                    .rows
                    .iter_mut()
                    .rev()
                    .find(|row| row.call_id.as_deref() == Some(call_id.as_str()))
                {
                    row.failed = is_error;
                }
            }
            RuntimeEvent::ApprovalRequest {
                id,
                name,
                summary,
                blocked,
                ..
            } => self.approvals.push(Approval {
                id,
                name,
                summary,
                blocked,
            }),
            // This client has no menu UI. Answer Esc at once: a Lua command
            // left waiting for a key holds the daemon's Lua VM.
            RuntimeEvent::KeyRequest { id } => {
                return vec![RuntimeCommand::KeyReply {
                    id,
                    key: KeyEvent {
                        code: "Esc".into(),
                        char: None,
                        ctrl: false,
                        alt: false,
                        shift: false,
                    },
                }];
            }
            // The final full response, not another delta.
            RuntimeEvent::Finished { content } => match self.assistant {
                Some(index) => self.rows[index].text = content,
                None if !content.is_empty() => {
                    self.rows.push(Row::new(RowKind::Assistant, content));
                }
                None => {}
            },
            RuntimeEvent::Failed { message } => {
                self.rows.push(Row::new(RowKind::Error, message));
                self.busy = false;
                self.approvals.clear();
            }
            RuntimeEvent::TurnCompleted { .. } | RuntimeEvent::TurnComplete => {
                self.busy = false;
                self.assistant = None;
                self.approvals.clear();
                self.status.clear();
                return vec![self.request_conversations()];
            }
            RuntimeEvent::Notice { message } => self.rows.push(Row::new(RowKind::Note, message)),
            RuntimeEvent::Status { message } => self.status = message,
            RuntimeEvent::HostResponse {
                request_id,
                response: HostResponse::Conversations(conversations),
            } if self.conversations_request == Some(request_id) => {
                self.conversations_request = None;
                self.conversations = conversations;
            }
            // Missed events: reload the authoritative transcript.
            RuntimeEvent::StreamLagged { .. } => {
                if let Some(id) = self.conversation_id {
                    return vec![self.open(id)];
                }
            }
            _ => {}
        }
        Vec::new()
    }
}

/// Display rows for a loaded transcript: prompts, replies, and one row per
/// tool call; tool results only mark their call's row.
fn rows_from_messages(messages: &[ChatMessage]) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for message in messages {
        match message.role {
            ChatRole::User if message.synthetic => {
                rows.push(Row::new(RowKind::Note, message.content.clone()));
            }
            ChatRole::User => rows.push(Row::new(RowKind::User, message.content.clone())),
            ChatRole::Assistant => {
                if !message.content.trim().is_empty() {
                    rows.push(Row::new(RowKind::Assistant, message.content.clone()));
                }
                for call in &message.tool_calls {
                    let mut row = Row::new(RowKind::Tool, call.name.clone());
                    row.call_id = Some(call.id.clone());
                    rows.push(row);
                }
            }
            ChatRole::Tool => {
                if let Some(row) = rows
                    .iter_mut()
                    .rev()
                    .find(|row| row.call_id.is_some() && row.call_id == message.tool_call_id)
                {
                    row.failed = message.is_error;
                }
            }
            ChatRole::System => {}
        }
    }
    rows
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
