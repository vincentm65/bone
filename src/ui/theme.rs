use ratatui::style::Color;

/// App-wide color theme.
pub struct Theme {
    pub user_msg: Color,
    pub status_text: Color,
    pub input_border: Color,
    pub system_msg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            user_msg: Color::Gray,
            status_text: Color::White,
            input_border: Color::DarkGray,
            system_msg: Color::DarkGray,
        }
    }
}
