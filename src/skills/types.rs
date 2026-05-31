use serde::{Deserialize, Serialize};

use crate::tools::command_policy::CommandSafety;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    pub prompt: Option<String>,
    pub script: Option<String>,
    #[serde(default)]
    pub safety: Option<CommandSafety>,
}

fn enabled_by_default() -> bool {
    true
}

impl Skill {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_skill_name(&self.name) {
            return Err(format!("invalid skill name: {}", self.name));
        }
        if is_builtin_command(&self.name) {
            return Err(format!(
                "skill name collides with builtin command: {}",
                self.name
            ));
        }
        if self.prompt.is_none() && self.script.is_none() {
            return Err(format!(
                "skill {} must provide prompt, script, or both",
                self.name
            ));
        }
        if self.prompt.as_ref().is_some_and(|prompt| prompt.is_empty()) {
            return Err(format!("skill {} has an empty prompt", self.name));
        }
        if self
            .script
            .as_ref()
            .is_some_and(|script| script.trim().is_empty())
        {
            return Err(format!("skill {} has an empty script", self.name));
        }
        Ok(())
    }

    pub fn effective_safety(&self) -> CommandSafety {
        if self.script.is_some() {
            CommandSafety::Danger
        } else {
            self.safety.unwrap_or(CommandSafety::ReadOnly)
        }
    }
}

fn valid_skill_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
}

fn is_builtin_command(name: &str) -> bool {
    matches!(
        name,
        "help"
            | "clear"
            | "new"
            | "model"
            | "provider"
            | "tools"
            | "config"
            | "skills"
            | "edit"
            | "e"
            | "quit"
            | "exit"
    )
}
