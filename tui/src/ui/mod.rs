//! Terminal UI (feature `ui`): ratatui app, rendering, input, panes, and commands.

pub mod app;
pub mod autocomplete;
pub mod catalog;
pub mod color;
pub mod commands;
pub mod fullscreen;
pub mod input;
pub mod jobs_pane;
pub mod pane_page;
pub mod picker;
pub mod process_view;
pub mod processes_pane;
pub mod prompt;
pub mod queue_pane;
pub mod render;
pub(crate) mod selectable_pane;
pub mod setup;
pub mod stats;
pub mod theme;
pub mod tool_display;
pub mod transcript_view;
