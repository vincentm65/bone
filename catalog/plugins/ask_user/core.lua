-- Interactive questions use Bone 3's transport-level ask mechanism.  Any
-- client can answer them; the catalog TUI supplies the richer popup.
local function question_spec(q)
  local out = { kind = "ask_user", text = q.question or q.text or "Question" }
  out.type = q.type or (q.options and "single_select" or "text_input")
  out.options = q.options or {}
  out.allow_custom = q.allow_custom == true
  return out
end

bone.tool.register({
  name = "ask_user",
  description = "Ask one or more questions and return the user's answers.",
  parameters = {
    type = "object",
    properties = {
      questions = {
        type = "array", minItems = 1,
        items = {
          type = "object",
          properties = {
            question = { type = "string" },
            type = { type = "string", enum = { "single_select", "multi_select", "text_input" } },
            options = {
              type = "array",
              items = {
                anyOf = {
                  { type = "string" },
                  { type = "object", properties = {
                    label = { type = "string" }, value = { type = "string" }, description = { type = "string" },
                  }, required = { "label" } },
                },
              },
            },
            allow_custom = { type = "boolean" },
          },
          required = { "question" },
        },
      },
    },
    required = { "questions" },
  },
  needs_approval = false,
  run = function(args)
    if type(args.questions) ~= "table" or #args.questions == 0 then
      return nil, "questions must contain at least one question"
    end
    local answers = {}
    for i, q in ipairs(args.questions) do
      if type(q) ~= "table" or type(q.question) ~= "string" then
        return nil, "question " .. tostring(i) .. " must have question text"
      end
      local answer = bone.ask(question_spec(q))
      if answer == nil then
        return { cancelled = true, answers = answers, cancelled_at = i }
      end
      if answer == "cancelled" then
        return { cancelled = true, answers = answers, cancelled_at = i }
      end
      answers[#answers + 1] = { question = q.question, type = q.type or "text_input", value = answer }
    end
    return { cancelled = false, answers = answers }
  end,
})
