-- The core and the frontend: requests, background work, settings, the
-- session, the prompt, processes (shared ops in bone-ui).
local listed, deferred, failed
bone.request("session/list", {}, function(result, err)
  listed = { result = result, err = err }
end)
bone.request("no/such/method", {}, function(result, err)
  failed = { result = result, err = err }
end)
bone.defer(1, function()
  deferred = true
end)

eq(type(bone.settings.all()), "table", "settings are a table")
eq(bone.api.session(), nil, "a new chat has no session yet")
eq(bone.api.prompt_get(), "", "an empty prompt")
eq(#bone.processes.list(), 0, "no processes")
bone.notify("from the conformance suite")
bone.ui.refresh()

return {
  function()
    eq(listed and listed.err, nil, "a request's callback gets its result")
    eq(type(listed.result), "table", "the session list")
    eq(failed and failed.result, nil, "a failed request gets nil")
    eq(type(failed.err), "string", "and the error")
    eq(deferred, true, "background work called back")
  end,
}
