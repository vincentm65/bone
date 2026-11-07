-- Net tracked Git changes against HEAD, including staged and unstaged edits.
-- Cache per working directory; never run Git synchronously while drawing.
local M = {}
local cache = {}
local interval = 2000
local command = [[
git rev-parse --show-toplevel >/dev/null 2>&1 || exit $?
base=HEAD
if ! git rev-parse --verify HEAD >/dev/null 2>&1; then
  base=$(git hash-object -t tree /dev/null) || exit $?
fi
git --no-pager diff --numstat --no-ext-diff --no-textconv "$base" --
]]

-- The colorscheme's InputAdded and InputRemoved; one without them gets the
-- diff colors on the input's background.
function M.setup()
  local bg = (bone.hl.get("InputBackground") or {}).bg
  if not bone.hl.get("InputAdded").fg then
    bone.hl.set("InputAdded", { fg = (bone.hl.get("DiffAdd") or {}).fg, bg = bg })
  end
  if not bone.hl.get("InputRemoved").fg then
    bone.hl.set("InputRemoved", { fg = (bone.hl.get("DiffDelete") or {}).fg, bg = bg })
  end
end

bone.on("turn/finished", function()
  for _, state in pairs(cache) do state.dirty = true end
end)

function M.top(ctx)
  local cwd = ctx.session and ctx.session.cwd
  -- A new chat has no session cwd yet; use the TUI's launch directory.
  local key = cwd or ""
  local state = cache[key]
  if not state then
    state = { next_poll = 0 }
    cache[key] = state
  end
  if not state.pending and (state.dirty or bone.now() >= state.next_poll) then
    state.pending, state.dirty = true, false
    bone.system(command, { cwd = cwd, timeout = 5000 }, function(result)
      state.pending = false
      state.next_poll = bone.now() + interval
      state.added, state.removed = nil, nil
      if not result or result.code ~= 0 or result.timed_out then return end
      state.added, state.removed = 0, 0
      for line in (result.stdout or ""):gmatch("[^\n]+") do
        local added, removed = line:match("^(%d+)\t(%d+)\t")
        -- Binary files have '-' counts, not lines; skip them.
        if added then
          state.added = state.added + tonumber(added)
          state.removed = state.removed + tonumber(removed)
        end
      end
    end)
  end
  bone.ui.refresh_in(interval)
  if state.added == nil then return {} end
  local added, removed = "+" .. state.added, "−" .. state.removed
  if bone.text.width(added .. " " .. removed .. " ") > ctx.width then return {} end
  return {
    { fill = " ", hl = "InputBackground" },
    { added, "InputAdded" }, { " ", "InputBackground" },
    { removed, "InputRemoved" }, { " ", "InputBackground" },
  }
end

return M
