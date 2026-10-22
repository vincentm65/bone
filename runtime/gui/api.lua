-- The GUI's own Lua API, loaded after runtime/ui/api.lua (the API every
-- frontend shares) and before runtime/gui/defaults.lua and ~/.bone/gui.lua.
-- Check bone.has_capability("gui.window") before using it from shared code.

local api = bone._api

bone.gui = {
  --- Change the window: { title, width, height (logical pixels),
  --- fullscreen, maximized, minimized }; only what is given changes.
  window = function(spec)
    api("gui_window", spec or {})
  end,
  --- { width, height, columns, fullscreen, maximized, focused }.
  window_info = function()
    return api("gui_window_info")
  end,
  --- A desktop notification (gui.notifications).
  notify = function(title, body)
    api("gui_notify", title, body)
  end,
  --- The system's file dialogs (gui.dialogs): opts = { title, dir,
  --- filters = { { "Images", { "png", "jpg" } } }, multiple (open), name
  --- (save) }; then(path) gets the path, a list with `multiple`, or nil
  --- when cancelled.
  open_file = function(opts, then_)
    api("gui_open_file", opts or {}, then_)
  end,
  save_file = function(opts, then_)
    api("gui_save_file", opts or {}, then_)
  end,
  --- The GUI's library, separate from the TUI's (/gui): info() →
  --- { active, builtin = { names }, custom = { names with a gui/ folder } };
  --- use(name) switches to `standard`, `bare`, a built-in one (`desktop`)
  --- or a custom one and loads the GUI's Lua again, going back if it does
  --- not load.
  library = {
    info = function()
      return api("gui_library_info")
    end,
    use = function(name)
      api("gui_library_use", name)
    end,
  },
}

--- Rich content for panels and windows (gui.blocks). A spec for
--- bone.ui.panel.open, bone.ui.popup or bone.ui.win may give
---   blocks = function(ctx) return { { id = …, block = {…}, selected = bool }, … } end
--- with ctx = { width, height } (points) and `block` as a chat view's
--- (see the GUI's blocks); the GUI stacks them in a scroll area of its
--- own, keeping the selected one in view, and runs on_block(id) when one
--- is clicked. Keys, focus and closing stay the panel's or window's.
bone._gui_blocks = {}
local function take_blocks(spec)
  if type(spec) ~= "table" or not spec.blocks then return nil end
  local rich = { blocks = spec.blocks, on_block = spec.on_block }
  spec.blocks, spec.on_block = nil, nil
  return rich
end

local panel_open = bone.ui.panel.open
bone.ui.panel.open = function(spec)
  local rich = take_blocks(spec)
  if rich and not spec.render and not spec.lines then spec.lines = {} end
  local p = panel_open(spec)
  bone._gui_blocks[p.id] = rich
  return p
end

for _, name in ipairs({ "popup", "win" }) do
  local open = bone.ui[name]
  bone.ui[name] = function(spec)
    local rich = take_blocks(spec)
    if rich and not spec.lines then spec.lines = {} end
    local id = open(spec)
    bone._gui_blocks["popup:" .. tostring(id)] = rich
    return id
  end
end
local update = bone.ui.update
bone.ui.update = function(id, spec)
  local rich = take_blocks(spec)
  if rich then bone._gui_blocks["popup:" .. tostring(id)] = rich end
  return update(id, spec)
end

-- What a closed panel or window drew with goes with it.
bone.on("panel/closed", function(ev)
  bone._gui_blocks[ev.kind == "popup" and ("popup:" .. tostring(ev.id)) or ev.id] = nil
end)
