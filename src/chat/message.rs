use crate::llm::{ChatMessage, ChatRole};

/// A single chat message.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: ChatRole,
    pub content: String,
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: ChatRole::User, content: content.into() }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: ChatRole::Assistant, content: content.into() }
    }

    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: ChatRole::System, content: content.into() }
    }

    #[must_use]
    pub fn to_chat_message(&self) -> ChatMessage {
        ChatMessage::new(self.role, self.content.clone())
    }
}
