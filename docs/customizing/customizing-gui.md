# Customizing the GUI's look and behavior

Where: `~/.bone/gui.lua` (the GUI only) or `~/.bone/ui.lua` (the TUI and the
GUI); a plugin's `gui.lua` and `ui.lua` work the same. The GUI is `bone --gui`
(a build with the `gui` feature). A Lua error never crashes it: it is shown
once and listed by `/health`.

The GUI shares *behavior* with the TUI, not a view: keymaps, commands,
options, events, plugins and the data Lua reads (chats, processes, usage) are
the same API. The corresponding sections of `customizing-tui.md` apply,
except for settings precedence and reload lifecycle, described below. How the
window looks is the GUI's alone: Lua describes it as a **tree of nodes** with
CSS-like styles, and Rust lays it out and paints it. There is no character
grid. Reference: `docs/lua.md` "Frontends". Source: `crates/bone-gui/src/`
(`node.rs`, `flex.rs`, `nodeview/` — `mod.rs`, `paint.rs`, `list.rs`, `focus.rs`, `ops.rs`) and the built-in library in
`runtime/gui/` and `runtime/lua/bone/nodes/`.

## Reloading

The GUI reloads its Lua when a watched file changes, unless
`bone.o.autoreload` is off. A fresh Lua state loads the runtime, plugins,
user and trusted project config, then saved GUI settings. A new load error
keeps the previous configuration. Option values carry over; chats, the
prompt and the session stay.

Watched files are `ui.lua`, `gui.lua`, the TUI's `library` selection file,
`lua/`, `runtime/`, `colors/`, the active GUI library directory, and plugin
Lua files. The GUI currently does not watch `gui-library`, `core.lua` or a
project's `.bone/`. Use `/gui use NAME` to switch libraries live. It does
not send `core/reload`, even when this window hosts the core. Plugin Lua
files are watched regardless of side, but a GUI reload still reloads only
GUI Lua. Changes made while autoreload is off are not picked up just by
turning it back on; a subsequent watched change triggers a reload.

Unlike TUI file reloads, GUI file reloads currently do not run outgoing
shutdown hooks or save plugin state. Explicit plugin unload/reload and
window close do. Save important state as it changes with
`bone.plugin.save_state()`. GUI `ready` has no data at startup or after
reload; do not expect the TUI's `{ reload = true, panels }` payload.

## Libraries

The GUI has its own library selector (`/gui`), separate from `/tui`:

```
/gui                     choose one from a menu
/gui use NAME            standard | bare | a copy of yours; reloads
/gui create NAME [base]  copy the standard UI (base: bare) to edit
/gui delete NAME         move a copy not in use aside
```

`/gui` writes `~/.bone/gui-library`. With no such file the GUI follows the
TUI's library (`/tui`) when that is `bare` or a copy that has a `gui/`
directory, and is `standard` otherwise, so a library made only for the
terminal leaves the GUI its own.

`standard` is the default: graphite colors,
bubbles for your messages, proportional Markdown answers, tool calls as
cards, a card composer, the sessions sidebar, a tray and a status bar. `bare`
is the same window with every chat item drawn as its text, and it does not
run your `ui.lua` and `gui.lua` or a project's (plugins still load). A copy lives in
`~/.bone/libraries/NAME/`; its `gui/defaults.lua` is the entry point, and any
file in it replaces the built-in one of the same path. For one change, a file
at the same path under `~/.bone/runtime/` does the same without a copy. The
window itself is `lua/bone/nodes/window.lua`, the chat is
`lua/bone/nodes.lua`: replace either to draw differently. The window is made
of files of its own, each replaceable: `nodes/composer.lua`, `statusbar.lua`,
`toolstrip.lua` (a turn's tool calls as a timeline, drawn in the chat),
`overlays.lua` (the toast, the core's questions, a large image), `popups.lua`
(Lua's windows), and `desktop/sidebar.lua` and `tray.lua`.

### Looks in one place: `bone.nodes.style`

Font sizes, corner radii, spaces, shadows and backdrops of the standard
library are tables in `bone.nodes.style`, read when the window is built, so a
`gui.lua` restyles all of it with one line each:

```lua
local style = require("bone.nodes.style")
style.radius.lg = 14                  -- sm 6, md 8, lg 10, xl 12, round 16
style.size.body = 13                  -- micro 11, small 12, caption 13, body 14, text 15, lead 17, title 20, display 24
style.shadow.card = { blur = 30, dy = 12, color = "#000000b0" }
```

Colors are highlight groups (the colorscheme's). A tint over what is under
it (`style.hover()`) follows whether the scheme is dark or light
(`style.dark()`), and `style.alpha("Panel", "bf", "bg")` is a group's color
with an alpha.

## The view: `bone.gui.view`

```lua
bone.gui.view(function(ctx)          -- ctx = { width, height } in pixels
  return { direction = "row", padding = 12, gap = 8, bg = "#1c1c1c",
    { width = 200, radius = 8, bg = "#262626", padding = 8,
      { text = "bone", bold = true, color = "Accent" } },
    { grow = 1, { text = "Hello", size = 18 } },
  }
end)
```

Lua builds the tree when something changed (an event, a key, a click, a
resize, an option, an op that changes the UI, a typed prompt) and not on every
frame; between those the last tree is laid out again. If your view reads
something else, call `bone.gui.invalidate()` (now) or `bone.gui.invalidate(ms)`.
An error in the view is shown once and the window says so until it is fixed.

A node is a table; its array part is its children (a `nil` among them is
skipped). A node with `text` or `spans` is text; with `input = true` it is the
prompt's text field; with `list = {...}` it is a long scrolling list;
otherwise it is a box.

### Boxes and sizes

| Field | |
|---|---|
| `direction` | `"column"` (default) or `"row"` |
| `gap`, `row_gap`, `column_gap` | space between children, px (`gap` is both; `column_gap` is along a row) |
| `flex_wrap` | a row whose children go on to another line when they run out of room |
| `align` | the cross axis: `"stretch"` (default), `"start"`, `"center"`, `"end"`; a child's `align_self` overrides it |
| `justify` | the main axis: `"start"`, `"center"`, `"end"`, `"between"` |
| `order` | where a child goes among its siblings (default 0) |
| `width`, `height`, `min_*`, `max_*` | px, `"50%"` of the parent, `"12px"`, `"1.5em"` (the node's font size), `"20vw"`, `"10vh"`, `"calc(50% - 20px)"`, or `"auto"` |
| `aspect_ratio` | width over height, for the side that is not set |
| `grow`, `shrink` | share what is left, or give back what is missing (an automatic size shrinks by default) |
| `padding`, `margin` | `n`, `{ vertical, horizontal }` or `{ top, right, bottom, left }`; a margin may be `"auto"` (`margin = { 0, 0, 0, "auto" }` pushes a child to the end) |
| `overflow` | `"visible"`, `"hidden"` or `"scroll"` (a column scrolls up and down, a row sideways); `scrollbar = { width, color, radius, inset, show = "auto" \| "hover" \| "hidden" }` |
| `position = "absolute"` | out of the flow, placed by `top`, `right`, `bottom`, `left` (px or `%`) in its parent, over the others; `z` orders them |
| `block_input` | takes the pointer from everything under it (a modal's backdrop), and keeps the keyboard focus among the nodes in it |

### Paint

| Field | |
|---|---|
| `bg`, `gradient` | a color, or `gradient = { angle = 180, stops = { "#fff", { 0.5, "#888" }, "#000" } }` (CSS angles; a stop is a color or `{ position, color }`) |
| `radius` | `n`, `{ top, bottom }` or `{ tl, tr, br, bl }` |
| `border`, `border_color`, `border_style` | `n` or `{ top, right, bottom, left }`; `"solid"`, `"dashed"`, `"dotted"` |
| `outline` | `{ width, color, offset }`: a line outside the box, over what is around it |
| `shadow` | `{ blur, spread, dx, dy, color }` or a list of them |
| `opacity` | 0 to 1, for the node and everything in it |
| `hover`, `active`, `focus` | `{ bg, gradient, border_color, color, opacity, shadow }`: what changes while the pointer is over the node, pressing it, or it has the keyboard focus. `color` recolors the text in it. Applied by Rust each frame, with no rebuild in Lua |
| `transition` | `150` (ms) or `{ duration = 150, easing = "ease-out" }` (`linear`, `ease-in`, `ease-out`, `ease-in-out`): the node's background, border color and opacity fade to what they change to (from `hover` and the other states, or when Lua builds the view again) instead of jumping. A color that appears or goes away fades its alpha. Size, margin and padding are not faded: step those in Lua |
| `tooltip` | text shown while the pointer rests on the node |

Colors are `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `"transparent"`, or the
name of a highlight group (its background for `bg`, its foreground
elsewhere), so colorschemes (`bone.colorscheme`, `bone.hl`) still work.

### Text

`text = "…"` or `spans = { "plain", { text = "x", bold = true, hl = "MdCode" } }`;
`size` (points), `font` (`"sans"`, `"mono"`), `bold`, `italic`, `strike`,
`color`, `hl` (a highlight group: its foreground, bold and italic; inherited),
`line_height` and `letter_spacing` (px), `text_align`, `wrap` (default true),
`text_overflow = "ellipsis"` (one line that ends in "…"), `text_shadow = {
dx, dy, color }` and `selectable` (drag to select; it takes the pointer, so a
click handler above it does not see clicks on the text). Underline and dim
from a group apply only on an explicit span's `hl` (`{ text = "x", hl =
"MdCode" }` or `{ "x", "MdCode" }`), not on a node's `text` with `hl`.
Text style otherwise is inherited from the boxes around it. A span also takes
`code`, `link` (underlined), `bg`, `strike`.

### Images

`{ width = 200, radius = 8, image = { id = attachment_id | path = "/x.png",
width = 640, height = 480, fit = "contain" } }`. The picture comes from an
attachment the core holds (an item's `images[i].id`) or a file on this
machine (PNG, JPEG, WebP), read once and kept. `width` and `height` inside
`image` are the picture's own size in pixels: they give the box its shape
before the pixels arrive (the node's `aspect_ratio` follows). `fit` is
`"contain"` (whole, centered), `"cover"` (fills, the overhang cut), `"fill"`
or `"none"`; the node's `radius`, `shadow` and `border` apply.
`bone.gui.preview(image)` opens an item's image large over the window (click
or esc closes); the standard library does it when you click an image row.

### Text inputs

`{ id = "search", padding = 8, bg = "InputBackground", radius = 8,
text_input = { placeholder = "Search", value = "", multiline = false,
max_rows = 6, password = false, read_only = false, focus = false, select = false,
on_change = function(text) end, on_submit = function(text) end } }`

A text field of its own, edited natively: cursor, selection, IME, paste. It
keeps its text by `id`; `value` replaces the text when it differs from what Lua
gave last time. `read_only` disables editing and focus. `focus = true` gives
it the keyboard the first time it appears, not again on later rebuilds.
`select` selects all of its text then, and again when `bone.gui.focus(id)`
asks, so typing replaces it (a rename field); a click into the field does
not. `bone.gui.input_text(id)` reads it, `bone.gui.set_input_text(id, text)`
sets it. `on_submit` is enter (ctrl+enter for a multiline field). While it
has the keyboard, typing and editing keys are its own, even inside a window
or panel that has the keys, except `ctrl+c`: that interrupts the turn (it is
not copy) and the field's `on_key` does not see it. An `on_key(name)` sees
every other key first (up, down, enter, esc, tab, ctrl+…, but not the
characters it types) and takes it by returning true: the sessions sidebar's
search field moves in the list under it that way. The field is `"mono"` or
the proportional face, not a family from `bone.gui.font`. The prompt is a
different node (`input = true`) and is left alone.

### Canvas

`{ width = 200, height = 100, canvas = { fps = 0, draw = function(ctx) return
{ ... } end } }` is a box Lua draws on. `ctx` is `{ width, height, time }`;
return shapes, coordinates in pixels from the box's top left:

```lua
local draw = bone.gui.draw
draw.rect{ x = 10, y = 10, w = 50, h = 30, fill = "#336699", stroke = "#fff", width = 2, radius = 4 }
draw.circle{ x = 100, y = 50, r = 20, fill = "Accent" }
draw.line{ x1 = 0, y1 = 0, x2 = 199, y2 = 99, color = "#fff", width = 2 }
draw.polyline{ points = { { 0, 0 }, { 20, 40 }, { 40, 10 } }, color = "#fa4", width = 2, closed = false, fill = "#fa42" }
draw.text{ x = 5, y = 90, text = "hello", size = 12, color = "#eee", align = "left", baseline = "top", font = "mono" }
draw.image{ x = 0, y = 0, w = 64, h = 64, path = "/x.png", radius = 8 }
```

With `fps = 0` it is drawn once each time the view is built (and when its size
changes); with `fps = 30` that many times a second, `ctx.time` being seconds
since the window opened. Colors are hex or highlight groups. Up to 20 000
shapes a draw.

### Events and the pointer

`on_click`, `on_double_click`, `on_secondary` (right button), `on_middle`
get `{ x, y }`
inside the node; `on_press` and `on_release` fire as the button goes down and
up on it; `on_drag` gets `{ x, y }`, the pointer in the window, as it moves with
the button held on the node; `on_hover(bool)`; `on_scroll(dy, { dx, ctrl,
alt, shift, super })` for the wheel (with ctrl or alt held too, which does
not scroll the scrolling boxes). `bone.o.wheel_speed` (1.6 in the standard
library, clamped to 0.1–10) multiplies how far a scrolling box and
`on_scroll` move; the wheel keys below use the system's step. The wheel over
nothing that scrolls is the `wheelup` / `wheeldown` key (`ctrl+wheelup`, …),
as in the TUI, so keymaps take it. Lua's `mouse` event sees each press and
release, and a drag when the pointer reaches another cell, before the nodes
do. `x` and `y` are character cells, `px` and `py` are pixels; over a window
it also names that window. It does not name a chat item or a layout leaf. A
handler that returns true takes the rest of that press, drag and release from
the nodes and text fields; taken at the press, they never see it.
`cursor` sets the
pointer: `pointer`, `text`, `grab`, `grabbing`, `move`, `not-allowed`, `wait`,
`progress`, `crosshair`, `cell`, `help`, `copy`, `zoom-in`, `zoom-out`,
`ew-resize`, `ns-resize`, `col-resize`, `row-resize` and more. A node with an `id` is
named for the rest: `bone.gui.scroll(id, to)`.

### The keyboard

The prompt has the keyboard until a node takes it. The keyboard reaches a node
with `on_key`, `on_click` or a `focus` look (`focusable = false` leaves one
out, `focusable = true` adds one); ctrl+p (`focus_next`) moves into them in the
order they are drawn, then tab, shift+tab and the up and down arrows move
between them. A press on a node with `on_key`, a `focus` look or `focusable =
true` gives it the focus; a press elsewhere gives it back to the prompt.

While a node has the focus, a key goes to its `on_key(name)` first (`"enter"`,
`"ctrl+s"`, `"a"`, as `bone.keymap` names them); returning true takes it.
Otherwise enter and space run its `on_click`, esc gives the keyboard back to
the prompt, typing goes back to the prompt and is typed there, and the
window's own keys (ctrl+c, ctrl+o, …) still work. A node reached with the
keyboard and no `focus` look of its own shows a ring (the `FocusRing` group,
else `Accent`).

```lua
{ id = "dialog", autofocus = true, key_guard = 300,
  on_key = function(k) if k == "esc" then close() end return true end, … }
```

`autofocus = true` takes the focus each time the node appears; `key_guard`
(ms) ignores keys for that long after, so an enter meant for the prompt does
not answer a question that has just come up. Inside a `block_input` box (a
modal) the focus stays among its nodes and typing does not reach the prompt
under it. `bone.gui.focus(id)` gives a node or text input the keyboard
(`nil`: back to the prompt); `bone.gui.focused()` is the id of the node or
text input that has it. Keymaps (`bone.keymap`) still apply to everything a focused node does
not take.

### Screen readers

Text is read as it is. A node with `on_click` is a button named by the text in
it (said once, not again as text). `role` says what else a node is:
`"button"`, `"link"`, `"checkbox"`, `"radio"`, `"option"` (one of a set to
choose from, such as a list entry), `"image"`, `"dialog"`, `"text"`, or
`"none"` (neither it nor its text is read); `label` names it in place of its
text (an image needs one), and `selected` says whether a checkbox, radio or
option is on. The node with the keyboard focus has the screen reader's too.

```lua
{ role = "option", selected = i == sel, on_click = pick, { text = name } }
{ role = "image", label = "the build graph", image = { path = "/tmp/g.png" } }
```

### Scrolling

`overflow = "scroll"` with an `id` is a scrolling column (wheel, a thin bar).
`follow = true` keeps it at its end while content is added there.

```lua
bone.gui.scroll("log", "end")       -- pixels, "top" or "end"
bone.gui.scroll_info("log")         -- { offset, max, viewport } or nil
bone.gui.scroll_item("log", 3, "nearest")
  -- item from 1; "top" (default), "center", "bottom" or "nearest";
  -- true once that list has been drawn, else false
```

### Long lists

`list` makes only the items in view, so a transcript of thousands of items
costs Lua nothing while it sits still:

```lua
{ id = "chat", grow = 1, follow = true,
  list = { count = n, estimate = 60, gap = 0, key = session_id,
           item = function(i) return { text = "item " .. i } end } }
```

`item(i)` (from 1) returns a node; heights are measured as items come into
view, and a changed `key` (another chat) forgets them. A list's items are made
again each time Lua builds the view, except with `chat = true` (the chat list
`bone.nodes.chat` makes): item `i` is then the chat's item `i`, and its node is
kept, through typing and other rebuilds, until the chat says it changed (its
data, the views, options or colors, what its view read with `bone.chat.item` or
`bone.chat.items`, or a `bone.chat.refresh_in` it asked for), as the TUI keeps
its rows. A view that reads anything else calls `bone.chat.redraw(index)`
when that changes.

### The prompt's text field

`{ input = true, placeholder = "Message…", size = 15, min_rows = 1, max_rows = 8 }`
is the native editor of the prompt (cursor, selection, IME, paste, history,
attachments are the shared ones). It is as tall as its text. Its text is
styled by `bone.ui.prompt_highlight` and shows its `ghost`, as in the TUI;
`bone.keymap.raw` and the `key` event see what is typed in it too. The
standard composer takes `bone.ui.prompt`'s `placeholder`, `prefix`, `min`
and `max` (else `bone.o.prompt_max_height` when set); the rest of that box
(border, background, padding, edges) is the composer's card.

## The chat

`require("bone.nodes").chat(opts)` is the list node for the chat on screen. Its
items come from `bone.ui.views.<kind>`, as in the TUI, with one difference: a
view returns `node = <a node>` (or just `lines`, drawn as monospace text). A
view may also return `click = function() … end` for a click on the item. The
standard library's views are built with `require("bone.desktop.markdown")`:
`nodes(text, group, opts)` lays Markdown out as nodes, `text(spans, opts)` is
selectable text and `band(spans, opts)` text on a band (a code block, a diff
row). A span is `{ text = …, hl = … }` or, as a TUI line's are written,
`{ "text", "Group" }`.

```lua
bone.ui.views.build = function(item)
  return { node = { padding = 8, radius = 8, bg = "CodeBlock",
    { text = item.text, font = "mono", size = 13 } } }
end
bone.chat.add("build", { text = "building…" })
```

`bone.ui.tool_views[name]` works as in the TUI: the standard tool card
draws its `title` in the window's font and its `lines` in monospace under
it, or a `node` it returns as it is.

`bone.chat.scroll`, `scroll_to` and `view` move and read the chat list.

### More than one chat

`chat(opts)` with `chat = n` shows chat number `n` (from `bone.chat.open`, or
`bone.chat.session().chat`) rather than the chat on screen. While `n` is not
on screen it is list `chat:<n>`, with its own scroll; while its views run,
`bone.chat` calls without a `session` read chat `n`. When chat `n` comes on
screen (`bone.api.open_session(tostring(n))`), list `chat:<n>` becomes
`chat`, keeping where it was scrolled, so a window can draw every chat with
`chat = n` and stay put as the focus moves. `bone.gui.draft(n)` is that
chat's unsent text.

The window's chat column is `require("bone.nodes.window").column(ctx, opts)`:
the chat with the composer, tray and status bar under it, or for a chat not
on screen (`opts.chat`) its draft, which `opts.on_focus` answers a press on.
`opts.width` is its width in pixels. What fills the room beside the sidebar
is `window.area(ctx)` (ctx = `{ width, height, cell }`), one column by
default; the catalog's `windows` plugin replaces it with tiled windows.

## Windows over the view

`bone.ui.popup` / `bone.ui.win` work as in the TUI: the same anchors
(`anchor`, and for a window of `lines`, `row` and `col`), the same keys and
focus. A window of `lines` is sized and offset in character cells of the
monospace font; the GUI places it in pixels in the window, the chat or above
the prompt (no grid is laid over the window). A window with `node` is
centered in its anchor and sized to the content box (capped), not by a cell
`width` and `height`. How a window looks is the
GUI's: `runtime/lua/bone/nodes/popups.lua` draws each one as nodes, and its
`M.chrome` table is the whole look of the card:

```lua
local chrome = require("bone.nodes.popups").chrome
chrome.bg = "#1a1a24"
chrome.radius = 18
chrome.border = 0
chrome.shadow = { blur = 30, dy = 10, color = "#000000c0" }
chrome.pad = 18                        -- room around a window of nodes
```

A window of `lines` (a plugin's, written for the TUI) is drawn in the
monospace font, one text row to a cell row, with the shared keys. Those rows
are not pointer targets: `on_click` and the other node handlers exist only on
a window made of nodes. The shared `mouse` event is separate and still fires
over a line window, the chat and the prompt.

With `node = function(ctx) return node end` in place of `lines` (on
`bone.ui.popup`, `win`, `update`, `bone.ui.panel.open` or `update`; `ctx` is
`{ width, height }` in pixels) a window or a docked panel draws that node; the
settings, help, health, setup, catalog, picker and pager windows are these.
Keys, focus and closing stay the window's. `require("bone.nodes.popups").list(id, nodes, selected)` is a
scrolling list for one, keeping entry `selected` in view:

```lua
local id
id = bone.ui.popup({ on_key = function(k) … end,
  node = function(ctx)
    -- ctx is the content box (the anchor minus padding, capped). The card
    -- is centered in the anchor. A window of lines stays sized in cells.
    return require("bone.nodes.popups").list("mine:" .. id, {
      { text = "Pick one", bold = true },
      { text = "first", on_click = function() … end },
    })
  end })
```

Replace the file to draw windows differently. Docked panels of `lines` and
`bone.ui.layout` are the TUI's: the GUI keeps their state but draws nothing
for them. Node panels can dock on all four sides. The built-in `/term`
(`/term <command>`) opens a terminal; `ctrl+]` or `ctrl+5` opens/refocuses it,
and the same key inside returns to the prompt.

A window manager can place any focusable node panel itself. Opening a terminal
with no explicit panel calls `require("bone.nodes.window").attach_panel(id)`;
that hook is nil until a manager installs one. `window.embed(id)` reserves the
panel and returns a release function. Only that latest reservation can release
it, releasing twice is harmless, and release never closes the panel. Draw it
with `window.panel(id, { width = pixels, height = pixels, cell = ctx.cell })`.
Those dimensions are only the first estimate: the callback then receives the
panel's real allocated rectangle, in GUI pixels rather than screenshot pixels.
Leave it visible while its pane is shown. Hide it only while zoom hides that
pane, and reveal it again before focusing it.

## A narrow window

Under `require("bone.desktop.sidebar").overlay_below` pixels (760) the
sessions sidebar floats over the chat on a backdrop that closes it, instead
of taking room from it; choosing a chat closes it.

## Slots: plugins add to the window

Named places in the window where any `gui.lua` adds nodes, without replacing
the library:

```lua
bone.gui.slots.add("statusbar.right", { id = "clock", order = 10,
  node = function(ctx) return { text = os.date("%H:%M"), hl = "Dim", size = 12 } end })
bone.gui.slots.remove("statusbar.right", "clock")
```

`node` is a node or a function returning one (nil for none), run each time the
window is built; a failing one is skipped and reported once. Lower `order`
comes first. The standard window asks for `statusbar.left`, `statusbar.right`,
`composer.above`, `composer.below`, `chat.top`, `sidebar.top`, `sidebar.bottom`,
`window.left`, `window.right` and `window.overlay` (absolute nodes over
everything). A library of yours can ask for others with
`bone.gui.slots.nodes(name)`.

## Events from the window

`bone.on("drop", function(ev) … end)`: files dropped on the window
(`ev.paths`); the standard library attaches images and puts other paths in the
message, and your handler replaces that. `bone.gui.font("serif", path)`
registers one file as `font = "serif"` on a text node or span (not `"sans"`
or `"mono"`). Bold and italic do not load other faces. An unknown name is
the face the text would have had, and a native `text_input` does not use it.

## What the window draws itself

The prompt's text field and selectable text are native, so egui draws their
cursor, selection and corners; they follow the colorscheme: the groups
`TextSelection` (bg: selected text, fg: its outline), `Cursor` (fg), `Accent`
(links), `Panel` (raised surfaces), `WinSeparator` (outlines), `Normal`, and
the options `widget_radius`, `window_radius` and `scroll_line` (pixels one
scroll-key line moves the chat). A scheme that sets none gets
dark defaults.

A failing view, or no view, shows `bone.gui.fallback`'s: a plain node tree with
the error. Replace it with `bone.gui.fallback(function(ctx) return node end)`
(`ctx.error` is the message).

## Options

Set in `gui.lua`, or with `/config` (saved as `gui.<name>` in `settings.json`).
Saved values are applied after plugins, your `ui.lua` and `gui.lua`, and a
trusted project's files, so they override those assignments. The TUI applies
`tui.*` before `tui.lua`, which therefore overrides the saved value. An
unknown saved name is ignored; a bad value of a known option is reported.

```lua
bone.o.font = "/path/Mono.ttf"        -- the monospace font (empty: built in)
bone.o.font_size = 14                 -- points
bone.o.font_sans = ""                 -- proportional text (empty: the system's)
bone.o.font_sans_bold = ""            -- its bold and italic faces
bone.o.font_sans_italic = ""
bone.o.queue_mode = "next"            -- as in the TUI: steer or next
bone.o.wheel_speed = 1.6              -- wheel travel, times the system's (0.1–10)
bone.o.opacity = 0.9                  -- standard library; bare is 1. See below
bone.o.tool_strip = true              -- standard library: each turn's tool call timeline
bone.o.fold_turns = true              -- standard library: earlier turns as one row each
```

The tool strip, under your message, draws its turn as one bar: a segment per tool
call as wide as it ran, the model's thinking between calls as fine dots and
its answer at the end. Each kind has a highlight group of its own, also used
by the TUI's toolstrip plugin, quiet by brightness: `ToolStripRead` (faint),
`ToolStripCommand` and `ToolStripAgent` (brighter), `ToolStripAnswer` (nearly
white), `ToolStripEdit` (green), `ToolStripFail` (red), `ToolStripLive` (a
running call, breathing amber) and `ToolStripWait` (one waiting on an answer,
outlined in blue); a colorscheme or
`bone.hl.set` that defines one keeps its color. A segment's tooltip says
what it did; a click scrolls the chat to it. Calls run before cores kept
their times (`session/messages` `tool_times`) get equal shares.

With `fold_turns`, every turn before the last folds into one row: your
message, that turn's strip and how long it took; its calls and answer are
hidden. A click on the row opens the turn, a click on a segment opens it at
that call. An open turn has its strip under your message, and "Fold" while
it is not the last (`desktop/views.lua`; `require("bone.desktop.views").open_turn(n, on)`
opens or folds turn `n`).

`opacity` is read when the window opens. Below 1 the window is created
see-through, so turning that on or off takes a restart; inside a window
opened that way the alpha follows the option live.
`bone.gui.window_info().opacity` is that alpha, or 1 when the window was
opened opaque. What shows through is the canvas under the view (the `Normal`
background). The standard library, when that alpha is below 1, leaves the
root unfilled and draws the chat as an opaque `Normal` surface: square and
full while the sidebar is closed, rounded and overlapping its edge by the
corner radius while it is open. A docked sidebar has no fill of its own
then, so the canvas shows through that column (another fill would stack).
Text, cards and bubbles stay opaque. On Linux a see-through window needs a
compositor, or the background is black; on Windows it is DirectX 12 on a
DirectComposition surface (the only wgpu one that can show through), unless
`WGPU_BACKEND` or `WGPU_DX12_PRESENTATION_SYSTEM` says otherwise. Blur behind
the window is the compositor's (Hyprland's `decoration:blur`, KWin's rules),
not bone's.

## The window: `bone.gui`

```lua
bone.gui.window{ title = "bone", width = 1100, height = 760, maximized = false }
bone.gui.window_info()                -- { width, height, columns, fullscreen, maximized, focused, opacity }
bone.gui.notify("bone", "done")       -- a desktop notification (gui.notifications)
bone.gui.open_file({ title = "Pick", filters = { { "Images", { "png" } } } }, function(path) end)
bone.gui.save_file({ name = "out.txt" }, function(path) end)   -- gui.dialogs
bone.gui.status()                     -- the message last shown: { text, error } or nil
bone.gui.status_clear()
bone.gui.rect("chat")                 -- { x, y, w, h }: where the node with that id was, last frame
bone.gui.preview(image)               -- open an attachment (an item's images[i])
```

Check `bone.has_capability("gui.nodes")` or `"gui.window"` before using these
from shared code.

Pieces for building one (`bone.nodes.widgets`, also under `bone.nodes.popups`
for the first seven): `title(text)`, `note(text, margin, hl)`, `entry(on,
children, click)` (a card, raised when selected), `button(label, click,
primary, on, o)`, `field(label, value, { on, editing, click, hint, empty })`
(a labelled box), `hints("↑↓ move · enter open")` (each key as a keycap) and
`list(id, items, selected, opts)`, which takes `opts.fit = true` (the window is
as tall as its entries, not the room it was given, and centered) and
`opts.footer = node` (stays under the list however far it scrolls). Also
`icon_button(glyph, label, click, o)`, `link(text, click)`, `pill(text)`,
`tabs(names, current, pick)`, `segmented(names, current, pick)`,
`checkbox(on)`, `surface(o)` (a raised card), `divider()` and
`spinner(size)`. Whatever answers a press gets the pointer cursor, a name for
screen readers, a hover look and a fade from `widgets.press(node, click)`; an
`o` table replaces any field of the node a widget makes.
`require("bone.nodes.popups").menu{ title, note, items, selected, choose,
hints }` opens a window of choices (`/gui` is one). The picker, pager and
setup windows are made of these.

### The windows the runtime opens

`bone.ui.select`, `bone.ui.pager`, `/setup` and `/catalog` are drawn by the
library: `bone.ui.windows.<select|pager|setup|catalog>` is a function of the
window's page (its state, and what to call on a press) that returns the
popup spec, with a `node`. Without one the window is drawn as `lines`. The
standard library sets all four (`bone.desktop.pick`, `.setup`, `.catalog`); a
library of yours can replace any:

```lua
bone.ui.windows.select = function(page)
  -- page.st (query, sel, items), page.opts, page.matches(), page.finish(match)
  return { node = function() return … end }
end
```
