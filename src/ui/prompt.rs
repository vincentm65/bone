/// User's response to a blocking prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Advise,
    Cancel,
}

/// A blocking selection prompt rendered between the transcript and input.
pub struct Prompt {
    pub title: String,
    pub options: Vec<String>,
    pub selected: usize,
    /// Raw command text for bash approval prompts (enables preview/peek).
    pub full_command: Option<String>,
    /// When true, show all command lines instead of the truncated preview.
    pub peek_mode: bool,
}

impl Prompt {
    pub fn new(title: impl Into<String>, options: Vec<impl Into<String>>) -> Self {
        Self {
            title: title.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            full_command: None,
            peek_mode: false,
        }
    }

    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    pub fn left(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn right(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
    }

    /// Toggle peek mode for command preview.
    pub fn toggle_peek(&mut self) {
        if self.full_command.is_some() {
            self.peek_mode = !self.peek_mode;
        }
    }

    pub fn decision(&self) -> Decision {
        match self.selected {
            0 => Decision::Accept,
            1 => Decision::Advise,
            _ => Decision::Cancel,
        }
    }
}
