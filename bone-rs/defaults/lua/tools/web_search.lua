local function execute(params, ctx)
    local query = params.query
    local num_results = params.num_results or 5
    num_results = math.max(1, math.min(10, num_results))

    -- Escape for safe embedding in double-quoted shell env var
    local safe_query = query:gsub('\\', '\\\\'):gsub('"', '\\"')

    local cmd = string.format(
        "export TOOL_QUERY=\"%s\"; export TOOL_NUM_RESULTS=%d; uv run --with ddgs -- python3 -c 'import json, os, sys; from ddgs import DDGS; query = os.environ[\"TOOL_QUERY\"]; num = max(1, min(10, int(os.environ.get(\"TOOL_NUM_RESULTS\", \"5\")))); [print(json.dumps(r)) for r in DDGS().text(query, max_results=num)]'",
        safe_query, num_results
    )

    local result = ctx.shell(cmd, { timeout_ms = 300000 })
    if result.stderr and #result.stderr > 0 then
        return "ERROR: " .. result.stderr
    end
    return result.stdout or ""
end

bone.register_tool({
    name = "web_search",
    description = "Search the web for information using DuckDuckGo. Returns titles, URLs and summaries. Useful for looking up documentation, current events, technical topics, and general knowledge.",
    parameters = {
        type = "object",
        properties = {
            query = {
                type = "string",
                description = "The search query",
            },
            num_results = {
                type = "number",
                description = "Number of results to return (default 5, max 10)",
            },
        },
        required = { "query" },
        additionalProperties = false,
    },
    safety = "read_only",
    execute = execute,
})
