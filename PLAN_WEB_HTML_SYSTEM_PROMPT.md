# Configurable System Prompt and Safe Web HTML Plan

## Scope

Implement two independent changes without altering the message protocol or conversation persistence:

1. Add `general.system_prompt` to daemon-managed configuration and use it as the main agent's configurable base prompt.
2. Render useful model-produced inline and block HTML in the web UI after strict sanitization, with Bone-owned styling.

## System prompt configuration

- Add `system_prompt: Option<String>` to `GeneralSettings`, defaulting to `None`.
- Expose it through the existing configuration schema, snapshot, `GetConfig`, `SetConfigValue`, and `ResetConfigValue` paths. Do not add a prompt field to message commands or a persistence migration.
- Refactor prompt construction into explicit parts:
  1. Base prompt: configured `general.system_prompt` when set, otherwise Bone's built-in base prompt.
  2. Runtime context: always-generated configuration-directory and working-directory context.
  3. Existing Lua `before_turn.system_prompt_append`: additive per turn.
- Read resolved settings when each main-session model turn is built so changes from any frontend apply on the next turn.
- Preserve headless `bone run --system-prompt` behavior and subagent prompt overrides unless existing tests establish a required shared helper.
- Resetting or unsetting `general.system_prompt` restores the built-in base prompt while retaining runtime context.

## Safe web HTML rendering

- Trace every Markdown rendering and HTML insertion path, including live streaming, completed messages, and replayed history.
- Vendor a pinned DOMPurify browser build in the existing web asset layout and load it before the Markdown renderer/application code.
- Extend the Markdown renderer to preserve model-produced inline and block HTML rather than escaping all raw tags.
- Sanitize the complete rendered result immediately before DOM insertion with an explicit semantic allowlist.
- Permit useful document markup such as headings, paragraphs, emphasis, lists, tables, code, links, images, details/summary, keyboard text, and other inert semantic containers where supported by application CSS.
- Reject scripts, event-handler attributes, unsafe URL schemes, external/active resources not explicitly allowed, forms and form controls, embedding/navigation primitives, dangerous metadata, and active SVG/MathML features.
- Apply safe link behavior in application code (including `rel` handling) rather than trusting model attributes.
- Add Bone-owned CSS for allowed HTML elements so presentation remains consistent with generated Markdown and the active theme.
- Keep sanitization centralized so streaming and replay cannot bypass it.

## Validation

### Rust

- Test the default built-in base prompt plus runtime context.
- Test configured-base replacement while retaining runtime context.
- Test reset/unset behavior.
- Test that reloaded configuration affects the next main-agent turn.
- Test that Lua `system_prompt_append` remains additive.
- Run focused configuration, prompt, runtime, and RPC tests, then formatting and broader affected-crate tests.

### Web

- Test ordinary Markdown rendering remains intact.
- Test inline and block HTML rendering.
- Test equivalent behavior for streaming updates and replayed messages.
- Test malformed HTML handling.
- Test removal of scripts, event handlers, unsafe links/resources, forms, and active SVG payloads.
- Run the existing web test suite and any asset/build validation.

## Final review

- Inspect the final diff for unrelated changes, protocol/persistence modifications, sanitizer bypasses, and prompt-composition regressions.
- Report changed files, validation results, and any remaining limitations.
