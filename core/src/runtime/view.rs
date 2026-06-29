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
mod tests {
    use super::*;
    use serde_json::json;

    fn float(id: &str, lines: Vec<PaneLineSpec>) -> Component {
        Component::Float {
            id: id.into(),
            title: "t".into(),
            lines,
            rect: FloatRect {
                anchor: Anchor::Center,
                width: 40,
                height: 10,
                col: 0,
                row: 0,
            },
            z: 0,
            border: true,
            scroll: 0,
        }
    }

    #[test]
    fn upsert_replaces_in_place_and_preserves_order() {
        let mut vm = ViewModel::new();
        vm.apply(&ViewDiff::Upsert {
            component: float("a", vec![PaneLineSpec::Plain("one".into())]),
        });
        vm.apply(&ViewDiff::Upsert {
            component: float("b", vec![]),
        });
        // Re-upsert "a" with new content — must replace, not append.
        vm.apply(&ViewDiff::Upsert {
            component: float("a", vec![PaneLineSpec::Plain("two".into())]),
        });

        assert_eq!(vm.components.len(), 2);
        assert_eq!(vm.components[0].id(), "a");
        assert_eq!(vm.components[1].id(), "b");
        let pc = vm.get("a").unwrap().as_pane_content().unwrap();
        assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "two"));
    }

    #[test]
    fn remove_drops_component() {
        let mut vm = ViewModel::new();
        vm.apply(&ViewDiff::Upsert {
            component: float("a", vec![]),
        });
        vm.apply(&ViewDiff::Remove { id: "a".into() });
        assert!(vm.get("a").is_none());
        // Removing an absent id is a no-op.
        vm.apply(&ViewDiff::Remove { id: "ghost".into() });
    }

    #[test]
    fn set_highlight_sets_and_clears() {
        let mut vm = ViewModel::new();
        vm.apply(&ViewDiff::SetHighlight {
            name: "error".into(),
            fg: Some("#ff0000".into()),
        });
        assert_eq!(
            vm.highlights.get("error").map(String::as_str),
            Some("#ff0000")
        );
        vm.apply(&ViewDiff::SetHighlight {
            name: "error".into(),
            fg: None,
        });
        assert!(!vm.highlights.contains_key("error"));
    }

    #[test]
    fn view_diff_round_trips_serde() {
        let diffs = vec![
            ViewDiff::Upsert {
                component: float("a", vec![PaneLineSpec::Plain("x".into())]),
            },
            ViewDiff::Upsert {
                component: Component::StatusLine {
                    id: "status".into(),
                    segments: vec![StatusSegment {
                        text: "ready".into(),
                        fg: Some("green".into()),
                        align: Align::Right,
                    }],
                },
            },
            ViewDiff::Remove { id: "a".into() },
            ViewDiff::SetHighlight {
                name: "h".into(),
                fg: Some("blue".into()),
            },
        ];
        for d in &diffs {
            let s = serde_json::to_string(d).unwrap();
            let back: ViewDiff = serde_json::from_str(&s).unwrap();
            assert_eq!(
                serde_json::to_value(d).unwrap(),
                serde_json::to_value(&back).unwrap()
            );
        }
    }

    #[test]
    fn float_component_parses_from_lua_style_json() {
        // Shape a Lua `open_float` opts table would serialize to.
        let val = json!({
            "kind": "float",
            "id": "help",
            "title": "Help",
            "lines": ["line one", {"spans": [{"text": "bold", "modifiers": ["bold"]}]}],
            "rect": {"anchor": "center", "width": 50, "height": 12},
            "z": 5,
            "border": true
        });
        let comp: Component = serde_json::from_value(val).unwrap();
        assert_eq!(comp.id(), "help");
        let pc = comp.as_pane_content().unwrap();
        assert_eq!(pc.lines.len(), 2);
        assert_eq!(pc.visible_rows, 12);
    }

    #[test]
    fn float_scroll_round_trips_into_pane_content() {
        let comp = Component::Float {
            id: "scroller".into(),
            title: "t".into(),
            lines: vec![PaneLineSpec::Plain("x".into())],
            rect: FloatRect {
                anchor: Anchor::Center,
                width: 40,
                height: 10,
                col: 0,
                row: 0,
            },
            z: 0,
            border: true,
            scroll: 7,
        };
        let pc = comp.as_pane_content().unwrap();
        assert_eq!(pc.scroll, 7);

        // A Float omitting `scroll` (e.g. from old Lua) defaults to 0.
        let val = json!({
            "kind": "float",
            "id": "default",
            "rect": {"width": 10, "height": 4}
        });
        let comp: Component = serde_json::from_value(val).unwrap();
        assert_eq!(comp.as_pane_content().unwrap().scroll, 0);
    }
}
