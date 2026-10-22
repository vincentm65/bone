-- The GUI's own Lua API, loaded after runtime/ui/api.lua (the API every
-- frontend shares) and before runtime/gui/defaults.lua and ~/.bone/gui.lua.
-- Check bone.has_capability("gui.window") / ("gui.menus") before using it
-- from shared code.

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
  --- The menu bar: { { "File", { { "New chat", "new_session" }, "-",
  --- { "Help", "/help" }, { "Say hi", function() bone.notify("hi") end } } } }.
  --- An item does a built-in action, a /command, or a function. {} removes it.
  menu = function(menus)
    api("gui_menu", menus or {})
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
