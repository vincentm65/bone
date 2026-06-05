pub mod types;

use crate::tools::ApprovalMode;
use crate::tools::script_runner::{ScriptRequest, run_script};

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use types::Skill;

include!(concat!(env!("OUT_DIR"), "/default_skills.rs"));
include!(concat!(env!("OUT_DIR"), "/skills_version.rs"));

#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub path: PathBuf,
    pub skill: Skill,
}

#[derive(Debug, Default)]
pub struct SkillStore {
    skills: BTreeMap<String, LoadedSkill>,
    warnings: Vec<String>,
}

impl SkillStore {
    pub fn load() -> io::Result<Self> {
        fs::create_dir_all(&crate::config::skills_dir())?;
        Self::scan(&crate::config::skills_dir())
    }

    pub fn load_from_dir(dir: &Path) -> io::Result<Self> {
        Self::scan(dir)
    }

    pub fn reload(&mut self) -> io::Result<()> {
        *self = Self::load()?;
        Ok(())
    }

    fn scan(dir: &Path) -> io::Result<Self> {
        let mut paths: Vec<_> = fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
            .collect();
        paths.sort();

        let mut store = Self::default();
        for path in paths {
            let raw = match fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(err) => {
                    store
                        .warnings
                        .push(format!("could not read {}: {err}", path.display()));
                    continue;
                }
            };
            let skill: Skill = match serde_yaml::from_str(&raw) {
                Ok(skill) => skill,
                Err(err) => {
                    store
                        .warnings
                        .push(format!("could not parse {}: {err}", path.display()));
                    continue;
                }
            };
            if let Err(err) = skill.validate() {
                store
                    .warnings
                    .push(format!("skipped {}: {err}", path.display()));
                continue;
            }
            if let Some(first) = store.skills.get(&skill.name) {
                store.warnings.push(format!(
                    "skipped duplicate skill {} in {}; first loaded from {}",
                    skill.name,
                    path.display(),
                    first.path.display()
                ));
                continue;
            }
            store
                .skills
                .insert(skill.name.clone(), LoadedSkill { path, skill });
        }
        Ok(store)
    }

    pub fn get_enabled(&self, name: &str) -> Option<&Skill> {
        self.skills
            .get(name)
            .map(|loaded| &loaded.skill)
            .filter(|skill| skill.enabled)
    }

    pub fn list(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values().map(|loaded| &loaded.skill)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Override skill enabled/disabled state from config page settings.
    /// Skills not listed in `enabled_names` are disabled.
    pub fn apply_config_enabled(&mut self, enabled_names: &[String]) {
        let enabled_set: std::collections::HashSet<&str> =
            enabled_names.iter().map(|s| s.as_str()).collect();
        for (_, loaded) in &mut self.skills {
            loaded.skill.enabled = enabled_set.contains(loaded.skill.name.as_str());
        }
    }
}

pub fn render_skill(
    skill: &Skill,
    args: &str,
    script_output: Option<&str>,
) -> Result<String, String> {
    match (&skill.prompt, script_output) {
        (Some(prompt), output) => Ok(render_template(prompt, args, output.unwrap_or(""))),
        (None, Some(output)) => Ok(output.to_string()),
        (None, None) => Err(format!("skill {} produced no input", skill.name)),
    }
}
pub async fn expand_skill_command(
    store: &SkillStore,
    input: &str,
    allow_scripts: bool,
    approval_mode: ApprovalMode,
) -> Result<String, String> {
    let trimmed = input.trim();
    let Some(command) = trimmed.strip_prefix('/') else {
        return Err("not a skill invocation".to_string());
    };
    let mut parts = command.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    if name.is_empty() {
        return Err("not a skill invocation".to_string());
    }
    let args = parts.next().unwrap_or("").trim_start();
    let skill = store
        .get_enabled(name)
        .ok_or_else(|| format!("unknown skill: /{name}"))?;

    if skill.script.is_some() {
        let safety = skill.effective_safety();
        if !approval_mode.allows_safety(safety) {
            return Err(format!(
                "skill /{} requires {:?} approval, but current mode is {}",
                skill.name,
                safety,
                approval_mode.mode_str()
            ));
        }
    }

    let script_output = if let Some(script) = skill.script.as_ref() {
        if !allow_scripts {
            return Err(format!(
                "skill /{} has a script; rerun with --allow-skill-scripts to execute headlessly",
                skill.name
            ));
        }
        let output = run_script(ScriptRequest {
            command: script.clone(),
            env: vec![("BONE_ARGS".to_string(), args.to_string())],
            timeout_ms: 120_000,
        })
        .await
        .map_err(|err| format!("Skill /{} failed: {err}", skill.name))?;
        if output.exit_code != Some(0) {
            let detail = if output.stderr.is_empty() {
                output.stdout
            } else {
                output.stderr
            };
            return Err(format!(
                "Skill /{} failed (exit code {}).\n{}",
                skill.name,
                output
                    .exit_code
                    .map_or_else(|| "signal".to_string(), |code| code.to_string()),
                detail
            ));
        }
        Some(output.stdout)
    } else {
        None
    };

    render_skill(skill, args, script_output.as_deref())
}

fn render_template(template: &str, args: &str, script_output: &str) -> String {
    let mut rendered = String::with_capacity(template.len() + args.len() + script_output.len());
    let mut rest = template;
    while let Some((marker_start, value, marker_len)) = next_marker(rest, args, script_output) {
        rendered.push_str(&rest[..marker_start]);
        rendered.push_str(value);
        rest = &rest[marker_start + marker_len..];
    }
    rendered.push_str(rest);
    rendered
}

fn next_marker<'a>(
    template: &'a str,
    args: &'a str,
    script_output: &'a str,
) -> Option<(usize, &'a str, usize)> {
    let args_marker = template.find("{{args}}").map(|start| (start, args, 8));
    let output_marker = template
        .find("{{script_output}}")
        .map(|start| (start, script_output, 17));
    match (args_marker, output_marker) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(marker), None) | (None, Some(marker)) => Some(marker),
        (None, None) => None,
    }
}

pub fn seed_example_skills() -> io::Result<()> {
    let dir = crate::config::skills_dir();
    let data_dir = crate::config::bone_dir().join("data");
    let version_path = data_dir.join("skills_version");
    let current_version = std::fs::read_to_string(&version_path).unwrap_or_default();

    if current_version == SKILLS_VERSION {
        return Ok(());
    }

    let has_yaml = fs::read_dir(&dir).ok().map_or(false, |entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "yaml"))
    });

    if !has_yaml {
        for (file_name, content) in DEFAULT_SKILLS {
            fs::write(dir.join(file_name), content)?;
        }
    }

    fs::create_dir_all(&data_dir)?;
    fs::write(&version_path, SKILLS_VERSION)
}
