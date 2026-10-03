-- testrun: run the tests in the background and watch the output stream into
-- a panel under the chat.
--
--   /test              run bone.o.test_command (default "cargo test")
--   /test npm test     run something else
--   /test cancel       stop it
--
-- In the panel: the usual scrolling keys, c cancels, f puts the failures in
-- the prompt for the model, esc goes back. The title shows the state.

bone.o.define("test_command", "cargo test", { desc = "what /test runs" })

local panel, job
local lines, failures = {}, {}
local state = "idle"

-- Lines that usually point at a failure.
local function failing(line)
  return line:match("FAILED") or line:match("panicked") or line:match("^error") or line:match("%f[%w]FAIL%f[%W]")
    or line:match("^%s*not ok")
end

local function title()
  local t = "Tests: " .. state
  if job and job:running() then
    t = t .. " " .. math.floor((job:status().elapsed_ms or 0) / 1000) .. "s"
  end
  if #failures > 0 then
    t = t .. " · " .. #failures .. " failing lines (f)"
  end
  return t
end

local function render()
  local out = {}
  for i, l in ipairs(lines) do
    out[i] = { { l, failing(l) and "ErrorMsg" or "ToolOutput" } }
  end
  return out
end

local function refresh()
  if panel then
    panel:update({ title = title() })
  end
end

local function add(line)
  lines[#lines + 1] = line
  if failing(line) then
    failures[#failures + 1] = line
  end
  refresh()
end

local function cancel()
  if job and job:running() then
    job:cancel()
  end
end

local function send_failures()
  if #failures == 0 then
    return bone.notify("no failures to send")
  end
  local shown = {}
  for i = 1, math.min(#failures, 40) do
    shown[i] = failures[i]
  end
  bone.prompt.set("These tests fail:\n\n" .. table.concat(shown, "\n") .. "\n\nFind the cause and fix it.")
  bone.ui.panel.focus(nil)
end

local function run(cmd)
  cancel()
  lines, failures, state = {}, {}, "running"
  local s = bone.chat.session()
  if panel and panel:is_open() then
    panel:show()
    panel:scroll("bottom")
  else
    panel = bone.ui.panel.open({
      id = "testrun",
      dock = "bottom",
      size = 0.4,
      follow = true,
      title = title(),
      render = render,
      keys = { c = cancel, f = send_failures },
    })
  end
  lines[1] = "$ " .. cmd -- the command itself never counts as a failure
  job = bone.job.start(cmd, {
    name = "tests",
    cwd = s and s.cwd or nil,
    lines = true,
    on_stdout = add,
    on_stderr = add,
    on_exit = function(r)
      if r.state == "exited" then
        state = r.code == 0 and "passed" or ("failed (exit " .. r.code .. ")")
      else
        state = r.state:gsub("_", " ")
      end
      add("")
      add(("%s in %.1fs"):format(state, r.duration_ms / 1000))
      bone.notify("tests " .. state, r.code == 0 and "info" or "error")
    end,
  })
  refresh()
end

bone.cmd.create("test", function(c)
  if c.args == "cancel" then
    return cancel()
  end
  run(c.args ~= "" and c.args or bone.o.test_command)
end, {
  desc = "run the tests into a panel; /test cancel stops them",
  complete = function()
    return { { value = "cancel", desc = "stop the running tests" } }
  end,
})

-- The elapsed time in the title moves while it runs.
local function tick()
  if job and job:running() then
    refresh()
    bone.defer(1000, tick)
  end
end
bone.on("job/started", function(ev)
  if job and ev.id == job.id then
    bone.defer(1000, tick)
  end
end)
