# Lua API & Docs Cleanup Plan

## Items

- [x] 1. Update `defaults/AGENTS.md` to document the real ctx API
- [x] 2. Add a compact reference table for ctx functions
- [x] 3. Make event ctx limitations explicit (done as part of 1/2 — Context Availability table + event handler note)
- [x] 4. Add `ctx.cwd` (docs claim it exists, implementation doesn't set it)
- [x] 5. Evaluate `ctx.edit_file` — delegate to native tool via policy, or keep using `ctx.tools.call`
- [x] 6. Audit OS/file mutation paths — confirm all go through policy-controlled primitives
- [x] 7. Fix/recheck reload — tools, commands, hooks, snapshots from same Lua VM
- [x] 8. Add tests: sandbox fails, defaults avoid sandboxed APIs, ctx.tools.call depth, ctx.agent.run depth/approval, event ctx fields
