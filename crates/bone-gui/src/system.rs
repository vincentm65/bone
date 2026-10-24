//! The desktop's own services for Lua (`bone.gui.notify`, `open_file`,
//! `save_file`): notifications, and the system's file dialogs. Each runs in
//! the background; a dialog's answer comes back to its callback on the UI
//! thread.

use std::path::PathBuf;

use bone_ui::lua::{Host, args, drop_callback, err, opt, ret, store_callback};
use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::Gui;

/// These ops; `None` for an op that is not one of them.
pub(crate) fn dispatch(
    gui: &mut Gui,
    lua: &Lua,
    op: &str,
    a: MultiValue,
) -> Result<mlua::Result<MultiValue>, MultiValue> {
    Ok(match op {
        "gui_notify" => args::<(String, Option<String>)>(lua, a).and_then(|(title, body)| {
            notify(gui, title, body.unwrap_or_default());
            ret(lua, ())
        }),
        "gui_open_file" | "gui_save_file" => {
            args::<(Option<Table>, Function)>(lua, a).and_then(|(opts, then)| {
                let dialog = dialog(gui, &opts)?;
                let multiple = opt::<bool>(&opts, "multiple")?.unwrap_or(false);
                let name = opt::<String>(&opts, "name")?;
                let cb = store_callback(gui, lua, then)?;
                let save = op == "gui_save_file";
                let pick = async move {
                    let files = if save {
                        let d = match name {
                            Some(n) => dialog.set_file_name(n),
                            None => dialog,
                        };
                        d.save_file().await.into_iter().collect::<Vec<_>>()
                    } else if multiple {
                        dialog.pick_files().await.unwrap_or_default()
                    } else {
                        dialog.pick_file().await.into_iter().collect()
                    };
                    let paths: Vec<PathBuf> = files.iter().map(|f| f.path().to_owned()).collect();
                    paths
                };
                Host::spawn(gui, pick, move |gui, paths| {
                    answer(gui, cb, paths, multiple)
                });
                ret(lua, ())
            })
        }
        _ => return Err(a),
    })
}

/// A file dialog from `{ title, dir, filters = { { "Images", { "png" } } } }`.
fn dialog(gui: &Gui, opts: &Option<Table>) -> mlua::Result<rfd::AsyncFileDialog> {
    let mut d = rfd::AsyncFileDialog::new();
    if let Some(title) = opt::<String>(opts, "title")? {
        d = d.set_title(title);
    }
    let dir = opt::<String>(opts, "dir")?.unwrap_or_else(|| gui.opts.cwd.clone());
    d = d.set_directory(dir);
    if let Some(filters) = opt::<Table>(opts, "filters")? {
        for f in filters.sequence_values::<Table>() {
            let f = f?;
            let name: String = f.get(1)?;
            let exts: Vec<String> = f.get(2)?;
            if exts.is_empty() {
                return Err(err(format!("filter {name} has no extensions")));
            }
            d = d.add_filter(name, &exts);
        }
    }
    Ok(d)
}

/// Give a dialog's answer to its callback: the path (a list of them with
/// `multiple`), or nil when it was cancelled.
fn answer(gui: &mut Gui, cb: u64, paths: Vec<PathBuf>, multiple: bool) {
    let text: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    gui.call_callback(cb, "file dialog", |lua| match (multiple, text.first()) {
        (_, None) => Ok(Value::Nil),
        (true, _) => Ok(Value::Table(lua.create_sequence_from(text.clone())?)),
        (false, Some(p)) => Ok(Value::String(lua.create_string(p)?)),
    });
    let _ = gui.with_api(|lua| drop_callback(lua, cb));
}

/// A desktop notification; one that cannot be shown is a status message.
fn notify(gui: &mut Gui, title: String, body: String) {
    let show = async move {
        tokio::task::spawn_blocking(move || {
            notify_rust::Notification::new()
                .appname("bone")
                .summary(&title)
                .body(&body)
                .show()
                .map(drop)
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
    };
    Host::spawn(gui, show, |gui, r| {
        if let Err(e) = r {
            Host::error(gui, format!("cannot show a notification: {e}"));
        }
    });
}
