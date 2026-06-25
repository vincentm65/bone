# Bone — repo-internal notes

Only relevant when editing this repository (`~/projects/bone`).

## Architecture
- The TUI uses the core Driver as its only turn loop. The old non-Driver TUI
  loop and the `BONE_DRIVER` toggle are gone.
- Text streaming is via `RuntimeEvent::TextDelta`. `AgentRunEvent` is only a
  compatibility alias to `RuntimeEvent` for now.

## Browser Tool
- The real `browser` catalog tool is the v7 observe/target daemon tool in
  `~/.bone-rust/lua/tools/browser.lua` / `bone-catalog/tools/browser.lua`, not
  the older browser-use autonomous-agent version described in some defaults.
- It exposes a small remote-control API: `open`, `observe`, `click`, `type`,
  `select`, `check`, `uncheck`, `press`, `scroll`, `wait_for`, `eval`, `tabs`,
  `current`, `back`, and `stop`. `read`/`scrape` remain aliases for `observe`.
- `observe` returns visible text plus `targets[]` with stable IDs like `t03`.
  The host model should pass `target=<id>` to actions and should not invent CSS
  selectors for normal browser tasks.
- The browser is always headful/visible. Headless mode is intentionally disabled
  and supplied `headless` values must be ignored.
- For browser tasks, keep using browser actions for page state and interaction;
  do not fall back to shell/curl/grep/Python scraping unless the user asks for
  shell-level inspection or the browser tool is blocked.
- The tool launches a persistent Python Playwright daemon. Each call sends one
  JSON request to localhost; the browser process, page state, cookies, and
  per-engine profile under `~/.bone-rust/data/browser/` persist across tool
  calls and sessions until `stop` is called.
- There is no separate LLM inside the browser runner and no provider/API-key
  plumbing for the tool. It runs a Python Playwright runner via
  `uv run --no-project --with playwright`.
- Screenshots return a PNG path, typically under `~/.bone-rust/data/browser/`;
  use `read_file` on that image path to inspect the page visually.
