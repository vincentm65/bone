-- /setup: add a model provider (and, optionally, catalog packages). It
-- opens by itself when bone starts with no provider at all, unless that
-- was dismissed before (setup.skipped). It writes settings.json (the
-- provider, chosen for use) and secrets.json (a key typed here), never
-- core.lua.

local M = {}

M.PRESETS = {
  {
    title = "A server on this machine (llama.cpp, vLLM, tabbyAPI, Ollama, LM Studio)",
    name = "local",
    base_url = "http://localhost:8080/v1",
    model = "",
  },
  { title = "OpenAI", name = "openai", base_url = "https://api.openai.com/v1", model = "", key_env = "OPENAI_API_KEY" },
  {
    title = "Anthropic",
    name = "anthropic",
    base_url = "https://api.anthropic.com/v1",
    model = "",
    key_env = "ANTHROPIC_API_KEY",
  },
  { title = "DeepSeek", name = "deepseek", base_url = "https://api.deepseek.com/v1", model = "", key_env = "DEEPSEEK_API_KEY" },
  { title = "OpenRouter", name = "openrouter", base_url = "https://openrouter.ai/api/v1", model = "", key_env = "OPENROUTER_API_KEY" },
  -- A plan you already pay for: a catalog plugin talks to it through the
  -- vendor's CLI login. `plugin` is the package, `name` the provider it adds.
  { title = "Codex (your ChatGPT plan, through the Codex CLI's login)", name = "codex", plugin = "codex",
    login = "codex login" },
  { title = "Claude Code (your Claude plan, through the Claude Code CLI's login)", name = "claude_code",
    plugin = "claude-code", login = "claude auth login" },
  { title = "Grok (your SuperGrok or X plan, through the Grok Build CLI's login)", name = "grok", plugin = "grok",
    login = "grok login" },
  { title = "Another OpenAI-compatible service", name = "", base_url = "", model = "" },
}

local function pad(s, w)
  s = tostring(s or "")
  local n = bone.text.width(s)
  if n >= w then
    return bone.text.truncate(s, w)
  end
  return s .. string.rep(" ", w - n)
end

--- With no model provider at all, open by itself on `ready`, unless it was
--- dismissed before. An empty provider list can still mean one from
--- BONE_BASE_URL, so the core's health check decides.
function M.first_run()
  bone.on("ready", function()
    -- A plugin whose core.lua failed was left out of the core: say which.
    bone.request("plugin/list", {}, function(list)
      for _, p in ipairs(list or {}) do
        if p.error then
          bone.notify(("plugin %s was not loaded: %s"):format(p.name, p.error), "error")
        end
      end
    end)
    if bone.settings.get("setup.skipped") then
      return
    end
    bone.model.list(function(list)
      if not list or #list > 0 then
        return
      end
      bone.request("health/check", {}, function(items)
        for _, it in ipairs(items or {}) do
          if it.name == "provider" and it.status == "error" then
            return M.open({ first_run = true })
          end
        end
      end)
    end)
  end)
end

--- The window's state and what its keys do, drawn by whoever opens it.
--- An error as a person reads it: without the RPC and Lua wrapping
--- ("rpc error -32602: runtime error: …").
function M.plain_error(err)
  local s = tostring(err or "unknown error")
  s = s:gsub("^rpc error %-?%d+: ", ""):gsub("^runtime error: ", "")
  return s
end

--- opts: { first_run = true } when it opened by itself. `hooks.changed()`
--- asks for a redraw, `hooks.close(skipped)` closes the window.
--- Returns { st, opts, rows(), hint(), on_key, paste(text) }.
function M.page(opts, hooks)
  local catalog = require("bone.catalog")
  local st = {
    step = "welcome",
    sel = 1,
    edit = nil,
    note = nil,
    form = nil, -- name, base_url, model, key, key_env
    packages = nil, -- catalog entries, nil while loading
    packages_err = nil,
    picked = {},
    log = {},
    models = nil, -- the server's model ids, for the Model field (↑↓ while editing)
    models_err = nil,
    busy = false, -- installing a subscription's plugin
    keyless_ok = false, -- Continue was pressed once with no key anywhere
    first_prompts = {
      "Explain this project",
      "Find the main entry point",
      "Review my git changes",
      "Run the tests and summarize failures",
    },
  }
  local redraw, close = hooks.changed, hooks.close
  -- Plugins the core left out because their core.lua failed: the welcome
  -- names them, as they may be why setup opened.
  st.broken = {}
  bone.request("plugin/list", {}, function(list)
    for _, p in ipairs(list or {}) do
      if p.error then
        st.broken[#st.broken + 1] = p
      end
    end
    if #st.broken > 0 then
      redraw()
    end
  end)

  -- The rows of the step on screen: { label, value, key | action }.
  local function rows()
    if st.step == "provider" then
      local out = {}
      for i, p in ipairs(M.PRESETS) do
        out[#out + 1] = { label = p.title, preset = i }
      end
      return out
    elseif st.step == "details" then
      local f = st.form
      return {
        { label = "Name", key = "name", value = f.name, hint = "how settings and /config call it" },
        { label = "URL", key = "base_url", value = f.base_url, hint = "the OpenAI-compatible endpoint, ending in /v1" },
        { label = "Model", key = "model", value = f.model, hint = st.models and #st.models > 0
          and ("enter, then ↑↓ to pick one of the server's %d models, or type a name"):format(#st.models)
          or st.models_err and ("the model name the server knows (cannot list them: " .. st.models_err .. ")")
          or "the model name the server knows" },
        {
          label = "API key",
          key = "key",
          value = f.key ~= "" and string.rep("•", math.min(#f.key, 24)) or "(none)",
          hint = "saved in ~/.bone/secrets.json, readable only by you; or leave it and use the variable below",
        },
        { label = "Key variable", key = "key_env", value = f.key_env, hint = "an environment variable that holds the key" },
        { label = "Continue", action = "details_done" },
      }
    elseif st.step == "packages" then
      local out = {}
      for _, e in ipairs(st.packages or {}) do
        if e.state == "available" then
          out[#out + 1] = {
            label = catalog.label(e),
            value = (st.picked[e.name] and "[x] " or "[ ] ") .. (e.description or ""),
            package = e,
          }
        end
      end
      out[#out + 1] = { label = "Continue", action = "apply" }
      return out
    end
    return {}
  end

  local function apply()
    st.step = "applying"
    local f = st.form
    local function say(line)
      st.log[#st.log + 1] = line
      redraw()
    end
    local spec = { base_url = f.base_url, model = f.model, api = f.api }
    if f.key_env ~= "" then
      spec.api_key_env = f.key_env
    end
    local queue = {}
    for _, e in ipairs(st.packages or {}) do
      if st.picked[e.name] then
        queue[#queue + 1] = e
      end
    end
    local function finished()
      st.step = "done"
      redraw()
    end
    local function after_key()
      bone.settings.set("provider", f.name, function(_, err)
        if err then
          say("error: " .. M.plain_error(err))
        else
          say("using " .. f.name .. (f.model ~= "" and (" (" .. f.model .. ")") or ""))
        end
        if #queue == 0 then
          return finished()
        end
        local remaining = #queue
        for _, e in ipairs(queue) do
          say("installing " .. catalog.label(e) .. "…")
          catalog.install(e, function(ok, err)
            say(ok and ("installed " .. catalog.label(e)) or ("error: " .. catalog.label(e) .. ": " .. M.plain_error(err)))
            remaining = remaining - 1
            if remaining == 0 then
              finished()
            end
          end)
        end
      end)
    end
    if f.subscription then
      return after_key()
    end
    say("saving " .. f.name .. "…")
    bone.settings.set("providers." .. f.name, spec, function(_, err)
      if err then
        st.step = "details"
        st.note = { tostring(err), true }
        return redraw()
      end
      if f.key == "" then
        return after_key()
      end
      bone.request("secrets/set", { provider = f.name, key = f.key }, function(_, kerr)
        say(kerr and ("error: the key was not saved: " .. M.plain_error(kerr)) or "saved the key")
        after_key()
      end)
    end)
  end

  -- The catalog's plugins, for the packages step (fetched once).
  local function load_packages()
    if st.packages then
      return
    end
    catalog.index(function(entries, err)
      st.packages, st.packages_err = {}, err
      for _, e in ipairs(entries or {}) do
        if e.kind ~= "tui" then
          st.packages[#st.packages + 1] = e
        end
      end
      redraw()
    end)
  end

  -- The key the details give: typed here, else the key variable's value.
  local function key_of(f)
    if f.key ~= "" then
      return f.key
    end
    local v = f.key_env ~= "" and os.getenv(f.key_env) or nil
    return v ~= "" and v or nil
  end

  local function is_local(url)
    return url:find("://localhost") or url:find("://127%.") or url:find("://%[::1%]") or url:find("://0%.0%.0%.0")
  end

  -- Ask the server which models it has (OpenAI-style GET /models), so the
  -- Model field can offer them. Quietly nothing when it cannot say.
  local function fetch_models()
    local f = st.form
    if not f or f.subscription or f.base_url == "" then
      return
    end
    local key = key_of(f)
    local headers = {}
    if key and f.base_url:find("anthropic%.com") then
      headers["x-api-key"], headers["anthropic-version"] = key, "2023-06-01"
    elseif key then
      headers["Authorization"] = "Bearer " .. key
    end
    local url = f.base_url:gsub("/+$", "") .. "/models"
    local asked = url .. "\0" .. (key or "")
    st.models_for, st.models, st.models_err = asked, nil, nil
    bone.http({ url = url, headers = headers, timeout = 8000 }, function(res, err)
      if st.models_for ~= asked then
        return
      end
      local names = {}
      if res and res.status == 200 then
        local ok, doc = pcall(bone.json.decode, res.body or "")
        for _, m in ipairs(ok and type(doc) == "table" and type(doc.data) == "table" and doc.data or {}) do
          if type(m) == "table" and type(m.id) == "string" then
            names[#names + 1] = m.id
          end
        end
      end
      table.sort(names)
      st.models = #names > 0 and names or nil
      st.models_err = #names == 0 and (err and "not reachable"
        or res and (res.status == 401 or res.status == 403) and "the key was refused"
        or res and ("HTTP " .. res.status) or "no answer") or nil
      redraw()
    end)
  end

  -- A subscription preset: install its plugin (unless it is), then use the
  -- provider it adds, which it does only when the vendor's CLI is logged in.
  local function subscribe(p)
    local label = p.title:match("^(.-)%s*%(") or p.title
    local function use()
      bone.model.list(function(list)
        st.busy = false
        for _, m in ipairs(list or {}) do
          if m.name == p.name then
            st.form = { name = p.name, base_url = "", model = m.model or "", key = "", key_env = "", subscription = label }
            st.step, st.sel, st.note = "packages", 1, nil
            load_packages()
            return redraw()
          end
        end
        st.note = { ("%s is installed, but found no %s login. Run `%s` in a terminal, then choose %s again.")
          :format(label, label, p.login, label), true }
        redraw()
      end)
    end
    st.busy, st.note = true, { "getting the " .. label .. " plugin…" }
    redraw()
    catalog.index(function(entries, err)
      local e
      for _, x in ipairs(entries or {}) do
        if x.name == p.plugin and x.kind ~= "tui" then
          e = x
        end
      end
      if not e then
        st.busy, st.note = false, { "the catalog has no " .. label .. " plugin" .. (err and (": " .. err) or ""), true }
        return redraw()
      elseif e.state == "unavailable" then
        st.busy, st.note = false, { label .. " " .. e.blocked, true }
        return redraw()
      elseif e.state ~= "available" then
        return use()
      end
      catalog.install(e, function(ok, ierr)
        if not ok then
          st.busy, st.note = false, { label .. ": " .. M.plain_error(ierr), true }
          return redraw()
        end
        use()
      end)
    end)
  end

  local function check_details()
    local f = st.form
    if not f.name:match("^[%w_%-]+$") then
      return "the name is letters, digits, _ and -"
    elseif f.base_url == "" then
      return "the URL is needed"
    elseif f.model == "" then
      return "the model is needed"
    elseif not key_of(f) and not is_local(f.base_url) and not st.keyless_ok then
      st.keyless_ok = true
      return f.key_env ~= ""
        and ("no API key: type one above, or set " .. f.key_env .. " (Continue again to go on without one)")
        or "no API key: type one above (Continue again to go on without one)"
    end
  end

  local function on_key(k)
    if st.step == "applying" or st.busy then
      return true
    end
    if st.step == "done" then
      if k == "up" or k == "wheelup" then
        st.sel = math.max(1, st.sel - 1)
      elseif k == "down" or k == "wheeldown" then
        st.sel = math.min(#st.first_prompts, st.sel + 1)
      elseif k == "enter" then
        local prompt = st.first_prompts[st.sel]
        close(false)
        bone.prompt.set(prompt)
      elseif k == "esc" or k == "q" then
        close(false)
      end
      return true
    end
    if st.step == "welcome" then
      if k == "enter" then
        st.step, st.sel = "provider", 1
      elseif k == "esc" or k == "ctrl+c" then
        close(true)
      end
      return true
    end
    local list = rows()
    local row = list[st.sel]
    if st.edit then
      if row.key == "model" and st.models and (k == "up" or k == "down") then
        -- ↑↓ step through the server's models that contain what was typed.
        local query, matches, at = (st.model_query or ""):lower(), {}, 0
        for _, m in ipairs(st.models) do
          if m:lower():find(query, 1, true) then
            matches[#matches + 1] = m
            if m == st.edit then
              at = #matches
            end
          end
        end
        if #matches > 0 then
          at = at + (k == "down" and 1 or -1)
          if at < 1 then at = #matches elseif at > #matches then at = 1 end
          st.edit = matches[at]
          st.note = { ("%d of %d models%s"):format(at, #matches, query ~= "" and (" matching " .. query) or "") }
        else
          st.note = { "no model contains " .. query, true }
        end
      elseif k == "enter" then
        st.form[row.key] = st.edit
        st.edit = nil
        if row.key == "base_url" or row.key == "key" or row.key == "key_env" then
          st.keyless_ok = false
          fetch_models()
        end
      elseif k == "esc" then
        st.edit = nil
      else
        st.edit = bone.text.edit(st.edit, k) or st.edit
        if row.key == "model" then
          st.model_query, st.note = st.edit, nil
        end
      end
      return true
    end
    st.note = nil
    if k == "esc" or k == "ctrl+c" then
      if st.step == "provider" then
        close(true)
      elseif st.step == "details" then
        st.step, st.sel = "provider", 1
      elseif st.step == "packages" then
        st.step, st.sel = st.form.subscription and "provider" or "details", 1
      end
    elseif k == "up" or k == "wheelup" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "wheeldown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif st.step == "provider" and k == "enter" and row then
      local p = M.PRESETS[row.preset]
      if p.plugin then
        subscribe(p)
        return true
      end
      st.form = { name = p.name, base_url = p.base_url, model = p.model, key = "", key_env = p.key_env or "", api = p.api }
      st.step, st.sel, st.keyless_ok = "details", p.base_url == "" and 1 or 3, false
      fetch_models()
    elseif st.step == "details" and k == "enter" and row then
      if row.action == "details_done" then
        local bad = check_details()
        if bad then
          st.note = { bad, true }
        else
          st.step, st.sel = "packages", 1
          load_packages()
        end
      else
        st.edit = row.key == "key" and st.form.key or (st.form[row.key] or "")
        st.model_query = row.key == "model" and st.edit or nil
      end
    elseif st.step == "packages" and row then
      if row.action == "apply" and k == "enter" then
        apply()
      elseif row.package and (k == "space" or k == "enter") then
        st.picked[row.package.name] = not st.picked[row.package.name] or nil
      end
    end
    return true
  end

  --- The keys that do something now.
  local function hint()
    if st.step == "welcome" then
      return "enter start · esc " .. (opts.first_run and "not now (/setup later)" or "close")
    elseif st.step == "applying" then
      return "working…"
    elseif st.step == "done" then
      return "↑↓ choose · enter use prompt · esc close"
    elseif st.edit and st.models and rows()[st.sel] and rows()[st.sel].key == "model" then
      return "↑↓ the server's models · enter save · esc cancel"
    elseif st.edit then
      return "enter save · esc cancel"
    elseif st.busy then
      return "working…"
    elseif st.step == "provider" then
      return "↑↓ move · enter choose · esc close"
    elseif st.step == "details" then
      return "↑↓ move · enter edit · esc back"
    end
    return "↑↓ move · space pick · enter on Continue · esc back"
  end

  local function paste(text)
    if st.edit then
      st.edit = st.edit .. text:gsub("[\r\n]", "")
      redraw()
    end
  end

  return { st = st, opts = opts, rows = rows, hint = hint, on_key = on_key, paste = paste }
end

local function render(page, ctx)
  local st, opts, rows = page.st, page.opts, page.rows
  local w = math.max(ctx.width, 30)
  local out = { { { fill = "─", hl = "Normal" } } }
  local titles = {
    welcome = "Setup",
    provider = "Setup · 1/3 provider",
    details = "Setup · 2/3 details",
    packages = "Setup · 3/3 packages",
    applying = "Setup",
    done = "Setup",
  }
  out[#out + 1] = " " .. titles[st.step]
  out[#out + 1] = ""
  if st.step == "welcome" then
    out[#out + 1] = "   Welcome to bone. It needs a model provider to talk to: a server on this"
    out[#out + 1] = "   machine, or a hosted service. This sets one up (saved in ~/.bone/settings.json;"
    out[#out + 1] = "   your core.lua is not touched) and can install plugins from the catalog."
    out[#out + 1] = ""
    out[#out + 1] = "   Customize the TUI: edit ~/.bone/tui.lua for simple changes, or run"
    out[#out + 1] = "   /tui create personal then /tui use personal for a full editable copy."
    out[#out + 1] = "   Changes under ~/.bone reload automatically."
    for _, p in ipairs(st.broken or {}) do
      out[#out + 1] = ""
      local wrapped = bone.text.wrap({ { "plugin " .. p.name .. " was not loaded: " .. p.error, "ErrorMsg" } }, w,
        { first = { { "   " } } })
      for i = 1, math.min(#wrapped, 3) do
        out[#out + 1] = wrapped[i]
      end
    end
  elseif st.step == "applying" or st.step == "done" then
    for _, l in ipairs(st.log) do
      out[#out + 1] = "   " .. l
    end
    if st.step == "done" then
      out[#out + 1] = ""
      out[#out + 1] = "   Bone is ready. Choose something to try:"
      for i, prompt in ipairs(st.first_prompts) do
        local mark = i == st.sel and " › " or "   "
        out[#out + 1] = bone.text.truncate(mark .. prompt, w)
      end
      out[#out + 1] = ""
    end
  else
    local list = rows()
    st.sel = math.max(1, math.min(st.sel, math.max(#list, 1)))
    if st.step == "packages" then
      if st.packages_err then
        out[#out + 1] = "   the catalog is not reachable (" .. st.packages_err .. "); /catalog later"
      elseif not st.packages then
        out[#out + 1] = "   loading the catalog…"
      end
    end
    local label_w = 0
    for _, r in ipairs(list) do
      if r.value ~= nil then
        label_w = math.max(label_w, bone.text.width(r.label))
      end
    end
    label_w = label_w + 3
    local room = math.max(math.min(14, (ctx.height or 24) - 9), 3)
    local first = math.max(1, st.sel - room + 1)
    for i = first, math.min(#list, first + room - 1) do
      local r = list[i]
      local mark = i == st.sel and " › " or "   "
      local text
      if r.value == nil then
        text = mark .. r.label
      elseif i == st.sel and st.edit then
        local shown = r.key == "key" and string.rep("•", #st.edit) or st.edit
        text = mark .. pad(r.label, label_w) .. shown .. "▏"
      else
        text = mark .. pad(r.label, label_w) .. (r.value ~= "" and r.value or "(empty)")
      end
      out[#out + 1] = bone.text.truncate(text, w)
    end
    out[#out + 1] = ""
    local cur = list[st.sel]
    -- The selected package's whole description; the row shows its start.
    local about = cur and cur.package and cur.package.description
    local note = st.note and ((st.note[2] and "error: " or "") .. st.note[1]) or about or (cur and cur.hint) or ""
    local wrapped = bone.text.wrap({ { note, st.note and st.note[2] and "ErrorMsg" or "Normal" } }, w,
      { first = { { "   " } } })
    for i = 1, math.min(#wrapped, 3) do
      out[#out + 1] = wrapped[i]
    end
  end
  out[#out + 1] = bone.text.truncate("   " .. page.hint(), w)
  return out
end

--- /setup: the page in a window. A library draws it differently by setting
--- `bone.ui.windows.setup(page)`, which returns the window's spec.
function M.open(opts)
  opts = opts or {}
  local id, paste_event
  local page = M.page(opts, {
    changed = function() if id then bone.ui.update(id, {}) end end,
    close = function(skipped)
      if paste_event then
        bone.off(paste_event)
        paste_event = nil
      end
      bone.ui.close(id)
      if skipped and opts.first_run then
        bone.settings.set("setup.skipped", true)
        bone.notify("Setup paused. Run /setup when you’re ready to connect a provider.")
      end
    end,
  })
  local view = bone.ui.windows.setup
  local spec = view and view(page) or { lines = function(ctx) return render(page, ctx) end, anchor = "prompt" }
  spec.on_key = page.on_key
  id = bone.ui.popup(spec)
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" then page.paste(ev.text or "") end
  end)
  return id
end

return M
