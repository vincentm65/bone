-- The bare library in the GUI (/gui use bare, or /tui use bare): the same
-- window without the standard item views (every item is its text), drawn
-- flat and square (no corners, shadows, outlines or fades), and without
-- the user's and project's `gui.lua`. Plugins still load.
require("bone.nodes.library").setup({ plain = true })
