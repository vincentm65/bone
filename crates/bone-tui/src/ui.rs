//! Where Lua draws the screen:
//!
//! - `bone.ui.views[kind](item, ctx)` renders each transcript item (user,
//!   reasoning, assistant, tool, notice) to lines.
//! - `bone.ui.regions[name]` fills `top`, `left`, `right` and `above_prompt`.
//! - `bone.ui.statusline(ctx)` and `bone.ui.divider(ctx)` draw those rows;
//!   without them the rows are not there.
//! - `bone.ui.prompt` sets how the prompt box looks: prefix, placeholder,
//!   border, background, padding and lines in its top and bottom edges.
//!   Rust keeps the text: it wraps, scrolls, selects and places the cursor.
//!
//! A line is a string or a list of items: `"text"`, `{ text, hl }`,
//! `{ fill = "─", hl = ... }` (stretches to fill the row) or `"%="` (a
//! blank fill). Without Lua, Rust draws plain text and nothing else.

use ratatui::text::{Line, Span};
use serde_json::Value as Json;

pub use bone_ui::model::*;

use crate::app::App;
use crate::chat::bare;
use crate::editor::TextBuffer;
use crate::layout::BufferId;
use crate::theme::Theme;

/// Lay items out in `width` columns. Fills share the leftover space; text
/// that does not fit is cut with `…`.
pub fn render_items(items: &[Item], width_cols: usize, theme: &Theme) -> Line<'static> {
    let spans: Vec<Span<'static>> = bone_ui::model::layout_items(items, width_cols)
        .into_iter()
        .map(|(t, hl)| Span::styled(t, theme.hl(hl)))
        .collect();
    Line::from(spans)
}

impl App {
    /// `bone.ui.spinner`, or the default; read once per frame.
    pub fn read_spinner(&mut self) -> Spinner {
        bone_ui::lua::ui::read_spinner(self)
    }

    /// `bone.ui.title(ctx)`: the terminal's title, or None to leave it.
    pub fn ui_title(&mut self) -> Option<String> {
        bone_ui::lua::ui::ui_title(self)
    }

    /// What chat `buf`'s statusline and divider get.
    pub fn statusline_ctx(&mut self, width_cols: u16, buf: BufferId) -> Json {
        bone_ui::lua::ui::statusline_ctx(self, width_cols, buf)
    }

    /// `bone.ui.layout` as a tree: the top level is a stack of rows.
    pub fn layout(&mut self) -> LayoutNode {
        bone_ui::lua::ui::layout(self)
    }

    /// Whether `bone.ui[name]` is a function.
    pub fn ui_defined(&self, name: &str) -> bool {
        bone_ui::lua::ui::ui_defined(self, name)
    }

    /// `bone.ui.prompt`, read with `ctx`. Nothing set means a bare prompt.
    pub fn prompt_spec(&mut self, ctx: &Json) -> PromptSpec {
        bone_ui::lua::ui::prompt_spec(self, ctx)
    }

    /// The line `bone.ui.prompt.top` or `.bottom` draws into that edge.
    pub fn prompt_edge(&mut self, edge: &str, ctx: &Json, hl: &str) -> Vec<Item> {
        bone_ui::lua::ui::prompt_edge(self, edge, ctx, hl)
    }

    /// `bone.ui.prompt_highlight(ctx)`.
    pub fn prompt_highlight(&mut self, ctx: &Json) -> PromptMarks {
        bone_ui::lua::ui::prompt_highlight(self, ctx)
    }

    /// Chat `buf`'s prompt box, holding `text`.
    pub fn prompt_ctx(&mut self, width_cols: u16, buf: BufferId, text: &TextBuffer) -> Json {
        bone_ui::lua::ui::prompt_ctx(self, width_cols, buf, text)
    }

    /// Run Lua that may fail; a failure is reported once and `name` is
    /// skipped until the UI is refreshed (`bone.ui.refresh`, or redefining it).
    pub(crate) fn guarded<R>(
        &mut self,
        name: &str,
        body: impl FnOnce(&mlua::Lua) -> mlua::Result<R>,
    ) -> Option<R> {
        bone_ui::lua::guarded(self, name, body)
    }

    /// Call `bone.ui[name](ctx)` for a single row (statusline, divider).
    pub fn ui_items(&mut self, name: &str, ctx: Json, default_hl: &str) -> Option<Vec<Item>> {
        bone_ui::lua::ui::ui_items(self, name, ctx, default_hl)
    }

    /// Render the stale items of a chat through `bone.ui.views` and cache them.
    pub fn render_chat(&mut self, buf: BufferId, width_cols: usize) {
        bone_ui::lua::render_chat(
            self,
            buf,
            width_cols,
            &|app: &App, items, width| render_items(items, width, &app.theme),
            &|data, width, first| bare(data, width, first),
        );
    }

    /// A region, sized and drawn (see `bone_ui::lua::ui::region_sized`).
    pub fn region_sized(
        &mut self,
        name: &str,
        width_cols: u16,
        height: u16,
        rows: bool,
        exact: bool,
    ) -> Option<(u16, Vec<Line<'static>>)> {
        let (size, lines) =
            bone_ui::lua::ui::region_sized(self, name, width_cols, height, rows, exact)?;
        let w = if rows { width_cols } else { size } as usize;
        Some((
            size,
            lines
                .iter()
                .map(|l| render_items(l, w, &self.theme))
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_fill_and_truncate() {
        let t = Theme::default();
        let items = vec![
            Item::Text("ab".into(), "Normal".into()),
            Item::Fill("-".into(), "Dim".into()),
            Item::Text("cd".into(), "Normal".into()),
        ];
        assert_eq!(render_items(&items, 8, &t).to_string(), "ab----cd");
        assert_eq!(render_items(&items, 3, &t).to_string(), "ab…");
        let long = vec![Item::Text("abcdef".into(), "Normal".into())];
        assert_eq!(render_items(&long, 4, &t).to_string(), "abc…");
    }
}
