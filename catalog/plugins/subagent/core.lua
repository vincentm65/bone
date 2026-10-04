-- Bone 3's first subagent port is intentionally model-only.  It keeps the
-- catalog tool useful without pretending that a nested tool-running agent is
-- available before the core protocol grows a subagent lifecycle.
bone.config.subagents = bone.config.subagents or {}
bone.tool.register({
  name = "subagent",
  description = "Delegate a question to a named model-only sub-agent.",
  parameters = {
    type = "object",
    properties = { name = { type = "string" }, prompt = { type = "string" } },
    required = { "prompt" },
  },
  needs_approval = false,
  run = function(args)
    local spec = bone.config.subagents[args.name or ""] or {}
    local result, err = bone.model.complete({
      provider = spec.provider,
      system = spec.system or "You are a focused sub-agent. Return only useful findings.",
      prompt = args.prompt,
    })
    if not result then return nil, err or "subagent failed" end
    return result.content or ""
  end,
})
bone.rpc.register("subagent/list", function()
  local out = {}
  for name, spec in pairs(bone.config.subagents or {}) do
    out[#out + 1] = { name = name, provider = spec.provider }
  end
  table.sort(out, function(a, b) return a.name < b.name end)
  return out
end)
