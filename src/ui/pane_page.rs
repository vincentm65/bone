use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::sync::{Arc, Mutex};

/// What kind of interaction the user is performing.
#[derive(Clone, Debug)]
pub enum InteractionMode {
    /// Select one option (Up/Down + Enter)
    SingleSelect,
    /// Select multiple options (Up/Down + Space to toggle + Enter to confirm)
    MultiSelect,
    /// Freeform text input only (no options list)
    TextInput,
}

/// Shared, thread-safe state for an interactive pane.
/// Cloning gives a new handle to the same inner state.
#[derive(Clone, Debug)]
pub struct PaneInteraction {
    inner: Arc<Mutex<InteractionInner>>,
}

#[derive(Debug)]
struct InteractionInner {
    pub mode: InteractionMode,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub input_buffer: String,
    pub cursor_pos: usize,
    pub allow_custom: bool,
    pub custom_focused: bool,
    pub active: bool,
    pub options: Vec<String>,
    pub result_tx: Option<tokio::sync::oneshot::Sender<serde_json::Value>>,
}

/// A single page of tool-provided content rendered in the bottom pane.
#[derive(Clone, Debug)]
pub struct PanePage {
    /// Unique source identifier, e.g. "build_status".
    /// Tools update their existing page or insert a new one via upsert.
    pub source: String,
    /// Short label shown in the tab indicator, e.g. "jobs (3)".
    pub title: String,
    /// The content lines to render.
    pub content: Vec<Line<'static>>,
    /// Maximum content rows this page wants visible at once.
    pub visible_rows: usize,
    /// Scroll offset for this page (rows into content).
    pub scroll: usize,
    /// Optional interactive configuration. When present, the pane responds to
    /// keyboard input (selection, text input, etc.).
    pub interaction: Option<PaneInteraction>,
}

impl PaneInteraction {
    /// Create a new interactive pane. The caller supplies the oneshot sender.
    pub fn new(
        mode: InteractionMode,
        options: Vec<String>,
        allow_custom: bool,
        default_selected: usize,
        result_tx: tokio::sync::oneshot::Sender<serde_json::Value>,
    ) -> Self {
        let has_options = !matches!(mode, InteractionMode::TextInput);
        Self {
            inner: Arc::new(Mutex::new(InteractionInner {
                selected: if has_options { default_selected.min(options.len().saturating_sub(1)) } else { 0 },
                checked: std::iter::repeat(false).take(if has_options { options.len() } else { 0 }).collect(),
                input_buffer: String::new(),
                cursor_pos: 0,
                allow_custom,
                custom_focused: false,
                active: true,
                options,
                result_tx: Some(result_tx),
                mode,
            })),
        }
    }

    pub fn mode(&self) -> InteractionMode {
        self.inner.lock().unwrap().mode.clone()
    }

    pub fn selected(&self) -> usize {
        self.inner.lock().unwrap().selected
    }

    pub fn set_selected(&self, val: usize) {
        let mut inner = self.inner.lock().unwrap();
        let max = if inner.custom_focused { inner.checked.len() } else { inner.checked.len().saturating_sub(1) };
        inner.selected = val.min(max);
    }

    pub fn checked(&self, idx: usize) -> bool {
        self.inner.lock().unwrap().checked.get(idx).copied().unwrap_or(false)
    }

    pub fn set_checked(&self, idx: usize, val: bool) {
        if let Some(v) = self.inner.lock().unwrap().checked.get_mut(idx) {
            *v = val;
        }
    }

    pub fn toggle_checked(&self, idx: usize) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(v) = inner.checked.get_mut(idx) {
            *v = !*v;
        }
    }

    pub fn input_buffer(&self) -> String {
        self.inner.lock().unwrap().input_buffer.clone()
    }

    /// Append a char to the input buffer at cursor position.
    pub fn input_insert_char(&self, c: char) {
        let mut inner = self.inner.lock().unwrap();
        let bp = inner.input_buffer
            .char_indices()
            .nth(inner.cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(inner.input_buffer.len());
        inner.input_buffer.insert(bp, c);
        inner.cursor_pos += 1;
    }

    /// Delete the char before the cursor (Backspace).
    pub fn input_delete_backward(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.cursor_pos == 0 {
            return;
        }
        let prev_idx = inner.cursor_pos - 1;
        let (start_byte, ch) = inner.input_buffer
            .char_indices()
            .nth(prev_idx)
            .unwrap_or((0, '\0'));
        inner.input_buffer.replace_range(start_byte..start_byte + ch.len_utf8(), "");
        inner.cursor_pos = prev_idx;
    }

    pub fn cursor_pos(&self) -> usize {
        self.inner.lock().unwrap().cursor_pos
    }

    pub fn set_cursor_pos(&self, pos: usize) {
        let mut inner = self.inner.lock().unwrap();
        inner.cursor_pos = pos.min(inner.input_buffer.chars().count());
    }

    pub fn is_active(&self) -> bool {
        self.inner.lock().unwrap().active
    }

    pub fn options(&self) -> Vec<String> {
        self.inner.lock().unwrap().options.clone()
    }

    pub fn num_options(&self) -> usize {
        self.inner.lock().unwrap().options.len()
    }

    pub fn allow_custom(&self) -> bool {
        self.inner.lock().unwrap().allow_custom
    }

    pub fn custom_focused(&self) -> bool {
        self.inner.lock().unwrap().custom_focused
    }

    pub fn set_custom_focused(&self, val: bool) {
        self.inner.lock().unwrap().custom_focused = val;
    }

    /// User pressed Enter: consume the oneshot sender and send the result.
    /// Returns true if the result was sent successfully.
    pub fn submit(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if !inner.active {
            return false;
        }
        inner.active = false;
        if let Some(tx) = inner.result_tx.take() {
            let value = match inner.mode {
                InteractionMode::TextInput => {
                    serde_json::json!({"value": inner.input_buffer.clone()})
                }
                InteractionMode::SingleSelect => {
                    if inner.custom_focused || inner.selected >= inner.options.len() {
                        serde_json::json!({"value": inner.input_buffer.clone(), "custom": true})
                    } else {
                        serde_json::json!({"value": inner.options[inner.selected]})
                    }
                }
                InteractionMode::MultiSelect => {
                    let selected: Vec<String> = inner.checked.iter().enumerate()
                        .filter(|(_, c)| **c)
                        .map(|(i, _)| inner.options[i].clone())
                        .collect();
                    let mut result = serde_json::json!({"values": selected});
                    if !inner.input_buffer.is_empty() && inner.allow_custom {
                        result["custom"] = serde_json::Value::String(inner.input_buffer.clone());
                    }
                    result
                }
            };
            tx.send(value).is_ok()
        } else {
            false
        }
    }



    /// User pressed Esc: cancel the interaction.
    pub fn cancel(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if !inner.active {
            return false;
        }
        inner.active = false;
        if let Some(tx) = inner.result_tx.take() {
            tx.send(serde_json::json!({"cancelled": true})).is_ok()
        } else {
            false
        }
    }

    /// Handle a keypress for this interactive pane.
    /// Returns `true` if the key was consumed.
    ///
    /// This is the single source of truth for interactive key handling,
    /// shared by both the idle event loop and the streaming drain loop.
    pub fn handle_key(&self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let mode = self.mode();
        let custom_focused = self.custom_focused();
        let num_options = self.num_options();

        let in_text_mode = matches!(mode, InteractionMode::TextInput) || custom_focused;

        if in_text_mode {
            match code {
                KeyCode::Char(c) if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT => {
                    self.input_insert_char(c);
                    return true;
                }
                KeyCode::Backspace if modifiers.is_empty() => {
                    self.input_delete_backward();
                    return true;
                }
                KeyCode::Left if modifiers.is_empty() => {
                    let cp = self.cursor_pos();
                    if cp > 0 {
                        self.set_cursor_pos(cp - 1);
                    }
                    return true;
                }
                KeyCode::Right if modifiers.is_empty() => {
                    let len = self.input_buffer().chars().count();
                    let cp = self.cursor_pos();
                    if cp < len {
                        self.set_cursor_pos(cp + 1);
                    }
                    return true;
                }
                KeyCode::Home if modifiers.is_empty() => {
                    self.set_cursor_pos(0);
                    return true;
                }
                KeyCode::End if modifiers.is_empty() => {
                    let len = self.input_buffer().chars().count();
                    self.set_cursor_pos(len);
                    return true;
                }
                _ => {}
            }
        }

        match code {
            KeyCode::Up if modifiers.is_empty() => {
                if custom_focused {
                    self.set_custom_focused(false);
                    self.set_selected(num_options.saturating_sub(1));
                } else {
                    let sel = self.selected();
                    if sel > 0 {
                        self.set_selected(sel - 1);
                    } else if self.allow_custom() {
                        self.set_custom_focused(true);
                    }
                }
                return true;
            }
            KeyCode::Down if modifiers.is_empty() => {
                if custom_focused {
                    self.set_custom_focused(false);
                    self.set_selected(0);
                } else {
                    let sel = self.selected();
                    let last_idx = num_options.saturating_sub(1);
                    if self.allow_custom() && sel >= last_idx {
                        self.set_custom_focused(true);
                    } else {
                        let max = if self.allow_custom() { num_options } else { last_idx };
                        if sel < max {
                            self.set_selected(sel + 1);
                        }
                    }
                }
                return true;
            }
            KeyCode::Char(' ') if modifiers.is_empty() && matches!(mode, InteractionMode::MultiSelect) => {
                if !custom_focused {
                    self.toggle_checked(self.selected());
                }
                return true;
            }
            KeyCode::Enter if modifiers.is_empty() => {
                self.submit();
                return true;
            }
            KeyCode::Tab if modifiers.is_empty() => {
                if self.allow_custom() {
                    self.set_custom_focused(!custom_focused);
                    return true;
                }
                return false; // let Tab cycle pages
            }
            KeyCode::Esc if modifiers.is_empty() => {
                self.cancel();
                return true;
            }
            _ => {}
        }
        false
    }
}

impl PanePage {
    /// Parse a pane definition from a serde_json::Value.
    pub fn from_json(val: &serde_json::Value) -> Result<Self, String> {
        let pane = val.as_object().ok_or("pane must be an object")?;
        let source = pane
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or("pane missing source")?
            .to_string();
        let title = pane
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or("pane missing title")?
            .to_string();
        let visible_rows = pane
            .get("visible_rows")
            .and_then(|v| v.as_u64())
            .unwrap_or(8) as usize;
        let scroll = pane.get("scroll").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        let lines: Vec<Line<'static>> = pane
            .get("lines")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|line_val| {
                        if let Some(text) = line_val.as_str() {
                            Some(Line::from(text.to_string()))
                        } else if let Some(obj) = line_val.as_object() {
                            let spans: Vec<Span<'static>> = obj
                                .get("spans")
                                .and_then(|v| v.as_array())
                                .map(|spans_arr| {
                                    spans_arr
                                        .iter()
                                        .filter_map(|span_val| {
                                            let span_obj = span_val.as_object()?;
                                            let text = span_obj
                                                .get("text")
                                                .and_then(|v| v.as_str())?
                                                .to_string();
                                            let mut style = Style::default();
                                            if let Some(fg) =
                                                span_obj.get("fg").and_then(|v| v.as_str())
                                                && let Some(c) = crate::ext::color::parse_color(fg)
                                            {
                                                style = style.fg(c);
                                            }
                                            if let Some(mods) =
                                                span_obj.get("modifiers").and_then(|v| v.as_array())
                                            {
                                                for m in mods {
                                                    if let Some(s) = m.as_str() {
                                                        match s {
                                                            "bold" => {
                                                                style = style
                                                                    .add_modifier(Modifier::BOLD)
                                                            }
                                                            "dim" => {
                                                                style = style
                                                                    .add_modifier(Modifier::DIM)
                                                            }
                                                            "italic" => {
                                                                style = style
                                                                    .add_modifier(Modifier::ITALIC)
                                                            }
                                                            "strike" | "crossed_out" => {
                                                                style = style.add_modifier(
                                                                    Modifier::CROSSED_OUT,
                                                                )
                                                            }
                                                            _ => {}
                                                        }
                                                    }
                                                }
                                            }
                                            Some(Span::styled(text, style))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            if spans.is_empty() {
                                Some(Line::from(""))
                            } else {
                                Some(Line::from(spans))
                            }
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(PanePage {
            source,
            title,
            content: lines,
            visible_rows,
            scroll,
            interaction: None,
        })
    }

    /// Remove a page by source name. Returns the new active_page index.
    pub fn remove(pages: &mut Vec<PanePage>, source: &str, active_page: usize) -> usize {
        if let Some(pos) = pages.iter().position(|p| p.source == source) {
            pages.remove(pos);
            if pages.is_empty() {
                0
            } else if active_page >= pages.len() {
                pages.len() - 1
            } else if pos < active_page {
                active_page - 1
            } else {
                active_page
            }
        } else {
            active_page
        }
    }

    /// Upsert a page. If a page with the same source exists, replace it.
    /// Returns the index of the page and the new active_page.
    pub fn upsert(pages: &mut Vec<PanePage>, active_page: usize, page: PanePage) -> (usize, usize) {
        if let Some(pos) = pages.iter().position(|p| p.source == page.source) {
            pages[pos] = page;
            (pos, active_page)
        } else {
            pages.push(page);
            let idx = pages.len() - 1;
            (idx, idx)
        }
    }
}
