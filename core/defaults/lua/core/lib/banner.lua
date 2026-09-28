local function short_dir(path)
    local parts = {}
    for segment in path:gmatch("[^/]+") do parts[#parts + 1] = segment end
    if #parts <= 2 then return path end
    local first = path:sub(1, 1) == "/" and "/" or parts[1]
    local separator = first:sub(-1) == "/" and "" or "/"
    return first .. separator .. ".../" .. parts[#parts]
end

-- One short line of context. Deliberately width-agnostic — no measured rules
-- or padding, so a narrow terminal or desktop panel wraps it instead of tearing
-- a frame. Frontends render this as a muted system message, so no styling codes
-- belong here. The greeting below already carries the version.
bone.banner = function()
    return {
        bone.provider .. " · " .. bone.model .. " · " .. short_dir(bone.cwd),
    }
end
