# Scheduled Tasks Plan

## Goal

Add first-class scheduled task support to bone so users can run prompts and skills automatically at a chosen time without opening the TUI.

The system should support daily jobs first, then general cron expressions later.

## Current State

Bone already has a headless agent path used by subagents:

```bash
bone agent --prompt "..." --approval read_only
bone agent --events --prompt "..." --approval edit
```

This calls `agent::run_agent()` and does not require the TUI.

The missing pieces are:

1. Headless skill expansion, e.g. running `/clean src/main.rs` outside the TUI.
2. A user-facing `bone run` command for one-shot headless execution.
3. Scheduled job storage and management commands.
4. A scheduler backend: system cron initially, optional daemon later.

## Design Principles

- Reuse existing `agent::run_agent()`.
- Do not duplicate agent logic in the TUI and CLI.
- Scheduled jobs must run without an interactive terminal.
- Scheduled jobs must have explicit approval mode.
- Skill expansion should behave the same in TUI and headless mode.
- Prefer simple file-backed state in the existing config directory.
- Keep system cron support separate from future daemon support.

## Phase 1: Add `bone run`

### Command

```bash
bone run [--approval read_only|edit|danger] [--provider <id>] [--model <name>] <prompt-or-skill>
```

Examples:

```bash
bone run "Summarize this repo"
bone run --approval read_only "/clean src/main.rs"
bone run --approval edit "/debug cargo test"
```

### Behavior

`bone run` should:

1. Parse CLI flags.
2. Accept prompt text from argv or stdin.
3. If input starts with `/skill-name`, expand the skill.
4. Call `agent::run_agent()` with the rendered prompt.
5. Print final response to stdout.
6. Exit non-zero on provider/tool/runtime failure.

### Skill Expansion

Currently TUI skill invocation is in `App::invoke_skill()`.

Move reusable skill execution/rendering into a shared function, for example:

```rust
pub async fn render_skill_invocation(input: &str) -> Result<Option<String>, String>
```

or more explicitly:

```rust
pub async fn expand_skill_command(command: &str, args: &str) -> Result<String, String>
```

Responsibilities:

- Load skills.
- Find enabled skill by name.
- Run skill script if present.
- Pass `BONE_ARGS` to scripted skills.
- Render prompt with `{{args}}` and `{{script_output}}`.

TUI should call this shared function too, so skill behavior stays consistent.

### Approval Caveat For Skill Scripts

TUI currently prompts before running a scripted skill.

Headless mode cannot prompt. Options:

- MVP: scripted skills only run if `--approval danger` or `--allow-skill-scripts` is provided.
- Safer default: non-script prompt-only skills work in all modes; scripted skills fail unless explicitly allowed.

Recommended MVP:

```bash
bone run --allow-skill-scripts "/some-scripted-skill args"
```

If omitted:

```text
error: skill /x has a script; rerun with --allow-skill-scripts to execute headlessly
```

## Phase 2: System Cron Backend

Use system cron on Linux/macOS as the first scheduler backend.

### Commands

```bash
bone cron list
bone cron add --name daily-clean --time 09:00 --approval edit --prompt "/clean src/main.rs"
bone cron remove daily-clean
bone cron show daily-clean
```

Optional aliases:

```bash
bone cron add daily-clean 09:00 "/clean src/main.rs"
```

### Cron Entry Format

The cron command should call `bone run`, not the TUI:

```cron
0 9 * * * cd /repo && /path/to/bone run --approval edit --prompt '/clean src/main.rs' # BONE:daily-clean
```

### Listing Jobs

`bone cron list` should read actual scheduler state, not a separate tracking file.

For cron backend:

```bash
crontab -l
```

Filter lines ending in:

```text
# BONE:<name>
```

Parse and display:

```text
NAME          SCHEDULE      APPROVAL   CWD                 PROMPT
daily-clean   09:00 daily   edit       /repo               /clean src/main.rs
```

### Removing Jobs

Remove only lines tagged with the exact name:

```text
# BONE:<name>
```

Do not modify unrelated crontab entries.

### Requirements

If `crontab` is missing, print platform-specific guidance.

Arch Linux:

```bash
sudo pacman -S cronie
sudo systemctl enable --now cronie
```

Debian/Ubuntu:

```bash
sudo apt install cron
sudo systemctl enable --now cron
```

macOS usually has cron support, but launchd is preferred for a later backend.

## Phase 3: Logs

Scheduled tasks need durable output.

Add default log directory:

```text
~/.bone-rust/runs/
```

Cron command should redirect stdout/stderr:

```cron
0 9 * * * cd /repo && /path/to/bone run --approval edit --prompt '/clean src/main.rs' >> ~/.bone-rust/runs/daily-clean.log 2>&1 # BONE:daily-clean
```

Add commands:

```bash
bone cron logs daily-clean
bone cron logs daily-clean --tail 100
```

## Phase 4: Cross-Platform Scheduler Backends

System cron is not enough for Windows, and launchd is preferred on macOS.

Add backend abstraction:

```rust
trait SchedulerBackend {
    fn list(&self) -> Result<Vec<ScheduledJob>, String>;
    fn add(&self, job: ScheduledJob) -> Result<(), String>;
    fn remove(&self, name: &str) -> Result<(), String>;
}
```

Backends:

| OS | Backend | Notes |
|---|---|---|
| Linux | cron initially, systemd timer later | cron is simplest |
| macOS | launchd | write plist under `~/Library/LaunchAgents` |
| Windows | Task Scheduler | use `schtasks.exe` |

### macOS launchd

Create plist:

```text
~/Library/LaunchAgents/ai.bone.<name>.plist
```

Use `StartCalendarInterval` for daily jobs.

Commands:

```bash
launchctl bootstrap gui/$UID ~/Library/LaunchAgents/ai.bone.<name>.plist
launchctl bootout gui/$UID ~/Library/LaunchAgents/ai.bone.<name>.plist
```

### Windows Task Scheduler

Use:

```powershell
schtasks /Create /SC DAILY /TN "Bone\daily-clean" /TR "bone run ..." /ST 09:00
schtasks /Query /TN "Bone\daily-clean"
schtasks /Delete /TN "Bone\daily-clean" /F
```

## Phase 5: Optional Bone Daemon

A daemon is not required for MVP because `bone agent` already runs headlessly and `bone run` can wrap it.

A daemon becomes useful later for:

- cross-platform scheduling without OS-specific schedulers
- retry policies
- live job status
- notifications
- in-app job management
- avoiding external cron/launchd/schtasks differences

Possible commands:

```bash
bone daemon
bone daemon install
bone daemon uninstall
bone daemon status
```

Daemon storage:

```text
~/.bone-rust/schedules/jobs.yaml
~/.bone-rust/schedules/runs/
```

## MVP Scope

Implement only:

1. `bone run`
2. Headless prompt-only skill expansion
3. Optional explicit scripted skill execution flag
4. `bone cron add/list/remove` using system cron
5. Logs redirected to `~/.bone-rust/runs/<job>.log`

Do not include in MVP:

- daemon
- Windows Task Scheduler
- macOS launchd
- natural language schedules
- notifications
- pause/resume
- one-shot jobs
- retry policy

## Implementation Steps

### Step 1: Extract Skill Expansion

Create shared code for skill command expansion.

Likely files:

- `src/skills/mod.rs`
- new `src/skills/invoke.rs` or similar
- `src/ui/app/mod.rs` updated to reuse shared logic

### Step 2: Add `bone run`

Update:

- `src/main.rs`
- `src/agent.rs` if request needs more flags

Add parser for:

```text
bone run --approval <mode> --provider <id> --model <name> --prompt <text>
bone run --approval <mode> <text>
```

### Step 3: Add Cron Module

Create:

```text
src/cron.rs
```

Types:

```rust
struct CronJob {
    name: String,
    minute: u8,
    hour: u8,
    approval: ApprovalMode,
    cwd: PathBuf,
    prompt: String,
    log_path: PathBuf,
}
```

Functions:

```rust
list_jobs() -> Result<Vec<CronJob>, String>
add_job(job: CronJob) -> Result<(), String>
remove_job(name: &str) -> Result<(), String>
```

### Step 4: Wire `bone cron`

Update `src/main.rs` dispatch:

```rust
if args.first().map(String::as_str) == Some("cron") { ... }
```

### Step 5: Tests

Add tests for:

- parsing `/skill args`
- rendering prompt-only skill
- rejecting scripted skill without explicit flag
- cron line generation
- cron line parsing
- remove preserves unrelated crontab lines

## Risks

### Quoting

Cron commands require careful shell quoting for prompts, paths, and provider/model values.

Mitigation:

- use single-quote shell escaping helper
- test prompts containing spaces, quotes, and slashes

### Approval Safety

Scheduled jobs cannot ask for approval interactively.

Mitigation:

- require explicit approval mode at schedule time
- default to `read_only`
- deny scripted skills unless explicitly allowed

### Environment Differences

Cron has a minimal environment and PATH.

Mitigation:

- use absolute path to bone binary
- set cwd explicitly
- optionally allow env file later

### Provider Credentials

Cron may not have the same env vars as the user's shell.

Mitigation:

- document that API key env vars must be available to cron
- later support an env file path per job

## Open Questions

1. Should `bone run` default approval be config approval mode or `read_only`?
2. Should scheduled jobs allow `danger`, or require an extra confirmation flag?
3. Should logs be append-only or one file per run?
4. Should `bone cron list` show only bone jobs or all cron entries with a `--all` flag?
5. Should cron backend exist only on Unix and return a clear unsupported error on Windows until Task Scheduler is implemented?

## Recommended Next Action

Implement `bone run` first. It unlocks reliable non-interactive execution and can be tested independently before scheduling.
