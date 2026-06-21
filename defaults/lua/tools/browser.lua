-- browser tool: persistent CDP daemon + page actions via Playwright.
--
-- A single Chromium process stays alive in the background, bound to a remote
-- debugging port. `start` spawns it detached, `stop` kills it, every other
-- action connects over CDP, reuses the live page, and disconnects only
-- (never browser.close() — that would terminate the remote browser).
--
-- Engine: Playwright (resolved via `uv run --with playwright`). Chromium path
-- comes from playwright.chromium.executable_path; the bundled browser in
-- ~/.cache/ms-playwright is used. No native Rust changes.

local PYTHON_SCRIPT = [=[
import base64, json, os, re, signal, socket, subprocess, sys, time, urllib.request
from pathlib import Path

def bone_dir():
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg:
        return Path(xdg) / "bone-rust"
    home = os.environ.get("HOME") or os.environ.get("USERPROFILE")
    if home:
        return Path(home) / ".bone-rust"
    return Path(".bone-rust").resolve()

DATA_DIR    = bone_dir() / "data" / "browser"
DAEMON_FILE = DATA_DIR / "daemon.json"
PROFILE_DIR = DATA_DIR / "profile"
SHOTS_DIR   = DATA_DIR / "shots"

def fail(msg, code=2):
    print(msg, file=sys.stderr)
    sys.exit(code)

def out(obj):
    print(json.dumps(obj, default=str))

def load_daemon():
    if not DAEMON_FILE.exists():
        return None
    try:
        return json.loads(DAEMON_FILE.read_text())
    except Exception:
        return None

def save_daemon(d):
    DAEMON_FILE.parent.mkdir(parents=True, exist_ok=True)
    DAEMON_FILE.write_text(json.dumps(d, separators=(",", ":")))

def clear_daemon():
    try:
        DAEMON_FILE.unlink()
    except FileNotFoundError:
        pass

IS_WINDOWS = os.name == "nt"

def pid_alive(pid):
    if not pid:
        return False
    try:
        pid = int(pid)
    except (TypeError, ValueError):
        return False
    if IS_WINDOWS:
        # Windows has no os.kill(pid, 0); query the exit code instead.
        # STILL_ACTIVE (259) is the sentinel for a running process.
        import ctypes
        kernel32 = ctypes.windll.kernel32
        h = kernel32.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
        if not h:
            return False
        code = ctypes.c_ulong()
        ok = kernel32.GetExitCodeProcess(h, ctypes.byref(code))
        kernel32.CloseHandle(h)
        return bool(ok) and code.value == 259
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except Exception:
        return False

def terminate_process_tree(pid):
    """Best-effort kill of `pid` and all descendants. POSIX reaps the process
    group (the daemon is started as its own session/group leader, so pgid ==
    pid); Windows uses `taskkill /T` to walk the tree."""
    if not pid:
        return
    try:
        pid = int(pid)
    except (TypeError, ValueError):
        return
    if IS_WINDOWS:
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return
    try:
        pgid = os.getpgid(pid)
    except ProcessLookupError:
        return
    except Exception:
        pgid = pid
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pgid, sig)
        except ProcessLookupError:
            return
        except Exception:
            pass
        for _ in range(20):
            if not pid_alive(pid):
                return
            time.sleep(0.15)

def free_port():
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port

def cdp_alive(port, timeout=1.0):
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=timeout) as r:
            return r.status == 200
    except Exception:
        return False

def daemon_running(d=None):
    d = d or load_daemon()
    if not d:
        return False
    return bool(pid_alive(d.get("pid")) and cdp_alive(d.get("port", 0)))

def resolve_chromium():
    from playwright.sync_api import sync_playwright
    with sync_playwright() as p:
        return p.chromium.executable_path

def require_daemon():
    d = load_daemon()
    if not daemon_running(d):
        fail("browser daemon not running — call browser action=start first", 3)
    return d

def connect():
    from playwright.sync_api import sync_playwright
    d = require_daemon()
    pw = sync_playwright().start()
    browser = pw.chromium.connect_over_cdp(f"http://127.0.0.1:{d['port']}")
    return pw, browser

def disconnect(pw, browser):
    # Do NOT call browser.close() — for connect_over_cdp that can terminate the
    # remote browser. Stopping the playwright runtime drops the CDP connection
    # and leaves Chromium running.
    try:
        pw.stop()
    except Exception:
        pass

def get_page(browser):
    ctx = browser.contexts[0] if browser.contexts else browser.new_context(viewport={"width": 1280, "height": 720})
    page = ctx.pages[0] if ctx.pages else ctx.new_page()
    # hide navigator.webdriver (belt-and-suspenders with the launch flag above)
    try:
        ctx.add_init_script("Object.defineProperty(navigator, 'webdriver', {get: () => undefined})")
    except Exception:
        pass
    return page

def timeout_of(p):
    try:
        return int(p.get("timeout_ms", 30000))
    except (TypeError, ValueError):
        return 30000

# ---------------------------------------------------------------------------
# daemon actions
# ---------------------------------------------------------------------------

def act_start(p):
    headless = p.get("headless", True)
    d = load_daemon()
    if daemon_running(d):
        out({"running": True, "reused": True, "port": d["port"], "pid": d["pid"], "headless": d.get("headless", True)})
        return
    if d:
        clear_daemon()
    try:
        exe = resolve_chromium()
    except Exception as e:
        fail(f"Playwright unavailable: {e}\nInstall with: uv run --with playwright python -m playwright install chromium", 1)
    if not exe or not Path(exe).exists():
        fail(f"Chromium not found at {exe}\nInstall with: uv run --with playwright python -m playwright install chromium", 1)
    port = free_port()
    PROFILE_DIR.mkdir(parents=True, exist_ok=True)
    args = [exe,
            f"--remote-debugging-port={port}",
            f"--user-data-dir={PROFILE_DIR}",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-blink-features=AutomationControlled",
            "--disable-features=IsolateOrigins,site-per-process",
            "--disable-dev-shm-usage"]
    if headless:
        args.extend(["--headless=new", "--window-size=1280,720"])
    else:
        args.append("--window-size=1280,720")
    # Detach the daemon so it survives this script: a new session/process-
    # group on POSIX (lets terminate_process_tree reap the whole tree), a new
    # process group on Windows (taskkill /T walks from this pid).
    popen_kwargs = dict(stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if IS_WINDOWS:
        popen_kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        popen_kwargs["start_new_session"] = True
    proc = subprocess.Popen(args, **popen_kwargs)
    deadline = time.time() + 15
    ready = False
    while time.time() < deadline:
        if cdp_alive(port, timeout=1.0):
            ready = True
            break
        if proc.poll() is not None:
            fail(f"Chromium exited immediately (code {proc.returncode}). Profile lock or disk issue?", 1)
        time.sleep(0.2)
    if not ready:
        terminate_process_tree(proc.pid)
        fail(f"Chromium launched but CDP not ready on port {port} within 15s", 1)
    d = {"port": port, "pid": proc.pid, "started_at": time.time(), "headless": bool(headless)}
    save_daemon(d)
    out({"running": True, "reused": False, "port": port, "pid": proc.pid, "headless": bool(headless)})

def act_stop(p):
    d = load_daemon()
    if not d:
        out({"stopped": True, "was_running": False})
        return
    pid = d.get("pid")
    was = pid_alive(pid)
    if was:
        terminate_process_tree(pid)
    clear_daemon()
    out({"stopped": True, "was_running": was})

def act_status(p):
    d = load_daemon()
    if not d:
        out({"running": False})
        return
    running = daemon_running(d)
    res = {"running": running, "pid": d.get("pid"), "port": d.get("port"),
           "headless": d.get("headless"), "started_at": d.get("started_at"), "pages": []}
    if running:
        try:
            pw, browser = connect()
            try:
                pages = []
                for c in browser.contexts:
                    for pg in c.pages:
                        try:
                            pages.append({"url": pg.url, "title": pg.title()})
                        except Exception:
                            pass
                res["pages"] = pages
            finally:
                disconnect(pw, browser)
        except SystemExit:
            raise
        except Exception:
            pass
    out(res)

# ---------------------------------------------------------------------------
# page actions
# ---------------------------------------------------------------------------

def act_navigate(p):
    url = p.get("url")
    if not url:
        fail("navigate requires 'url'", 2)
    pw, browser = connect()
    try:
        page = get_page(browser)
        resp = page.goto(url, timeout=timeout_of(p), wait_until="domcontentloaded")
        out({"title": page.title(), "url": page.url, "status": resp.status if resp else None})
    finally:
        disconnect(pw, browser)

def act_text(p):
    pw, browser = connect()
    try:
        page = get_page(browser)
        sel = p.get("selector")
        if sel:
            txt = page.locator(sel).first.inner_text(timeout=timeout_of(p))
        else:
            txt = page.inner_text("body")
        out({"text": txt})
    finally:
        disconnect(pw, browser)

def act_interactive(p):
    pw, browser = connect()
    try:
        page = get_page(browser)
        sel = p.get("selector") or "a,button,input,select,textarea,[role=button],[onclick]"
        els = page.query_selector_all(sel)
        # Tag -> implicit ARIA role. role= selectors need a real ARIA role,
        # not the tag name (role=a matches nothing; a[href] has role "link").
        input_role = {"submit": "button", "button": "button", "reset": "button",
                      "image": "button", "checkbox": "checkbox", "radio": "radio"}
        tag_role = {"a": "link", "button": "button", "select": "combobox", "textarea": "textbox"}
        role_known = {"link", "button", "textbox", "checkbox", "radio", "combobox",
                      "listbox", "menuitem", "tab", "option", "searchbox"}
        items = []
        for i, el in enumerate(els[:80]):
            try:
                tag = el.evaluate("e => e.tagName.toLowerCase()")
                text = (el.inner_text() or "").strip().replace("\n", " ")[:120]
                href = el.get_attribute("href") or ""
                name_attr = el.get_attribute("name") or ""
                aria = el.get_attribute("aria-label") or ""
                value = el.get_attribute("value") or ""
                eid = el.get_attribute("id") or ""
                if tag == "input":
                    itype = el.get_attribute("type") or ""
                    aria_role = el.get_attribute("role") or input_role.get(itype, "textbox")
                else:
                    aria_role = el.get_attribute("role") or tag_role.get(tag, "")
                # Build the most precise selector that will actually resolve.
                selector = None
                if eid and re.fullmatch(r"[A-Za-z][\w-]*", eid):
                    selector = f"#{eid}"
                elif aria:
                    selector = f'[aria-label="{aria}"]'
                elif aria_role in role_known and text:
                    selector = f'role={aria_role}[name="{text[:60]}"]'
                elif name_attr and tag in ("input", "textarea", "select"):
                    selector = f'[name="{name_attr}"]'
                elif text:
                    selector = f'text={text[:60]}'
                items.append({"index": i, "selector": selector, "tag": tag, "text": text,
                              "role": aria_role, "name": name_attr, "href": href, "value": value})
            except Exception:
                continue
        out({"count": len(items), "elements": items})
    finally:
        disconnect(pw, browser)

def act_click(p):
    sel = p.get("selector")
    if not sel:
        fail("click requires 'selector'", 2)
    pw, browser = connect()
    try:
        page = get_page(browser)
        page.click(sel, timeout=timeout_of(p))
        out({"clicked": True})
    finally:
        disconnect(pw, browser)

def act_type(p):
    sel = p.get("selector")
    text = p.get("text")
    if not sel or text is None:
        fail("type requires 'selector' and 'text'", 2)
    pw, browser = connect()
    try:
        page = get_page(browser)
        page.fill(sel, text, timeout=timeout_of(p))
        out({"typed": len(text)})
    finally:
        disconnect(pw, browser)

def act_eval(p):
    script = p.get("script")
    if not script:
        fail("eval requires 'script'", 2)
    pw, browser = connect()
    try:
        page = get_page(browser)
        stripped = script.strip()
        if stripped.startswith(("function", "(", "async")):
            result = page.evaluate(script)
        else:
            result = page.evaluate(f"() => ({script})")
        out({"result": result})
    finally:
        disconnect(pw, browser)

def act_wait(p):
    pw, browser = connect()
    try:
        page = get_page(browser)
        sel = p.get("selector")
        text = p.get("text")
        if sel:
            page.wait_for_selector(sel, timeout=timeout_of(p))
        elif text:
            page.wait_for_function(
                f"() => (document.body.innerText || '').includes({json.dumps(text)})",
                timeout=timeout_of(p))
        else:
            page.wait_for_load_state("domcontentloaded", timeout=timeout_of(p))
        out({"ok": True})
    finally:
        disconnect(pw, browser)

def act_screenshot(p):
    pw, browser = connect()
    try:
        page = get_page(browser)
        SHOTS_DIR.mkdir(parents=True, exist_ok=True)
        path = p.get("path") or str(SHOTS_DIR / f"shot-{int(time.time() * 1000)}.png")
        opts = {"path": path}
        if p.get("full"):
            opts["full_page"] = True
        sel = p.get("selector")
        if sel:
            page.locator(sel).first.screenshot(**opts)
        else:
            page.screenshot(**opts)
        out({"path": path})
    finally:
        disconnect(pw, browser)

def act_history_nav(p, kind):
    pw, browser = connect()
    try:
        page = get_page(browser)
        if kind == "back":
            page.go_back(timeout=timeout_of(p))
        elif kind == "forward":
            page.go_forward(timeout=timeout_of(p))
        else:
            page.reload(timeout=timeout_of(p))
        out({"title": page.title(), "url": page.url})
    finally:
        disconnect(pw, browser)

def main():
    # Params arrive as a base64 argv token (see execute() in browser.lua) so the
    # transport is shell-agnostic — no `export`/`$env:` and no heredoc.
    raw = sys.argv[1] if len(sys.argv) > 1 else ""
    try:
        p = json.loads(base64.b64decode(raw.encode()).decode()) if raw else {}
    except Exception as e:
        fail(f"bad params payload: {e}", 2)
    action = (p.get("action") or "").strip()
    handlers = {
        "start": act_start, "stop": act_stop, "status": act_status,
        "navigate": act_navigate, "text": act_text, "interactive": act_interactive,
        "click": act_click, "type": act_type, "eval": act_eval, "wait": act_wait,
        "screenshot": act_screenshot,
        "back": lambda x: act_history_nav(x, "back"),
        "forward": lambda x: act_history_nav(x, "forward"),
        "reload": lambda x: act_history_nav(x, "reload"),
    }
    fn = handlers.get(action)
    if not fn:
        fail(f"unknown action '{action}'. Valid: {', '.join(sorted(handlers))}", 2)
    try:
        fn(p)
    except SystemExit:
        raise  # fail() already printed — keep its exit code
    except Exception as e:
        fail(f"{action or '<empty>'} failed: {e}", 1)

main()
]=]

-- Base64 encoder (Lua has none built in). Operates on raw bytes, so multibyte
-- UTF-8 params round-trip correctly. Used to pass arbitrary JS/URLs/selectors
-- to the Python script without shell-escaping pitfalls.
local B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
local function b64encode(s)
    local out, i, n = {}, 1, #s
    while i <= n do
        local a, b, c = s:byte(i, i + 2)
        local v = a * 65536 + (b or 0) * 256 + (c or 0)
        local x = math.floor(v / 262144) % 64
        local y = math.floor(v / 4096) % 64
        local z = math.floor(v / 64) % 64
        local w = v % 64
        local chunk = B64:sub(x + 1, x + 1) .. B64:sub(y + 1, y + 1)
        if b then
            chunk = chunk .. B64:sub(z + 1, z + 1)
            chunk = chunk .. (c and B64:sub(w + 1, w + 1) or "=")
        else
            chunk = chunk .. "=="
        end
        out[#out + 1] = chunk
        i = i + 3
    end
    return table.concat(out)
end

-- Rewritten on the first call of each bone process so it always matches the
-- embedded script. Held in a closure upvalue, so subsequent calls skip the write.
local runner_installed = false

local function execute(params, ctx)
    -- Shell-agnostic transport: PowerShell (Windows) has no heredoc and the
    -- shell primitive nulls stdin, so we can't feed the script inline. Instead
    -- the runner is written to disk (config_dir always exists, so io.open needs
    -- no parent-dir setup) and params ride as a base64 argv token — safe to
    -- single-quote in both bash and PowerShell with no per-shell escaping, and
    -- no `export`/`$env:` dance for an env var.
    local runner = ctx.config_dir .. "/_browser_runner.py"
    if not runner_installed then
        local f, err = io.open(runner, "w")
        if not f then
            return "ERROR: cannot install browser runner at " .. runner .. ": " .. tostring(err)
        end
        f:write(PYTHON_SCRIPT)
        f:close()
        runner_installed = true
    end

    local payload = b64encode(cjson.encode(params or {}))
    -- python3 on POSIX; Windows ships no python3.exe, only python.
    local py = (package.config:sub(1, 1) == "\\") and "python" or "python3"
    -- Pin playwright==1.59.0 (matches the bundled chromium-1217). Keeps the
    -- engine deterministic and avoids random browser re-downloads.
    local cmd = "uv run --no-project --with 'playwright==1.59.0' -- "
        .. py .. " '" .. runner .. "' '" .. payload .. "'"

    -- Budget = per-action timeout + headroom, floored at 60s so start's own
    -- waits and uv/playwright resolution always fit. Without this, a large
    -- timeout_ms would be silently capped and killed by the outer shell call.
    local per_action = tonumber(params and params.timeout_ms) or 30000
    local budget = math.max(60000, per_action + 15000)
    local result = ctx.shell(cmd, { timeout_ms = budget })
    if result.exit_code ~= 0 then
        local err = (result.stderr and #result.stderr > 0) and result.stderr or (result.stdout or "")
        return "ERROR: " .. err
    end
    return result.stdout or ""
end

bone.register_tool({
    name = "browser",
    description = [[Drive a real web browser (Chromium) over a persistent CDP daemon. Call action=start once, then navigate, read, click, type, eval JS, wait, screenshot, and navigate history. The daemon stays running across calls (fast, keeps page state); call action=stop when done.

Lifecycle:
- start: launch the daemon (headless by default; headless=false to see the window). Idempotent.
- stop: kill the daemon.
- status: report running/pid/port/open pages.

Page actions (require the daemon running — call start first):
- navigate: go to url
- text: visible text of the page or an element (selector)
- interactive: list clickable elements with ready-to-use selectors — call this to see what you can click or type into
- click: click selector
- type: fill selector with text (replaces content)
- eval: run a JS expression (script) and return the JSON result
- wait: wait for selector, text, or load — pass selector or text
- screenshot: save a PNG to data/browser/shots (returns path; bone tool results are text so you reason from text+interactive, not the image)
- back / forward / reload

Selectors use Playwright syntax: css (#id, .cls), text=Foo, role=button[name="Sign in"], xpath=. Prefer the selector strings returned by interactive. Default timeout 30000ms; override with timeout_ms.]],
    parameters = {
        type = "object",
        properties = {
            action = {
                type = "string",
                description = "start, stop, status, navigate, text, interactive, click, type, eval, wait, screenshot, back, forward, reload",
                enum = {"start", "stop", "status", "navigate", "text", "interactive", "click", "type", "eval", "wait", "screenshot", "back", "forward", "reload"},
            },
            url = { type = "string", description = "URL for navigate." },
            selector = { type = "string", description = "Playwright selector for text/interactive/click/type/wait/screenshot." },
            text = { type = "string", description = "Value to type (action=type), or text to wait for (action=wait)." },
            script = { type = "string", description = "JS expression for eval. A bare expression like document.title is wrapped as () => (...); pass a function/async literal to control it." },
            headless = { type = "boolean", description = "start only: run headless (default true)." },
            full = { type = "boolean", description = "screenshot only: capture full page (default false)." },
            path = { type = "string", description = "screenshot only: output PNG path (default data/browser/shots/shot-<ts>.png)." },
            timeout_ms = { type = "number", description = "Per-action timeout in ms (default 30000)." },
        },
        required = { "action" },
        additionalProperties = false,
    },
    safety = "danger",
    display = { show = true, args = { "action", "url", "selector", "text" } },
    execute = execute,
})
