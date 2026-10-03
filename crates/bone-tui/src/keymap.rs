//! Keymaps: a key, in a context, runs an [`Action`].
//!
//! There are no modes. Keys normally go to the prompt (`main`); while a popup
//! has the keyboard its own context is used: `popup` (a Lua popup) or `picker` (the session list). Unmapped keys type text.

use std::collections::HashMap;

use crate::keys::{self, Key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    Main,
    Popup,
    Picker,
}

impl Context {
    pub fn name(self) -> &'static str {
        match self {
            Context::Main => "main",
            Context::Popup => "popup",
            Context::Picker => "picker",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        [Context::Main, Context::Popup, Context::Picker]
            .into_iter()
            .find(|c| c.name() == s)
    }
}

macro_rules! builtins {
    ($($variant:ident = $name:literal,)*) => {
        /// Built-in actions, addressable by name from Lua and keymaps.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Builtin { $($variant,)* }

        impl Builtin {
            #[cfg(test)]
            pub const ALL: &[Builtin] = &[$(Builtin::$variant,)*];

            #[cfg(test)]
            pub fn name(self) -> &'static str {
                match self { $(Builtin::$variant => $name,)* }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Builtin::$variant),)* _ => None }
            }
        }
    };
}

builtins! {
    Submit = "submit",
    Newline = "newline",
    Left = "left",
    Right = "right",
    Up = "up",
    Down = "down",
    WordLeft = "word_left",
    WordRight = "word_right",
    LineStart = "line_start",
    LineEnd = "line_end",
    Backspace = "backspace",
    Delete = "delete",
    DeleteWord = "delete_word",
    DeleteToStart = "delete_to_start",
    DeleteToEnd = "delete_to_end",
    ScrollUp = "scroll_up",
    ScrollDown = "scroll_down",
    PageUp = "page_up",
    PageDown = "page_down",
    ScrollTop = "scroll_top",
    ScrollBottom = "scroll_bottom",
    Complete = "complete",
    Dismiss = "dismiss",
    Interrupt = "interrupt",
    Quit = "quit",
    QuitIfEmpty = "quit_if_empty",
    NewSession = "new_session",
    Sessions = "sessions",
    PickerUp = "picker_up",
    PickerDown = "picker_down",
    PickerOpen = "picker_open",
    PickerClose = "picker_close",
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Builtin(Builtin),
    /// A slash command line, without the slash.
    Command(String),
    /// A Lua function, by callback id.
    Lua(u64),
}

impl Action {
    /// `"/cmd args"` is a command; anything else names a builtin.
    pub fn parse(s: &str) -> Result<Self, String> {
        if let Some(cmd) = s.strip_prefix('/') {
            return Ok(Action::Command(cmd.to_owned()));
        }
        Builtin::from_name(s)
            .map(Action::Builtin)
            .ok_or_else(|| format!("unknown action: {s}"))
    }
}

#[derive(Default)]
pub struct Keymaps {
    maps: HashMap<(Context, Key), Action>,
}

impl Keymaps {
    pub fn set(&mut self, ctx: Context, key: &str, action: Action) -> Result<(), String> {
        self.maps.insert((ctx, keys::parse(key)?), action);
        Ok(())
    }

    pub fn del(&mut self, ctx: Context, key: &str) -> Result<(), String> {
        self.maps
            .remove(&(ctx, keys::parse(key)?))
            .map(|_| ())
            .ok_or_else(|| format!("no mapping for {key}"))
    }

    pub fn get(&self, ctx: Context, key: Key) -> Option<&Action> {
        self.maps.get(&(ctx, key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_per_context() {
        let mut k = Keymaps::default();
        k.set(Context::Main, "ctrl+r", Action::parse("/sessions").unwrap())
            .unwrap();
        k.set(Context::Popup, "y", Action::parse("dismiss").unwrap())
            .unwrap();
        let y = keys::parse("y").unwrap();
        assert_eq!(
            k.get(Context::Popup, y),
            Some(&Action::Builtin(Builtin::Dismiss))
        );
        assert_eq!(k.get(Context::Main, y), None);
        assert_eq!(
            k.get(Context::Main, keys::parse("ctrl+r").unwrap()),
            Some(&Action::Command("sessions".into()))
        );
        k.del(Context::Main, "ctrl+r").unwrap();
        assert!(k.del(Context::Main, "ctrl+r").is_err());
        assert!(Action::parse("nope").is_err());
        for b in Builtin::ALL {
            assert_eq!(Builtin::from_name(b.name()), Some(*b));
        }
        assert_eq!(Context::from_name("picker"), Some(Context::Picker));
    }
}
