use ratatui::style::Color;

/// App-wide color theme.
pub struct Theme {
    pub user_msg: Color,
    pub status_text: Color,
    pub input_border: Color,
    pub system_msg: Color,
    pub approval_safe: Color,
    pub approval_edits: Color,
    pub approval_danger: Color,
    pub tool_call: Color,
    pub tool_error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user_msg: Color::Gray,
            status_text: Color::White,
            input_border: Color::DarkGray,
            system_msg: Color::DarkGray,
            approval_safe: Color::Rgb(92, 214, 140), // muted green
            approval_edits: Color::LightYellow,
            approval_danger: Color::Rgb(232, 120, 120), // muted red
            tool_call: Color::DarkGray,
            tool_error: Color::Red,
        }
    }
}
