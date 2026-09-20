//! Non-overridable guard against catastrophic shell commands.
//!
//! The configurable command policy (`command-policy.yaml`) is deliberately NOT
//! consulted here: it lives in the user's config directory and the agent itself
//! can rewrite it, so a rule stored there could be self-approved away. Every
//! rule in this module is code, is evaluated in every approval mode, and is
//! surfaced to the caller as a hard, unapprovable verdict.
//!
//! Classification is purely lexical: no process is spawned and no filesystem
//! access happens (the only ambient read is `dirs::home_dir()` when roots are
//! detected). Ambiguity — globs, `$VAR`, backticks, an unknown working
//! directory, a target that does not resolve — resolves to a denial, so the
//! guard fails closed.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::tools::types::ToolCall;

/// Roots the guard refuses to let a single command destroy. Captured once per
/// tool dispatch from the ambient environment.
#[derive(Debug, Clone, Default)]
pub struct GuardRoots {
    pub home: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
}

impl GuardRoots {
    /// Detect the guarded roots. `workspace` is the turn's working directory.
    ///
    /// A workspace that normalizes to the filesystem root (empty string, `"."`,
    /// or `/` itself) is treated as unknown: leaving it as `/` would make the
    /// "inside the workspace" allow-rule match every absolute path and turn the
    /// guard into a no-op. With `workspace: None`, relative destructive targets
    /// resolve to `Ambiguous` and therefore deny.
    pub fn detect(workspace: Option<&Path>) -> Self {
        let workspace = workspace
            .map(normalize_lexical)
            .filter(|path| path != Path::new("/"));
        Self {
            home: dirs::home_dir().map(|path| normalize_lexical(&path)),
            workspace,
        }
    }
}

/// Shells used with `-c`/`--command`: their quoted argument is the real command.
const SHELL_WRAPPERS: &[&str] = &[
    "bash", "sh", "zsh", "dash", "ksh", "fish", "csh", "tcsh", "pwsh", "powershell",
];

/// Prefix commands that wrap another command and must be skipped to find the
/// real verb.
const COMMAND_WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "setsid", "time", "command", "builtin", "exec", "nice",
    "ionice", "stdbuf", "busybox", "!",
];

/// Verbs that delete or overwrite their operands.
const DESTRUCTIVE_VERBS: &[&str] = &["rm", "rmdir", "unlink", "shred", "truncate", "mv"];

/// Paths that must never be the target of a destructive command. `/tmp` is
/// intentionally absent so scratch-space cleanup stays legal.
const SYSTEM_ROOTS: &[&str] = &[
    "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib64", "/media", "/mnt", "/opt",
    "/proc", "/root", "/run", "/sbin", "/srv", "/sys", "/usr", "/var",
];

/// Tokens that turn the following words into a nested invocation.
const EXECUTOR_VERBS: &[&str] = &["xargs", "eval", "-exec", "-execdir"];

/// Command-position flags that consume a following value.
const COMMAND_VALUE_FLAGS: &[&str] = &[
    "-u", "-g", "-C", "-n", "-c", "--user", "--group", "--chdir", "--command",
];

/// Operand-position flags that consume a following value. `-t` is deliberately
/// excluded so `mv -t ~` still exposes `~` as an operand.
const OPERAND_VALUE_FLAGS: &[&str] = &[
    "--user", "--group", "--chdir", "--command", "--target-directory", "--suffix",
];

/// Safe, deliberately blocked sentinel commands used to prove end-to-end that the
/// guard's refusal path is live. These are not real programs: if the guard ever
/// failed to fire, running one would merely report "command not found", so a
/// canary can never damage anything. Matched as a whole token, including when it
/// appears inside a shell wrapper, so it also exercises wrapper peeling and
/// per-segment scanning.
const GUARD_CANARIES: &[&str] = &["bone-guard-selftest"];

/// The guard recursively inspects shell `-c` payloads. Beyond this depth the
/// command is too ambiguous to classify safely, so it is refused rather than
/// treated as an ordinary command.
const MAX_SHELL_NESTING: usize = 8;

/// Returns a refusal reason when `command` must never run, else `None`.
pub fn hard_deny(command: &str, roots: &GuardRoots) -> Option<String> {
    if command.trim().is_empty() {
        return None;
    }
    if is_fork_bomb(command) {
        return Some(deny(format!("it looks like a shell fork bomb (`{command}`)")));
    }
    if let Some(syntax) = unsupported_nested_syntax(command) {
        return Some(deny(format!(
            "it contains unsupported nested shell syntax `{syntax}`"
        )));
    }
    let mut cwd = roots.workspace.clone();
    if let Some(reason) = scan_segments(command, roots, &mut cwd, 0) {
        return Some(deny(reason));
    }
    None
}

fn scan_segments(
    command: &str,
    roots: &GuardRoots,
    cwd: &mut Option<PathBuf>,
    depth: usize,
) -> Option<String> {
    if depth > MAX_SHELL_NESTING {
        return Some(
            "it contains more nested shell wrappers than the guard can inspect safely".to_string(),
        );
    }
    for segment in split_segments(command) {
        if let Some(reason) = check_segment(&segment, roots, cwd, depth) {
            return Some(reason);
        }
    }
    None
}

/// Returns a refusal reason when `call` targets the `shell` tool with a command
/// the guard forbids. Non-shell tools, and the non-executing `list`/`status`/
/// `kill` actions, are left to the normal policy path.
pub fn hard_deny_call(call: &ToolCall, roots: &GuardRoots) -> Option<String> {
    if call.name != "shell" {
        return None;
    }
    match call.arguments.get("action").and_then(Value::as_str) {
        Some("list" | "status" | "kill") => None,
        _ => call
            .arguments
            .get("command")
            .and_then(Value::as_str)
            .and_then(|command| hard_deny(command, roots)),
    }
}

fn check_segment(
    segment: &str,
    roots: &GuardRoots,
    cwd: &mut Option<PathBuf>,
    depth: usize,
) -> Option<String> {
    let tokens = match tokenize(segment) {
        Ok(tokens) => tokens,
        Err(reason) => return Some(reason.to_string()),
    };
    if tokens.is_empty() {
        return None;
    }
    // Deliberate self-test canary: always refused, in every segment and under any
    // wrapper, so the guard's refusal path can be proven live without ever
    // issuing a genuinely destructive command.
    if let Some(canary) = tokens
        .iter()
        .find(|token| GUARD_CANARIES.contains(&token.as_str()))
    {
        return Some(format!(
            "it ran the guard self-test canary `{canary}`, a deliberately blocked no-op command"
        ));
    }
    // A `cd`/`pushd` segment rebinds the directory subsequent relative targets
    // resolve against.
    update_cwd(&tokens, roots, cwd);
    if let Some(reason) = check_redirection(&tokens, roots, cwd.as_deref()) {
        return Some(reason);
    }

    // Shell wrappers can occur after a connector, a privilege wrapper, or an
    // executor. Inspect every `-c` payload in the segment rather than assuming
    // that the first token is the shell being invoked.
    let payloads = match shell_payloads(&tokens) {
        Ok(payloads) => payloads,
        Err(reason) => return Some(reason),
    };
    for payload in &payloads {
        let mut nested_cwd = cwd.clone();
        if let Some(reason) = scan_segments(payload, roots, &mut nested_cwd, depth + 1) {
            return Some(reason);
        }
    }
    if !payloads.is_empty() {
        return None;
    }

    if let Some(index) = command_index(&tokens) {
        let raw = tokens[index].clone();
        let verb = verb_name(&raw);
        let args = &tokens[index + 1..];
        if is_absolute_verb(&verb) {
            return Some(format!(
                "it runs `{raw}`, which rewrites disks, partitions, or swap"
            ));
        }
        if DESTRUCTIVE_VERBS.contains(&verb.as_str()) {
            if let Some(reason) = check_destructive(&verb, &raw, args, roots, cwd.as_deref()) {
                return Some(reason);
            }
        } else if verb == "dd" {
            if let Some(reason) = check_dd(args, roots, cwd.as_deref()) {
                return Some(reason);
            }
        } else if verb == "find" {
            if let Some(reason) = check_find(args, roots, cwd.as_deref()) {
                return Some(reason);
            }
        } else if verb == "git" {
            if let Some(reason) = check_git(args, roots, cwd.as_deref()) {
                return Some(reason);
            }
        } else if verb == "chmod" || verb == "chown" {
            if let Some(reason) = check_recursive(&raw, args, roots, cwd.as_deref()) {
                return Some(reason);
            }
        } else if verb == "rsync" {
            if let Some(reason) = check_rsync(args, roots, cwd.as_deref()) {
                return Some(reason);
            }
        }
    }
    secondary_scan(&tokens, roots, cwd.as_deref())
}

/// Rebinds the tracked working directory when a segment is a `cd`/`pushd`, so
/// relative targets in later segments resolve against the directory the shell
/// actually ends up in. A bare `cd`/`pushd` goes to `$HOME`; `cd -`, `$VAR`, a
/// glob, or an otherwise unresolvable target leaves the cwd unknown, which makes
/// later relative targets deny (fail closed).
fn update_cwd(tokens: &[String], roots: &GuardRoots, cwd: &mut Option<PathBuf>) {
    let Some(index) = command_index(tokens) else {
        return;
    };
    let verb = verb_name(&tokens[index]);
    if verb != "cd" && verb != "pushd" {
        return;
    }
    let targets = operands(&tokens[index + 1..]);
    let Some(target) = targets.first() else {
        *cwd = roots.home.clone();
        return;
    };
    if target == "-" {
        *cwd = None;
        return;
    }
    let resolved = match resolve_target(target, roots, cwd.as_deref()) {
        Resolution::Resolved(path) => Some(path),
        Resolution::Ambiguous => None,
    };
    *cwd = resolved;
}

// ── Verb dispatch ───────────────────────────────────────────────────────────

fn check_destructive(
    verb: &str,
    raw: &str,
    args: &[String],
    roots: &GuardRoots,
    cwd: Option<&Path>,
) -> Option<String> {
    let targets = operands(args);
    if targets.is_empty() {
        return Some(format!(
            "it runs `{raw}` with no explicit target, so its scope cannot be bounded"
        ));
    }
    let action = match verb {
        "mv" => "move",
        "truncate" => "overwrite",
        _ => "delete",
    };
    for target in &targets {
        if let Some(reason) = target_verdict(target, roots, cwd) {
            return Some(format!("it would {action} {reason}"));
        }
    }
    None
}

fn check_dd(args: &[String], roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    let mut has_output = false;
    for arg in args {
        if let Some(value) = arg.strip_prefix("of=") {
            has_output = true;
            if let Some(reason) = target_verdict(value, roots, cwd) {
                return Some(format!("it writes raw data over {reason}"));
            }
        }
    }
    if !has_output {
        return Some(
            "it runs `dd` without an explicit `of=` output, so the target is unknown".to_string(),
        );
    }
    None
}

fn check_find(args: &[String], roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    if !args.iter().any(|arg| arg == "-delete") {
        return None;
    }
    let mut paths = Vec::new();
    for arg in args {
        if is_flag_like(arg) {
            break;
        }
        paths.push(arg.clone());
    }
    if paths.is_empty() {
        return Some(
            "it runs `find -delete` without a path, so its scope cannot be bounded".to_string(),
        );
    }
    for path in &paths {
        if let Some(reason) = target_verdict(path, roots, cwd) {
            return Some(format!("it would delete files under {reason}"));
        }
    }
    None
}

fn check_git(args: &[String], roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    if args.first().map(String::as_str) != Some("clean") {
        return None;
    }
    let rest = &args[1..];
    if !rest.iter().any(|arg| is_git_force_flag(arg)) {
        return None;
    }
    let targets = operands(rest);
    if targets.is_empty() {
        return Some(
            "it runs `git clean` with a force flag and no path, which would wipe untracked files \
             across the workspace"
                .to_string(),
        );
    }
    for target in &targets {
        if let Some(reason) = target_verdict(target, roots, cwd) {
            return Some(format!("it would delete untracked files under {reason}"));
        }
    }
    None
}

fn check_recursive(
    raw: &str,
    args: &[String],
    roots: &GuardRoots,
    cwd: Option<&Path>,
) -> Option<String> {
    if !args.iter().any(|arg| is_recursive_flag(arg)) {
        return None;
    }
    for target in operands(args) {
        if let Some(reason) = target_verdict(&target, roots, cwd) {
            return Some(format!("it applies `{raw}` recursively to {reason}"));
        }
    }
    None
}

fn check_rsync(args: &[String], roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    if !args.iter().any(|arg| arg.starts_with("--delete")) {
        return None;
    }
    for target in operands(args) {
        if let Some(reason) = target_verdict(&target, roots, cwd) {
            return Some(format!("it would delete extra files under {reason}"));
        }
    }
    None
}

/// Catches destructive verbs that are not at command position (`xargs rm -rf`,
/// `find … -exec rm`) and disk verbs buried inside an executor. Anything driven
/// by an executor has an operand set this guard cannot bound, so a destructive
/// verb there is refused outright: `find / -exec rm {} +` deletes with no `-r`
/// flag and would otherwise slip past the flag heuristic below.
fn secondary_scan(tokens: &[String], roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    let has_executor = tokens
        .iter()
        .any(|token| EXECUTOR_VERBS.contains(&verb_name(token).as_str()));
    if has_executor {
        for token in tokens {
            let verb = verb_name(token);
            if is_unbounded_nested_verb(&verb) {
                return Some(format!(
                    "it contains a nested `{token}` invocation that this guard cannot bound"
                ));
            }
        }
    }
    let has_protected = tokens
        .iter()
        .filter(|token| !is_flag_like(token))
        .any(|token| target_verdict(token, roots, cwd).is_some());
    if !has_protected {
        return None;
    }
    for (index, token) in tokens.iter().enumerate() {
        let verb = verb_name(token);
        if DESTRUCTIVE_VERBS.contains(&verb.as_str())
            && tokens.get(index + 1).is_some_and(|next| is_flag_like(next))
        {
            return Some(format!(
                "it contains a nested `{token}` invocation that this guard cannot bound"
            ));
        }
    }
    None
}

fn is_unbounded_nested_verb(verb: &str) -> bool {
    is_absolute_verb(verb)
        || DESTRUCTIVE_VERBS.contains(&verb)
        || matches!(verb, "dd" | "find" | "git" | "chmod" | "chown" | "rsync" | "eval")
}

fn check_redirection(tokens: &[String], roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    for (index, token) in tokens.iter().enumerate() {
        let Some(position) = token.find('>') else {
            continue;
        };
        let rest = token[position + 1..].trim_start_matches('>');
        let target = if rest.is_empty() {
            match tokens.get(index + 1) {
                Some(next) => next.as_str(),
                None => continue,
            }
        } else {
            rest
        };
        let target = target.trim_matches(|ch: char| matches!(ch, '"' | '\''));
        if target.is_empty() || target.starts_with('&') || is_dev_sink(target) {
            continue;
        }
        if let Some(reason) = target_verdict(target, roots, cwd) {
            return Some(format!("it would redirect output into {reason}"));
        }
    }
    None
}

fn is_dev_sink(target: &str) -> bool {
    matches!(
        target,
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty" | "/dev/zero"
    ) || target.starts_with("/dev/fd/")
}

// ── Path resolution and protection ──────────────────────────────────────────

enum Resolution {
    Resolved(PathBuf),
    Ambiguous,
}

fn target_verdict(token: &str, roots: &GuardRoots, cwd: Option<&Path>) -> Option<String> {
    match resolve_target(token, roots, cwd) {
        Resolution::Ambiguous => Some(format!("the unresolvable target `{token}`")),
        Resolution::Resolved(path) => protected_reason(&path, roots),
    }
}

fn resolve_target(token: &str, roots: &GuardRoots, cwd: Option<&Path>) -> Resolution {
    let raw = strip_key_prefix(token);
    if raw == "~" {
        return roots
            .home
            .clone()
            .map_or(Resolution::Ambiguous, Resolution::Resolved);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return match &roots.home {
            Some(home) => Resolution::Resolved(normalize_lexical(&home.join(rest))),
            None => Resolution::Ambiguous,
        };
    }
    if raw.starts_with('~') || is_ambiguous(raw) {
        return Resolution::Ambiguous;
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Resolution::Resolved(normalize_lexical(path));
    }
    match cwd {
        Some(dir) => Resolution::Resolved(normalize_lexical(&dir.join(raw))),
        None => Resolution::Ambiguous,
    }
}

fn protected_reason(path: &Path, roots: &GuardRoots) -> Option<String> {
    if path == Path::new("/") {
        return Some("the filesystem root `/`".to_string());
    }
    if path
        .components()
        .any(|component| component.as_os_str() == std::ffi::OsStr::new(".git"))
    {
        return Some(format!("the git metadata under `{}`", path.display()));
    }
    if let Some(home) = &roots.home {
        if path == home {
            return Some(format!("your home directory `{}`", home.display()));
        }
        if home.starts_with(path) {
            return Some(format!(
                "`{}`, which contains your home directory",
                path.display()
            ));
        }
    }
    if let Some(workspace) = &roots.workspace {
        if path == workspace {
            return Some(format!("the workspace root `{}`", workspace.display()));
        }
        if workspace.starts_with(path) {
            return Some(format!(
                "`{}`, which contains the workspace",
                path.display()
            ));
        }
    }
    if let Some(workspace) = &roots.workspace
        && path.starts_with(workspace)
    {
        return None;
    }
    if let Some(home) = &roots.home
        && path.starts_with(home)
    {
        return Some(format!("`{}`, inside your home directory", path.display()));
    }
    for root in SYSTEM_ROOTS {
        let root = Path::new(root);
        if path == root || path.starts_with(root) || root.starts_with(path) {
            return Some(format!("the system location `{}`", path.display()));
        }
    }
    None
}

/// Lexical (no filesystem) normalization: drops `.` and resolves `..`.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("/");
    }
    out
}

fn unsupported_nested_syntax(command: &str) -> Option<&'static str> {
    [
        ("command substitution", "$("),
        ("backtick", "`"),
        ("process substitution", "<("),
        ("process substitution", ">("),
    ]
    .into_iter()
    .find_map(|(label, syntax)| command.contains(syntax).then_some(label))
}

fn is_ambiguous(raw: &str) -> bool {
    raw.is_empty()
        || raw.contains('$')
        || raw.contains('`')
        || raw.contains('*')
        || raw.contains('?')
        || raw.contains('[')
        || raw.contains(']')
}

fn strip_key_prefix(token: &str) -> &str {
    match token.split_once('=') {
        Some((key, value)) if is_name(key) => value,
        _ => token,
    }
}

// ── Tokenization helpers ────────────────────────────────────────────────────

fn tokenize(segment: &str) -> Result<Vec<String>, &'static str> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = segment.chars().peekable();
    let mut quote = None;
    let mut token_started = false;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    let Some(next) = chars.next() else {
                        return Err("it contains a trailing escape that the guard cannot parse");
                    };
                    match next {
                        '"' | '\\' | '$' | '`' => current.push(next),
                        '\n' => {}
                        _ => {
                            current.push('\\');
                            current.push(next);
                        }
                    }
                }
                _ => current.push(ch),
            },
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    token_started = true;
                }
                '\\' => {
                    let Some(next) = chars.next() else {
                        return Err("it contains a trailing escape that the guard cannot parse");
                    };
                    if next != '\n' {
                        current.push(next);
                    }
                    token_started = true;
                }
                ch if ch.is_whitespace() => {
                    if token_started {
                        tokens.push(std::mem::take(&mut current));
                        token_started = false;
                    }
                }
                _ => {
                    current.push(ch);
                    token_started = true;
                }
            },
        }
    }

    if quote.is_some() {
        return Err("it contains an unterminated shell quote");
    }
    if token_started {
        tokens.push(current);
    }
    Ok(tokens)
}

fn shell_payloads(tokens: &[String]) -> Result<Vec<String>, String> {
    let mut payloads = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if !SHELL_WRAPPERS.contains(&verb_name(token).as_str()) {
            continue;
        }
        let mut cursor = index + 1;
        while let Some(flag) = tokens.get(cursor) {
            if !flag.starts_with('-') {
                break;
            }
            let flag = flag.trim_start_matches('-').to_ascii_lowercase();
            if is_shell_command_flag(&flag) {
                let Some(payload) = tokens.get(cursor + 1) else {
                    return Err(format!(
                        "it invokes `{token}` with a command flag but no command payload"
                    ));
                };
                payloads.push(payload.clone());
                break;
            }
            cursor += 1;
        }
    }
    Ok(payloads)
}

fn is_shell_command_flag(flag: &str) -> bool {
    flag == "command" || flag == "c" || (flag.len() <= 3 && flag.contains('c'))
}

fn command_index(tokens: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        let verb = verb_name(token);
        if COMMAND_WRAPPERS.contains(&verb.as_str()) {
            index += 1;
            continue;
        }
        if is_flag_like(token) {
            index += if command_flag_takes_value(token) { 2 } else { 1 };
            continue;
        }
        if is_numeric(token) || is_env_assignment(token) {
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

fn command_flag_takes_value(token: &str) -> bool {
    let name = token.split('=').next().unwrap_or(token);
    !token.contains('=') && COMMAND_VALUE_FLAGS.contains(&name)
}

fn operands(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut positional_only = false;
    let mut index = 0;
    while index < args.len() {
        let token = args[index].as_str();
        if !positional_only && token == "--" {
            positional_only = true;
            index += 1;
            continue;
        }
        if !positional_only && is_flag_like(token) {
            let name = token.split('=').next().unwrap_or(token);
            let takes_value = !token.contains('=') && operand_flag_takes_value(name);
            index += if takes_value { 2 } else { 1 };
            continue;
        }
        out.push(args[index].clone());
        index += 1;
    }
    out
}

fn operand_flag_takes_value(name: &str) -> bool {
    OPERAND_VALUE_FLAGS.contains(&name) || matches!(name, "-s" | "-n")
}

fn verb_name(token: &str) -> String {
    let token = token.trim_start_matches('\\');
    let base = token.rsplit('/').next().unwrap_or(token);
    base.to_ascii_lowercase()
}

fn is_flag_like(token: &str) -> bool {
    token.len() > 1 && token.starts_with('-')
}

fn is_numeric(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

fn is_env_assignment(token: &str) -> bool {
    token
        .split_once('=')
        .is_some_and(|(key, _)| is_name(key))
}

fn is_name(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(|ch: char| ch.is_ascii_digit())
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_absolute_verb(verb: &str) -> bool {
    verb.starts_with("mkfs")
        || matches!(
            verb,
            "fdisk" | "sfdisk" | "cfdisk" | "parted" | "wipefs" | "mkswap" | "mdadm"
        )
}

fn is_git_force_flag(arg: &str) -> bool {
    arg == "--force"
        || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('f'))
}

fn is_recursive_flag(arg: &str) -> bool {
    arg == "--recursive"
        || (arg.starts_with('-')
            && !arg.starts_with("--")
            && (arg.contains('R') || arg.contains('r')))
}

fn is_fork_bomb(command: &str) -> bool {
    command.contains(":(){") || command.contains(":|:&") || command.contains(": |: &")
}

// ── Shell segment handling ───────────────────────────────────────────────────

fn split_segments(command: &str) -> Vec<String> {
    crate::shell_split::shell_split(
        command,
        &crate::shell_split::ShellSplitOptions {
            keep_separators: false,
            split_newlines: true,
            strip_comments: true,
        },
    )
}

fn deny(detail: String) -> String {
    format!(
        "Refused by bone's destructive-command guard: {detail}. This is blocked in every \
         approval mode and cannot be approved, overridden, or disabled by configuration. Do not \
         retry the command or try to work around the guard; if the operation is genuinely \
         required, ask the user to run it themselves."
    )
}

#[cfg(test)]
#[path = "command_guard_tests.rs"]
mod tests;
