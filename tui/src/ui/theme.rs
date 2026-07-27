//! App-wide ratatui color theme.

use ratatui::style::Color;
use syntect::highlighting::{
    Color as SyColor, FontStyle, StyleModifier, Theme as SyntectTheme, ThemeItem,
};

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, PartialEq)]
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
    pub markdown_marker: Color,
    pub markdown_heading: Color,
    pub markdown_link: Color,
    pub markdown_inline_code: Color,
    pub markdown_rule: Color,
    pub markdown_table_border: Color,
    pub markdown_table_header: Color,
    pub chart: Color,
    pub chart_empty: Color,
    pub heat_low: Color,
    pub heat_high: Color,
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
    #[cfg(test)]
    code_rebuilds: usize,
    /// Configured base and sparse temporary overrides are kept separately;
    /// public fields above are the effective theme consumed by renderers.
    configured: Option<Box<Theme>>,
    runtime_overrides: std::collections::BTreeMap<String, String>,
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
            markdown_marker: palette.muted,
            markdown_heading: palette.fg,
            markdown_link: palette.muted,
            markdown_inline_code: palette.muted,
            markdown_rule: palette.subtle,
            markdown_table_border: palette.border,
            markdown_table_header: palette.accent,
            chart: palette.accent,
            chart_empty: palette.subtle,
            heat_low: palette.subtle,
            heat_high: palette.good,
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
            #[cfg(test)]
            code_rebuilds: 0,
            configured: None,
            runtime_overrides: std::collections::BTreeMap::new(),
        };
        theme.rebuild_code();
        theme
    }
}

/// Convert a ratatui palette color for syntect. Terminal-dependent indexed and
/// reset colors have no stable RGB representation and are omitted.
fn to_syntect(color: Color) -> Option<SyColor> {
    crate::ui::color::color_to_rgb(color).map(|(r, g, b)| SyColor { r, g, b, a: 0xFF })
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
    /// Load the configured application theme directly from canonical settings.
    /// A missing settings file has no configured overrides and uses defaults;
    /// malformed settings remain an error so entry points can warn explicitly.
    pub fn load_configured() -> Result<Self, crate::config::settings::SettingsError> {
        let Some(settings) = crate::config::settings::Settings::load()? else {
            return Ok(Self::default());
        };
        Ok(Self::from_snapshot(&settings.resolved().theme))
    }

    /// Build the configured application theme from a resolved settings snapshot.
    pub fn from_snapshot(snap: &crate::config::settings::ThemeSettings) -> Self {
        let mut theme = Self::default();
        theme.apply_snapshot(snap);
        theme
    }

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
        let mut code = SyntectTheme::default();
        code.settings.foreground = to_syntect(self.syntax_text);
        code.scopes = vec![
            scope_item("comment", to_syntect(self.syntax_comment), None),
            scope_item("string", to_syntect(self.syntax_string), None),
            scope_item("constant.numeric", to_syntect(self.syntax_number), None),
            scope_item(
                "constant.language, variable.language",
                to_syntect(self.syntax_constant),
                None,
            ),
            scope_item(
                "constant.character.escape",
                to_syntect(self.syntax_escape),
                None,
            ),
            scope_item("constant.regexp", to_syntect(self.syntax_regex), None),
            scope_item(
                "keyword, storage, meta.preprocessor",
                to_syntect(self.syntax_keyword),
                None,
            ),
            scope_item(
                "keyword.control",
                to_syntect(self.syntax_keyword_control),
                None,
            ),
            scope_item(
                "entity.name.type, support.class, support.type",
                to_syntect(self.syntax_type),
                None,
            ),
            scope_item(
                "entity.name.function, support.function, meta.decorator, storage.type.annotation",
                to_syntect(self.syntax_function),
                None,
            ),
            scope_item(
                "variable, support.variable, entity.name.variable",
                to_syntect(self.syntax_variable),
                None,
            ),
            scope_item("entity.name.tag", to_syntect(self.syntax_tag), None),
            scope_item(
                "entity.other.attribute-name",
                to_syntect(self.syntax_attribute),
                None,
            ),
            scope_item(
                "punctuation, keyword.operator",
                to_syntect(self.syntax_punctuation),
                None,
            ),
            scope_item(
                "punctuation.definition.tag",
                to_syntect(self.syntax_subtle),
                None,
            ),
            scope_item(
                "markup.heading",
                to_syntect(self.syntax_markup),
                Some(FontStyle::BOLD),
            ),
            scope_item("markup.bold", None, Some(FontStyle::BOLD)),
            scope_item("markup.italic", None, Some(FontStyle::ITALIC)),
            scope_item("invalid", to_syntect(self.syntax_invalid), None),
        ];
        self.code = code;
        #[cfg(test)]
        {
            self.code_rebuilds += 1;
        }
    }

    fn syntax_colors(&self) -> [Color; 18] {
        [
            self.syntax_text,
            self.syntax_comment,
            self.syntax_string,
            self.syntax_number,
            self.syntax_constant,
            self.syntax_escape,
            self.syntax_regex,
            self.syntax_keyword,
            self.syntax_keyword_control,
            self.syntax_type,
            self.syntax_function,
            self.syntax_variable,
            self.syntax_tag,
            self.syntax_attribute,
            self.syntax_punctuation,
            self.syntax_subtle,
            self.syntax_markup,
            self.syntax_invalid,
        ]
    }

    #[cfg(test)]
    fn code_rebuilds(&self) -> usize {
        self.code_rebuilds
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
        self.markdown_marker = self.palette.muted;
        self.markdown_heading = self.palette.fg;
        self.markdown_link = self.palette.muted;
        self.markdown_inline_code = self.palette.muted;
        self.markdown_rule = self.palette.subtle;
        self.markdown_table_border = self.palette.border;
        self.markdown_table_header = self.palette.accent;
        self.chart = self.palette.accent;
        self.chart_empty = self.palette.subtle;
        self.heat_low = self.palette.subtle;
        self.heat_high = self.palette.good;
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
            "markdown_marker" => self.markdown_marker = color,
            "markdown_heading" => self.markdown_heading = color,
            "markdown_link" => self.markdown_link = color,
            "markdown_inline_code" => self.markdown_inline_code = color,
            "markdown_rule" => self.markdown_rule = color,
            "markdown_table_border" => self.markdown_table_border = color,
            "markdown_table_header" => self.markdown_table_header = color,
            "chart" => self.chart = color,
            "chart_empty" => self.chart_empty = color,
            "heat_low" => self.heat_low = color,
            "heat_high" => self.heat_high = color,
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

    fn apply_highlight_spec(&mut self, name: &str, spec: &crate::config::settings::ThemeStyleSpec) {
        match spec {
            crate::config::settings::ThemeStyleSpec::Color(s) => {
                if let Some(c) = self.resolve_color_ref(s) {
                    if !self.set_named_color(name, c) {
                        bone_core::ext::ctx::runtime_warn_once(format!(
                            "bone-lua warn: unknown highlight group: {name}"
                        ));
                    }
                } else {
                    bone_core::ext::ctx::runtime_warn_once(format!(
                        "bone-lua warn: invalid highlight color for {name}: {s}"
                    ));
                }
            }
            crate::config::settings::ThemeStyleSpec::Style { fg, bg, .. } => {
                if let Some(fg) = fg {
                    if let Some(c) = self.resolve_color_ref(fg) {
                        if !self.set_named_color(name, c) {
                            bone_core::ext::ctx::runtime_warn_once(format!(
                                "bone-lua warn: unknown highlight group: {name}"
                            ));
                        }
                    } else {
                        bone_core::ext::ctx::runtime_warn_once(format!(
                            "bone-lua warn: invalid highlight fg for {name}: {fg}"
                        ));
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
                                bone_core::ext::ctx::runtime_warn_once(format!(
                                    "bone-lua warn: unknown highlight bg group: {bg_name}"
                                ));
                            }
                        } else {
                            bone_core::ext::ctx::runtime_warn_once(format!(
                                "bone-lua warn: highlight has no bg role: {name}"
                            ));
                        }
                    } else {
                        bone_core::ext::ctx::runtime_warn_once(format!(
                            "bone-lua warn: invalid highlight bg for {name}: {bg}"
                        ));
                    }
                }
            }
        }
    }

    /// Apply resolved theme settings, overriding defaults with set values.
    pub fn apply_snapshot(&mut self, snap: &crate::config::settings::ThemeSettings) {
        let prior_syntax = self.syntax_colors();
        let prior_code = self.code.clone();
        #[cfg(test)]
        let prior_rebuilds = self.code_rebuilds;
        let mut theme = Theme::default();
        let default_syntax = theme.syntax_colors();
        #[cfg(test)]
        {
            theme.code_rebuilds = prior_rebuilds;
        }
        macro_rules! apply_palette {
            ($field:ident) => {
                if let Some(ref s) = snap.palette.$field {
                    if let Some(c) = theme.resolve_color_ref(s) {
                        theme.palette.$field = c;
                    } else {
                        bone_core::ext::ctx::runtime_warn_once(format!(
                            "bone-lua warn: invalid theme palette color for {}: {s}",
                            stringify!($field)
                        ));
                    }
                }
            };
        }
        if let Some(ref s) = snap.palette.bg {
            match theme.resolve_color_ref(s) {
                Some(c) => theme.palette.bg = Some(c),
                None => bone_core::ext::ctx::runtime_warn_once(format!(
                    "bone-lua warn: invalid theme palette color for bg: {s}"
                )),
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
                        bone_core::ext::ctx::runtime_warn_once(format!(
                            "bone-lua warn: invalid theme color for {}: {s}",
                            stringify!($target)
                        ));
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
        apply_ref!(markdown_marker, snap.markdown_marker);
        apply_ref!(markdown_heading, snap.markdown_heading);
        apply_ref!(markdown_link, snap.markdown_link);
        apply_ref!(markdown_inline_code, snap.markdown_inline_code);
        apply_ref!(markdown_rule, snap.markdown_rule);
        apply_ref!(markdown_table_border, snap.markdown_table_border);
        apply_ref!(markdown_table_header, snap.markdown_table_header);
        apply_ref!(chart, snap.chart);
        apply_ref!(chart_empty, snap.chart_empty);
        apply_ref!(heat_low, snap.heat_low);
        apply_ref!(heat_high, snap.heat_high);
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
        if theme.syntax_colors() != default_syntax {
            theme.rebuild_code();
        }
        let overrides = self.runtime_overrides.clone();
        theme.configured = Some(Box::new(theme.clone()));
        theme.runtime_overrides = overrides;
        *self = theme;
        self.reapply_runtime_overrides(false, prior_syntax, prior_code);
    }

    /// Apply a resolved frontend snapshot: configured settings first, then the
    /// daemon's sparse temporary layer. Runtime values never enter settings.
    pub fn apply_resolved_snapshot(
        &mut self,
        snap: &crate::config::settings::ThemeSettings,
        overrides: &std::collections::HashMap<String, String>,
    ) {
        let validated: std::collections::BTreeMap<_, _> = overrides
            .iter()
            .filter(|(name, value)| {
                let valid_name = Self::is_runtime_name(name);
                let valid_color = crate::ui::color::parse_color(value).is_some();
                if !valid_name || !valid_color {
                    bone_core::ext::ctx::runtime_warn_once(format!(
                        "bone-lua warn: invalid runtime highlight update: {name}"
                    ));
                }
                valid_name && valid_color
            })
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if validated.len() != overrides.len() {
            return;
        }
        self.runtime_overrides = validated;
        self.apply_snapshot(snap);
    }

    /// The configured base, excluding temporary runtime overrides.
    pub fn configured_theme(&self) -> &Theme {
        self.configured.as_deref().unwrap_or(self)
    }

    /// Current sparse runtime layer.
    pub fn runtime_overrides(&self) -> &std::collections::BTreeMap<String, String> {
        &self.runtime_overrides
    }

    fn reapply_runtime_overrides(
        &mut self,
        force_syntax_rebuild: bool,
        prior_syntax: [Color; 18],
        prior_code: SyntectTheme,
    ) {
        #[cfg(test)]
        let prior_rebuilds = self.code_rebuilds;
        let configured = self.configured.take();
        let mut effective = configured
            .as_deref()
            .cloned()
            .unwrap_or_else(Theme::default);
        effective.configured = None;
        effective.runtime_overrides = self.runtime_overrides.clone();
        let overrides = effective.runtime_overrides.clone();
        for (name, value) in &overrides {
            if let Some(color) = crate::ui::color::parse_color(value) {
                if name == "bg" {
                    effective.palette.bg = Some(color);
                } else {
                    effective.set_named_color(name, color);
                }
            }
        }
        #[cfg(test)]
        {
            effective.code_rebuilds = prior_rebuilds;
        }
        let effective_syntax = effective.syntax_colors();
        let configured_syntax = configured.as_deref().map(Theme::syntax_colors);
        if force_syntax_rebuild {
            effective.rebuild_code();
        } else if effective_syntax == prior_syntax && configured_syntax != Some(effective_syntax) {
            effective.code = prior_code;
        } else if configured_syntax != Some(effective_syntax) {
            effective.rebuild_code();
        }
        effective.configured = Some(Box::new(
            configured
                .as_deref()
                .cloned()
                .unwrap_or_else(Theme::default),
        ));
        *self = effective;
    }

    /// Set a single named highlight group at runtime. `None` removes the
    /// temporary override and reveals the configured value beneath it.
    pub fn set_highlight(&mut self, name: &str, color: Option<&str>) -> bool {
        let Some(role) = bone_core::config::theme::role(name).filter(|role| role.runtime) else {
            bone_core::ext::ctx::runtime_warn_once(format!(
                "bone-lua warn: unknown highlight group: {name}"
            ));
            return false;
        };
        let prior_syntax = self.syntax_colors();
        let prior_code = self.code.clone();
        if let Some(value) = color {
            if crate::ui::color::parse_color(value).is_none() {
                bone_core::ext::ctx::runtime_warn_once(format!(
                    "bone-lua warn: invalid highlight color for {name}: {value}"
                ));
                return false;
            }
            self.runtime_overrides
                .insert(name.to_string(), value.to_string());
        } else {
            self.runtime_overrides.remove(name);
        }
        self.reapply_runtime_overrides(role.syntax, prior_syntax, prior_code);
        true
    }

    fn is_runtime_name(name: &str) -> bool {
        bone_core::config::theme::role(name).is_some_and(|role| role.runtime)
    }
}

#[cfg(test)]
#[path = "theme_tests.rs"]
mod theme_tests;
