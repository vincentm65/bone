//! Options, set with `bone.o` in Lua.

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// Show model reasoning in chat buffers.
    pub show_reasoning: bool,
    /// Rows of tool output shown under each tool call.
    pub tool_preview_lines: usize,
    /// Rows of diff shown under each edit.
    pub diff_preview_lines: usize,
    /// Maximum height of the prompt window.
    pub prompt_max_height: usize,
    /// Milliseconds to wait for the rest of an ambiguous key sequence.
    pub timeoutlen: u64,
    /// Use the mouse: wheel scrolling and drag-to-copy. Off leaves the
    /// mouse to the terminal (its own selection, no wheel scrolling).
    pub mouse: bool,
    /// Reload Lua when its files change: the TUI's own, and the core's
    /// through `core/reload`.
    pub autoreload: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            show_reasoning: false,
            tool_preview_lines: 4,
            diff_preview_lines: 8,
            prompt_max_height: 10,
            timeoutlen: 1000,
            mouse: true,
            autoreload: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicKind {
    Boolean,
    Integer,
    Number,
    String,
}

impl DynamicKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "bool" | "boolean" => Some(Self::Boolean),
            "int" | "integer" => Some(Self::Integer),
            "number" | "float" => Some(Self::Number),
            "string" | "str" => Some(Self::String),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DynamicValue {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    String(String),
}

impl DynamicValue {
    pub fn kind(&self) -> DynamicKind {
        match self {
            Self::Boolean(_) => DynamicKind::Boolean,
            Self::Integer(_) => DynamicKind::Integer,
            Self::Number(_) => DynamicKind::Number,
            Self::String(_) => DynamicKind::String,
        }
    }

    pub fn type_name(&self) -> &'static str {
        self.kind().name()
    }
}

impl std::fmt::Display for DynamicValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Boolean(value) => write!(f, "{value}"),
            Self::Integer(value) => write!(f, "{value}"),
            Self::Number(value) => write!(f, "{value}"),
            Self::String(value) => write!(f, "{value}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DynamicOption {
    pub value: DynamicValue,
    pub default: DynamicValue,
    pub kind: DynamicKind,
    pub desc: String,
    /// The values it may take, when it is a string with a fixed set.
    pub choices: Vec<String>,
    pub on_change: Option<u64>,
    /// The plugin that defined it.
    pub owner: Option<String>,
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => write!(f, "{n}"),
        }
    }
}

/// What a built-in option does, for /config and bone.o.info.
pub fn describe(name: &str) -> &'static str {
    match name {
        "show_reasoning" => "show the model's reasoning (ctrl+r)",
        "tool_preview_lines" => "rows of tool output under each call (style plugin)",
        "diff_preview_lines" => "rows of diff under each edit (style plugin)",
        "prompt_max_height" => "rows the prompt grows to before it scrolls",
        "timeoutlen" => "milliseconds to wait for the rest of a key sequence",
        "mouse" => "wheel scrolling and drag-to-copy (off: the terminal's own)",
        "autoreload" => "reload TUI and core Lua when their files change",
        _ => "",
    }
}

/// A dynamic option's value is one of its choices, if it has any.
pub fn check_choice(
    name: &str,
    option: &DynamicOption,
    value: &DynamicValue,
) -> Result<(), String> {
    match value {
        DynamicValue::String(s) if !option.choices.is_empty() && !option.choices.contains(s) => {
            Err(format!(
                "{name} must be one of: {}",
                option.choices.join(", ")
            ))
        }
        _ => Ok(()),
    }
}

pub const NAMES: &[&str] = &[
    "show_reasoning",
    "tool_preview_lines",
    "diff_preview_lines",
    "prompt_max_height",
    "timeoutlen",
    "mouse",
    "autoreload",
];

impl Options {
    pub fn get(&self, name: &str) -> Option<Value> {
        Some(match name {
            "show_reasoning" => Value::Bool(self.show_reasoning),
            "tool_preview_lines" => Value::Number(self.tool_preview_lines as u64),
            "diff_preview_lines" => Value::Number(self.diff_preview_lines as u64),
            "prompt_max_height" => Value::Number(self.prompt_max_height as u64),
            "timeoutlen" => Value::Number(self.timeoutlen),
            "mouse" => Value::Bool(self.mouse),
            "autoreload" => Value::Bool(self.autoreload),
            _ => return None,
        })
    }

    pub fn set(&mut self, name: &str, value: Value) -> Result<(), String> {
        match (name, value) {
            ("show_reasoning", Value::Bool(b)) => self.show_reasoning = b,
            ("tool_preview_lines", Value::Number(n)) => self.tool_preview_lines = n as usize,
            ("diff_preview_lines", Value::Number(n)) => self.diff_preview_lines = n as usize,
            ("prompt_max_height", Value::Number(n)) => self.prompt_max_height = (n as usize).max(1),
            ("timeoutlen", Value::Number(n)) => self.timeoutlen = n,
            ("mouse", Value::Bool(b)) => self.mouse = b,
            ("autoreload", Value::Bool(b)) => self.autoreload = b,
            (name, _) if self.get(name).is_some() => return Err(format!("wrong type for {name}")),
            (name, _) => return Err(format!("unknown option: {name}")),
        }
        Ok(())
    }

    /// Apply one option argument (`bone.o.apply`): `name`, `noname`, `invname`, `name!`,
    /// `name=value` or `name?`. Returns text to show, if any.
    pub fn apply(&mut self, arg: &str) -> Result<Option<String>, String> {
        if let Some(name) = arg.strip_suffix('?') {
            let v = self
                .get(name)
                .ok_or_else(|| format!("unknown option: {name}"))?;
            return Ok(Some(format!("{name}={v}")));
        }
        if let Some((name, raw)) = arg.split_once('=') {
            let value = match self.get(name) {
                Some(Value::Bool(_)) => Value::Bool(match raw {
                    "true" | "on" | "1" => true,
                    "false" | "off" | "0" => false,
                    _ => return Err(format!("{name} takes true or false")),
                }),
                Some(Value::Number(_)) => {
                    Value::Number(raw.parse().map_err(|_| format!("{name} takes a number"))?)
                }
                None => return Err(format!("unknown option: {name}")),
            };
            self.set(name, value)?;
            return Ok(None);
        }
        let toggle = |o: &Self, name: &str| match o.get(name) {
            Some(Value::Bool(b)) => Ok(b),
            Some(_) => Err(format!("{name} is not a boolean; use {name}=N")),
            None => Err(format!("unknown option: {name}")),
        };
        if let Some(name) = arg.strip_prefix("no").filter(|n| self.get(n).is_some()) {
            toggle(self, name)?;
            self.set(name, Value::Bool(false))?;
        } else if let Some(name) = arg.strip_prefix("inv").or_else(|| arg.strip_suffix('!')) {
            let b = toggle(self, name)?;
            self.set(name, Value::Bool(!b))?;
        } else {
            match self.get(arg) {
                Some(Value::Bool(_)) => self.set(arg, Value::Bool(true))?,
                Some(v) => return Ok(Some(format!("{arg}={v}"))),
                None => return Err(format!("unknown option: {arg}")),
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_syntax() {
        let mut o = Options::default();
        o.apply("noshow_reasoning").unwrap();
        assert!(!o.show_reasoning);
        o.apply("show_reasoning!").unwrap();
        assert!(o.show_reasoning);
        o.apply("tool_preview_lines=2").unwrap();
        assert_eq!(o.tool_preview_lines, 2);
        assert_eq!(
            o.apply("tool_preview_lines").unwrap().as_deref(),
            Some("tool_preview_lines=2")
        );
        assert_eq!(
            o.apply("show_reasoning?").unwrap().as_deref(),
            Some("show_reasoning=true")
        );
        assert!(o.apply("bogus").is_err());
        assert!(o.apply("show_reasoning=7").is_err());
        assert!(o.apply("notimeoutlen").is_err());
    }
}
