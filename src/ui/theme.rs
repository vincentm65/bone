use ratatui::style::Color;

/// App-wide color theme.
pub struct Theme {
    pub user_msg: Color,
    pub user_msg_bg: Color,
    pub status_text: Color,
    pub input_border: Color,
    pub system_msg: Color,
    pub approval_safe: Color,
    pub approval_danger: Color,
    pub tool_call: Color,
    pub tool_error: Color,
    pub diff_removed: Color,
    pub diff_added: Color,
    pub thinking: Color,
    pub tab_active: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user_msg: Color::White,
            user_msg_bg: Color::Rgb(48, 48, 48),
            status_text: Color::DarkGray,
            input_border: Color::DarkGray,
            system_msg: Color::White,
            approval_safe: Color::Rgb(120, 179, 115), // #78B373 — safe green (vmcode)
            approval_danger: Color::Rgb(224, 80, 80), // #E05050 — danger red (vmcode)
            tool_call: Color::DarkGray,
            tool_error: Color::Red,
            diff_removed: Color::Rgb(135, 1, 1),
            diff_added: Color::Rgb(0, 95, 0),
            thinking: Color::Rgb(140, 220, 220), // pastel cyan
            tab_active: Color::Rgb(140, 220, 220),
        }
    }
}

impl Theme {
    /// Apply a Lua theme snapshot, overriding defaults with set values.
    ///
    /// This is the UI boundary where raw color strings (stored in the snapshot)
    /// are parsed into `ratatui::style::Color` values.
    pub fn apply_snapshot(&mut self, snap: &crate::ext::snapshots::LuaThemeSnapshot) {
        macro_rules! apply {
            ($field:ident) => {
                if let Some(ref s) = snap.$field {
                    if let Some(c) = crate::ui::color::parse_color(s) {
                        self.$field = c;
                    } else {
                        eprintln!(
                            "bone-lua warn: invalid theme color for {}: {s}",
                            stringify!($field)
                        );
                    }
                }
            };
        }
        apply!(user_msg);
        apply!(user_msg_bg);
        apply!(status_text);
        apply!(input_border);
        apply!(system_msg);
        apply!(approval_safe);
        apply!(approval_danger);
        apply!(tool_call);
        apply!(tool_error);
        apply!(diff_removed);
        apply!(diff_added);
        apply!(thinking);
        apply!(tab_active);
    }

    /// Set a single named highlight group at runtime (`bone.api.ui.set_highlight`).
    ///
    /// The group `name` is one of the [`Theme`] field names (`user_msg`,
    /// `input_border`, `status_text`, …). `color` is a hex/named string, or
    /// `None` to reset that group to its built-in default. Unknown names and
    /// unparseable colors warn and are ignored. Returns `true` when a field
    /// changed, so the caller knows to redraw.
    pub fn set_highlight(&mut self, name: &str, color: Option<&str>) -> bool {
        let default = Theme::default();
        macro_rules! set {
            ($field:ident) => {{
                match color {
                    None => {
                        self.$field = default.$field;
                        true
                    }
                    Some(s) => match crate::ui::color::parse_color(s) {
                        Some(c) => {
                            self.$field = c;
                            true
                        }
                        None => {
                            eprintln!("bone-lua warn: invalid highlight color for {name}: {s}");
                            false
                        }
                    },
                }
            }};
        }
        match name {
            "user_msg" => set!(user_msg),
            "user_msg_bg" => set!(user_msg_bg),
            "status_text" => set!(status_text),
            "input_border" => set!(input_border),
            "system_msg" => set!(system_msg),
            "approval_safe" => set!(approval_safe),
            "approval_danger" => set!(approval_danger),
            "tool_call" => set!(tool_call),
            "tool_error" => set!(tool_error),
            "diff_removed" => set!(diff_removed),
            "diff_added" => set!(diff_added),
            "thinking" => set!(thinking),
            "tab_active" => set!(tab_active),
            other => {
                eprintln!("bone-lua warn: unknown highlight group: {other}");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_highlight_sets_resets_and_rejects() {
        let mut theme = Theme::default();
        let original = theme.input_border;

        // A valid color is applied and reports a change.
        assert!(theme.set_highlight("input_border", Some("#ff0000")));
        assert_eq!(theme.input_border, Color::Rgb(255, 0, 0));

        // None resets to the built-in default.
        assert!(theme.set_highlight("input_border", None));
        assert_eq!(theme.input_border, original);

        // Unknown group and unparseable color report no change.
        assert!(!theme.set_highlight("nope", Some("#ffffff")));
        assert!(!theme.set_highlight("input_border", Some("not-a-color")));
    }
}
