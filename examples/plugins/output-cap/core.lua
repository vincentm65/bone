-- output-cap: keep any one tool result from flooding the context. Results
-- longer than the limit keep their start and end, with a note of what was
-- left out. (The shell tool already cuts its own output; this covers every
-- tool: Lua tools, MCP tools, read_file.)
--
--   bone.config.output_cap = { max = 100000 }   -- bytes

bone.config.output_cap = { max = 100000 }

-- Last among tool_result hooks, so others see the full output.
bone.hook("tool_result", function(ev)
  local max = (bone.config.output_cap or {}).max or 100000
  local out = ev.output or ""
  if #out <= max then
    return
  end
  local head = math.floor(max * 3 / 4)
  local tail = max - head
  -- Cut on UTF-8 character boundaries.
  while head > 0 and out:byte(head + 1) and out:byte(head + 1) >= 0x80 and out:byte(head + 1) < 0xC0 do
    head = head - 1
  end
  local from = #out - tail + 1
  while from <= #out and out:byte(from) >= 0x80 and out:byte(from) < 0xC0 do
    from = from + 1
  end
  return {
    output = out:sub(1, head)
      .. ("\n\n[... %d bytes omitted ...]\n\n"):format(from - head - 1)
      .. out:sub(from),
  }
end, { priority = -1000 })
