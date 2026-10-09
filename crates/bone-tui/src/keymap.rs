//! Keymaps: keys and key sequences in named contexts.
//!
//! There are no modes. Keys normally go to the prompt (`main`); while a popup
//! has the keyboard its own context is used: `popup`, and while a panel has
//! it, `panel`. Plugins may define named contexts with fallback contexts and
//! priorities.

use std::collections::HashMap;

use crate::keys::{self, Key};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Context {
    Main,
    Popup,
    Panel,
    Named(String),
}

impl Context {
    pub fn name(&self) -> &str {
        match self {
            Context::Main => "main",
            Context::Popup => "popup",
            Context::Panel => "panel",
            Context::Named(name) => name,
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        if s == "main" {
            Some(Context::Main)
        } else if s == "popup" {
            Some(Context::Popup)
        } else if s == "panel" {
            Some(Context::Panel)
        } else if valid_name(s) {
            Some(Context::Named(s.to_owned()))
        } else {
            None
        }
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self, Context::Main | Context::Popup | Context::Panel)
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

macro_rules! builtins {
    ($($variant:ident = $name:literal,)*) => {
        /// Built-in actions, addressable by name from Lua and keymaps.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Builtin { $($variant,)* }

        impl Builtin {
            #[cfg(test)]
            pub const ALL: &[Builtin] = &[$(Builtin::$variant,)*];

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
    PasteClipboard = "paste_clipboard",
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
    FocusNext = "focus_next",
    FocusPrev = "focus_prev",
    FocusPrompt = "focus_prompt",
    QueueSteer = "queue_steer",
    QueueNext = "queue_next",
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Builtin(Builtin),
    /// A slash command line, without the slash.
    Command(String),
    /// A Lua function, by callback id. Returning `false` passes the key on.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup {
    pub action: Option<Action>,
    pub prefix: bool,
}

#[derive(Debug, Clone)]
struct ContextSpec {
    fallbacks: Vec<Context>,
    priority: i32,
}

#[derive(Debug)]
pub struct Keymaps {
    maps: HashMap<(Context, Vec<Key>), Action>,
    contexts: HashMap<Context, ContextSpec>,
}

impl Default for Keymaps {
    fn default() -> Self {
        let mut contexts = HashMap::new();
        contexts.insert(
            Context::Main,
            ContextSpec {
                fallbacks: Vec::new(),
                priority: 0,
            },
        );
        for builtin in [Context::Popup, Context::Panel] {
            contexts.insert(
                builtin,
                ContextSpec {
                    fallbacks: Vec::new(),
                    priority: 0,
                },
            );
        }
        Self {
            maps: HashMap::new(),
            contexts,
        }
    }
}

impl Keymaps {
    /// Define or update a context. A newly-created named context falls back to
    /// `main` unless callers provide an explicit fallback list.
    pub fn define(
        &mut self,
        ctx: Context,
        fallbacks: Option<Vec<Context>>,
        priority: Option<i32>,
    ) -> Result<(), String> {
        if let Some(fallbacks) = &fallbacks {
            if fallbacks.iter().any(|fallback| fallback == &ctx) {
                return Err(format!("context {} cannot fall back to itself", ctx.name()));
            }
            for fallback in fallbacks {
                self.ensure_context(fallback.clone());
            }
        }
        let default_fallbacks = if ctx.is_builtin() {
            Vec::new()
        } else {
            vec![Context::Main]
        };
        let spec = self.contexts.entry(ctx).or_insert(ContextSpec {
            fallbacks: default_fallbacks,
            priority: 0,
        });
        if let Some(fallbacks) = fallbacks {
            spec.fallbacks = fallbacks;
        }
        if let Some(priority) = priority {
            spec.priority = priority;
        }
        Ok(())
    }

    pub fn has_context(&self, ctx: &Context) -> bool {
        self.contexts.contains_key(ctx)
    }

    pub fn remove_context(&mut self, ctx: &Context) -> Result<(), String> {
        if ctx.is_builtin() {
            return Err(format!("cannot delete built-in context {}", ctx.name()));
        }
        if self.contexts.remove(ctx).is_none() {
            return Err(format!("unknown context {}", ctx.name()));
        }
        self.maps.retain(|(context, _), _| context != ctx);
        for spec in self.contexts.values_mut() {
            spec.fallbacks.retain(|fallback| fallback != ctx);
        }
        Ok(())
    }

    pub fn set(&mut self, ctx: Context, key: &str, action: Action) -> Result<(), String> {
        self.ensure_context(ctx.clone());
        self.maps.insert((ctx, keys::parse_sequence(key)?), action);
        Ok(())
    }

    pub fn del(&mut self, ctx: Context, key: &str) -> Result<(), String> {
        self.maps
            .remove(&(ctx, keys::parse_sequence(key)?))
            .map(|_| ())
            .ok_or_else(|| format!("no mapping for {key}"))
    }

    #[cfg(test)]
    /// Compatibility lookup for a single key in exactly one context.
    pub fn get(&self, ctx: Context, key: Key) -> Option<&Action> {
        self.maps.get(&(ctx, vec![key]))
    }

    /// Resolve a sequence in a context and its fallback graph. An action and a
    /// prefix may both be present when a short mapping is ambiguous with a
    /// longer one.
    pub fn lookup(&self, ctx: &Context, sequence: &[Key]) -> Lookup {
        let order = self.search_order(ctx);
        let action = order.iter().find_map(|context| {
            self.maps
                .get(&(context.clone(), sequence.to_vec()))
                .cloned()
        });
        Lookup {
            action,
            // A prefix in a more specific context must keep an exact fallback
            // action ambiguous until the caller's timeout expires.
            prefix: order
                .iter()
                .any(|context| self.has_longer_mapping(context, sequence)),
        }
    }

    fn ensure_context(&mut self, ctx: Context) {
        if self.contexts.contains_key(&ctx) {
            return;
        }
        let fallbacks = if ctx.is_builtin() {
            Vec::new()
        } else {
            vec![Context::Main]
        };
        self.contexts.insert(
            ctx,
            ContextSpec {
                fallbacks,
                priority: 0,
            },
        );
    }

    fn search_order(&self, ctx: &Context) -> Vec<Context> {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        self.visit(ctx, &mut seen, &mut out);
        out
    }

    fn visit(&self, ctx: &Context, seen: &mut Vec<Context>, out: &mut Vec<Context>) {
        if seen.iter().any(|seen| seen == ctx) {
            return;
        }
        seen.push(ctx.clone());
        out.push(ctx.clone());
        let mut fallbacks = self
            .contexts
            .get(ctx)
            .map(|spec| spec.fallbacks.clone())
            .unwrap_or_default();
        fallbacks.sort_by(|a, b| {
            self.priority(b)
                .cmp(&self.priority(a))
                .then_with(|| a.name().cmp(b.name()))
        });
        for fallback in fallbacks {
            self.visit(&fallback, seen, out);
        }
    }

    fn priority(&self, ctx: &Context) -> i32 {
        self.contexts.get(ctx).map_or(0, |spec| spec.priority)
    }

    fn has_longer_mapping(&self, ctx: &Context, sequence: &[Key]) -> bool {
        self.maps.keys().any(|(context, mapped)| {
            context == ctx && mapped.len() > sequence.len() && mapped.starts_with(sequence)
        })
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
        assert_eq!(
            Context::from_name("picker"),
            Some(Context::Named("picker".into()))
        );
        assert!(Context::from_name("bad context").is_none());
    }

    #[test]
    fn sequences_and_fallbacks_report_ambiguous_prefixes() {
        let mut k = Keymaps::default();
        let review = Context::from_name("review").unwrap();
        k.define(review.clone(), Some(vec![Context::Main]), Some(10))
            .unwrap();
        k.set(review.clone(), "g g", Action::parse("sessions").unwrap())
            .unwrap();
        k.set(Context::Main, "x", Action::parse("dismiss").unwrap())
            .unwrap();

        let g = [keys::parse("g").unwrap()];
        assert_eq!(
            k.lookup(&review, &g),
            Lookup {
                action: None,
                prefix: true
            }
        );
        let gg = [keys::parse("g").unwrap(), keys::parse("g").unwrap()];
        assert_eq!(
            k.lookup(&review, &gg),
            Lookup {
                action: Some(Action::Builtin(Builtin::Sessions)),
                prefix: false,
            }
        );
        let x = [keys::parse("x").unwrap()];
        assert_eq!(
            k.lookup(&review, &x),
            Lookup {
                action: Some(Action::Builtin(Builtin::Dismiss)),
                prefix: false,
            }
        );
    }

    #[test]
    fn fallback_priority_is_deterministic() {
        let mut k = Keymaps::default();
        let root = Context::from_name("root").unwrap();
        let low = Context::from_name("low").unwrap();
        let high = Context::from_name("high").unwrap();
        k.define(root.clone(), Some(vec![low.clone(), high.clone()]), None)
            .unwrap();
        k.define(low.clone(), None, Some(1)).unwrap();
        k.define(high.clone(), None, Some(2)).unwrap();
        k.set(low, "x", Action::parse("dismiss").unwrap()).unwrap();
        k.set(high, "x", Action::parse("quit").unwrap()).unwrap();
        assert_eq!(
            k.lookup(&root, &[keys::parse("x").unwrap()]).action,
            Some(Action::Builtin(Builtin::Quit))
        );
    }
}
