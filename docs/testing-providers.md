# Testing providers

`crates/bone-fake-provider` is a strict stand-in for model APIs. It speaks
OpenAI Chat Completions (`/v1/chat/completions`) and Responses
(`/v1/responses`) and Anthropic Messages (`/v1/messages`) and checks requests
the way those APIs do: unanswered tool calls, unknown fields, malformed items,
empty text blocks, more than four cache breakpoints, and reasoning sent back
that it did not seal. It answers with a local model or with a script. No API
keys are involved: reasoning is "encrypted" (and Anthropic thinking signed)
with a key only this server knows, so bone has to send it back exactly as the
real APIs require.

Anthropic's rules are enforced as on newer accounts: thinking comes back only
unchanged, for the same model, after the same history (another model's is
dropped). Prompt caching is simulated with the real rules (breakpoints write
their prefix, a breakpoint looks back 20 positions for an earlier entry, a
run of tool calls or of tool results is one position, at least 1024 tokens),
so `GET /_fake/log` shows each request's `cache_read`, `cache_write` and
`prompt_tokens`, and the matrix checks that every call reads the whole
previous prompt.

## The matrix

```sh
cargo test -p bone-fake-provider                 # scripted: offline, every format and fault
cargo test -p bone-fake-provider --test matrix -- --ignored --nocapture   # live
```

The live tests run real multi-turn tool sessions through every format, with
the model on `BONE_FAKE_BACKEND` (default tabbyAPI on
`http://localhost:8081/v1`; `BONE_FAKE_MODEL` picks the model).

Formats and dialects (path prefixes before `/v1`):

| Prefix | What it imitates |
|---|---|
| (none) | OpenAI Chat Completions: no reasoning in the stream, no unknown fields |
| `/flavor/deepseek` | `reasoning_content` streamed; sending it back is an error |
| `/flavor/qwen` | `reasoning_content` streamed and accepted back (`replay_reasoning`) |
| `/flavor/openrouter` | `reasoning` streamed |
| `/flavor/unsealed` | Responses reasoning without `encrypted_content` (a gateway dropping it) |
| (none), `/v1/messages` | Anthropic Messages: `anthropic-version` required, preserved thinking enforced |

Faults: `/fault/<name>` on every request, `/fault/<name>@n` on the n-th:
`cut`, `incomplete`, `error_body`, `http429`, `http500`, `http529`, `empty`,
`slow`.
A fault must end the turn with an error that says what happened, never
silently; 429 and 5xx must be retried; an `empty` reply that finished
properly is a normal (if wordless) answer.

## By hand

```sh
cargo run -p bone-fake-provider -- --port 4100   # --backend URL, --model NAME
```

Point a provider at it (`api = "responses"` for the Responses format):

```lua
bone.config.providers.fake = {
  base_url = "http://127.0.0.1:4100/fault/cut@3/v1",
  model = "gpt-fake", api_key = "x", api = "responses",
}
```

`GET http://127.0.0.1:4100/_fake/log` lists every request: its status, why it
was rejected, and how many sealed reasoning items it carried.

## When a real provider misbehaves

Run bone with `BONE_DEBUG_HTTP=<dir>`: each model request and its raw
response are saved there (keys are not; they are headers). Find the rule the
real API enforced or the event it sent, teach the fake server the same, add
a matrix case that fails, then fix bone until it passes.
