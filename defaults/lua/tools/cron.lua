local PYTHON_SCRIPT = [=[
import base64, json, os, re, shlex, subprocess, sys
from pathlib import Path

def env(name, default=""):
    return os.environ.get(name, default)

def bone_dir():
    xdg = env("XDG_CONFIG_HOME")
    if xdg: return Path(xdg) / "bone-rust"
    home = env("HOME") or env("USERPROFILE")
    if home: return Path(home) / ".bone-rust"
    return Path(".bone-rust").resolve()

def find_bone():
    explicit = env("BONE_BIN")
    if explicit and os.access(explicit, os.X_OK):
        return str(Path(explicit).resolve())
    for part in env("PATH").split(os.pathsep):
        candidate = Path(part) / "bone"
        if os.access(candidate, os.X_OK):
            return str(candidate.resolve())
    print("bone binary not found. Set BONE_BIN=/path/to/bone", file=sys.stderr)
    sys.exit(127)

def validate_name(name):
    if not re.fullmatch(r"[A-Za-z0-9_-]+", name or ""):
        fail("job name must contain only letters, numbers, '-' and '_'")

def parse_time(value):
    m = re.fullmatch(r"(\d{1,2}):(\d{2})", value or "")
    if not m: fail("time must be HH:MM")
    hour, minute = int(m.group(1)), int(m.group(2))
    if hour > 23 or minute > 59: fail("time must be between 00:00 and 23:59")
    return hour, minute

def validate_approval(value):
    if value not in ("read_only", "edit", "danger"):
        fail("approval must be read_only, edit, or danger")
    return value

def fail(message, code=2):
    print(message, file=sys.stderr)
    sys.exit(code)

def cron_missing():
    fail("crontab not found. Install cronie or cron.", 127)

def current_crontab():
    try:
        p = subprocess.run(["crontab", "-l"], text=True, capture_output=True)
    except FileNotFoundError:
        cron_missing()
    if p.returncode == 0: return p.stdout
    if "no crontab" in p.stderr.lower(): return ""
    fail(p.stderr.strip() or f"crontab -l exited with {p.returncode}", p.returncode)

def write_crontab(content):
    try:
        p = subprocess.run(["crontab", "-"], input=content, text=True, capture_output=True)
    except FileNotFoundError:
        cron_missing()
    if p.returncode != 0:
        fail(p.stderr.strip() or f"crontab exited with {p.returncode}", p.returncode)

def encode_metadata(job):
    raw = json.dumps(job, separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")

def decode_metadata(value):
    try:
        padded = value + "=" * ((4 - len(value) % 4) % 4)
        return json.loads(base64.urlsafe_b64decode(padded.encode()))
    except Exception:
        return None

def parse_cron_line(line):
    marker = "# BONE:"
    if marker not in line: return None
    body, encoded = line.rsplit(marker, 1)
    fields = body.split()
    if len(fields) < 5 or fields[2:5] != ["*", "*", "*"]: return None
    try:
        minute, hour = int(fields[0]), int(fields[1])
    except ValueError: return None
    meta = decode_metadata(encoded.strip())
    if not meta:
        name = encoded.strip()
        if not re.fullmatch(r"[A-Za-z0-9_-]+", name): return None
        meta = {"name": name, "approval": "", "cwd": "", "prompt": "", "log_path": "", "allow_skill_scripts": False}
    meta["minute"] = minute
    meta["hour"] = hour
    return meta

def build_cron_line(job):
    args = [job["bone_bin"], "run", "--approval", job["approval"]]
    if job["allow_skill_scripts"]: args.append("--allow-skill-scripts")
    args.extend(["--prompt", job["prompt"]])
    command = "cd " + shlex.quote(job["cwd"]) + " && " + " ".join(shlex.quote(a) for a in args)
    command += " >> " + shlex.quote(job["log_path"]) + " 2>&1"
    meta = {k: job[k] for k in ("name", "approval", "cwd", "prompt", "log_path", "allow_skill_scripts")}
    return f'{job["minute"]} {job["hour"]} * * * {command} # BONE:{encode_metadata(meta)}'

def list_jobs():
    jobs = [j for j in (parse_cron_line(line) for line in current_crontab().splitlines()) if j]
    if not jobs:
        print("No bone cron jobs.")
        return
    print("NAME\tTIME\tAPPROVAL\tCWD\tPROMPT")
    for j in jobs:
        print(f'{j.get("name", "")}\t{j["hour"]:02d}:{j["minute"]:02d}\t{j.get("approval", "")}\t{j.get("cwd", "")}\t{j.get("prompt", "")}')

def add_job():
    name, time, prompt = env("TOOL_NAME"), env("TOOL_TIME"), env("TOOL_PROMPT")
    approval = validate_approval(env("TOOL_APPROVAL", "read_only") or "read_only")
    if not name or not time or not prompt:
        fail("Usage: cron add requires name, time, and prompt.")
    validate_name(name)
    hour, minute = parse_time(time)
    cwd = str(Path(env("TOOL_CWD") or os.getcwd()).resolve())
    log_dir = bone_dir() / "runs"
    log_dir.mkdir(parents=True, exist_ok=True)
    allow = env("TOOL_ALLOW_SKILL_SCRIPTS", "false").lower() in ("true", "1", "yes", "on")
    job = {"name": name, "hour": hour, "minute": minute, "approval": approval,
           "cwd": cwd, "prompt": prompt, "log_path": str(log_dir / f"{name}.log"),
           "allow_skill_scripts": allow, "bone_bin": find_bone()}
    existing = current_crontab().splitlines()
    kept = []
    for line in existing:
        parsed = parse_cron_line(line)
        legacy_tag = line.rstrip().endswith(f"# BONE:{name}")
        if (parsed and parsed.get("name") == name) or legacy_tag: continue
        kept.append(line)
    kept.append(build_cron_line(job))
    write_crontab("\n".join(kept) + "\n")
    print(f"Added cron job {name}.")

def remove_job():
    name = env("TOOL_NAME")
    if not name: fail("Usage: cron remove requires name.")
    validate_name(name)
    removed = False
    kept = []
    for line in current_crontab().splitlines():
        parsed = parse_cron_line(line)
        legacy_tag = line.rstrip().endswith(f"# BONE:{name}")
        if (parsed and parsed.get("name") == name) or legacy_tag: removed = True
        else: kept.append(line)
    write_crontab(("\n".join(kept) + "\n") if kept else "")
    print(f"Removed cron job {name}." if removed else f"No cron job named {name}.")

def show_logs():
    name = env("TOOL_NAME")
    if not name: fail("Usage: cron logs requires name.")
    validate_name(name)
    path = bone_dir() / "runs" / f"{name}.log"
    try: lines = path.read_text().splitlines()
    except OSError as e: fail(f"failed to read {path}: {e}", 1)
    tail = env("TOOL_TAIL")
    if tail:
        try: n = int(tail)
        except ValueError: fail("tail must be a number")
        lines = lines[-n:]
    print("\n".join(lines))

def help_text():
    print("""Manage Bone scheduled jobs.

  Examples:
    cron(action=list)
    cron(action=add, name=daily-clean, time=09:00, approval=edit, prompt=/clean src/main.rs)
    cron(action=remove, name=daily-clean)
    cron(action=logs, name=daily-clean, tail=100)""")

action = env("TOOL_ACTION")
if action == "list": list_jobs()
elif action == "add": add_job()
elif action in ("remove", "rm"): remove_job()
elif action == "logs": show_logs()
elif action in ("help", "--help", "-h", ""): help_text()
else: fail(f"Unknown cron action: {action}")
]=]

local function execute(params, ctx)
    local action = params.action or ""
    local name = params.name or ""
    local time = params.time or ""
    local approval = params.approval or "read_only"
    local prompt = params.prompt or ""
    local cwd = params.cwd or ""
    local tail = params.tail or ""
    local allow_skill_scripts = params.allow_skill_scripts or false

    -- Build export commands for TOOL_* variables
    local exports = {}
    table.insert(exports, 'export TOOL_ACTION="' .. action:gsub('"', '\\"') .. '"')
    if name ~= "" then table.insert(exports, 'export TOOL_NAME="' .. name:gsub('"', '\\"') .. '"') end
    if time ~= "" then table.insert(exports, 'export TOOL_TIME="' .. time:gsub('"', '\\"') .. '"') end
    if approval ~= "" then table.insert(exports, 'export TOOL_APPROVAL="' .. approval:gsub('"', '\\"') .. '"') end
    if prompt ~= "" then table.insert(exports, 'export TOOL_PROMPT="' .. prompt:gsub('"', '\\"') .. '"') end
    if cwd ~= "" then table.insert(exports, 'export TOOL_CWD="' .. cwd:gsub('"', '\\"') .. '"') end
    if tail ~= "" then table.insert(exports, 'export TOOL_TAIL="' .. tail:gsub('"', '\\"') .. '"') end
    if allow_skill_scripts then table.insert(exports, 'export TOOL_ALLOW_SKILL_SCRIPTS=true') end

    local cmd = table.concat(exports, "; ")
    cmd = cmd .. "; uv run --no-project --no-sync -- python3 <<'PYEOF'\n"
    cmd = cmd .. PYTHON_SCRIPT
    cmd = cmd .. "\nPYEOF"

    local result = ctx.shell(cmd, { timeout_ms = 300000 })
    if result.stderr and #result.stderr > 0 then
        return "ERROR: " .. result.stderr
    end
    return result.stdout or ""
end

bone.register_tool({
    name = "cron",
    description = "Manage Bone scheduled jobs for the user. Use this when the user asks to schedule, list, remove, or inspect recurring Bone tasks. Fully implemented as a custom tool; supports daily HH:MM schedules.",
    parameters = {
        type = "object",
        properties = {
            action = {
                type = "string",
                required = true,
                description = "Action: add, list, remove, logs, or help.",
            },
            name = {
                type = "string",
                description = "Job name for add/remove/logs. Use letters, numbers, '-' or '_'.",
            },
            time = {
                type = "string",
                description = "Daily run time in HH:MM 24-hour local time, required for add.",
            },
            approval = {
                type = "string",
                description = "Approval mode for add: read_only, edit, or danger. Defaults to read_only.",
            },
            prompt = {
                type = "string",
                description = "Prompt or skill invocation for add.",
            },
            cwd = {
                type = "string",
                description = "Working directory for add. Defaults to current directory.",
            },
            tail = {
                type = "number",
                description = "Number of log lines for logs.",
            },
            allow_skill_scripts = {
                type = "boolean",
                description = "For add: pass --allow-skill-scripts to scheduled bone run. Defaults to false.",
            },
        },
        required = { "action" },
        additionalProperties = false,
    },
    safety = "edit",
    execute = execute,
})
