-- bone.keymap: mappings, contexts, and the context in use (shared ops in bone-ui).
eq(bone.keymap.current(), "main", "main has the keyboard")

bone.keymap.set("ctrl+x z", function() end, { desc = "conformance mapping" })
bone.keymap.context("conf-ctx", { fallback = "main", priority = 5 })
bone.keymap.set("q", "page_up", { context = "conf-ctx", desc = "conformance in a context" })

local found = {}
for _, m in ipairs(bone.keymap.list()) do
  found[m.desc] = m
end
eq(found["conformance mapping"].keys, "ctrl+x z", "a sequence, by its canonical name")
eq(found["conformance mapping"].context, "main", "in main by default")
eq(found["conformance in a context"].context, "conf-ctx", "in its context")

eq(pcall(bone.keymap.set, "nope+x", "page_up"), false, "bad keys are refused")
eq(pcall(bone.keymap.set, "x", "not_a_builtin"), false, "bad actions are refused")
eq(pcall(bone.keymap.context, "bad name!"), false, "bad context names are refused")

bone.keymap.del("ctrl+x z")
found = {}
for _, m in ipairs(bone.keymap.list()) do
  found[m.desc] = true
end
eq(found["conformance mapping"], nil, "deleted")
