//! Panels in the TUI: their state, sizing and ops are `bone-ui`'s
//! (`bone_ui::panel`, `bone_ui::lua`); here they meet the TUI's keys, mouse
//! and painting.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;

pub use bone_ui::panel::*;

use crate::app::App;
use bone_ui::lua::panels as shared;

use crate::ui::render_items;

impl App {
    pub fn panel(&self, id: &str) -> Option<&Panel> {
        shared::panel(&self.panels, id)
    }

    pub fn panel_mut(&mut self, id: &str) -> Option<&mut Panel> {
        self.panels.iter_mut().find(|p| p.id == id)
    }

    /// The panel with the keyboard, if any. A focused popup still comes first.
    pub fn focused_panel(&self) -> Option<&Panel> {
        shared::focused(&self.panels, &self.panel_focus)
    }

    /// Give a panel the keyboard, or (`None`) give it back to the prompt.
    pub fn focus_panel(&mut self, id: Option<&str>) -> Result<(), String> {
        shared::focus_panel(self, id)
    }

    /// The panel drawn at a screen position.
    fn panel_at(&self, (x, y): (u16, u16)) -> Option<String> {
        shared::at(&self.panels, x, y).map(|p| p.id.clone())
    }

    /// The wheel over a panel scrolls it. Returns whether it did.
    pub fn panel_wheel(&mut self, at: (u16, u16), up: bool) -> bool {
        if self.focused_popup().is_some() {
            return false;
        }
        let Some(id) = self.panel_at(at) else {
            return false;
        };
        if let Some(p) = self.panel_mut(&id) {
            p.scroll_by(if up { -WHEEL_ROWS } else { WHEEL_ROWS });
        }
        self.dirty = true;
        true
    }

    /// A click focuses the focusable panel under it, or the prompt.
    pub fn panel_click(&mut self, at: (u16, u16)) {
        if self.focused_popup().is_some() {
            return;
        }
        match self.panel_at(at) {
            Some(id) if self.panel(&id).is_some_and(|p| p.focusable) => {
                let _ = self.focus_panel(Some(&id));
            }
            Some(_) => {}
            None => {
                let in_prompt = self.placed.get(&crate::app::PROMPT_WIN).is_some_and(|p| {
                    p.area.height > 0 && at.1 >= p.area.y && at.1 < p.area.bottom()
                });
                if in_prompt {
                    let _ = self.focus_panel(None);
                }
            }
        }
    }

    /// Measure an auto tree leaf (see `bone_ui::lua::panels::measure_panel`).
    pub(crate) fn measure_panel(&mut self, id: &str, width: u16, height: u16, rows: bool) -> u16 {
        shared::measure_panel(self, id, width, height, rows)
    }

    /// Dock the panels the layout does not place into `room`; what is left.
    pub(crate) fn allocate_panels(
        &mut self,
        room: Rect,
        full_height: bool,
        layout: &crate::ui::LayoutNode,
        placements: &mut Vec<PanelPlacement>,
    ) -> Rect {
        let room = bone_ui::layout::Rect::new(room.x, room.y, room.width, room.height);
        let r = shared::allocate_panels(self, room, full_height, layout, placements);
        Rect::new(r.x, r.y, r.width, r.height)
    }

    /// Publish every panel rectangle before any final render callback runs.
    pub(crate) fn publish_panels(&mut self, placements: &[PanelPlacement]) {
        shared::publish_panels(self, placements);
    }

    pub(crate) fn paint_panels(&mut self, frame: &mut Frame<'_>, placements: &[PanelPlacement]) {
        let rect = |r: bone_ui::layout::Rect| Rect::new(r.x, r.y, r.width, r.height);
        for placement in placements {
            let Some(f) = shared::panel_frame(self, placement) else {
                continue;
            };
            let area = rect(f.area);
            if let Some(items) = &f.title {
                frame.render_widget(
                    Paragraph::new(render_items(items, area.width as usize, &self.theme)),
                    Rect { height: 1, ..area },
                );
            }
            let body = rect(f.body);
            let shown: Vec<_> = (f.lines.iter())
                .map(|l| render_items(l, body.width as usize, &self.theme))
                .collect();
            frame.render_widget(Paragraph::new(shown), body);
            if let Some(x) = f.separator {
                let style = self.theme.hl("WinSeparator");
                for y in area.y..area.bottom() {
                    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                        cell.set_symbol("│").set_style(style);
                    }
                }
            }
        }
    }
}
