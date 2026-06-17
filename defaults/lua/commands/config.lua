-- /config — interactive settings editor.

local menu = require("ui.menu")

local function split_args(arg)
    local words = {}
    for word in tostring(arg or ""):gmatch("%S+") do
        words[#words + 1] = word
    end
    return words
end

local function clear_interact(ctx)
    menu.clear(ctx)
end

local function ask(ctx, opts)
    local fn = menu.select
    if opts.type == "text_input" or opts.type == "text" then
        fn = menu.text_input
    elseif opts.type == "multi_select" or opts.type == "multi" then
        fn = menu.multi_select
    end
    local ok, result = pcall(fn, ctx, opts)
    if not ok then
        ctx.ui.notify("Config picker failed: " .. tostring(result), "error")
        return nil
    end
    if type(result) ~= "table" or result.cancelled then
        return nil
    end
    return result
end

local function mask_secret(value)
    if not value or value == "" then return "(empty)" end
    local len = math.min(12, math.max(4, #tostring(value)))
    return string.rep("*", len)
end

local function tab_bar(pages, active)
    local labels = {}
    for i, page in ipairs(pages or {}) do
        local title = page.title or page.namespace or tostring(i)
        if i == active then
            labels[#labels + 1] = "[" .. title .. "]"
        else
            labels[#labels + 1] = title
        end
    end
    return table.concat(labels, "  ")
end

local function find_page_index(pages, namespace)
    for i, page in ipairs(pages or {}) do
        if page.namespace == namespace then return i end
    end
    return 1
end

local function previous_index(active, count)
    if active <= 1 then return count end
    return active - 1
end

local function next_index(active, count)
    if active >= count then return 1 end
    return active + 1
end

local function field_display(field)
    if field.type == "bool" then
        local marker = field.value == "true" and "●" or "○"
        return marker .. " " .. field.label
    end
    return string.format("%-30s %s", field.label or field.key, tostring(field.value or ""))
end

local function field_by_display(page)
    local labels = {}
    local by_label = {}
    for _, field in ipairs(page.fields or {}) do
        if field.type ~= "provider" then
            local label = field_display(field)
            labels[#labels + 1] = label
            by_label[label] = field
        end
    end
    return labels, by_label
end

local function edit_text(ctx, label, initial)
    local result = ask(ctx, {
        question = "Edit " .. label .. ". Enter saves, Esc cancels.",
        type = "text_input",
        options = {},
        allow_custom = true,
    })
    if not result then return nil end
    if result.value == "" and initial and initial ~= "" then
        return ""
    end
    return result.value or ""
end

local function provider_labels(ctx)
    local providers = ctx.config.list_providers()
    local labels = {}
    local by_label = {}
    for _, provider in ipairs(providers or {}) do
        local marker = provider.active and "●" or "○"
        local kind = provider.handler or "openai"
        local label = string.format(
            "%s %s · %s · %s · %s",
            marker,
            provider.id,
            provider.model or "",
            provider.label or "",
            kind
        )
        labels[#labels + 1] = label
        by_label[label] = provider
    end
    return labels, by_label
end

local function edit_provider(ctx, provider)
    local entry = {
        label = provider.label or "",
        model = provider.model or "",
        base_url = provider.base_url or "",
        endpoint = provider.endpoint or "/chat/completions",
        handler = provider.handler or "openai",
        api_key = provider.api_key or "",
    }

    while true do
        local labels = {
            "label · " .. entry.label,
            "model · " .. entry.model,
            "base_url · " .. entry.base_url,
            "endpoint · " .. entry.endpoint,
            "handler · " .. entry.handler,
            "api_key · " .. mask_secret(entry.api_key),
            "Save changes",
        }
        local result = ask(ctx, {
            question = "Edit provider: " .. provider.id,
            type = "single_select",
            options = labels,
            allow_custom = false,
        })
        if not result then return false end
        local choice = result.value
        if choice == labels[1] then
            local value = edit_text(ctx, "label", entry.label)
            if value ~= nil then entry.label = value end
        elseif choice == labels[2] then
            local value = edit_text(ctx, "model", entry.model)
            if value ~= nil then entry.model = value end
        elseif choice == labels[3] then
            local value = edit_text(ctx, "base_url", entry.base_url)
            if value ~= nil then entry.base_url = value end
        elseif choice == labels[4] then
            local value = edit_text(ctx, "endpoint", entry.endpoint)
            if value ~= nil then entry.endpoint = value end
        elseif choice == labels[5] then
            entry.handler = entry.handler == "codex" and "openai" or "codex"
        elseif choice == labels[6] then
            local value = edit_text(ctx, "api_key", "")
            if value ~= nil then entry.api_key = value end
        elseif choice == labels[7] then
            ctx.config.set_provider_entry(provider.id, entry)
            return true
        end
    end
end

bone.register_command("config", {
    description = "edit configuration",
    handler = function(arg, ctx)
        local words = split_args(arg)
        if words[1] == "tools" and words[2] == "reload" then
            return { action = "config.reload_tools", submit = false }
        end

        local pages = ctx.config.get_pages()
        if not pages or #pages == 0 then
            ctx.ui.notify("No config pages found.", "warn")
            return nil
        end

        local active = find_page_index(pages, words[1])
        local changed = false
        local last_selected = {}
        while true do
            pages = ctx.config.get_pages()
            local page = pages[active] or pages[1]
            active = find_page_index(pages, page.namespace)

            local options = {}
            local by_label = {}
            if page.namespace == "providers" then
                options, by_label = provider_labels(ctx)
            else
                options, by_label = field_by_display(page)
            end

            local is_providers = page.namespace == "providers"
            local result = ask(ctx, {
                question = tab_bar(pages, active) .. (is_providers and "  [e] edit" or ""),
                type = "single_select",
                options = options,
                default = last_selected[page.namespace] or 1,
                allow_custom = false,
                left_value = "__prev_tab",
                right_value = "__next_tab",
                action_keys = is_providers and { e = "__edit_provider" } or nil,
            })
            if not result then
                clear_interact(ctx)
                if changed then
                    return { action = "config.apply", submit = false }
                end
                return nil
            end

            last_selected[page.namespace] = result.selected

            local choice = result.value
            if choice == "__prev_tab" then
                active = previous_index(active, #pages)
            elseif choice == "__next_tab" then
                active = next_index(active, #pages)
            elseif page.namespace == "providers" then
                if choice == "__edit_provider" then
                    local idx = result.selected
                    if idx and idx <= #options then
                        local provider = by_label[options[idx]]
                        if provider and edit_provider(ctx, provider) then
                            changed = true
                        end
                    end
                else
                    local provider = by_label[choice]
                    if provider then
                        clear_interact(ctx)
                        return {
                            action = "config.switch_provider",
                            provider = provider.id,
                            submit = false,
                        }
                    end
                end
            else
                local field = by_label[choice]
                if field then
                    if field.type == "bool" or field.type == "enum" then
                        local next_value = ctx.config.cycle_field(page.namespace, field.key, field.value or "")
                        if next_value then
                            ctx.config.set_value(page.namespace, field.key, next_value)
                            changed = true
                        end
                    else
                        local value = edit_text(ctx, field.label or field.key, field.value or "")
                        if value ~= nil then
                            ctx.config.set_value(page.namespace, field.key, value)
                            changed = true
                        end
                    end
                end
            end
        end
    end,
})
