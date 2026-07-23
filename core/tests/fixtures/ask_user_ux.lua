local tool_path = assert(arg[1], "ask_user.lua path required")
local registered
local calls = { q1 = 0, q2 = 0 }

package.preload["ui.menu"] = function()
    return {
        select = function(_, spec)
            if spec.question == "First?" then
                calls.q1 = calls.q1 + 1
                assert(spec.progress == "Question 1 of 2")
                assert(spec.allow_back == false)
                assert(spec.allow_forward == true)
                if calls.q1 == 1 then
                    return { value = "old custom", custom = true, selected = 1 }
                end
                assert(spec.initial == "old custom")
                assert(spec.initial_custom == true)
                return { value = "new custom", custom = true, selected = 1 }
            end
            assert(spec.question:match("Review your answers"))
            return { value = "submit", selected = 1 }
        end,
        multi_select = function() error("unexpected multi-select") end,
        text_input = function(_, spec)
            calls.q2 = calls.q2 + 1
            assert(spec.progress == "Question 2 of 2")
            assert(spec.allow_back == true)
            assert(spec.allow_forward == false)
            if calls.q2 == 1 then return { back = true, value = "draft" } end
            assert(spec.initial == "draft")
            return { value = "final" }
        end,
        clear = function() end,
    }
end

bone = { tool = { register = function(spec) registered = spec end } }
cjson = { encode = function(value) return value end }

dofile(tool_path)
assert(registered and registered.name == "ask_user")
local result = registered.execute({
    questions = {
        { question = "First?", options = { "a" }, allow_custom = true },
        { question = "Second?", type = "text_input" },
    },
}, {})

assert(result.cancelled == false)
assert(result.answers[1].value == "new custom")
assert(result.answers[2].value == "final")
assert(calls.q1 == 2 and calls.q2 == 2)
print("ask_user integration tests passed")
