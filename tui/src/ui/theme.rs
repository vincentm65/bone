//! App-wide ratatui color theme.

use ratatui::style::Color;
use syntect::highlighting::{
    Color as SyColor, FontStyle, StyleModifier, Theme as SyntectTheme, ThemeItem,
};

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Option<Color>,
    pub fg: Color,
    pub muted: Color,
    pub subtle: Color,
    pub border: Color,
    pub accent: Color,
    pub good: Color,
    pub warn: Color,
    pub error: Color,
    pub selection: Color,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            bg: None,
            fg: Color::White,
            muted: Color::DarkGray,
            subtle: Color::Rgb(48, 48, 48),
            border: Color::DarkGray,
            accent: Color::Rgb(140, 220, 220),
            good: Color::Rgb(120, 179, 115),
            warn: Color::Rgb(215, 186, 125),
            error: Color::Rgb(224, 80, 80),
            selection: Color::Rgb(48, 48, 48),
        }
    }
}

/// App-wide color theme.
pub struct Theme {
    pub palette: Palette,
    pub user_msg: Color,
    pub user_msg_bg: Color,
    pub status_text: Color,
    pub input_border: Color,
    pub input_bg: Color,
    pub input_prefix: Color,
    pub input_cursor: Color,
    pub system_msg: Color,
    pub approval_safe: Color,
    pub approval_danger: Color,
    pub tool_call: Color,
    pub tool_error: Color,
    pub shell_program: Color,
    pub shell_separator: Color,
    pub shell_redirect: Color,
    pub shell_flag: Color,
    pub shell_string: Color,
    pub shell_variable: Color,
    pub shell_comment: Color,
    pub shell_path: Color,
    pub diff_removed: Color,
    pub diff_added: Color,
    pub thinking: Color,
    pub tab_active: Color,
    // Code-block syntax highlighting (chat transcript). Defaults replicate the
    // VS Code Dark+ palette previously embedded as a .tmTheme file.
    pub syntax_text: Color,
    pub syntax_comment: Color,
    pub syntax_string: Color,
    pub syntax_number: Color,
    pub syntax_constant: Color,
    pub syntax_escape: Color,
    pub syntax_regex: Color,
    pub syntax_keyword: Color,
    pub syntax_keyword_control: Color,
    pub syntax_type: Color,
    pub syntax_function: Color,
    pub syntax_variable: Color,
    pub syntax_tag: Color,
    pub syntax_attribute: Color,
    pub syntax_punctuation: Color,
    pub syntax_subtle: Color,
    pub syntax_markup: Color,
    pub syntax_invalid: Color,
    /// syntect theme derived from the `syntax_*` fields. Kept private so it can
    /// only drift from those fields through `rebuild_code`, which every
    /// mutation path (`apply_snapshot`, `set_highlight`) calls.
    code: SyntectTheme,
}

impl Default for Theme {
    fn default() -> Self {
        let palette = Palette::default();
        let mut theme = Self {
            palette,
            user_msg: palette.fg,
            user_msg_bg: palette.selection,
            status_text: palette.muted,
            input_border: palette.border,
            input_bg: palette.selection,
            input_prefix: palette.fg,
            input_cursor: palette.fg,
            system_msg: palette.fg,
            approval_safe: palette.good,
            approval_danger: palette.error,
            tool_call: palette.muted,
            tool_error: palette.error,
            shell_program: Color::Rgb(180, 200, 150),
            shell_separator: Color::Rgb(90, 90, 90),
            shell_redirect: Color::Rgb(120, 120, 120),
            shell_flag: Color::Rgb(150, 180, 220),
            shell_string: Color::Rgb(200, 170, 120),
            shell_variable: Color::Rgb(180, 160, 220),
            shell_comment: Color::DarkGray,
            shell_path: Color::Rgb(140, 190, 190),
            diff_removed: Color::Rgb(135, 1, 1),
            diff_added: Color::Rgb(0, 95, 0),
            thinking: palette.accent,
            tab_active: palette.accent,
            syntax_text: Color::Rgb(0xD4, 0xD4, 0xD4),
            syntax_comment: Color::Rgb(0x6A, 0x99, 0x55),
            syntax_string: Color::Rgb(0xCE, 0x91, 0x78),
            syntax_number: Color::Rgb(0xB5, 0xCE, 0xA8),
            syntax_constant: Color::Rgb(0x56, 0x9C, 0xD6),
            syntax_escape: Color::Rgb(0xD7, 0xBA, 0x7D),
            syntax_regex: Color::Rgb(0x64, 0x66, 0x95),
            syntax_keyword: Color::Rgb(0x56, 0x9C, 0xD6),
            syntax_keyword_control: Color::Rgb(0xC5, 0x86, 0xC0),
            syntax_type: Color::Rgb(0x4E, 0xC9, 0xB0),
            syntax_function: Color::Rgb(0xDC, 0xDC, 0xAA),
            syntax_variable: Color::Rgb(0x9C, 0xDC, 0xFE),
            syntax_tag: Color::Rgb(0x56, 0x9C, 0xD6),
            syntax_attribute: Color::Rgb(0x9C, 0xDC, 0xFE),
            syntax_punctuation: Color::Rgb(0xD4, 0xD4, 0xD4),
            syntax_subtle: Color::Rgb(0x80, 0x80, 0x80),
            syntax_markup: Color::Rgb(0x56, 0x9C, 0xD6),
            syntax_invalid: Color::Rgb(0xF4, 0x47, 0x47),
            code: SyntectTheme::default(),
        };
        theme.rebuild_code();
        theme
    }
}

/// Approximate a ratatui palette color as RGB for syntect. Named ANSI colors
/// use the VS Code terminal defaults; `parse_color` produces `Rgb` for hex
/// input, so these only matter for named-color theme values.
fn to_syntect(c: Color) -> SyColor {
    let (r, g, b) = match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0x00, 0x00, 0x00),
        Color::Red => (0xCD, 0x31, 0x31),
        Color::Green => (0x0D, 0xBC, 0x79),
        Color::Yellow => (0xE5, 0xE5, 0x10),
        Color::Blue => (0x24, 0x72, 0xC8),
        Color::Magenta => (0xBC, 0x3F, 0xBC),
        Color::Cyan => (0x11, 0xA8, 0xCD),
        Color::Gray => (0xC0, 0xC0, 0xC0),
        Color::DarkGray => (0x80, 0x80, 0x80),
        Color::LightRed => (0xF1, 0x4C, 0x4C),
        Color::LightGreen => (0x23, 0xD1, 0x8B),
        Color::LightYellow => (0xF5, 0xF5, 0x43),
        Color::LightBlue => (0x3B, 0x8E, 0xEA),
        Color::LightMagenta => (0xD6, 0x70, 0xD6),
        Color::LightCyan => (0x29, 0xB8, 0xDB),
        Color::White => (0xFF, 0xFF, 0xFF),
        Color::Indexed(_) | Color::Reset => (0xD4, 0xD4, 0xD4),
    };
    SyColor { r, g, b, a: 0xFF }
}

fn scope_item(scopes: &str, fg: Option<SyColor>, font_style: Option<FontStyle>) -> ThemeItem {
    ThemeItem {
        scope: scopes.parse().expect("static scope selector parses"),
        style: StyleModifier {
            foreground: fg,
            background: None,
            font_style,
        },
    }
}

impl Theme {
    /// The syntect theme for code-block highlighting, derived from the
    /// `syntax_*` fields.
    pub fn code(&self) -> &SyntectTheme {
        &self.code
    }

    /// Rebuild the cached syntect theme from the `syntax_*` fields. Called at
    /// every mutation point rather than at render time: scope-selector parsing
    /// goes through syntect's global scope repo and `render_markdown` runs per
    /// message per frame.
    fn rebuild_code(&mut self) {
        let fg = |c: Color| Some(to_syntect(c));
        let mut code = SyntectTheme::default();
        code.settings.foreground = Some(to_syntect(self.syntax_text));
        code.scopes = vec![
            scope_item("comment", fg(self.syntax_comment), None),
            scope_item("string", fg(self.syntax_string), None),
            scope_item("constant.numeric", fg(self.syntax_number), None),
            scope_item(
                "constant.language, variable.language",
                fg(self.syntax_constant),
                None,
            ),
            scope_item("constant.character.escape", fg(self.syntax_escape), None),
            scope_item("constant.regexp", fg(self.syntax_regex), None),
            scope_item(
                "keyword, storage, meta.preprocessor",
                fg(self.syntax_keyword),
                None,
            ),
            scope_item("keyword.control", fg(self.syntax_keyword_control), None),
            scope_item(
                "entity.name.type, support.class, support.type",
                fg(self.syntax_type),
                None,
            ),
            scope_item(
                "entity.name.function, support.function, meta.decorator, storage.type.annotation",
                fg(self.syntax_function),
                None,
            ),
            scope_item(
                "variable, support.variable, entity.name.variable",
                fg(self.syntax_variable),
                None,
            ),
            scope_item("entity.name.tag", fg(self.syntax_tag), None),
            scope_item(
                "entity.other.attribute-name",
                fg(self.syntax_attribute),
                None,
            ),
            scope_item(
                "punctuation, keyword.operator",
                fg(self.syntax_punctuation),
                None,
            ),
            scope_item("punctuation.definition.tag", fg(self.syntax_subtle), None),
            scope_item(
                "markup.heading",
                fg(self.syntax_markup),
                Some(FontStyle::BOLD),
            ),
            scope_item("markup.bold", None, Some(FontStyle::BOLD)),
            scope_item("markup.italic", None, Some(FontStyle::ITALIC)),
            scope_item("invalid", fg(self.syntax_invalid), None),
        ];
        self.code = code;
    }

    fn resolve_color_ref(&self, value: &str) -> Option<Color> {
        match value {
            "bg" => self.palette.bg,
            "fg" => Some(self.palette.fg),
            "muted" => Some(self.palette.muted),
            "subtle" => Some(self.palette.subtle),
            "border" => Some(self.palette.border),
            "accent" => Some(self.palette.accent),
            "good" => Some(self.palette.good),
            "warn" => Some(self.palette.warn),
            "error" => Some(self.palette.error),
            "selection" => Some(self.palette.selection),
            _ => crate::ui::color::parse_color(value),
        }
    }

    fn derive_palette_roles(&mut self) {
        self.user_msg = self.palette.fg;
        self.user_msg_bg = self.palette.selection;
        self.status_text = self.palette.muted;
        self.input_border = self.palette.border;
        self.input_bg = self.palette.selection;
        self.input_prefix = self.palette.fg;
        self.input_cursor = self.palette.fg;
        self.system_msg = self.palette.fg;
        self.approval_safe = self.palette.good;
        self.approval_danger = self.palette.error;
        self.tool_call = self.palette.muted;
        self.tool_error = self.palette.error;
        self.thinking = self.palette.accent;
        self.tab_active = self.palette.accent;
    }

    fn set_named_color(&mut self, name: &str, color: Color) -> bool {
        match name {
            "user_msg" => self.user_msg = color,
            "user_msg_bg" => self.user_msg_bg = color,
            "status_text" => self.status_text = color,
            "input_border" => self.input_border = color,
            "input_bg" => self.input_bg = color,
            "input_prefix" => self.input_prefix = color,
            "input_cursor" => self.input_cursor = color,
            "system_msg" => self.system_msg = color,
            "approval_safe" => self.approval_safe = color,
            "approval_danger" => self.approval_danger = color,
            "tool_call" => self.tool_call = color,
            "tool_error" => self.tool_error = color,
            "shell_program" => self.shell_program = color,
            "shell_separator" => self.shell_separator = color,
            "shell_redirect" => self.shell_redirect = color,
            "shell_flag" => self.shell_flag = color,
            "shell_string" => self.shell_string = color,
            "shell_variable" => self.shell_variable = color,
            "shell_comment" => self.shell_comment = color,
            "shell_path" => self.shell_path = color,
            "diff_removed" => self.diff_removed = color,
            "diff_added" => self.diff_added = color,
            "thinking" => self.thinking = color,
            "tab_active" => self.tab_active = color,
            "syntax_text" => self.syntax_text = color,
            "syntax_comment" => self.syntax_comment = color,
            "syntax_string" => self.syntax_string = color,
            "syntax_number" => self.syntax_number = color,
            "syntax_constant" => self.syntax_constant = color,
            "syntax_escape" => self.syntax_escape = color,
            "syntax_regex" => self.syntax_regex = color,
            "syntax_keyword" => self.syntax_keyword = color,
            "syntax_keyword_control" => self.syntax_keyword_control = color,
            "syntax_type" => self.syntax_type = color,
            "syntax_function" => self.syntax_function = color,
            "syntax_variable" => self.syntax_variable = color,
            "syntax_tag" => self.syntax_tag = color,
            "syntax_attribute" => self.syntax_attribute = color,
            "syntax_punctuation" => self.syntax_punctuation = color,
            "syntax_subtle" => self.syntax_subtle = color,
            "syntax_markup" => self.syntax_markup = color,
            "syntax_invalid" => self.syntax_invalid = color,
            _ => return false,
        }
        true
    }

    fn apply_highlight_spec(&mut self, name: &str, spec: &crate::ext::snapshots::LuaStyleSpec) {
        match spec {
            crate::ext::snapshots::LuaStyleSpec::Color(s) => {
                if let Some(c) = self.resolve_color_ref(s) {
                    if !self.set_named_color(name, c) {
                        eprintln!("bone-lua warn: unknown highlight group: {name}");
                    }
                } else {
                    eprintln!("bone-lua warn: invalid highlight color for {name}: {s}");
                }
            }
            crate::ext::snapshots::LuaStyleSpec::Style { fg, bg, .. } => {
                if let Some(fg) = fg {
                    if let Some(c) = self.resolve_color_ref(fg) {
                        if !self.set_named_color(name, c) {
                            eprintln!("bone-lua warn: unknown highlight group: {name}");
                        }
                    } else {
                        eprintln!("bone-lua warn: invalid highlight fg for {name}: {fg}");
                    }
                }
                if let Some(bg) = bg {
                    if let Some(c) = self.resolve_color_ref(bg) {
                        let bg_name = match name {
                            "user_msg" => Some("user_msg_bg"),
                            other if other.ends_with("_bg") => Some(other),
                            _ => None,
                        };
                        if let Some(bg_name) = bg_name {
                            if !self.set_named_color(bg_name, c) {
                                eprintln!("bone-lua warn: unknown highlight bg group: {bg_name}");
                            }
                        } else {
                            eprintln!("bone-lua warn: highlight has no bg role: {name}");
                        }
                    } else {
                        eprintln!("bone-lua warn: invalid highlight bg for {name}: {bg}");
                    }
                }
            }
        }
    }

    /// Apply a Lua theme snapshot, overriding defaults with set values.
    ///
    /// This is the UI boundary where raw color strings (stored in the snapshot)
    /// are parsed into `ratatui::style::Color` values.
    pub fn apply_snapshot(&mut self, snap: &crate::ext::snapshots::LuaThemeSnapshot) {
        let mut theme = Theme::default();
        macro_rules! apply_palette {
            ($field:ident) => {
                if let Some(ref s) = snap.palette.$field {
                    if let Some(c) = theme.resolve_color_ref(s) {
                        theme.palette.$field = c;
                    } else {
                        eprintln!(
                            "bone-lua warn: invalid theme palette color for {}: {s}",
                            stringify!($field)
                        );
                    }
                }
            };
        }
        if let Some(ref s) = snap.palette.bg {
            match theme.resolve_color_ref(s) {
                Some(c) => theme.palette.bg = Some(c),
                None => eprintln!("bone-lua warn: invalid theme palette color for bg: {s}"),
            }
        }
        apply_palette!(fg);
        apply_palette!(muted);
        apply_palette!(subtle);
        apply_palette!(border);
        apply_palette!(accent);
        apply_palette!(good);
        apply_palette!(warn);
        apply_palette!(error);
        apply_palette!(selection);
        theme.derive_palette_roles();

        macro_rules! apply_ref {
            ($target:ident, $value:expr) => {
                if let Some(s) = $value.as_ref() {
                    if let Some(c) = theme.resolve_color_ref(s) {
                        theme.$target = c;
                    } else {
                        eprintln!(
                            "bone-lua warn: invalid theme color for {}: {s}",
                            stringify!($target)
                        );
                    }
                }
            };
        }

        apply_ref!(shell_program, snap.shell.program);
        apply_ref!(shell_separator, snap.shell.separator);
        apply_ref!(shell_redirect, snap.shell.redirect);
        apply_ref!(shell_flag, snap.shell.flag);
        apply_ref!(shell_string, snap.shell.string);
        apply_ref!(shell_variable, snap.shell.variable);
        apply_ref!(shell_comment, snap.shell.comment);
        apply_ref!(shell_path, snap.shell.path);

        apply_ref!(syntax_text, snap.syntax.text);
        apply_ref!(syntax_comment, snap.syntax.comment);
        apply_ref!(syntax_string, snap.syntax.string);
        apply_ref!(syntax_number, snap.syntax.number);
        apply_ref!(syntax_constant, snap.syntax.constant);
        apply_ref!(syntax_escape, snap.syntax.escape);
        apply_ref!(syntax_regex, snap.syntax.regex);
        apply_ref!(syntax_keyword, snap.syntax.keyword);
        apply_ref!(syntax_keyword_control, snap.syntax.keyword_control);
        apply_ref!(syntax_type, snap.syntax.r#type);
        apply_ref!(syntax_function, snap.syntax.function_name);
        apply_ref!(syntax_variable, snap.syntax.variable);
        apply_ref!(syntax_tag, snap.syntax.tag);
        apply_ref!(syntax_attribute, snap.syntax.attribute);
        apply_ref!(syntax_punctuation, snap.syntax.punctuation);
        apply_ref!(syntax_subtle, snap.syntax.subtle);
        apply_ref!(syntax_markup, snap.syntax.markup);
        apply_ref!(syntax_invalid, snap.syntax.invalid);

        apply_ref!(user_msg, snap.user_msg);
        apply_ref!(user_msg_bg, snap.user_msg_bg);
        apply_ref!(status_text, snap.status_text);
        apply_ref!(input_border, snap.input_border);
        apply_ref!(system_msg, snap.system_msg);
        apply_ref!(approval_safe, snap.approval_safe);
        apply_ref!(approval_danger, snap.approval_danger);
        apply_ref!(tool_call, snap.tool_call);
        apply_ref!(tool_error, snap.tool_error);
        apply_ref!(shell_program, snap.shell_program);
        apply_ref!(shell_separator, snap.shell_separator);
        apply_ref!(shell_redirect, snap.shell_redirect);
        apply_ref!(shell_flag, snap.shell_flag);
        apply_ref!(shell_string, snap.shell_string);
        apply_ref!(shell_variable, snap.shell_variable);
        apply_ref!(shell_comment, snap.shell_comment);
        apply_ref!(shell_path, snap.shell_path);
        apply_ref!(diff_removed, snap.diff_removed);
        apply_ref!(diff_added, snap.diff_added);
        apply_ref!(thinking, snap.thinking);
        apply_ref!(tab_active, snap.tab_active);
        apply_ref!(syntax_text, snap.syntax_text);
        apply_ref!(syntax_comment, snap.syntax_comment);
        apply_ref!(syntax_string, snap.syntax_string);
        apply_ref!(syntax_number, snap.syntax_number);
        apply_ref!(syntax_constant, snap.syntax_constant);
        apply_ref!(syntax_escape, snap.syntax_escape);
        apply_ref!(syntax_regex, snap.syntax_regex);
        apply_ref!(syntax_keyword, snap.syntax_keyword);
        apply_ref!(syntax_keyword_control, snap.syntax_keyword_control);
        apply_ref!(syntax_type, snap.syntax_type);
        apply_ref!(syntax_function, snap.syntax_function);
        apply_ref!(syntax_variable, snap.syntax_variable);
        apply_ref!(syntax_tag, snap.syntax_tag);
        apply_ref!(syntax_attribute, snap.syntax_attribute);
        apply_ref!(syntax_punctuation, snap.syntax_punctuation);
        apply_ref!(syntax_subtle, snap.syntax_subtle);
        apply_ref!(syntax_markup, snap.syntax_markup);
        apply_ref!(syntax_invalid, snap.syntax_invalid);

        for (name, spec) in &snap.highlights {
            theme.apply_highlight_spec(name, spec);
        }
        theme.rebuild_code();
        *self = theme;
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
        macro_rules! set_optional {
            ($field:ident) => {{
                match color {
                    None => {
                        self.palette.$field = default.palette.$field;
                        true
                    }
                    Some(s) => match crate::ui::color::parse_color(s) {
                        Some(c) => {
                            self.palette.$field = Some(c);
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
        let changed = match name {
            "bg" => set_optional!(bg),
            "user_msg" => set!(user_msg),
            "user_msg_bg" => set!(user_msg_bg),
            "status_text" => set!(status_text),
            "input_border" => set!(input_border),
            "input_bg" => set!(input_bg),
            "input_prefix" => set!(input_prefix),
            "input_cursor" => set!(input_cursor),
            "system_msg" => set!(system_msg),
            "approval_safe" => set!(approval_safe),
            "approval_danger" => set!(approval_danger),
            "tool_call" => set!(tool_call),
            "tool_error" => set!(tool_error),
            "shell_program" => set!(shell_program),
            "shell_separator" => set!(shell_separator),
            "shell_redirect" => set!(shell_redirect),
            "shell_flag" => set!(shell_flag),
            "shell_string" => set!(shell_string),
            "shell_variable" => set!(shell_variable),
            "shell_comment" => set!(shell_comment),
            "shell_path" => set!(shell_path),
            "diff_removed" => set!(diff_removed),
            "diff_added" => set!(diff_added),
            "thinking" => set!(thinking),
            "tab_active" => set!(tab_active),
            "syntax_text" => set!(syntax_text),
            "syntax_comment" => set!(syntax_comment),
            "syntax_string" => set!(syntax_string),
            "syntax_number" => set!(syntax_number),
            "syntax_constant" => set!(syntax_constant),
            "syntax_escape" => set!(syntax_escape),
            "syntax_regex" => set!(syntax_regex),
            "syntax_keyword" => set!(syntax_keyword),
            "syntax_keyword_control" => set!(syntax_keyword_control),
            "syntax_type" => set!(syntax_type),
            "syntax_function" => set!(syntax_function),
            "syntax_variable" => set!(syntax_variable),
            "syntax_tag" => set!(syntax_tag),
            "syntax_attribute" => set!(syntax_attribute),
            "syntax_punctuation" => set!(syntax_punctuation),
            "syntax_subtle" => set!(syntax_subtle),
            "syntax_markup" => set!(syntax_markup),
            "syntax_invalid" => set!(syntax_invalid),
            other => {
                eprintln!("bone-lua warn: unknown highlight group: {other}");
                false
            }
        };
        if changed && name.starts_with("syntax_") {
            self.rebuild_code();
        }
        changed
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod theme_tests;
