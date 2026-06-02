/// User's response to a blocking prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Accept,
    Advise(String),
    Cancel,
}

/// A blocking selection prompt rendered between the transcript and input.
pub struct Prompt {
    pub title: String,
    pub options: Vec<String>,
    pub selected: usize,
    pub scroll: usize,
    pub visible_rows: usize,
    pub hint: Option<String>,
    /// Raw command text for shell approval prompts (enables preview/peek).
    pub full_command: Option<String>,
    /// When true, show all command lines instead of the truncated preview.
    pub peek_mode: bool,
    /// Tab labels for multi-section prompts (e.g. Config / Subagent).
    pub tabs: Vec<String>,
    /// Index of the currently active tab.
    pub active_tab: usize,
}

impl Prompt {
    pub fn new(title: impl Into<String>, options: Vec<impl Into<String>>) -> Self {
        Self {
            title: title.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected: 0,
            scroll: 0,
            visible_rows: 10,
            hint: None,
            full_command: None,
            peek_mode: false,
            tabs: Vec::new(),
            active_tab: 0,
        }
    }

    pub fn up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
        self.ensure_visible();
    }

    pub fn down(&mut self) {
        if self.selected + 1 < self.options.len() {
            self.selected += 1;
        }
        self.ensure_visible();
    }

    pub fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.visible_rows.max(1));
        self.ensure_visible();
    }

    pub fn page_down(&mut self) {
        self.selected =
            (self.selected + self.visible_rows.max(1)).min(self.options.len().saturating_sub(1));
        self.ensure_visible();
    }

    pub fn visible_options(&self) -> std::ops::Range<usize> {
        self.scroll..(self.scroll + self.visible_rows).min(self.options.len())
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.visible_rows.max(1) {
            self.scroll = self.selected + 1 - self.visible_rows.max(1);
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
            1 => Decision::Advise(String::new()),
            _ => Decision::Cancel,
        }
    }
}
