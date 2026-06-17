//! The ViewModel — UI as data.
//!
//! Generalizes `PaneContent` (a single pane) into a small tree of
//! frontend-agnostic components the core owns, Lua mutates, and frontends
//! render. This is the abstraction that makes the UI both Lua-drawable (Lua
//! emits [`ViewDiff`]s) and remoteable (diffs serialize over the Phase 5
//! transport). The built-in TUI renders a `ViewModel` by converting each
//! component to its existing ratatui widgets — e.g. a [`Component::Float`]
//! becomes a `PanePage` via [`Component::as_pane_content`].
//!
//! Pure data: no ratatui, no `crate::ui`. Compiles with `--no-default-features`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::pane_content::{PaneContent, PaneLineSpec};

/// Where a float is anchored before applying its `col`/`row` offset.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    #[default]
    Center,
}

/// Placement of a floating window. `width`/`height` are columns/rows; `col`/
/// `row` are signed offsets from the anchor (so a frontend can nudge floats).
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FloatRect {
    #[serde(default)]
    pub anchor: Anchor,
    pub width: u16,
    pub height: u16,
    #[serde(default)]
    pub col: i16,
    #[serde(default)]
    pub row: i16,
}

/// Horizontal alignment of a status-line segment.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// One segment of a status line.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusSegment {
    pub text: String,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub align: Align,
}

/// A renderable component in the view tree. Every component has a stable string
/// `id` so [`ViewDiff::Upsert`] can replace it in place and [`ViewDiff::Remove`]
/// can drop it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Component {
    /// A floating window of styled text (reuses `PaneLineSpec` content).
    Float {
        id: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        lines: Vec<PaneLineSpec>,
        rect: FloatRect,
        #[serde(default)]
        z: i32,
        #[serde(default)]
        border: bool,
        #[serde(default)]
        scroll: usize,
    },
    /// A status line composed of aligned segments.
    StatusLine {
        id: String,
        #[serde(default)]
        segments: Vec<StatusSegment>,
    },
}

impl Component {
    pub fn id(&self) -> &str {
        match self {
            Component::Float { id, .. } | Component::StatusLine { id, .. } => id,
        }
    }

    /// Build a `Component::Float` from the equivalent `PaneContent` fields.
    /// Used by the channel transport (`ctx.ui.pane`) to emit the unified
    /// `ViewDiff` type without a separate pane-specific variant.
    pub fn float_from_pane_content(pc: &PaneContent) -> Component {
        Component::Float {
            id: pc.source.clone(),
            title: pc.title.clone(),
            lines: pc.lines.clone(),
            rect: FloatRect {
                anchor: Anchor::default(),
                width: 0,
                height: pc.visible_rows.max(1) as u16,
                col: 0,
                row: 0,
            },
            z: 0,
            border: false,
            scroll: pc.scroll,
        }
    }

    /// Render a `Float` as `PaneContent` so the TUI can display it with its
    /// existing pane machinery (`PanePage::from_content`). Non-float components
    /// return `None`.
    pub fn as_pane_content(&self) -> Option<PaneContent> {
        match self {
            Component::Float {
                id,
                title,
                lines,
                rect,
                scroll,
                ..
            } => Some(PaneContent {
                source: id.clone(),
                title: title.clone(),
                lines: lines.clone(),
                visible_rows: rect.height.max(1) as usize,
                scroll: *scroll,
            }),
            Component::StatusLine { .. } => None,
        }
    }
}

/// Convert a `PaneContent` into the `ViewDiff` that carries the same meaning:
/// empty lines signal removal (matching `PaneContent::is_empty()`), anything
/// else is an upsert of a `Float` built from the pane content.
pub fn view_diff_from_pane_content(pc: PaneContent) -> ViewDiff {
    if pc.is_empty() {
        ViewDiff::Remove { id: pc.source }
    } else {
        ViewDiff::Upsert {
            component: Component::float_from_pane_content(&pc),
        }
    }
}

/// An incremental change to a [`ViewModel`]. The unit of view update that flows
/// from Lua (or a remote client) into the core view, and on to frontends.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewDiff {
    /// Insert a component, or replace the existing one with the same id.
    Upsert { component: Component },
    /// Remove the component with this id (no-op if absent).
    Remove { id: String },
    /// Set or clear a named highlight group (a color, e.g. `"#ff0000"`).
    SetHighlight { name: String, fg: Option<String> },
}

/// The frontend-agnostic UI state: an ordered set of components plus named
/// highlight groups. Frontends render it; [`ViewModel::apply`] is the canonical
/// reducer so every frontend folds diffs identically.
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
