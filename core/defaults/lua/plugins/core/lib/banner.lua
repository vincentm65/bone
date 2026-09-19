local function width(s)
    local n = 0
    for _ in utf8.codes(s) do n = n + 1 end
    return n
end

local function short_dir(path)
    local parts = {}
    for segment in path:gmatch("[^/]+") do parts[#parts + 1] = segment end
    if #parts <= 2 then return path end
    local first = path:sub(1, 1) == "/" and "/" or parts[1]
    local separator = first:sub(-1) == "/" and "" or "/"
    return first .. separator .. ".../" .. parts[#parts]
end

bone.banner = function()
    local width_available = bone.api.ui.term_width()
    local content_width = width_available - 3
    local function row(left, right)
        local padding = math.max(0, content_width - width(left) - width(right) - 1)
        return "│ " .. left .. (" "):rep(padding) .. right .. " │"
    end
    local rule = ("─"):rep(math.max(0, width_available - 2))
    return {
        "╭" .. rule .. "╮",
        row("bone", "v" .. bone.version),
        row(bone.provider .. " · " .. bone.model, short_dir(bone.cwd)),
        "╰" .. rule .. "╯",
    }
end
