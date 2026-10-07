-- bone core config: providers, system prompt, tools and hooks.
-- Reference: docs/lua.md in the bone repository.
--
-- Pick a provider below (any OpenAI-compatible /chat/completions endpoint),
-- uncomment it, and set bone.config.provider to its name.

local providers = bone.config.providers

-- A local server (llama.cpp, vLLM, tabbyAPI, Ollama's /v1, ...):
-- providers.local_model = {
--   base_url = "http://localhost:8080/v1",
--   model = "your-model-name",
-- }

-- A hosted API. Keep keys in the environment, not in this file:
-- providers.hosted = {
--   base_url = "https://api.example.com/v1",
--   model = "model-name",
--   api_key = os.getenv("EXAMPLE_API_KEY"),
--   -- reasoning_effort = "medium",
-- }

-- bone.config.provider = "local_model"

-- Tool calls run without asking. Add a tool_call hook using bone.ask to
-- request confirmation; see docs/lua.md, "Asking the user".

-- Refuse dangerous commands (hooks run at turn_start, request, message,
-- tool_call, tool_result and turn_end; see docs/lua.md):
-- bone.hook("tool_call", function(ev)
--   if ev.name == "shell" and (ev.arguments.command or ""):match("rm %-rf /") then
--     return { deny = "not allowed" }
--   end
-- end)
