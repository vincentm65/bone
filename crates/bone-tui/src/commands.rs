//! Slash commands: running a command line against the registry behind
//! `bone.cmd.create`, shared with the GUI (`bone_ui::lua::execute`). Every
//! command, `/help` included, is Lua (`runtime/lua/bone/commands.lua`); so is
//! the `/` menu (`runtime/lua/bone/menu.lua`).

use crate::app::App;

impl App {
    /// Run a command line (without the leading `/`).
    pub fn execute(&mut self, line: &str) {
        bone_ui::lua::execute(self, line);
    }
}
