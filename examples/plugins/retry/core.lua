-- retry: retry model calls that fail for passing reasons (rate limits,
-- overload, timeouts), with backoff, and optionally move to another provider.
--
--   bone.config.retry = {
--     attempts = 4,          -- tries per model call, the first one included
--     delay = 2000,          -- ms before the first retry; doubles each time
--     fallback = nil,        -- a bone.config.providers key to switch to ...
--     fallback_after = 2,    -- ... from this attempt on
--   }

bone.config.retry = { attempts = 4, delay = 2000, fallback = nil, fallback_after = 2 }

local PASSING = { "429", "500", "502", "503", "504", "529", "overloaded", "rate limit", "timed out", "timeout", "connection" }

local function passing(err)
  err = err:lower()
  for _, p in ipairs(PASSING) do
    if err:find(p, 1, true) then
      return true
    end
  end
  return false
end

-- Low priority: other request_error hooks decide first.
bone.hook("request_error", function(ev)
  local cfg = bone.config.retry or {}
  if ev.retry or ev.attempt >= (cfg.attempts or 4) or not passing(ev.error) then
    return
  end
  local out = { retry = (cfg.delay or 2000) * 2 ^ (ev.attempt - 1) }
  if cfg.fallback and ev.attempt >= (cfg.fallback_after or 2) then
    out.provider = cfg.fallback
  end
  return out
end, { priority = -10 })
