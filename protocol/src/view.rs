//! View model types: components, diffs, and pane content for the wire protocol.

use serde::{Deserialize, Serialize};

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

// ── Diffs ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewDiff {
    Upsert { component: Component },
    Remove { id: String },
    SetHighlight { name: String, fg: Option<String> },
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
