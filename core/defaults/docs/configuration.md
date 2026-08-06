# Configuration and Themes

Core is the only live configuration authority. It loads the peer domain files,
validates them against Rust and extension schemas, combines them into one
revisioned snapshot, and sends that snapshot to every frontend.

## Config location and files

The resolved config directory is provided in the system prompt. Its default is
`~/.bone-rust`; `BONE_DIR` takes precedence, followed by
`$XDG_CONFIG_HOME/bone-rust` and `$HOME/.bone-rust`.

| File | Purpose |
|---|---|
| `config.yaml` | General approval, UI, theme, keymap, and enablement values |
| `providers.yaml` | Providers, models, endpoints, and credentials |
| `subagents.yaml` | Named static subagent definitions and prompts |
| `extensions.yaml` | Namespaced extension values |
| `command-policy.yaml` | Shell command safety classifications |
| `init.lua` | Optional runtime wiring; not a competing settings store |
| `AGENTS.md` and `docs/` | Bone-owned bundled reference documents |

Built-in schemas, labels, defaults, types, and option lists live in Rust.
Canonical YAML stores user-selected values. Extension schemas are registered
through Lua and their values are stored under their namespace in
`extensions.yaml`.

## Mutation and restart rules

Prefer `/config`, the web settings client, or the daemon configuration APIs for
supported mutations. Typed changes are validated against the current schema
revision and persist only the affected domain. Depending on the setting, a
change applies immediately, on the next model turn, or after extensions reload.

Direct YAML edits are read at startup. After directly editing
`providers.yaml`, `subagents.yaml`, `extensions.yaml`, `config.yaml`, or
`command-policy.yaml`, tell the user to restart Bone. `command-policy.yaml` is
file-edited and always restart-required. Provider API keys may be plaintext or
an exact `${ENV_VAR}` reference; only the complete reference form resolves from
the environment.

`init.lua` is for lightweight startup wiring. Put substantial implementations in
purpose-specific Lua files and do not define a second settings table there.

## Main-agent system prompt

`general.system_prompt` is a nullable daemon-owned string that replaces Bone's
built-in base prompt for normal main-agent turns. Bone still appends the generated
configuration-directory and current-working-directory context, and Lua
`before_turn.system_prompt_append` hooks remain additive.

Changes made through a frontend or the daemon configuration API apply when the
next main-agent turn is built. Unsetting or resetting the value to `null` restores
the built-in base prompt without removing the generated runtime context. Explicit
`bone run --system-prompt` values and delegated or subagent prompt overrides are
separate and are not replaced by this setting.

## Theme values

Theme modules live at `lua/themes/<name>.lua`, return a settings table, and are
listed or loaded with `bone.theme.list()` and `bone.theme.load(name)`. Loading a
theme validates it and persists its name and resolved values in `config.yaml`.
Runtime highlight overrides are ephemeral and are applied after configured
values; passing `nil` to a runtime override reveals the configured value.

Most themes only need palette values. Shell, syntax, and exact UI-role overrides
use separate maps:

```yaml
theme:
  palette:
    accent: "#8cdcdc"
    good: "#78b373"
    warn: "#d7ba7d"
    error: "#e05050"
    selection: "#303030"
  shell:
    program: "#b4c896"
    flag: "#96b4dc"
    string: "#c8aa78"
  syntax:
    comment: "#6a9955"
    keyword: "#569cd6"
    function_name: "#dcdcaa"
  highlights:
    user_msg: { fg: fg, bg: selection }
    input_border: border
    tool_error: error
```

Colors may be `#RRGGBB` (with or without `#`) or a supported named color.
Resolution precedence is native defaults → palette → derived roles → structured
shell/syntax values → legacy flat fields → `highlights`. Palette names may be
referenced by role values. `input_bg`, `input_prefix`, and `input_cursor` are
configured only under `theme.highlights`; palette roles belong under
`theme.palette`.

<!-- BEGIN GENERATED THEME ROLES -->
| Role | Channel | Runtime |
|---|---|:---:|
| `bg` | bg | yes |
| `fg` | fg | no |
| `muted` | fg | no |
| `subtle` | fg | no |
| `border` | fg | no |
| `accent` | fg | no |
| `good` | fg | no |
| `warn` | fg | no |
| `error` | fg | no |
| `selection` | fg | no |
| `user_msg` | fg + bg | yes |
| `user_msg_bg` | bg | yes |
| `status_text` | fg | yes |
| `input_border` | fg | yes |
| `input_bg` | bg | yes |
| `input_prefix` | fg | yes |
| `input_cursor` | fg | yes |
| `system_msg` | fg | yes |
| `approval_safe` | fg | yes |
| `approval_danger` | fg | yes |
| `tool_call` | fg | yes |
| `tool_error` | fg | yes |
| `diff_removed` | fg | yes |
| `diff_added` | fg | yes |
| `thinking` | fg | yes |
| `shell_program` | fg | yes |
| `shell_separator` | fg | yes |
| `shell_redirect` | fg | yes |
| `shell_flag` | fg | yes |
| `shell_string` | fg | yes |
| `shell_variable` | fg | yes |
| `shell_comment` | fg | yes |
| `shell_path` | fg | yes |
| `syntax_text` | fg | yes |
| `syntax_comment` | fg | yes |
| `syntax_string` | fg | yes |
| `syntax_number` | fg | yes |
| `syntax_constant` | fg | yes |
| `syntax_escape` | fg | yes |
| `syntax_regex` | fg | yes |
| `syntax_keyword` | fg | yes |
| `syntax_keyword_control` | fg | yes |
| `syntax_type` | fg | yes |
| `syntax_function` | fg | yes |
| `syntax_variable` | fg | yes |
| `syntax_tag` | fg | yes |
| `syntax_attribute` | fg | yes |
| `syntax_punctuation` | fg | yes |
| `syntax_subtle` | fg | yes |
| `syntax_markup` | fg | yes |
| `syntax_invalid` | fg | yes |
| `markdown_marker` | fg | yes |
| `markdown_heading` | fg | yes |
| `markdown_link` | fg | yes |
| `markdown_inline_code` | fg | yes |
| `markdown_rule` | fg | yes |
| `markdown_table_border` | fg | yes |
| `markdown_table_header` | fg | yes |
| `chart` | fg | yes |
| `chart_empty` | fg | yes |
| `heat_low` | fg | yes |
| `heat_high` | fg | yes |
<!-- END GENERATED THEME ROLES -->

## Keymaps

Bindings are an ordered list persisted under `keymaps.bindings`:

```yaml
keymaps:
  bindings:
    - { key: "<C-p>", action: toggle_panes }
    - { key: "<S-Tab>", action: cycle_approval_mode }
    - { key: "<C-a>", action: cursor_to_start }
    - { key: "<C-e>", action: cursor_to_end }
```

`bone.keymap.set(key, rhs)` adds a runtime binding from `init.lua` or an
extension. The right-hand side may be a built-in action, slash command, prompt
text, or function returning one of those. Keys and actions must not be empty and
each key may be bound only once. Built-in actions include `toggle_panes`,
`cycle_approval_mode`, `cursor_to_start`, and `cursor_to_end`.

## Bundled reference documents

The files under `.bone-rust/docs/` are materialized from `core/defaults/docs/`
by the running Bone build. They are included in new builds and refreshed at
startup so the reference matches the installed version. Treat those generated
copies as read-only; update the bundled source when documenting core behavior.
User configuration files and extension-owned documentation are separate and are
not replaced by this synchronization.
