pub mod types;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use types::Skill;

const COMMIT_SKILL: &str = include_str!("../../defaults/skills/commit.yaml");

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
        Self::load_from_dir(&crate::config::skills_dir(), true)
    }

    pub fn load_from_dir(dir: &Path, seed_examples: bool) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        if seed_examples {
            seed_example_skills(dir)?;
        }
        Self::scan(dir)
    }

    pub fn reload(&mut self) -> io::Result<()> {
        let dir = crate::config::skills_dir();
        *self = Self::load_from_dir(&dir, true)?;
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

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), String> {
        let loaded = self
            .skills
            .get_mut(name)
            .ok_or_else(|| format!("Unknown skill: {name}"))?;
        let mut updated = loaded.skill.clone();
        updated.enabled = enabled;
        let yaml = serde_yaml::to_string(&updated).map_err(|err| err.to_string())?;
        write_skill_atomic(&loaded.path, &yaml)?;
        loaded.skill = updated;
        Ok(())
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

fn seed_example_skills(dir: &Path) -> io::Result<()> {
    let marker = dir.join(".examples-initialized");
    if marker.exists() {
        return Ok(());
    }
    let has_yaml = fs::read_dir(dir)?.filter_map(Result::ok).any(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "yaml")
    });
    if !has_yaml {
        for (name, contents) in [
            ("commit.yaml", COMMIT_SKILL),
        ] {
            fs::write(dir.join(name), contents)?;
        }
    }
    fs::write(marker, "seeded\n")
}

fn write_skill_atomic(path: &Path, content: &str) -> Result<(), String> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_else(|_| std::process::id() as u128);
    let temporary = path.with_extension(format!("bone-tmp-{}-{suffix}", std::process::id()));
    let permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let write_result = (|| -> io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.flush()?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temporary, permissions)?;
        }
        fs::rename(&temporary, path)
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(err.to_string());
    }
    Ok(())
}
