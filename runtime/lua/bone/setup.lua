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
  { title = "DeepSeek", name = "deepseek", base_url = "https://api.deepseek.com/v1", model = "", key_env = "DEEPSEEK_API_KEY" },
  { title = "OpenRouter", name = "openrouter", base_url = "https://openrouter.ai/api/v1", model = "", key_env = "OPENROUTER_API_KEY" },
  { title = "Another OpenAI-compatible service", name = "", base_url = "", model = "" },
}

local CHAR = "^[%z\1-\127\194-\244][\128-\191]*$"

local function pad(s, w)
  s = tostring(s or "")
  local n = bone.text.width(s)
  if n >= w then
    return bone.text.truncate(s, w)
  end
  return s .. string.rep(" ", w - n)
end

--- opts: { first_run = true } when it opened by itself.
function M.open(opts)
  opts = opts or {}
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
  }
  local id
  local paste_event

  local function redraw()
    if id then
      bone.ui.update(id, {})
    end
  end

  local function close(skipped)
    if paste_event then
      bone.off(paste_event)
      paste_event = nil
    end
    bone.ui.close(id)
    if skipped and opts.first_run then
      bone.settings.set("setup.skipped", true)
    end
  end

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
        { label = "Model", key = "model", value = f.model, hint = "the model name the server knows" },
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
            label = e.name,
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
    local spec = { base_url = f.base_url, model = f.model }
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
    local function install_next(i)
      local e = queue[i]
      if not e then
        return finished()
      end
      say("installing " .. e.name .. "…")
      catalog.install(e, function(ok, err)
        say(ok and ("installed " .. e.name) or ("error: " .. e.name .. ": " .. tostring(err)))
        install_next(i + 1)
      end)
    end
    local function after_key()
      bone.settings.set("provider", f.name, function(_, err)
        if err then
          say("error: " .. tostring(err))
        else
          say("using " .. f.name .. " (" .. f.model .. ")")
        end
        install_next(1)
      end)
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
        say(kerr and ("error: the key was not saved: " .. tostring(kerr)) or "saved the key")
        after_key()
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
    end
  end

  local function render(ctx)
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
    local hint
    if st.step == "welcome" then
      out[#out + 1] = "   Welcome to bone. It needs a model provider to talk to: a server on this"
      out[#out + 1] = "   machine, or a hosted service. This sets one up (saved in ~/.bone/settings.json;"
      out[#out + 1] = "   your core.lua is not touched) and can install plugins from the catalog."
      hint = "enter start · esc " .. (opts.first_run and "not now (/setup later)" or "close")
    elseif st.step == "applying" or st.step == "done" then
      for _, l in ipairs(st.log) do
        out[#out + 1] = "   " .. l
      end
      if st.step == "done" then
        out[#out + 1] = ""
        out[#out + 1] = "   Done. Type a message to start; /config changes any of this later."
        hint = "enter or esc close"
      else
        hint = "working…"
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
      local note = st.note and ((st.note[2] and "error: " or "") .. st.note[1]) or (cur and cur.hint) or ""
      out[#out + 1] = bone.text.truncate("   " .. note, w)
      if st.edit then
        hint = "enter save · esc cancel"
      elseif st.step == "provider" then
        hint = "↑↓ move · enter choose · esc close"
      elseif st.step == "details" then
        hint = "↑↓ move · enter edit · esc back"
      else
        hint = "↑↓ move · space pick · enter on Continue · esc back"
      end
    end
    out[#out + 1] = bone.text.truncate("   " .. hint, w)
    return out
  end

  local function on_key(k)
    if st.step == "applying" then
      return true
    end
    if st.step == "done" then
      if k == "enter" or k == "esc" or k == "q" then
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
      if k == "enter" then
        st.form[row.key] = st.edit
        st.edit = nil
      elseif k == "esc" then
        st.edit = nil
      elseif k == "backspace" then
        st.edit = st.edit:gsub("[%z\1-\127\194-\244][\128-\191]*$", "")
      elseif k == "ctrl+u" then
        st.edit = ""
      elseif k == "space" then
        st.edit = st.edit .. " "
      elseif k:match(CHAR) then
        st.edit = st.edit .. k
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
        st.step, st.sel = "details", 1
      end
    elseif k == "up" or k == "wheelup" then
      st.sel = math.max(1, st.sel - 1)
    elseif k == "down" or k == "wheeldown" then
      st.sel = math.min(math.max(#list, 1), st.sel + 1)
    elseif st.step == "provider" and k == "enter" and row then
      local p = M.PRESETS[row.preset]
      st.form = { name = p.name, base_url = p.base_url, model = p.model, key = "", key_env = p.key_env or "" }
      st.step, st.sel = "details", p.base_url == "" and 1 or 3
    elseif st.step == "details" and k == "enter" and row then
      if row.action == "details_done" then
        local bad = check_details()
        if bad then
          st.note = { bad, true }
        else
          st.step, st.sel = "packages", 1
          if not st.packages then
            catalog.index(function(entries, err)
              st.packages, st.packages_err = entries or {}, err
              redraw()
            end)
          end
        end
      else
        st.edit = row.key == "key" and st.form.key or (st.form[row.key] or "")
      end
    elseif st.step == "packages" and row then
      if row.action == "apply" and k == "enter" then
        apply()
      elseif row.package and (k == "space" or k == "enter") then
        st.picked[row.label] = not st.picked[row.label] or nil
      end
    end
    return true
  end

  id = bone.ui.popup({ lines = render, on_key = on_key, anchor = "prompt" })
  paste_event = bone.on("paste", function(ev)
    if ev.context == "popup" and st.edit then
      st.edit = st.edit .. (ev.text or ""):gsub("[\r\n]", "")
      redraw()
    end
  end)
  return id
end

return M
