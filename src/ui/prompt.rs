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
}

impl Prompt {
    pub fn new(title: impl Into<String>, options: Vec<impl Into<String>>) -> Self {
        Self {
            title: title.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
        }
    }

    /// Extra rows this prompt adds to the bottom pane (title + options).
    pub fn height(&self) -> u16 {
        1 + self.options.len().min(12) as u16
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

    pub fn decision(&self) -> Decision {
        match self.selected {
            0 => Decision::Accept,
            1 => Decision::Advise,
            _ => Decision::Cancel,
        }
    }
}
