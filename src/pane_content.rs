//! Pure-data pane types for core.
//!
//! These types flow through core data structures (`ToolResult`,
//! `ToolLiveEvent`) without any dependency on ratatui or `crate::ui`.
//! The TUI converts them to its internal `PanePage` (with
//! `Vec<ratatui::text::Line>`) via `PanePage::from_content` at the render boundary.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// Deserialize a `Vec<T>` from either a JSON array or an empty JSON object.
///
/// Lua's empty table `{}` is ambiguous: mlua's serde serializer emits it as a
/// JSON object `{}`, not an array `[]`. Without this, `lines = {}` from Lua
/// fails to deserialize as `Vec<PaneLineSpec>`, silently breaking pane
/// removal (the `pcall` in Lua swallows the error).
///
/// **Leniency:** malformed / un-deserializable elements inside the array are
/// silently skipped (matching the pre-refactor `filter_map` behaviour). This
/// tolerates occasional garbage from Lua tools without blanking the whole pane.
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

/// One span within a styled line.
///
/// `fg` is a color name string (parsed by the frontend at render time, e.g.
/// `ui::color::parse_color`). `modifiers` is a list of strings ("bold",
/// "dim", "italic", "strike"/"crossed_out"); unknown values are silently
/// ignored at the TUI boundary (same behavior as the old inline `from_json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaneSpanSpec {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub fg: Option<String>,
    #[serde(default, deserialize_with = "deserialize_vec_or_empty_map")]
    pub modifiers: Vec<String>,
}

/// One line of pane content.
///
/// Either a plain string or a list of styled spans. `#[serde(untagged)]`
/// gives us the dual format that the old `from_json` parsed: a line element
/// is either `"text"` or `{"spans": [...]}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaneLineSpec {
    Plain(String),
    Spans {
        #[serde(default, deserialize_with = "deserialize_vec_or_empty_map")]
        spans: Vec<PaneSpanSpec>,
    },
}

impl PaneLineSpec {
    /// True if the line renders no visible text.
    pub fn is_empty(&self) -> bool {
        match self {
            PaneLineSpec::Plain(s) => s.is_empty(),
            PaneLineSpec::Spans { spans } => spans.is_empty(),
        }
    }
}

/// Pure-data representation of a pane page.
///
/// This is what flows through core types (`ToolResult`, `ToolLiveEvent`).
/// The TUI converts it to its internal `PanePage` (with
/// `Vec<ratatui::text::Line>`) via `PanePage::from_content`.
///
/// Replaces the old `PanePage` in all core type definitions. `lines` replaces
/// the old `PanePage.content: Vec<Line>`.
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

fn default_visible_rows() -> usize {
    8
}

impl PaneContent {
    /// True when this content signals pane removal (empty lines = remove).
    ///
    /// Replaces the `page.content.is_empty()` checks that were scattered
    /// across core.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Parse from the JSON value that Lua produces.
    ///
    /// Same wire format that the old `PanePage::from_json` accepted.
    pub fn from_json(val: &serde_json::Value) -> Result<Self, String> {
        serde_json::from_value::<PaneContent>(val.clone())
            .map_err(|e| format!("pane parse error: {e}"))
    }
}

/// Frontend-neutral key event delivered to Lua by `ctx.ui.key()`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub code: String,
    #[serde(default)]
    pub char: Option<String>,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
}

/// A blocking request for the next terminal key.
#[derive(Debug)]
pub struct KeyRequest {
    pub reply: oneshot::Sender<KeyEvent>,
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A number element in `lines` is SKIPPED; the two string siblings parse.
    #[test]
    fn skip_numeric_line() {
        let val = json!({
            "source": "s",
            "title": "t",
            "lines": ["ok", 42, "also-ok"]
        });
        let pc = PaneContent::from_json(&val).unwrap();
        assert_eq!(pc.lines.len(), 2);
        // First parsed line is the string before the number.
        assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "ok"));
        // Second parsed line is the string after the number.
        assert!(matches!(&pc.lines[1], PaneLineSpec::Plain(s) if s == "also-ok"));
    }

    /// `{"spans":"not-an-array"}` as a line element is skipped without failing.
    #[test]
    fn skip_bad_spans_type() {
        let val = json!({
            "source": "s",
            "title": "t",
            "lines": [
                "first",
                {"spans": "not-an-array"},
                "last"
            ]
        });
        let pc = PaneContent::from_json(&val).unwrap();
        assert_eq!(pc.lines.len(), 2);
        assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "first"));
        assert!(matches!(&pc.lines[1], PaneLineSpec::Plain(s) if s == "last"));
    }

    /// Happy path: mix of plain-string lines and `{"spans":[...]}` lines.
    #[test]
    fn happy_path_mixed() {
        let val = json!({
            "source": "s",
            "title": "t",
            "lines": [
                "plain",
                {"spans": [
                    {"text": "bold", "modifiers": ["bold"]},
                    {"text": "plain"}
                ]},
                "another plain"
            ]
        });
        let pc = PaneContent::from_json(&val).unwrap();
        assert_eq!(pc.lines.len(), 3);
        assert!(matches!(&pc.lines[0], PaneLineSpec::Plain(s) if s == "plain"));
        assert!(matches!(&pc.lines[1], PaneLineSpec::Spans { .. }));
        assert!(matches!(&pc.lines[2], PaneLineSpec::Plain(s) if s == "another plain"));
    }

    /// `"lines": {}` still parses to 0 lines (empty-map case).
    #[test]
    fn empty_object_yields_zero_lines() {
        let val = json!({
            "source": "s",
            "title": "t",
            "lines": {}
        });
        let pc = PaneContent::from_json(&val).unwrap();
        assert!(pc.lines.is_empty());
    }

    /// `"lines": null` parses to 0 lines.
    #[test]
    fn null_yields_zero_lines() {
        let val = json!({
            "source": "s",
            "title": "t",
            "lines": null
        });
        let pc = PaneContent::from_json(&val).unwrap();
        assert!(pc.lines.is_empty());
    }
}
