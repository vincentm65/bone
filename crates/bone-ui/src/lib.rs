//! Client UI state shared by the bone frontends: everything that is not
//! drawing. No terminal or windowing dependency; frontends convert these
//! values for their renderer.

pub mod chat;
pub mod commands;
pub mod composer;
pub mod data;
pub mod editor;
pub mod keymap;
pub mod keys;
pub mod layout;
pub mod lua;
pub mod markdown;
pub mod model;
pub mod options;
pub mod popup;
pub mod prompt;
pub mod shellhl;
pub mod style;
pub mod term;
pub mod text;
pub mod theme;
