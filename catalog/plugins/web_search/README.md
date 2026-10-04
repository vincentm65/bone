# web_search

Search the web with DuckDuckGo. With `uv` on the PATH the tool runs a real
search through the `ddgs` package (uv fetches it on first use); without it,
it falls back to the Instant Answer endpoint, which only knows
encyclopedia-style topics.

This is the Bone 3 port of the `web_search` catalog package.

- core half: yes
- TUI half: no

Install with `python3 catalog/install.py web_search` from the repository root.
