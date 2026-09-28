//! Transcript rendering shared by the TUI and the desktop client: Markdown,
//! tool rows and previews, wrapping, and the color theme, producing styled
//! terminal lines. Frontends only decide how to paint those lines.

pub mod ansi;
pub mod approval;
pub mod clipboard;
pub mod color;
pub mod editor;
pub mod markdown;
pub mod messages;
pub mod panes;
pub mod prompt;
pub mod screens;
pub mod status;
pub mod theme;
pub mod timing;
pub mod tool_display;
pub mod transcript;
pub mod wrap;

use bone_protocol::ChatRole;

/// Display metadata for compact tool rows shown in chat.
#[derive(Debug, Clone)]
pub struct ToolDisplay {
    pub label: String,
    pub is_error: bool,
    pub is_shell: bool,
}

/// A single rendered chat row.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: ChatRole,
    pub content: String,
    /// Present when this message represents a tool call or result.
    pub tool: Option<ToolDisplay>,
    pub image_count: usize,
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self::with_role(ChatRole::User, content)
    }

    #[must_use]
    pub fn user_with_images(content: impl Into<String>, image_count: usize) -> Self {
        Self {
            image_count,
            ..Self::user(content)
        }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::with_role(ChatRole::Assistant, content)
    }

    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self::with_role(ChatRole::System, content)
    }

    #[must_use]
    pub fn tool_row(label: String, is_error: bool) -> Self {
        Self {
            tool: Some(ToolDisplay {
                label,
                is_error,
                is_shell: false,
            }),
            ..Self::with_role(ChatRole::Tool, "")
        }
    }

    fn with_role(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool: None,
            image_count: 0,
        }
    }
}

static WARN_HOOK: std::sync::OnceLock<fn(String)> = std::sync::OnceLock::new();

/// Route theme warnings (unknown highlight groups, invalid colors) to the
/// host's warning channel. Without a hook they go to stderr.
pub fn set_warn_hook(hook: fn(String)) {
    let _ = WARN_HOOK.set(hook);
}

/// Report `message` once per process.
pub fn warn_once(message: impl Into<String>) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let message = message.into();
    let first = WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(message.clone());
    if first {
        match WARN_HOOK.get() {
            Some(hook) => hook(message),
            None => eprintln!("{message}"),
        }
    }
}
