pub mod bash;
pub mod edit_file;
pub mod handler;
pub mod read_file;
pub mod registry;
pub mod types;
pub mod write_file;

use registry::ToolRegistry;

pub use handler::ToolHandler;
pub use types::{ApprovalMode, ToolCall, ToolDefinition, ToolResult};

pub fn builtin_tools() -> ToolRegistry {
    ToolRegistry::new()
        .register(read_file::ReadFileTool)
        .register(write_file::WriteFileTool)
        .register(edit_file::EditFileTool)
        .register(bash::BashTool)
}
