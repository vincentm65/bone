-- Standalone Lua/LuaJIT fixtures; no bone server or third-party modules required.
local dir = (arg[0]:match("^(.*)/") or ".") .. "/"
local function copy(v)
  if type(v) ~= "table" then return v end
  local out = {}; for k, x in pairs(v) do out[k] = copy(x) end; return out
end
local rpc, sessions, saved, creates = {}, {}, 0, 0
local state = {}
local function fixture()
  sessions.a = { info = { session_id = "a", cwd = "/tmp", title = "Source" }, generation = 1,
    model = { provider = "test", model = "pinned" }, summary = { through = 4, text = "summary" }, messages = {
      { role = "user", content = "first", images = { { id = "blob", name = "img" } } },
      { role = "assistant", reasoning_state = { items = { "sealed" } }, tool_calls = { { id = "t1", name = "shell", arguments = { command = "true" } } } },
      { role = "tool", call_id = "t1", content = "ok" },
      { role = "assistant", content = "done" },
      { role = "user", content = "second", images = { { id = "blob2" } } },
      { role = "assistant", content = "answer" },
    } }
end
bone = {
  state = { load = function() return state end, save = function(name, s) assert(name == "fork" and s.edges); saved = saved + 1 end },
  rpc = { register = function(n, fn) rpc[n] = fn end },
  session = { snapshot = function(id) return copy(assert(sessions[id], "missing")) end,
    create = function(opts)
      creates = creates + 1
      local id = "child" .. creates
      sessions[id] = copy(opts); sessions[id].info = { session_id = id, cwd = opts.cwd }
      sessions[id].generation = 1
      return sessions[id].info
    end },
}
fixture(); dofile(dir .. "core.lua")
local ctx = { session_id = "a" }
local function tree(args) return rpc["fork.tree"](args or {}, ctx) end
local function create(args) return rpc["fork.create"](args or {}, ctx) end
local function reject(args, pattern)
  local n = creates
  local ok, err = pcall(create, args)
  assert(not ok and tostring(err):find(pattern), tostring(err)); assert(creates == n)
end
local t = tree()
assert(#t.points == 2 and t.points[2].copied_count == 4 and t.points[2].safe)
for _, n in ipairs({ 0, -1, 1.5, 3, "2" }) do reject({ before_turn = n }, n == 3 and "out of range" or "positive integer") end
local r = create({ before_turn = 2, expected = t.expected })
local child = sessions[r.info.session_id]
assert(#child.messages == 4 and child.summary.through == 4 and child.model.model == "pinned")
assert(child.messages[2].reasoning_state.items[1] == "sealed" and child.messages[1].images[1].id == "blob")
assert(r.draft.text == "second" and r.draft.images[1].id == "blob2" and not child.owner and not child.parent)
assert(saved == 1 and tree().edges[r.info.session_id].before_turn == 2)
assert(not sessions[create({ before_turn = 1 }).info.session_id].summary)
local whole = create({})
assert(#sessions[whole.info.session_id].messages == 6)
sessions.a.messages[1].content = "changed without generation bump"
reject({ expected = t.expected }, "source changed")
fixture(); t = tree(); sessions.a.generation = 2
reject({ expected = t.expected }, "source changed")
sessions.a.active_turn = 7
reject({}, "wait for the source")
assert(create({ before_turn = 1 })) -- active source is safe for an early complete prefix
fixture(); table.remove(sessions.a.messages, 3) -- cutoff before second user now leaves t1 unresolved
assert(not tree().points[2].safe)
reject({ before_turn = 2 }, "unresolved tool calls")
reject({}, "unresolved tool calls")
fixture(); sessions.a.info.parent = "legacy-root"
local legacy = tree({ sessions = { { session_id = "sibling", parent = "legacy-root" } } })
assert(legacy.edges.a.legacy and legacy.edges.sibling.parent == "legacy-root")
assert(#legacy.sessions >= 3 and saved >= 5)
-- Parsed fields deliberately cannot represent the nested null/empty-array
-- distinctions. The create API must receive the exact original JSON strings.
fixture()
sessions.a.raw_messages = {
  '{"role":"user","content":"first","images":[{"id":"blob","name":"img"}]}',
  '{"role":"assistant","reasoning_state":{"items":[null,[],{"sealed":null}]},"tool_calls":[{"id":"t1","name":"shell","arguments":{"command":"true","nullable":null,"empty":[]}}]}',
  '{"role":"tool","call_id":"t1","content":"ok"}',
  '{"role":"assistant","content":"done"}',
  '{"role":"user","content":"second","images":[{"id":"blob2"}]}',
  '{"role":"assistant","content":"answer"}',
}
t = tree()
r = create({ before_turn = 2, expected = t.expected })
child = sessions[r.info.session_id]
assert(#child.messages == 4 and child.summary.through == 4)
for i = 1, 4 do assert(type(child.messages[i]) == "string" and child.messages[i] == sessions.a.raw_messages[i]) end
assert(r.draft.text == "second" and r.draft.images[1].id == "blob2")
whole = sessions[create({}).info.session_id]
for i = 1, 6 do assert(whole.messages[i] == sessions.a.raw_messages[i]) end
-- Raw fingerprints catch changes invisible in the parsed snapshot.
sessions.a.raw_messages[2] = sessions.a.raw_messages[2]:gsub('"sealed":null', '"sealed":[]')
reject({ expected = t.expected }, "source changed")
t = tree()
sessions.a.messages[2].reasoning_state = {} -- a lossy decoded representation is not the identity
assert(create({ before_turn = 2, expected = t.expected }))
table.remove(sessions.a.messages, 3); table.remove(sessions.a.raw_messages, 3)
assert(not tree().points[2].safe)
reject({ before_turn = 2 }, "unresolved tool calls")
reject({}, "unresolved tool calls")
print("core: exact raw JSON/parsed copy, model/summary, bounds, stale views, active/tool safety, family/legacy OK")

-- Async TUI fixture: RPCs are queued, rendering must not issue one.
fixture(); state.edges = {}
local queue, shown, shutdown, command, wins, nextwin, id = {}, nil, nil, nil, {}, 0, "a"
local prompt, images, notices, opens = "source draft", {}, {}, 0
bone.api = { session = function() return { session_id = id } end,
  prompt_set = function(text) prompt = text end,
  open_session = function(target) opens = opens + 1; id = nil; prompt, images = "", {}; shown.target = target end }
bone.prompt = { set_images = function(refs) images = refs end,
  empty = function() return prompt == "" and #images == 0 end }
bone.plugin = { on_shutdown = function(fn) shutdown = fn end }
bone.on = function(event, fn) if event == "chat/shown" then shown = { fn = fn } end end
bone.notify = function(text) notices[#notices + 1] = text end
bone.cmd = { create = function(name, fn) assert(name == "fork"); command = fn end }
bone.text = { truncate = function(text, width) return text:sub(1, width) end }
bone.ui = { popup = function(opts) nextwin = nextwin + 1; wins[nextwin] = opts; return nextwin end,
  is_open = function(w) return wins[w] ~= nil end, close = function(w) wins[w] = nil end,
  box = function(lines) return lines end }
bone.rpc.call = function(name, args, cb) queue[#queue + 1] = function()
  local ok, result = pcall(rpc[name], args, ctx); cb(ok and result or nil, not ok and result or nil)
end end
bone.request = function(name, args, cb) assert(name == "session/list"); queue[#queue + 1] = function() cb({ sessions.a.info }) end end
local function flush() local fn = table.remove(queue, 1); assert(fn); fn() end
local function run(arg) command({ args = arg }) end
local function shown_child() id = shown.target; shown.fn() end
dofile(dir .. "tui.lua")
run("2"); flush(); flush()
assert(prompt == "" and id == nil)
shown_child(); assert(prompt == "second" and images[1].id == "blob2")
-- User edits made while the destination loads must never be overwritten.
id = "a"; run("2"); flush(); flush()
prompt, images = "new user draft", { { id = "new-image" } }
shown_child(); assert(prompt == "new user draft" and images[1].id == "new-image")
assert(notices[#notices]:find("kept your edits", 1, true))
id = "a"; run(""); local w = wins[nextwin]
flush(); flush()
local n = #queue; w.lines({ width = 84, height = 22 }); assert(#queue == n)
w.keys.down(); w.keys.enter() -- first user point: prefix is empty
assert(#queue == 1); w.keys.esc(); local before = opens; flush(); assert(opens == before)
run(""); w = wins[nextwin]; w.keys.esc(); flush(); assert(#queue == 0)
-- Known cutoffs are adjacent to their user point, not after every user row.
state.edges = {
  first = { parent = "a", before_turn = 1 },
  second = { parent = "a", before_turn = 2 },
  legacy = { parent = "a", legacy = true },
}
local function rendered()
  local lines, out = w.lines({ width = 200, height = 22 }), {}
  for _, line in ipairs(lines) do out[#out + 1] = line[1][1] end
  return table.concat(out, "\n")
end
run(""); w = wins[nextwin]; flush(); flush()
local text = rendered()
local firstpos, point1 = text:find("first  ← before #1", 1, true), text:find("↳ 1", 1, true)
local secondpos, point2 = text:find("second  ← before #2", 1, true), text:find("↳ 2", 1, true)
assert(firstpos and point1 and secondpos and point2 and firstpos < point1 and point1 < secondpos and secondpos < point2, text)
assert(text:find("legacy (unknown cutoff)", 1, true) and text:find("[missing]", 1, true), text)
w.keys.esc()
-- Missing ancestors still expose their descendants without a snapshot RPC.
state.edges = { a = { parent = "missing", before_turn = 2 } }
run(""); w = wins[nextwin]; flush(); flush(); text = rendered()
assert(text:find("missing [missing]", 1, true) and text:find("Source", 1, true), text)
w.keys.home(); w.keys.enter()
assert(notices[#notices]:find("missing") and opens == before)
w.keys.down(); w.keys.enter(); assert(opens == before + 1) -- source still opens
id = "a"
-- A malformed legacy cycle terminates and displays each session only once.
state.edges = { a = { parent = "loop", legacy = true }, loop = { parent = "a", legacy = true } }
run(""); w = wins[nextwin]; flush(); flush(); text = rendered()
local _, source_count = text:gsub("Source", "")
local _, loop_count = text:gsub("loop  ←", "")
assert(source_count == 1 and loop_count == 1, text)
w.keys.esc()
run("copy"); shutdown(); flush(); assert(#queue == 0 and opens == before + 1)
print("tui: fork-point order, missing/cyclic ancestors, delayed prefill, no render RPC, closed/unload guards OK")
