//! The ViewModel — UI as data.
//!
//! Wire-format types are re-exported from `bone-protocol`; only
//! [`ViewModel`] (which has a `HashMap` of in-memory components) stays
//! core-local.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-export wire-format types from protocol.
pub use bone_protocol::view::{
    Align, Anchor, Component, FloatRect, PaneContent, PaneLineSpec, StatusSegment, ViewDiff,
    view_diff_from_pane_content,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ViewModel {
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub highlights: HashMap<String, String>,
}

impl ViewModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one diff into the model. Upserts preserve component order (an
    /// existing id is replaced in place; a new id is appended).
    pub fn apply(&mut self, diff: &ViewDiff) {
        match diff {
            ViewDiff::Upsert { component } => {
                if let Some(slot) = self
                    .components
                    .iter_mut()
                    .find(|c| c.id() == component.id())
                {
                    *slot = component.clone();
                } else {
                    self.components.push(component.clone());
                }
            }
            ViewDiff::Remove { id } => {
                self.components.retain(|c| c.id() != id);
            }
            ViewDiff::SetHighlight { name, fg } => match fg {
                Some(color) => {
                    self.highlights.insert(name.clone(), color.clone());
                }
                None => {
                    self.highlights.remove(name);
                }
            },
        }
    }

    /// Find a component by id.
    pub fn get(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|c| c.id() == id)
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod view_tests;
