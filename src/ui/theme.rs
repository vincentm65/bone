use ratatui::style::Color;

/// App-wide color theme.
pub struct Theme {
    pub user_msg: Color,
    pub user_msg_bg: Color,
    pub status_text: Color,
    pub input_border: Color,
    pub system_msg: Color,
    pub approval_safe: Color,
    pub approval_edits: Color,
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
            approval_edits: Color::Rgb(184, 160, 64), // #B8A040 — muted gold (vmcode)
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
