-- The GUI's standard library (the default): the window as a tree of nodes, in
-- Lua (runtime/lua/bone/nodes/): graphite colors, bubbles for your messages,
-- proportional Markdown answers, tool calls as cards, a card composer, the
-- sessions sidebar, the tray and a status bar. Its looks are the GUI's
-- alone; what it shares with the TUI is behaviour (commands, keys, the `/`
-- menu, options) and data. ~/.bone/ui.lua and gui.lua run after.
require("bone.nodes.library").setup()

-- Reasoning effort, on a slider in the composer's toolbar. Press anywhere on
-- it to jump to a stop, drag to slide (the knob follows the pointer; the
-- setting is written once, when you let go), or turn the wheel over it to
-- step. It writes the session's provider's `reasoning_effort`, the same field
-- /config edits; "auto" leaves it unset, so the provider chooses.
local EFFORTS = { "default", "none", "low", "medium", "high", "xhigh", "max" }
local NAMES = { default = "auto" }
local TRACK_W, H = 92, 28
local KNOB, RING = 5.5, 1.5 -- the knob's radius, and the gap cut around it
local INSET = KNOB + RING + 1 -- the rail ends where the knob still fits

-- Kept across rebuilds (the slot is built again on every redraw).
local ui = {
  hover = false,
  drag = nil,    -- the stop under the pointer while the button is held
  pending = nil, -- the value written, until settings report it back
  wheel = 0,     -- wheel travel not yet turned into a step
}

local function name(e) return NAMES[e] or e end

-- `#rrggbb` + alpha, from a highlight group's foreground (or background).
local function tone(group, aa, field, fallback)
  local h = bone.hl.get(group) or {}
  local c = h[field or "fg"]
  if type(c) == "string" and c:match("^#%x%x%x%x%x%x$") then return c .. (aa or "") end
  return fallback
end

bone.gui.slots.add("composer.tools", {
  id = "reasoning", order = 10,
  node = function(ctx)
    if ctx.width and ctx.width < 420 then return nil end
    local usage = require("bone.usage")
    local style = require("bone.nodes.style")
    local draw = bone.gui.draw
    local session = ctx.session and ctx.session.session_id
    local provider = (usage.of(session) or {}).provider
    if not provider then return nil end
    local path = "providers." .. provider .. ".reasoning_effort"
    local stored = bone.settings.get(path) or "default"
    if ui.pending == stored then ui.pending = nil end
    local current = ui.pending or stored
    local last = #EFFORTS
    local at = 1
    for k, e in ipairs(EFFORTS) do if e == current then at = k end end
    -- What the knob shows: the stop being dragged to, else the setting.
    local shown = ui.drag or at

    local function commit(k)
      k = math.max(1, math.min(last, k))
      local value = EFFORTS[k]
      if value == current then return end
      ui.pending = value
      local done = function() ui.pending = nil; bone.gui.invalidate() end
      if value == "default" then bone.settings.reset(path, done)
      else bone.settings.set(path, value, done) end
      bone.gui.invalidate()
    end

    -- The stop nearest to `x`, pixels from the track's left edge.
    local function stop_at(x, w)
      local span = math.max(1, (w or TRACK_W) - 2 * INSET)
      local t = math.max(0, math.min(1, ((x or 0) - INSET) / span))
      return math.floor(t * (last - 1) + 0.5) + 1
    end

    local function drag_to(k)
      if k ~= ui.drag then ui.drag = k; bone.gui.invalidate() end
    end

    local track = {
      id = "reasoning-track", width = TRACK_W, height = H, shrink = 0, cursor = "pointer",
      label = "Reasoning effort",
      tooltip = "Reasoning effort: drag, click a stop, or scroll",
      canvas = { draw = function(c)
        local w, mid = c.width or TRACK_W, (c.height or H) / 2
        local fg = tone("Normal", nil, "fg", "#ededed")
        local surface = tone("InputBackground", nil, "bg", nil) or tone("Normal", nil, "bg", "#0f0f0f")
        local rail = tone("Normal", "2e", "fg", "#ffffff2e")
        local fill = tone("Normal", "cc", "fg", "#ededed")
        local x0, span = INSET, w - 2 * INSET
        local step = span / (last - 1)
        local knob = x0 + (shown - 1) * step
        local active = ui.hover or ui.drag ~= nil
        local unset = EFFORTS[shown] == "default"

        local out = {
          draw.rect{ x = x0 - 2, y = mid - 2, w = span + 4, h = 4, radius = 2, fill = rail },
        }
        if knob > x0 then
          out[#out + 1] = draw.rect{ x = x0 - 2, y = mid - 2, w = knob - x0 + 2, h = 4, radius = 2, fill = fill }
        end
        -- The stops: small dots on the rail, cut out of the filled part.
        for k = 1, last do
          local x = x0 + (k - 1) * step
          if math.abs(x - knob) > KNOB + RING then
            out[#out + 1] = draw.circle{ x = x, y = mid, r = 1,
              fill = k < shown and surface or tone("Normal", "73", "fg", "#ededed73") }
          end
        end
        -- The knob: a gap of the composer's color around it, a soft halo
        -- while the pointer is on it, solid when set and a ring on auto.
        if active then
          out[#out + 1] = draw.circle{ x = knob, y = mid, r = KNOB + 5, fill = tone("Normal", "1a", "fg", "#ffffff1a") }
        end
        out[#out + 1] = draw.circle{ x = knob, y = mid, r = KNOB + RING, fill = surface }
        local r = active and KNOB + 0.5 or KNOB
        if unset then
          out[#out + 1] = draw.circle{ x = knob, y = mid, r = r - 0.75, fill = surface, stroke = fg, width = 1.5 }
        else
          out[#out + 1] = draw.circle{ x = knob, y = mid, r = r, fill = fg }
        end
        return out
      end },
      on_press = function(p) drag_to(stop_at(p.x)) end,
      -- on_drag's position is the window's; the track's box makes it local.
      on_drag = function(p)
        local box = bone.gui.rect("reasoning-track")
        if box then drag_to(stop_at(p.x - box.x, box.w)) end
      end,
      on_release = function()
        local k = ui.drag
        ui.drag = nil
        if k then commit(k) end
        bone.gui.invalidate()
      end,
      on_hover = function(on)
        if ui.hover ~= on then ui.hover = on; ui.wheel = 0; bone.gui.invalidate() end
      end,
      -- One notch of a mouse wheel is one stop; a trackpad slides through
      -- them as it travels. Up is more effort.
      on_scroll = function(dy)
        local notch = 50 * ((bone.o and bone.o.wheel_speed) or 1)
        ui.wheel = ui.wheel + (dy or 0)
        local steps = 0
        while ui.wheel >= notch do steps = steps + 1; ui.wheel = ui.wheel - notch end
        while ui.wheel <= -notch do steps = steps - 1; ui.wheel = ui.wheel + notch end
        if steps ~= 0 then commit(at + steps) end
      end,
    }

    local row = {
      direction = "row", align = "center", gap = 8, height = H, padding = { 0, 6, 0, 8 },
      shrink = 0, label = "Reasoning effort: " .. name(current),
    }
    -- (Children are appended: a nil in a list would end it there.)
    if not ctx.width or ctx.width >= 640 then
      row[#row + 1] = { text = "Effort", size = style.size.small, hl = "Dim", wrap = false }
    end
    row[#row + 1] = track
    -- A fixed width, so the toolbar does not shift as the word changes.
    row[#row + 1] = { width = 44, direction = "row",
      { text = name(EFFORTS[shown]), size = style.size.small, wrap = false,
        hl = ui.drag and "Normal" or (EFFORTS[shown] == "default" and "Dim" or "Muted") } }
    return style.finish(row)
  end,
})
