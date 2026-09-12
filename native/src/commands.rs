//! Slash-command metadata and inline autocomplete, mirrored from the TUI.
//!
//! The canonical built-in list lives in `bone_core::commands`, but the native
//! client never links `bone-core` (see `NATIVE_APP_PLAN.md`). `BUILTINS` is
//! therefore mirrored here, and the `builtins_match_core_source` fixture test
//! parses `core/src/commands.rs` so drift fails the native build.

use std::collections::BTreeMap;

/// Built-in slash commands as (name, description) pairs. Must stay identical to
/// `bone_core::commands::BUILTINS`; the fixture test enforces that.
pub const BUILTINS: &[(&str, &str)] = &[
    ("catalog", "browse & install optional tools and commands"),
    ("clear", "clear chat history"),
    ("config", "change application settings"),
    ("edit", "open system editor for input"),
    ("e", "open system editor for input"),
    ("exit", "exit bone"),
    ("help", "show this message"),
    (
        "incognito",
        "pause saving this and future chats to the database (toggle)",
    ),
    ("model", "set or show model (/model <name>)"),
    ("new", "clear chat history (alias for /clear)"),
    ("provider", "pick or switch provider (/provider <name>)"),
    ("quit", "exit bone"),
    ("setup", "re-run the onboarding setup wizard"),
    ("stats", "open full-screen token stats dashboard"),
    ("update", "check and apply bone updates"),
];

/// Whether `name` is a protected built-in (cannot be overridden by Lua).
/// Test-only helper: the native client never filters Lua commands locally.
#[cfg(test)]
pub fn is_protected_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(builtin, _)| *builtin == name)
}

/// Built-ins plus daemon-advertised Lua commands, name-sorted and deduped.
/// Built-in descriptions win on a name conflict, matching the TUI's
/// `commands::merge_commands`.
pub fn merge_commands(advertised: &[(String, String)]) -> Vec<(String, String)> {
    let mut commands: BTreeMap<String, String> = BUILTINS
        .iter()
        .map(|(name, description)| ((*name).into(), (*description).into()))
        .collect();
    for (name, description) in advertised {
        commands
            .entry(name.clone())
            .or_insert_with(|| description.clone());
    }
    commands.into_iter().collect()
}

/// Render the `/help` reference: the merged command list plus the native input
/// and window shortcut cheat sheets. Plain text (no ANSI) because the native
/// transcript renders Markdown, not a terminal; the command section mirrors
/// `tui::ui::commands::help` while the shortcuts reflect the desktop app's
/// actual bindings.
pub fn help(advertised: &[(String, String)]) -> String {
    let commands = merge_commands(advertised);
    let max_name = commands
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0)
        .max(10);
    let mut lines = vec!["Commands".to_string()];
    for (name, description) in commands {
        lines.push(format!("  /{name:<max_name$} — {description}"));
    }
    lines.push("  :           — run a shell command inline (: <command>)".to_string());
    lines.push(String::new());
    lines.push("Input shortcuts".to_string());
    lines.push("  Enter        — send; queue while a turn is running".to_string());
    lines.push("  Ctrl+Enter   — steer the running turn".to_string());
    lines.push("  Shift+Enter  — queue for after the running turn".to_string());
    lines.push("  Ctrl+Up      — recall the previous prompt".to_string());
    lines.push("  Ctrl+Down    — recall the next prompt".to_string());
    lines.push("  Ctrl+D       — clear the queued prompts (empty composer)".to_string());
    lines.push("  Esc          — dismiss a picker; keep the draft".to_string());
    lines.push("  Copy / Cut   — standard text editing".to_string());
    lines.push("  /edit        — open the draft in a larger editor".to_string());
    lines.push(String::new());
    lines.push("Window shortcuts".to_string());
    lines.push("  Ctrl+T        — new conversation".to_string());
    lines.push("  Ctrl+W        — close conversation".to_string());
    lines.push("  Ctrl+PageUp   — previous conversation".to_string());
    lines.push("  Ctrl+PageDown — next conversation".to_string());
    lines.push("  Ctrl+1…9      — select conversation".to_string());
    lines.push("  Ctrl+\\        — split right".to_string());
    lines.push("  Ctrl+Shift+\\  — split down".to_string());
    lines.push("  Ctrl+Shift+N  — new window".to_string());
    lines.push("  Ctrl+K        — command palette".to_string());
    lines.push("  Ctrl+P        — switch task".to_string());
    lines.join("\n")
}

/// The command-name fragment after a leading `/`, while the user is still
/// typing it. `None` when the buffer does not start with `/` or once arguments
/// (whitespace) begin — mirrors the TUI, which hides autocomplete at that point.
pub fn slash_query(buffer: &str) -> Option<&str> {
    let query = buffer.strip_prefix('/')?;
    if query.contains(char::is_whitespace) {
        return None;
    }
    Some(query)
}

/// Maximum number of suggestions shown at once (mirrors the TUI dropdown).
pub const MAX_VISIBLE: usize = 5;

/// Inline autocomplete state for the composer's `/` command picker.
#[derive(Debug, Clone)]
pub struct AutocompleteState {
    /// All commands, name-sorted and deduped, rebuilt when the set changes.
    all_commands: Vec<(String, String)>,
    /// Currently filtered matches.
    pub matches: Vec<(String, String)>,
    /// Index of the highlighted item in `matches`.
    pub selected: usize,
    /// Top index of the visible window within `matches`.
    pub scroll_offset: usize,
    /// Last applied query, so per-frame refreshes do not reset the selection.
    query: String,
}

impl AutocompleteState {
    pub fn new(all_commands: Vec<(String, String)>) -> Self {
        let mut all_commands = all_commands;
        all_commands.sort_by(|a, b| a.0.cmp(&b.0));
        all_commands.dedup_by(|a, b| a.0 == b.0);
        let matches = all_commands.clone();
        Self {
            all_commands,
            matches,
            selected: 0,
            scroll_offset: 0,
            query: String::new(),
        }
    }

    /// The command set this state was built from, so the caller can rebuild when
    /// the daemon advertises new commands.
    pub fn all_commands(&self) -> &[(String, String)] {
        &self.all_commands
    }

    /// Re-filter on a changed query. No-op when `query` is unchanged, which
    /// keeps arrow-key selection stable across per-frame refreshes.
    pub fn update(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query.clear();
        self.query.push_str(query);
        let needle = query.to_lowercase();
        self.matches = self
            .all_commands
            .iter()
            .filter(|(name, _)| name.to_lowercase().starts_with(&needle))
            .cloned()
            .collect();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Move selection up, wrapping to the bottom.
    pub fn up(&mut self) {
        if !self.matches.is_empty() {
            self.selected = if self.selected > 0 {
                self.selected - 1
            } else {
                self.matches.len() - 1
            };
            self.clamp_scroll();
        }
    }

    /// Move selection down, wrapping to the top.
    pub fn down(&mut self) {
        if !self.matches.is_empty() {
            self.selected = if self.selected + 1 < self.matches.len() {
                self.selected + 1
            } else {
                0
            };
            self.clamp_scroll();
        }
    }

    fn clamp_scroll(&mut self) {
        let max_offset = self.matches.len().saturating_sub(MAX_VISIBLE);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + MAX_VISIBLE {
            self.scroll_offset = self.selected.saturating_sub(MAX_VISIBLE - 1);
        }
        self.scroll_offset = self.scroll_offset.min(max_offset);
    }

    /// The highlighted command name, if any.
    pub fn selected_command(&self) -> Option<&str> {
        self.matches
            .get(self.selected)
            .map(|(name, _)| name.as_str())
    }

    /// Number of matches below the visible window.
    pub fn more_count(&self) -> usize {
        self.matches
            .len()
            .saturating_sub(self.scroll_offset + MAX_VISIBLE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the (name, description) pairs out of `core/src/commands.rs` so the
    /// mirrored `BUILTINS` above cannot silently drift from the canonical list.
    fn parse_core_builtins(source: &str) -> Vec<(String, String)> {
        let header = "BUILTINS: &[(&str, &str)] = &[";
        let start = source.find(header).expect("core BUILTINS const") + header.len();
        let rest = &source[start..];
        let end = rest.find("\n];").expect("core BUILTINS terminator");
        let mut literals = Vec::new();
        let mut chars = rest[..end].chars().peekable();
        while let Some(c) = chars.next() {
            if c != '"' {
                continue;
            }
            let mut value = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => break,
                    _ => value.push(c),
                }
            }
            literals.push(value);
        }
        literals
            .chunks_exact(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect()
    }

    #[test]
    fn builtins_match_core_source() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../core/src/commands.rs");
        let source = std::fs::read_to_string(path).expect("read core/src/commands.rs");
        let parsed = parse_core_builtins(&source);
        let mirrored: Vec<(String, String)> = BUILTINS
            .iter()
            .map(|(name, desc)| (name.to_string(), desc.to_string()))
            .collect();
        assert_eq!(
            parsed, mirrored,
            "native BUILTINS drifted from bone_core::commands::BUILTINS"
        );
    }

    #[test]
    fn slash_query_only_while_typing_the_name() {
        assert_eq!(slash_query("/he"), Some("he"));
        assert_eq!(slash_query("/"), Some(""));
        assert_eq!(slash_query("/help me"), None);
        assert_eq!(slash_query("hello"), None);
        assert_eq!(slash_query("/help\nmore"), None);
    }

    #[test]
    fn merge_prefers_builtin_descriptions_and_adds_lua() {
        let advertised = vec![
            ("help".to_string(), "overridden".to_string()),
            ("custom".to_string(), "a Lua command".to_string()),
        ];
        let merged = merge_commands(&advertised);
        assert!(merged.windows(2).all(|w| w[0].0 < w[1].0), "sorted by name");
        let help = merged.iter().find(|(n, _)| n == "help").unwrap();
        assert_eq!(help.1, "show this message");
        assert!(
            merged
                .iter()
                .any(|(n, d)| n == "custom" && d == "a Lua command")
        );
        assert!(is_protected_builtin("help"));
        assert!(!is_protected_builtin("custom"));
    }

    #[test]
    fn help_lists_commands_and_shortcuts() {
        let text = help(&[]);
        assert!(text.contains("Commands"), "has a commands section");
        assert!(text.contains("/help"), "lists the help command");
        assert!(text.contains("/catalog"), "lists built-ins");
        assert!(
            text.contains("run a shell command inline"),
            "documents the inline shell prefix"
        );
        assert!(text.contains("Input shortcuts"), "has input shortcuts");
        assert!(text.contains("Ctrl+Enter"), "documents send shortcut");
        assert!(text.contains("Window shortcuts"), "has window shortcuts");
        assert!(text.contains("Ctrl+T"), "documents new-tab shortcut");
        assert!(
            text.contains("Ctrl+Shift+N"),
            "documents new-window shortcut"
        );
        assert!(text.contains("Ctrl+K"), "documents command palette");
        assert!(text.contains("Ctrl+P"), "documents switch task");
        assert!(text.contains("split right"), "documents split right");
        assert!(text.contains("split down"), "documents split down");
        assert!(
            !text.contains("toggle split view"),
            "no stale single-split wording"
        );
        // Lua-advertised commands are merged into the listing.
        let text = help(&[("custom".into(), "a Lua command".into())]);
        assert!(text.contains("/custom"), "lists advertised Lua commands");
    }

    #[test]
    fn autocomplete_filters_and_navigates() {
        let mut ac = AutocompleteState::new(merge_commands(&[]));
        ac.update("c");
        assert!(ac.matches.iter().all(|(n, _)| n.starts_with('c')));
        assert!(ac.matches.len() >= 2);
        ac.update("c"); // unchanged query must not reset selection
        ac.down();
        assert_eq!(ac.selected, 1);
        ac.up();
        assert_eq!(ac.selected, 0);
        ac.up();
        assert_eq!(ac.selected, ac.matches.len() - 1);
    }

    #[test]
    fn autocomplete_window_scrolls_and_reports_more() {
        let mut ac = AutocompleteState::new(merge_commands(&[]));
        ac.update("");
        assert!(ac.matches.len() > MAX_VISIBLE);
        for _ in 0..MAX_VISIBLE {
            ac.down();
        }
        assert_eq!(ac.scroll_offset, 1);
        assert!(ac.more_count() > 0);
        let name = ac.selected_command().expect("selection");
        assert!(ac.matches.iter().any(|(n, _)| n == name));
    }
}
