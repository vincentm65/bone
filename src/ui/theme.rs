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
            tab_active: Color::Cyan,
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
                        eprintln!("bone-lua warn: invalid theme color for {}: {s}", stringify!($field));
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
}
