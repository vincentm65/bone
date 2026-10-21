-- bone.hl and bone.colorscheme (shared ops in bone-ui).
bone.hl.set("ConfGroup", { fg = "#102030", bold = true, link = "ErrorMsg" })
local g = bone.hl.get("ConfGroup")
eq(g.fg, "#102030", "its own fg wins over the link's")
eq(g.bold, true, "bold")

bone.hl.set("ConfLinked", { link = "ConfGroup", italic = true })
local l = bone.hl.get("ConfLinked")
eq(l.fg, "#102030", "follows its link")
eq(l.italic and l.bold, true, "and adds to it")

eq(pcall(bone.hl.set, "ConfBad", { fg = "mauve" }), false, "unknown colors are refused")

local names = table.concat(bone.hl.names(), " ")
eq(names:find("ConfLinked", 1, true) ~= nil, true, "listed")

bone.colorscheme("black")
eq(bone.api.colors_name(), "black", "the scheme loaded last")
eq(bone.hl.get("ConfGroup").fg, "#102030", "a scheme keeps groups set outside it")
eq(pcall(bone.colorscheme, "no-such-scheme"), false, "missing schemes are an error")
