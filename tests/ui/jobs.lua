-- bone.job, the same in every frontend: a command's output and exit,
-- lines as they come, writing to one, and a terminal job's screen.
local got = { lines = {} }
bone.job.start({ "sh", "-c", "echo one; echo two >&2; exit 3" }, {
  on_exit = function(r) got.exit = r end,
})
bone.job.start({ "sh", "-c", "printf 'a\\nb\\n'" }, {
  lines = true,
  on_stdout = function(line) got.lines[#got.lines + 1] = line end,
})
local cat = bone.job.start({ "cat" }, {
  stdin = true,
  on_stdout = function(text) got.echo = (got.echo or "") .. text end,
})
local term = bone.job.start({ "sh", "-c", "printf 'in a terminal'; sleep 5" }, { pty = { width = 30, height = 4 } })

local function first_line()
  local s = term:screen()
  local t = {}
  for _, item in ipairs(s and s.lines[1] or {}) do t[#t + 1] = item[1] end
  return table.concat(t)
end

return {
  function()
    cat:write("echoed\n")
    cat:close_stdin()
    __wait = function()
      return got.exit ~= nil and got.echo ~= nil and #got.lines == 2
        and first_line():match("in a terminal") ~= nil
    end
  end,
  function()
    eq(got.exit.code, 3, "the exit code")
    eq(got.exit.stdout, "one\n", "stdout kept for on_exit")
    eq(got.exit.stderr, "two\n", "and stderr")
    eq(got.lines[1], "a", "whole lines")
    eq(got.lines[2], "b", "in order")
    eq(got.echo, "echoed\n", "written to stdin, read back")
    assert(term:cancel(), "cancelled while running")
  end,
}
