//! What only the GUI has, for Lua (`bone.gui`, in `runtime/gui/api.lua`):
//! the window (`gui.window`).

use bone_ui::lua::{Host, args, err, json, opt, ret};
use eframe::egui;
use mlua::{Lua, MultiValue, Table};

use crate::Gui;

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
        "gui_library_info" => {
            let dir = gui.opts.config_dir.clone();
            let active = (dir.as_deref())
                .and_then(|d| bone_lua::library(d, bone_lua::Side::Gui))
                .unwrap_or_else(|| "standard".into());
            let custom: Vec<String> = (dir.as_deref())
                .map(|d| {
                    let has_gui = |n: &String| d.join("libraries").join(n).join("gui").is_dir();
                    bone_lua::tui_libraries(d)
                        .into_iter()
                        .filter(has_gui)
                        .collect()
                })
                .unwrap_or_default();
            json(
                lua,
                &serde_json::json!({
                    "active": active,
                    "builtin": bone_lua::GUI_LIBRARIES,
                    "custom": custom,
                }),
            )
        }
        "gui_library_use" => args::<String>(lua, a).and_then(|name| {
            let dir =
                (gui.opts.config_dir.clone()).ok_or_else(|| err("there is no config directory"))?;
            let prev = bone_lua::gui_library(&dir);
            bone_lua::set_gui_library(&dir, &name).map_err(err)?;
            gui.gui_library_prev = Some(prev);
            Host::changed(gui);
            ret(lua, ())
        }),
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
}
