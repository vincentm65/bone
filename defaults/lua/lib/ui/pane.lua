-- ui.pane — reusable pane object + shared helpers for interactive panes.
--
-- Owns a channel-transport pane (`ctx.ui.pane`) so interactive Lua (tools,
-- commands) can open, update, and close a bottom-pane window without
-- re-implementing the event loop each time. Built on the channel transport
-- (not `bone.api.ui`) because it must render even while a tool blocks on
-- `ctx.ui.key()` — the VM mutex is held then, so the `UiState` drain would
-- fail. The channel is the escape hatch.
--
-- Exposes the styled-line helpers (`span`, `line`, `clamp`) and the nil-safe
-- key reader (`wait_key`, `key_name`, `is_text_key`) that every interactive
-- pane needs, so `ui.menu` and any new pane share one toolkit.

local M = {}

-- ── styled-line helpers ──────────────────────────────────────────────────

function M.span(text, fg, modifiers)
    return { text = tostring(text or ""), fg = fg, modifiers = modifiers or {} }
end

function M.line(...)
    return { spans = { ... } }
end

function M.clamp(n, lo, hi)
    if n < lo then return lo end
    if n > hi then return hi end
    return n
end

-- ── key helpers ──────────────────────────────────────────────────────────

-- Read one key from the terminal via `ctx.ui.key()`.
-- Returns the key table or nil (unavailable / cancelled / malformed).
function M.wait_key(ctx)
    if not ctx or not ctx.ui or type(ctx.ui.key) ~= "function" then
        return nil
    end
    local ok, key = pcall(ctx.ui.key)
    if not ok or type(key) ~= "table" then
        return nil
    end
    return key
end

function M.key_name(key)
    if type(key) ~= "table" then return nil end
    return key.code
end

function M.is_text_key(key)
    return type(key) == "table"
        and key.code == "Char"
        and key.char
        and not key.ctrl
        and not key.alt
end

-- ── Pane object ──────────────────────────────────────────────────────────

local Pane = {}
Pane.__index = Pane

--- Open a pane backed by the channel transport.
--- opts: { id = "source", title = "...", visible_rows = N }
function M.new(ctx, opts)
    opts = opts or {}
    local self = setmetatable({}, Pane)
    self.ctx = ctx
    self.id = opts.id or "interact"
    self.title = opts.title or ""
    self.visible_rows = opts.visible_rows
    return self
end

function Pane:set_title(title)
    self.title = title
    return self
end

--- Re-emit the pane with new lines (incremental re-render).
--- visible_rows (optional) overrides the default for this emit.
function Pane:set_lines(lines, visible_rows)
    local n = lines and #lines or 0
    pcall(self.ctx.ui.pane, {
        source = self.id,
        title = self.title,
        lines = lines or {},
        visible_rows = visible_rows or self.visible_rows or math.max(3, n),
    })
    return self
end

--- Close the pane (emit empty lines → removal).
function Pane:close()
    pcall(self.ctx.ui.pane, { source = self.id, title = "", lines = {} })
end

--- Read one key (nil-safe wrapper over `ctx.ui.key`).
function Pane:wait_key()
    return M.wait_key(self.ctx)
end

--- Loop: render-then-wait is the caller's job; this is a pure key loop.
--- fn(key) → truthy stops the loop and returns that value; falsy keeps looping.
function Pane:key_loop(fn)
    while true do
        local key = self:wait_key()
        if not key then return nil end
        local result = fn(key)
        if result then return result end
    end
end

return M
