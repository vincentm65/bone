# Reflective Runtime Control Plan

## Goal

Give Bone Core a live, self-describing way to inspect and control connected frontends without adding a separate model tool or hardcoded Core action for every feature.

The model should receive one generic Bone control interface and discover available capabilities at runtime.

## Design

### 1. Capability registry

Add a registry of namespaced capabilities such as:

- `conversation.clear`
- `conversation.start_with_prompt`
- `input.enqueue`
- `theme.list`
- `theme.load`
- `pane.open`
- `pane.close`
- `ui.get_state`

Each capability declares:

- Name and description
- Input schema
- Current availability
- Approval requirements
- Which frontend owns its handler

Frontends advertise their capabilities when they connect. Lua extensions may register additional capabilities dynamically.

### 2. Generic protocol

Add protocol messages for:

- Listing capabilities
- Inspecting one capability
- Reading exposed frontend state
- Invoking a capability
- Returning success, cancellation, or an error

Route an invocation to the frontend that owns the active request. Do not broadcast an action that multiple connected frontends could execute.

### 3. One model interface

Expose one generic model tool, tentatively named `bone`, with operations equivalent to:

```text
bone.list()
bone.inspect("conversation.start_with_prompt")
bone.get("ui.theme")
bone.call("conversation.start_with_prompt", { prompt = plan })
bone.call("theme.load", { name = "nord" })
```

Generate the model-facing capability descriptions from the registry instead of maintaining a hardcoded capability list in the system prompt.

### 4. Safe state access

Allow generic discovery and read-only state inspection.

Require all mutations to go through registered methods. Do not expose arbitrary property or memory writes, because they could bypass validation and break frontend invariants.

Capabilities must validate their arguments and may require user approval before execution.

## Plan handoff workflow

Use the reflective control plane for fresh-context implementation:

1. Keep the completed plan as a separate handoff artifact.
2. Ask whether the user wants to implement it in the current context, clear context first, or continue planning.
3. On approval, invoke `conversation.start_with_prompt` with the saved plan.
4. The frontend clears its transcript and requests a new conversation.
5. Wait until the new conversation is ready.
6. Submit the saved plan as the first prompt in the fresh conversation.
7. End the old model turn without adding a stale tool result to the new context.

Suggested initial prompt:

```text
A previous agent produced the approved plan below. Implement it in a fresh context. Treat the plan as the source of user intent, re-read files as needed, and complete implementation and verification.

<approved plan>
```

## Implementation order

1. Define capability metadata and registry types in Core.
2. Add capability advertisement, invocation, and result messages to the protocol.
3. Add frontend registration and request routing.
4. Implement the generic `bone` model tool.
5. Register a small initial set of TUI capabilities.
6. Add equivalent WebUI capabilities where supported.
7. Add Lua capability registration.
8. Implement the fresh-context plan handoff using `conversation.start_with_prompt`.

## Validation

Test that:

- Core discovers capabilities without a hardcoded central action list.
- Invalid or unavailable capabilities fail clearly.
- Approval requirements are enforced.
- Only the originating frontend executes an invocation.
- Disconnects and cancellations do not leave requests hanging.
- Clearing and submitting waits for the fresh conversation before starting the new turn.
- The new model context contains the saved plan but none of the old transcript.
- TUI and WebUI expose consistent behavior where their capabilities overlap.
- Lua capabilities can be registered, discovered, invoked, reloaded, and removed safely.
