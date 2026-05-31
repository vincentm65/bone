use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config;
use crate::run::parse_approval;
use crate::tools::ApprovalMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronJob {
    pub name: String,
    pub minute: u8,
    pub hour: u8,
    pub approval: ApprovalMode,
    pub cwd: PathBuf,
    pub prompt: String,
    pub log_path: PathBuf,
    pub allow_skill_scripts: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CronMetadata {
    name: String,
    approval: String,
    cwd: String,
    prompt: String,
    log_path: String,
    #[serde(default)]
    allow_skill_scripts: bool,
}

pub fn handle_cron_args(args: &[String]) -> Result<(), String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(cron_usage());
    };
    match command {
        "list" => {
            let jobs = list_jobs()?;
            if jobs.is_empty() {
                println!("No bone cron jobs.");
            } else {
                println!("NAME\tTIME\tAPPROVAL\tCWD\tPROMPT");
                for job in jobs {
                    println!(
                        "{}\t{:02}:{:02}\t{}\t{}\t{}",
                        job.name,
                        job.hour,
                        job.minute,
                        job.approval.mode_str(),
                        job.cwd.display(),
                        job.prompt
                    );
                }
            }
            Ok(())
        }
        "add" => {
            let job = parse_add_args(&args[1..])?;
            add_job(job)
        }
        "remove" | "rm" => {
            let name = args.get(1).ok_or("Usage: bone cron remove <name>")?;
            remove_job(name)
        }
        "logs" => show_logs(&args[1..]),
        "--help" | "-h" => Err(cron_usage()),
        other => Err(format!("unknown cron command: {other}\n{}", cron_usage())),
    }
}

fn parse_add_args(args: &[String]) -> Result<CronJob, String> {
    let mut name = None;
    let mut time = None;
    let mut approval = None;
    let mut prompt = None;
    let mut cwd = None;
    let mut allow_skill_scripts = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                name = Some(args.get(i).ok_or("--name requires a value")?.clone());
            }
            "--time" => {
                i += 1;
                time = Some(args.get(i).ok_or("--time requires a value")?.clone());
            }
            "--approval" => {
                i += 1;
                approval = Some(args.get(i).ok_or("--approval requires a value")?.clone());
            }
            "--prompt" => {
                i += 1;
                prompt = Some(args.get(i).ok_or("--prompt requires a value")?.clone());
            }
            "--cwd" => {
                i += 1;
                cwd = Some(PathBuf::from(args.get(i).ok_or("--cwd requires a value")?));
            }
            "--allow-skill-scripts" => allow_skill_scripts = true,
            other => return Err(format!("unknown argument: {other}\n{}", cron_add_usage())),
        }
        i += 1;
    }

    let name = name.ok_or_else(cron_add_usage)?;
    validate_name(&name)?;
    let (hour, minute) = parse_time(&time.ok_or_else(cron_add_usage)?)?;
    let prompt = prompt.ok_or_else(cron_add_usage)?;
    let cwd = cwd
        .unwrap_or(std::env::current_dir().map_err(|err| err.to_string())?)
        .canonicalize()
        .map_err(|err| format!("invalid cwd: {err}"))?;
    let log_dir = config::bone_dir().join("runs");
    fs::create_dir_all(&log_dir).map_err(|err| format!("failed to create log dir: {err}"))?;

    Ok(CronJob {
        log_path: log_dir.join(format!("{name}.log")),
        name,
        minute,
        hour,
        approval: parse_approval(approval.as_deref())?,
        cwd,
        prompt,
        allow_skill_scripts,
    })
}

pub fn list_jobs() -> Result<Vec<CronJob>, String> {
    Ok(current_crontab()?
        .lines()
        .filter_map(parse_cron_line)
        .collect())
}

pub fn add_job(job: CronJob) -> Result<(), String> {
    with_cron_lock(|| {
        let existing = current_crontab()?;
        let mut lines: Vec<String> = existing.lines().map(ToString::to_string).collect();
        lines.retain(|line| {
            let parsed_name = parse_cron_line(line).map(|parsed| parsed.name);
            parsed_name.as_deref() != Some(job.name.as_str())
                && !line.trim_end().ends_with(&format!("# BONE:{}", job.name))
        });
        lines.push(build_cron_line(&job)?);
        let content = lines.join("\n") + "\n";
        write_crontab(&content)?;
        println!("Added cron job {}.", job.name);
        Ok(())
    })
}

pub fn remove_job(name: &str) -> Result<(), String> {
    validate_name(name)?;
    with_cron_lock(|| {
        let existing = current_crontab()?;
        let tag = format!("# BONE:{name}");
        let mut removed = false;
        let lines: Vec<String> = existing
            .lines()
            .filter_map(|line| {
                let matches_name = parse_cron_line(line)
                    .map(|job| job.name == name)
                    .unwrap_or_else(|| line.trim_end().ends_with(&tag));
                if matches_name {
                    removed = true;
                    None
                } else {
                    Some(line.to_string())
                }
            })
            .collect();
        let content = lines.join("\n") + if lines.is_empty() { "" } else { "\n" };
        write_crontab(&content)?;
        if removed {
            println!("Removed cron job {name}.");
        } else {
            println!("No cron job named {name}.");
        }
        Ok(())
    })
}

fn show_logs(args: &[String]) -> Result<(), String> {
    let name = args
        .first()
        .ok_or("Usage: bone cron logs <name> [--tail N]")?;
    let mut tail: Option<usize> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--tail" => {
                i += 1;
                tail = Some(
                    args.get(i)
                        .ok_or("--tail requires a value")?
                        .parse()
                        .map_err(|_| "--tail must be a number".to_string())?,
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }
    validate_name(name)?;
    let path = config::bone_dir().join("runs").join(format!("{name}.log"));
    let content = fs::read_to_string(&path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    if let Some(n) = tail {
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(n);
        println!("{}", lines[start..].join("\n"));
    } else {
        print!("{content}");
    }
    Ok(())
}

pub fn build_cron_line(job: &CronJob) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let script_flag = if job.allow_skill_scripts {
        " --allow-skill-scripts"
    } else {
        ""
    };
    Ok(format!(
        "{} {} * * * cd {} && {} run --approval {}{} --prompt {} >> {} 2>&1 # BONE:{}",
        job.minute,
        job.hour,
        sh_escape(&job.cwd.display().to_string()),
        sh_escape(&exe.display().to_string()),
        job.approval.mode_str(),
        script_flag,
        sh_escape(&job.prompt),
        sh_escape(&job.log_path.display().to_string()),
        encode_cron_metadata(job)?
    ))
}

pub fn parse_cron_line(line: &str) -> Option<CronJob> {
    let (body, encoded) = line.rsplit_once("# BONE:")?;
    let mut parts = body.split_whitespace();
    let minute: u8 = parts.next()?.parse().ok()?;
    let hour: u8 = parts.next()?.parse().ok()?;
    if parts.next()? != "*" || parts.next()? != "*" || parts.next()? != "*" {
        return None;
    }
    let metadata = decode_cron_metadata(encoded.trim())
        .or_else(|| parse_legacy_metadata(body, encoded.trim()))?;
    Some(CronJob {
        name: metadata.name,
        minute,
        hour,
        approval: parse_approval(Some(&metadata.approval)).ok()?,
        cwd: PathBuf::from(metadata.cwd),
        prompt: metadata.prompt,
        log_path: PathBuf::from(metadata.log_path),
        allow_skill_scripts: metadata.allow_skill_scripts,
    })
}

fn encode_cron_metadata(job: &CronJob) -> Result<String, String> {
    let metadata = CronMetadata {
        name: job.name.clone(),
        approval: job.approval.mode_str().to_string(),
        cwd: job.cwd.display().to_string(),
        prompt: job.prompt.clone(),
        log_path: job.log_path.display().to_string(),
        allow_skill_scripts: job.allow_skill_scripts,
    };
    let json = serde_json::to_vec(&metadata).map_err(|err| err.to_string())?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode_cron_metadata(encoded: &str) -> Option<CronMetadata> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn parse_legacy_metadata(body: &str, name: &str) -> Option<CronMetadata> {
    validate_name(name).ok()?;
    let command = body.splitn(6, char::is_whitespace).nth(5)?.trim();
    Some(CronMetadata {
        name: name.to_string(),
        cwd: extract_between(command, "cd '", "' &&")
            .map(unescape_sh_single)
            .unwrap_or_default(),
        approval: command
            .split(" --approval ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())?
            .to_string(),
        prompt: extract_between(command, " --prompt '", "' >>")
            .map(unescape_sh_single)
            .unwrap_or_default(),
        log_path: extract_between(command, " >> '", "' 2>&1")
            .map(unescape_sh_single)
            .unwrap_or_default(),
        allow_skill_scripts: command.contains(" --allow-skill-scripts"),
    })
}

fn with_cron_lock<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    fs::create_dir_all(config::bone_dir()).map_err(|err| err.to_string())?;
    let lock_path = config::bone_dir().join("cron.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| format!("failed to open {}: {err}", lock_path.display()))?;
    lock.lock_exclusive()
        .map_err(|err| format!("failed to lock {}: {err}", lock_path.display()))?;
    let result = f();
    if let Err(err) = lock.unlock() {
        return Err(format!("failed to unlock {}: {err}", lock_path.display()));
    }
    result
}

fn current_crontab() -> Result<String, String> {
    let output = Command::new("crontab")
        .arg("-l")
        .output()
        .map_err(|_| cron_missing_message())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        if stderr.contains("no crontab") {
            Ok(String::new())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

fn write_crontab(content: &str) -> Result<(), String> {
    let mut child = Command::new("crontab")
        .arg("-")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|_| cron_missing_message())?;
    child
        .stdin
        .as_mut()
        .ok_or("failed to open crontab stdin")?
        .write_all(content.as_bytes())
        .map_err(|err| err.to_string())?;
    let status = child.wait().map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("crontab exited with {status}"))
    }
}

pub fn sh_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn unescape_sh_single(value: &str) -> String {
    value.replace("'\\''", "'")
}

fn extract_between<'a>(value: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = value.split_once(start)?.1;
    Some(rest.split_once(end)?.0)
}

fn parse_time(value: &str) -> Result<(u8, u8), String> {
    let (hour, minute) = value.split_once(':').ok_or("time must be HH:MM")?;
    let hour: u8 = hour
        .parse()
        .map_err(|_| "hour must be a number".to_string())?;
    let minute: u8 = minute
        .parse()
        .map_err(|_| "minute must be a number".to_string())?;
    if hour > 23 || minute > 59 {
        return Err("time must be between 00:00 and 23:59".to_string());
    }
    Ok((hour, minute))
}

fn validate_name(name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        Ok(())
    } else {
        Err("job name must contain only letters, numbers, '-' and '_'".to_string())
    }
}

fn cron_missing_message() -> String {
    "crontab not found. Install cronie (Arch: sudo pacman -S cronie && sudo systemctl enable --now cronie) or cron (Debian/Ubuntu: sudo apt install cron && sudo systemctl enable --now cron).".to_string()
}

fn cron_usage() -> String {
    "Usage: bone cron list|add|remove|logs".to_string()
}

fn cron_add_usage() -> String {
    "Usage: bone cron add --name <name> --time HH:MM --approval read_only|edit|danger --prompt <text> [--cwd <dir>] [--allow-skill-scripts]".to_string()
}
