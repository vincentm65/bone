//! Plugin lifecycle in the TUI: `bone-ui`'s (`bone_ui::lua::plugins`,
//! shared with the GUI), reached through these. The TUI runs a plugin's
//! `ui.lua`, then its `tui.lua`.

use std::path::{Path, PathBuf};

use bone_ui::lua::plugins as shared;

use crate::app::App;

pub use bone_ui::lua::Owned;
pub use shared::{Kind, Plugin};

impl App {
    pub fn register_plugin(&mut self, name: &str, dir: &Path, kind: Kind) {
        shared::register_plugin(self, name, dir, kind);
    }

    pub fn load_plugin(&mut self, name: &str, dir: &Path, kind: Kind) {
        shared::load_plugin(self, name, dir, kind);
    }

    #[cfg(test)]
    pub fn unload_plugin(&mut self, name: &str) -> Result<(), String> {
        shared::unload_plugin(self, name)
    }

    /// Run the shutdown hooks of `owner` (`None`: every one) and save the
    /// `bone.plugin.state` tables they opened.
    pub fn run_shutdown(&mut self, owner: Option<&str>) {
        shared::run_shutdown(self, owner);
    }

    pub fn save_plugin_states(&mut self, owner: Option<&str>) {
        shared::save_plugin_states(self, owner);
    }

    /// Before quitting: every plugin's shutdown hooks and state.
    pub fn shutdown(&mut self) {
        self.run_shutdown(None);
    }

    /// The trusted project's `.bone/`, while its config is registered.
    pub fn project_dir(&mut self) -> Option<PathBuf> {
        shared::project_dir(self)
    }

    /// At startup, after the user's config: load a trusted project's
    /// config, or say that there is one.
    pub fn load_project(&mut self) {
        shared::load_project(self);
    }

    /// A core Lua file changed: reload the core's configuration. The core
    /// keeps the old one if the new one fails to load.
    pub fn reload_core_config(&mut self) {
        self.request::<bone_proto::methods::CoreReload>(bone_proto::methods::Empty {}, |app, r| {
            match r {
                Ok(r) if r.warnings.is_empty() => app.info("core configuration reloaded"),
                Ok(r) => app.info(format!(
                    "core configuration reloaded: {}",
                    r.warnings.join("; ")
                )),
                Err(e) => app.error(format!(
                    "core reload failed, kept the previous configuration: {e}"
                )),
            }
        });
    }
}
