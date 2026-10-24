//! Keyboard focus, the wheel, and calling the handlers that fired.

use bone_ui::lua::Host;
use eframe::egui;
use mlua::Function;

use crate::Gui;

use super::{Arg, Focusable, ViewState};

impl ViewState {
    /// Move the focus to the next (or previous) node the keyboard reaches;
    /// out of a modal, the prompt is one of them.
    pub(crate) fn move_focus(&mut self, forward: bool) {
        let mut ring: Vec<Option<String>> = Vec::new();
        if self.trap.is_none() {
            ring.push(None);
        }
        let trap = self.trap;
        ring.extend(
            (self.focusables.iter())
                .filter(|f| trap.is_none() || f.scope == trap)
                .map(|f| Some(f.key.clone())),
        );
        if ring.is_empty() {
            return;
        }
        let n = ring.len();
        let next = match ring.iter().position(|r| *r == self.focused) {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None if forward => 0,
            None => n - 1,
        };
        self.focused = ring[next].clone();
        self.focus_visible = true;
    }
}

impl Gui {
    /// Where the keyboard focus is after a frame: on what was pressed, asked
    /// for (`bone.gui.focus`) or has just appeared with `autofocus`, kept in
    /// the topmost modal, and gone with a node that has.
    pub(super) fn settle_focus(
        &mut self,
        ctx: &egui::Context,
        focusables: Vec<Focusable>,
        trap: Option<usize>,
        press_focus: Option<String>,
    ) {
        let vs = &mut self.view;
        let before = (vs.focused.clone(), vs.focus_visible);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            (vs.focused, vs.focus_visible) = (press_focus, false);
        }
        if let Some(id) = vs
            .focus_request
            .take_if(|id| focusables.iter().any(|f| f.key == *id))
        {
            (vs.focused, vs.focus_visible) = (Some(id), true);
        }
        let shown: std::collections::HashSet<String> = (focusables.iter())
            .filter(|f| f.autofocus)
            .map(|f| f.key.clone())
            .collect();
        for f in focusables.iter().filter(|f| f.autofocus) {
            if !vs.autofocused.contains(&f.key) {
                (vs.focused, vs.focus_visible) = (Some(f.key.clone()), false);
                if f.key_guard > 0.0 {
                    let guard = std::time::Duration::from_secs_f32(f.key_guard / 1000.0);
                    vs.guard_until = Some(std::time::Instant::now() + guard);
                }
            }
        }
        vs.autofocused = shown;
        let here = |id: &Option<String>, scope: Option<usize>| {
            (focusables.iter())
                .any(|f| Some(&f.key) == id.as_ref() && (scope.is_none() || f.scope == scope))
        };
        if trap.is_some() && !here(&vs.focused, trap) {
            vs.focused = (focusables.iter())
                .find(|f| f.scope == trap)
                .map(|f| f.key.clone());
            vs.focus_visible = false;
        } else if !here(&vs.focused, None) {
            vs.focused = None;
        }
        vs.focusables = focusables;
        vs.trap = trap;
        if (vs.focused.clone(), vs.focus_visible) != before {
            // The node that had egui's focus (for screen readers) gives it
            // back to the prompt, unless something else took it.
            if let (Some(old), None) = (&before.0, &vs.focused) {
                let id = egui::Id::new(("node", old));
                if ctx.memory(|m| m.focused()) == Some(id) {
                    ctx.memory_mut(|m| m.request_focus(crate::prompt::edit_id()));
                }
            }
            ctx.request_repaint();
        }
    }

    /// Call node `on_key` handler `id` with key `name`: whether it took it.
    pub(crate) fn call_key(&mut self, id: u64, name: &str) -> bool {
        let Some(table) = self.view.sink_table() else {
            return false;
        };
        let r = self.with_api(|_| match table.raw_get::<Option<Function>>(id)? {
            Some(f) => f.call::<Option<bool>>(name),
            None => Ok(None),
        });
        match r {
            Ok(taken) => taken.unwrap_or(false),
            Err(e) => {
                Host::lua_error(self, "gui on_key", &e);
                false
            }
        }
    }

    /// Run a node's `on_click` (enter or space on a focused node).
    pub(crate) fn activate(&mut self, id: u64) {
        self.call_handlers(vec![(id, Arg::Pos { x: 0.0, y: 0.0 })]);
    }

    /// The wheel: the innermost scroll container under the pointer that can
    /// move that way takes it, else the first `on_scroll`.
    pub(super) fn wheel(&mut self, ctx: &egui::Context) {
        let (dx, dy, shift) = ctx.input(|i| {
            (
                i.smooth_scroll_delta.x,
                i.smooth_scroll_delta.y,
                i.modifiers.shift,
            )
        });
        let Some(pos) = ctx
            .input(|i| i.pointer.hover_pos())
            .filter(|_| dx != 0.0 || dy != 0.0)
        else {
            return;
        };
        let hits = std::mem::take(&mut self.view.hits);
        let mut taken_scroll = false;
        for hit in hits.iter().rev().filter(|h| h.rect.contains(pos)) {
            let mut taken = false;
            // A row moves with the sideways wheel, or the wheel with shift.
            let amount = match (hit.sideways, dx != 0.0, shift) {
                (false, _, _) => dy,
                (true, true, _) => dx,
                (true, false, true) => dy,
                (true, false, false) => 0.0,
            };
            if let Some(s) = (hit.scroll.as_ref())
                .filter(|_| amount != 0.0)
                .and_then(|id| self.view.scroll.get_mut(id))
            {
                let to = (s.offset - amount).clamp(0.0, s.max);
                taken = to != s.offset;
                s.offset = to;
                s.target = None;
            }
            if let Some(f) = hit.on_scroll {
                self.call_handlers(vec![(f, Arg::Delta(dy))]);
                taken = true;
            }
            if taken {
                ctx.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                taken_scroll = true;
                break;
            }
        }
        self.view.hits = hits;
        if taken_scroll {
            ctx.request_repaint();
        }
    }

    pub(super) fn call_handlers(&mut self, events: Vec<(u64, Arg)>) {
        let Some(table) = self.view.bound.sink.as_ref().map(|s| s.table.clone()) else {
            return;
        };
        for (id, arg) in events {
            let r = self.with_api(|lua| {
                let Some(f) = table.raw_get::<Option<Function>>(id)? else {
                    return Ok(());
                };
                match arg {
                    Arg::Text(s) => f.call::<()>(s),
                    Arg::Bool(b) => f.call::<()>(b),
                    Arg::Delta(d) => f.call::<()>(d),
                    Arg::Pos { x, y } => {
                        let t = lua.create_table()?;
                        t.set("x", x)?;
                        t.set("y", y)?;
                        f.call::<()>(t)
                    }
                }
            });
            if let Err(e) = r {
                Host::lua_error(self, "gui view handler", &e);
            }
        }
    }
}
