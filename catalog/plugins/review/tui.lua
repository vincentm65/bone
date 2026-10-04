-- review: the files the session on screen has changed, in a panel under the
-- chat, with quick ways to look at them and to ask the model about them.
--
--   /review          show or hide the panel (it follows the session)
--   /review all      put a review request for every changed file in the prompt
--
-- In the panel: up/down select, enter asks for a review of that file (in
-- the prompt, for you to edit and send), g shows its git diff, esc goes back.

local panel
local files, sel = {}, 1

-- Changed files, from the edit_file/write_file calls of this session.
local function collect()
  files = {}
  local by_path = {}
  for _, it in ipairs(bone.chat.items({ kind = "tool" })) do
    local path = type(it.arguments) == "table" and it.arguments.path
    if path and (it.name == "edit_file" or it.name == "write_file") then
      local f = by_path[path]
      if not f then
        f = { path = path, edits = 0, failed = 0, turns = {} }
        by_path[path] = f
        files[#files + 1] = f
      end
      f.edits = f.edits + 1
      if it.is_error then
        f.failed = f.failed + 1
      end
      if f.turns[#f.turns] ~= it.turn then
        f.turns[#f.turns + 1] = it.turn
      end
    end
  end
  sel = math.max(1, math.min(sel, #files))
end

local function render(ctx)
  collect()
  if #files == 0 then
    return { { { "no files changed in this session yet", "Dim" } } }
  end
  local lines = {}
  for i, f in ipairs(files) do
    local hl = (ctx.focused and i == sel) and "Selection" or "Normal"
    local note = f.edits .. (f.edits == 1 and " edit" or " edits") .. ", turn " .. table.concat(f.turns, ",")
    if f.failed > 0 then
      note = note .. ", " .. f.failed .. " failed"
    end
    lines[i] = { { f.path, hl == "Normal" and "ToolPath" or hl }, { "  " .. note, hl == "Normal" and "Dim" or hl }, { fill = " ", hl = hl } }
  end
  return lines
end

local function ask(list)
  if #list == 0 then
    return bone.notify("no changed files to review")
  end
  local names = {}
  for _, f in ipairs(list) do
    names[#names + 1] = f.path
  end
  bone.prompt.set("Review your changes to " .. table.concat(names, ", ")
    .. ": look for bugs, missing error handling and missing tests, and say what you would change.")
  bone.prompt.select(0) -- all of it, so typing replaces it
  bone.ui.panel.focus(nil)
end

-- The file's diff from git, in a pager, read in the background.
local function diff(f)
  local s = bone.chat.session()
  local pager = bone.ui.pager("loading…", { title = "git diff " .. f.path })
  bone.job.start({ "git", "diff", "--no-color", "--", f.path }, {
    cwd = s and s.cwd or nil,
    on_exit = function(r)
      local text = r.stdout ~= "" and r.stdout or (r.stderr ~= "" and r.stderr or "no changes against git")
      pager:set(text)
    end,
  })
end

local keys = {
  up = function()
    sel = math.max(1, sel - 1)
  end,
  down = function()
    sel = math.min(math.max(#files, 1), sel + 1)
  end,
  enter = function()
    if files[sel] then
      ask({ files[sel] })
    end
  end,
  g = function()
    if files[sel] then
      diff(files[sel])
    end
  end,
}

bone.cmd.create("review", function(c)
  if c.args == "all" then
    collect()
    return ask(files)
  end
  if panel and panel:is_open() then
    panel:toggle()
  else
    panel = bone.ui.panel.open({ id = "review", dock = "bottom", size = "auto", max = 8, title = "Changed files", render = render, keys = keys })
  end
end, {
  desc = "files changed in this session; /review all asks for a review",
  complete = function()
    return { { value = "all", desc = "ask for a review of every changed file" } }
  end,
})
