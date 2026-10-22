//! What only the GUI has, for Lua (`bone.gui`, in `runtime/gui/api.lua`):
//! the window (`gui.window`) and a menu bar (`gui.lua.menus`).

use bone_ui::keymap::Action;
use bone_ui::lua::{Host, args, drop_callback, err, json, opt, ret, store_callback};
use eframe::egui;
use mlua::{Lua, MultiValue, Table, Value};

use crate::Gui;

/// A menu bar entry.
pub enum MenuItem {
    Separator,
    /// A label and what it does: an action (a built-in name or a
    /// `/command`) or a stored Lua callback.
    Item(String, MenuAction),
}

pub enum MenuAction {
    Action(Action),
    Lua(u64),
}

/// The GUI's own ops; `None` for an op that is not one of them.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Option<mlua::Result<MultiValue>> {
    Some(match op {
        "gui_window" => {
            args::<Table>(lua, a).and_then(|spec| window(gui, &spec).and_then(|()| ret(lua, ())))
        }
        "gui_window_info" => json(lua, &gui.window_info),
        "gui_menu" => {
            args::<Table>(lua, a).and_then(|spec| menu(gui, lua, &spec).and_then(|()| ret(lua, ())))
        }
        _ => return None,
    })
}

/// `bone.gui.window{ title, width, height, fullscreen, maximized,
/// minimized }`: change what is given (sizes in logical pixels).
fn window(gui: &mut Gui, spec: &Table) -> mlua::Result<()> {
    let some = Some(spec.clone());
    if let Some(title) = opt::<String>(&some, "title")? {
        gui.window_title = Some(title);
    }
    let size = (opt::<f32>(&some, "width")?, opt::<f32>(&some, "height")?);
    if size.0.is_some() || size.1.is_some() {
        let now = gui.window_info["width"]
            .as_f64()
            .zip(gui.window_info["height"].as_f64());
        let (w, h) = now.map_or((960.0, 720.0), |(w, h)| (w as f32, h as f32));
        let size = egui::vec2(
            size.0.unwrap_or(w).max(200.0),
            size.1.unwrap_or(h).max(150.0),
        );
        gui.viewport.push(egui::ViewportCommand::InnerSize(size));
    }
    for (key, cmd) in [
        (
            "fullscreen",
            egui::ViewportCommand::Fullscreen as fn(bool) -> _,
        ),
        ("maximized", egui::ViewportCommand::Maximized),
        ("minimized", egui::ViewportCommand::Minimized),
    ] {
        if let Some(on) = opt::<bool>(&some, key)? {
            gui.viewport.push(cmd(on));
        }
    }
    Host::changed(gui);
    Ok(())
}

/// `bone.gui.menu{ { "File", { { "New chat", "new_session" }, "-",
/// { "Help", "/help" }, { "Hello", function() end } } }, ... }`: the menu
/// bar, replacing the last one (`{}` removes it).
fn menu(gui: &mut Gui, lua: &Lua, spec: &Table) -> mlua::Result<()> {
    let mut menus = Vec::new();
    for m in spec.sequence_values::<Table>() {
        let m = m?;
        let name: String = m.get(1)?;
        let mut items = Vec::new();
        for item in m.get::<Table>(2)?.sequence_values::<Value>() {
            items.push(match item? {
                Value::String(s) if s.to_str()?.as_ref() == "-" => MenuItem::Separator,
                Value::Table(t) => {
                    let label: String = t.get(1)?;
                    let action = match t.get::<Value>(2)? {
                        Value::String(s) => {
                            MenuAction::Action(Action::parse(&s.to_str()?).map_err(err)?)
                        }
                        Value::Function(f) => MenuAction::Lua(store_callback(gui, lua, f)?),
                        other => {
                            return Err(err(format!(
                                "a menu item does an action name or a function, not {}",
                                other.type_name()
                            )));
                        }
                    };
                    MenuItem::Item(label, action)
                }
                other => {
                    return Err(err(format!(
                        "a menu item is {{ label, action }} or \"-\", not {}",
                        other.type_name()
                    )));
                }
            });
        }
        menus.push((name, items));
    }
    for (_, items) in std::mem::replace(&mut gui.lua.menus, menus) {
        for item in items {
            if let MenuItem::Item(_, MenuAction::Lua(cb)) = item {
                drop_callback(lua, cb)?;
            }
        }
    }
    Host::changed(gui);
    Ok(())
}

impl Gui {
    /// The window's state for `bone.gui.window_info()`, from this frame,
    /// and the window changes Lua asked for.
    pub(crate) fn window_ui(&mut self, ctx: &egui::Context) {
        let (rect, fullscreen, maximized, focused) = ctx.input(|i| {
            let v = i.viewport();
            (v.inner_rect, v.fullscreen, v.maximized, v.focused)
        });
        // Without a native window (headless), egui's own screen.
        let rect = rect.unwrap_or_else(|| ctx.content_rect());
        let (w, h) = (rect.width(), rect.height());
        self.window_info = serde_json::json!({
            "width": w, "height": h,
            "fullscreen": fullscreen.unwrap_or(false),
            "maximized": maximized.unwrap_or(false),
            "focused": focused.unwrap_or(true),
            "columns": self.lua.screen_cols,
        });
        for cmd in std::mem::take(&mut self.viewport) {
            ctx.send_viewport_cmd(cmd);
        }
    }

    /// The menu bar Lua set, if any; what was chosen runs after drawing.
    pub(crate) fn menu_ui(&mut self, ui: &mut egui::Ui) {
        if self.lua.menus.is_empty() {
            return;
        }
        let mut chosen = None;
        let menus = &self.lua.menus;
        let bg = self.canvas();
        let rule = self
            .lua
            .theme
            .hl("WinSeparator")
            .fg
            .and_then(crate::view::color)
            .unwrap_or(egui::Color32::from_rgb(0x2c, 0x2c, 0x2c));
        egui::Panel::top("menu")
            .frame(
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(10, 3))
                    .fill(bg),
            )
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    for (i, (name, items)) in menus.iter().enumerate() {
                        ui.menu_button(name, |ui| {
                            for (j, item) in items.iter().enumerate() {
                                match item {
                                    MenuItem::Separator => {
                                        ui.separator();
                                    }
                                    MenuItem::Item(label, _) => {
                                        if ui.button(label).clicked() {
                                            chosen = Some((i, j));
                                            ui.close();
                                        }
                                    }
                                }
                            }
                        });
                    }
                });
                let bar = ui.max_rect();
                ui.painter()
                    .hline(bar.x_range(), bar.bottom(), egui::Stroke::new(1.0, rule));
            });
        let Some((i, j)) = chosen else { return };
        match &self.lua.menus[i].1[j] {
            MenuItem::Item(_, MenuAction::Action(a)) => {
                let a = a.clone();
                bone_ui::lua::input::run(self, a);
            }
            MenuItem::Item(_, MenuAction::Lua(cb)) => {
                let cb = *cb;
                self.call_callback(cb, "menu item", |_| Ok(Value::Nil));
            }
            MenuItem::Separator => {}
        }
    }
}
