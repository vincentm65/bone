-- Libraries, the same in every frontend: listing, making and deleting one
-- of yours. (Choosing one loads a new Lua state, so each frontend's own
-- tests check that.)
local info = bone.library.info()
eq(info.active, "standard", "the standard library to begin with")
eq(info.drawn, "standard", "and drawn with")
local dir = bone.library.create("conf_copy")
assert(dir:match("conf_copy$"), "made in libraries/: " .. dir)
eq(bone.library.info().custom[1], "conf_copy", "listed")
eq(bone.library.info().drawn, "standard", "not drawn until chosen")
local mine = bone.ui_libraries().mine
eq(mine[1].name, "conf_copy", "bone.ui_libraries lists it")
bone.library.delete("conf_copy")
eq(#bone.library.info().custom, 0, "deleted")
