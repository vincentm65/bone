//! View model types: components, diffs, and pane content for the wire protocol.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Helpers ────────────────────────────────────────────────────────────────

fn deserialize_vec_or_empty_map<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let val = serde_json::Value::deserialize(d)?;
    match val {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(arr) => Ok(arr
            .into_iter()
            .filter_map(|v| serde_json::from_value::<T>(v).ok())
            .collect()),
        serde_json::Value::Object(m) if m.is_empty() => Ok(Vec::new()),
        other => Err(serde::de::Error::custom(format!(
            "expected array, got {}",
            other
        ))),
    }
}

fn default_visible_rows() -> usize {
    8
}

// ── Pane primitives ────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSpanSpec {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default, deserialize_with = "deserialize_vec_or_empty_map")]
    pub modifiers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaneLineSpec {
    Plain(String),
    Spans {
        #[serde(default, deserialize_with = "deserialize_vec_or_empty_map")]
        spans: Vec<PaneSpanSpec>,
        #[serde(default)]
        bg: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PanelSlot {
    Left,
    Right,
    Bottom,
    Top,
    Overlay,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanelPlacement {
    pub slot: PanelSlot,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub size_hint: Option<u16>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default = "default_closable")]
    pub closable: bool,
}

fn default_closable() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneContent {
    pub source: String,
    pub title: String,
    #[serde(default, deserialize_with = "deserialize_vec_or_empty_map")]
    pub lines: Vec<PaneLineSpec>,
    #[serde(default = "default_visible_rows")]
    pub visible_rows: usize,
    #[serde(default)]
    pub scroll: usize,
    /// Optional semantic placement. Older producers omit this and retain the
    /// frontend's existing default placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<PanelPlacement>,
    /// Extension that owns this panel, used for lifecycle cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

impl PaneContent {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn from_json(val: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value::<PaneContent>(val.clone())
            .map_err(|e| format!("pane parse error: {e}"))
    }
}

// ── View components ────────────────────────────────────────────────────────

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

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusSegment {
    pub text: String,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default)]
    pub align: Align,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Component {
    Float {
        id: String,
        /// Pane envelopes participate in the chat layout; explicit floats overlay it.
        #[serde(default)]
        presentation: PanePresentation,
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
        /// Optional semantic placement for frontend layout managers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placement: Option<PanelPlacement>,
        /// Extension that owns this component.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
    },
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

    pub fn float_from_pane_content(pc: &PaneContent) -> Component {
        Component::Float {
            id: pc.source.clone(),
            presentation: PanePresentation::Live,
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
            placement: pc.placement.clone(),
            owner: pc.owner.clone(),
        }
    }

    pub fn as_pane_content(&self) -> Option<PaneContent> {
        match self {
            Component::Float {
                id,
                title,
                lines,
                rect,
                scroll,
                placement,
                owner,
                ..
            } => Some(PaneContent {
                source: id.clone(),
                title: title.clone(),
                lines: lines.clone(),
                visible_rows: rect.height.max(1) as usize,
                scroll: *scroll,
                placement: placement.clone(),
                owner: owner.clone(),
            }),
            Component::StatusLine { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PanePresentation {
    #[default]
    Overlay,
    Live,
}

/// Complete daemon-owned UI projection used to initialize or repair a client.
///
/// Live updates normally travel as [`ViewDiff`]s. A full model is sent on
/// attach and synchronization so a client can also discard components or
/// highlights whose removal it missed while disconnected or lagging.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ViewModel {
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub highlights: HashMap<String, String>,
}

// ── Diffs ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewDiff {
    Upsert {
        component: Component,
    },
    Remove {
        id: String,
    },
    /// Update a panel's semantic placement without replacing its content.
    UpdatePlacement {
        id: String,
        placement: Option<PanelPlacement>,
    },
    SetHighlight {
        name: String,
        fg: Option<String>,
    },
    /// Complete resolved theme settings for immediate interactive preview.
    /// Opaque JSON keeps this crate independent of the core config types.
    SetTheme {
        theme: serde_json::Value,
    },
}

pub fn view_diff_from_pane_content(pc: PaneContent) -> ViewDiff {
    if pc.is_empty() {
        ViewDiff::Remove { id: pc.source }
    } else {
        ViewDiff::Upsert {
            component: Component::float_from_pane_content(&pc),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_content_metadata_is_optional_and_round_trips() {
        let legacy: PaneContent = serde_json::from_value(serde_json::json!({
            "source": "legacy",
            "title": "Legacy"
        }))
        .unwrap();
        assert_eq!(legacy.lines.len(), 0);
        assert_eq!(legacy.visible_rows, 8);
        assert!(legacy.placement.is_none());
        assert!(legacy.owner.is_none());

        let placement = PanelPlacement {
            slot: PanelSlot::Right,
            order: 2,
            size_hint: Some(24),
            pinned: true,
            closable: false,
        };
        let pane = PaneContent {
            source: "panel".into(),
            title: "Panel".into(),
            lines: vec![PaneLineSpec::Plain("content".into())],
            visible_rows: 4,
            scroll: 1,
            placement: Some(placement.clone()),
            owner: Some("plugin.example".into()),
        };
        let restored: PaneContent =
            serde_json::from_value(serde_json::to_value(&pane).unwrap()).unwrap();
        assert_eq!(restored.placement, Some(placement));
        assert_eq!(restored.owner.as_deref(), Some("plugin.example"));
    }

    #[test]
    fn placement_diff_round_trips() {
        let diff = ViewDiff::UpdatePlacement {
            id: "panel".into(),
            placement: Some(PanelPlacement {
                slot: PanelSlot::Overlay,
                order: 0,
                size_hint: None,
                pinned: false,
                closable: true,
            }),
        };
        let restored: ViewDiff =
            serde_json::from_value(serde_json::to_value(&diff).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(diff).unwrap(),
            serde_json::to_value(restored).unwrap()
        );
    }

    #[test]
    fn live_pane_presentation_survives_serialization_and_legacy_floats_default_to_overlay() {
        let pane = PaneContent {
            source: "task_list".into(),
            title: "Tasks".into(),
            lines: vec![PaneLineSpec::Plain("Review changes".into())],
            visible_rows: 8,
            scroll: 2,
            placement: None,
            owner: None,
        };
        let component = Component::float_from_pane_content(&pane);
        let mut json = serde_json::to_value(&component).unwrap();
        let restored: Component = serde_json::from_value(json.clone()).unwrap();
        assert!(matches!(
            restored,
            Component::Float {
                presentation: PanePresentation::Live,
                ..
            }
        ));
        assert_eq!(restored.as_pane_content().unwrap().scroll, 2);
        json.as_object_mut().unwrap().remove("presentation");
        let legacy: Component = serde_json::from_value(json).unwrap();
        assert!(matches!(
            legacy,
            Component::Float {
                presentation: PanePresentation::Overlay,
                ..
            }
        ));
    }
}
