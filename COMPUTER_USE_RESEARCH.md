# AI Computer Use on Linux and Hyprland

## Research summary

This report explains how current AI computer-use systems control browsers and desktops, what useful open-source projects are doing, and what is practical on Hyprland.

The short version:

- Most general computer-use agents run the same loop: **take a screenshot, ask a vision model what to do, execute a structured action, then take another screenshot**.
- Browser agents are usually more reliable because they can use the **DOM, accessibility tree, Chrome DevTools Protocol (CDP), or Playwright**, instead of guessing everything from pixels.
- On Hyprland, screen observation is practical through **grim, Hyprland IPC, or PipeWire portals**. Input is practical through **Hyprland dispatchers, wtype, the virtual-pointer protocol, or ydotool/uinput**.
- Ordinary Wayland input injection moves the session's real pointer and may change focus. It does **not** provide a hidden second pointer.
- A truly separate background pointer is practical inside a **separate browser, nested compositor, separate Wayland session, or VM**. It is not currently practical as a second independent pointer in the same normal Hyprland seat.
- The strongest design is hybrid: use semantic APIs first, screenshots when needed, and coordinate clicks only as a fallback.

This is architectural research, not copied implementation code. Project details can change, so the linked primary documentation and repositories should be checked before implementation.

---

## 1. The common computer-use strategy

### 1.1 The basic observe–decide–act loop

OpenAI, Anthropic, Google, UI-TARS, and many open-source agents use some version of this loop:

1. Capture the current screen or target window.
2. Give the image and task history to a vision-capable model.
3. Receive a structured action such as:
   - click at `(x, y)`;
   - type text;
   - press keys;
   - scroll;
   - drag;
   - wait;
   - request another screenshot.
4. Validate the action against a safety policy.
5. Execute it in a browser, desktop session, or sandbox.
6. Capture the result.
7. Repeat until the task is complete or blocked.

This is popular because it works with almost any visible interface. The target application does not need a special automation API.

Its weaknesses are equally important:

- Pixel coordinates are fragile when windows move or displays resize.
- Screenshots are slower and more expensive than structured text.
- The model can misread small controls or click the wrong nearby element.
- Animations, pop-ups, and delayed loading can invalidate a planned click.
- Text visible on a page can contain prompt injection aimed at the agent.
- A screenshot may expose private information to the model provider.

A robust implementation therefore treats each action as a small transaction: observe, choose one or a small batch of safe actions, execute, then verify.

### 1.2 Three ways to understand an interface

Computer-use systems generally use one or more of these perception methods.

#### A. Pure screenshots

The model sees the interface as pixels, like a person looking at a monitor.

Advantages:

- Works across native applications, browsers, remote desktops, games, and custom UI toolkits.
- Does not depend on the application exposing useful metadata.

Disadvantages:

- Highest latency and model cost.
- Coordinate grounding is error-prone.
- Difficult to identify hidden, clipped, disabled, or off-screen controls.
- Requires careful display scaling and multi-monitor coordinate handling.

#### B. Structured interface data

The agent reads the browser DOM, browser accessibility tree, Linux AT-SPI tree, Windows UI Automation tree, or macOS Accessibility tree.

Advantages:

- Gives controls stable names, roles, values, and states.
- Usually faster and cheaper than vision.
- Can activate a control semantically instead of clicking its current pixels.

Disadvantages:

- Coverage varies by application and toolkit.
- Custom-drawn interfaces may expose little useful information.
- Browser DOM access only solves browser tasks.
- Linux AT-SPI is not a complete compositor-wide map of every visible surface.

#### C. Hybrid perception

The best practical systems combine structured data with a screenshot:

- use DOM/accessibility data to identify controls;
- use a screenshot to understand visual layout and verify the result;
- use OCR or a screen parser when structured metadata is missing;
- fall back to coordinate clicks only when a semantic action is unavailable.

This is the most promising strategy for a Hyprland computer-use tool.

---

## 2. What major systems are doing

### 2.1 OpenAI computer use and Codex

OpenAI's public Computer Use API exposes a model that returns structured UI actions. The developer supplies the execution environment and repeatedly sends screenshots back after executing actions. Publicly documented actions include pointer movement, clicks, typing, key presses, scrolling, dragging, waiting, and screenshots.

The important architectural point is that the model does not directly control the operating system. A local or hosted **harness** translates model actions into actual browser or OS events. That harness is also where sandboxing, approval prompts, coordinate conversion, and policy checks belong.

OpenAI's hosted agent products use an isolated browser or computer environment. Isolation keeps automated activity separate from the user's main desktop and is a major reason a hosted agent can keep working without fighting the user's pointer.

Codex's exact macOS implementation details are not fully public. macOS computer control normally requires Screen Recording permission for observation and Accessibility permission for input/UI control. Reports of computer use continuing separately from the user's interaction should not be assumed to mean macOS supports two normal system pointers. It may instead use an isolated environment, application-level automation, accessibility actions, or a separately rendered agent cursor. Without a primary technical description, the mechanism should be treated as unknown.

Sources:

- [OpenAI Computer Use API guide](https://developers.openai.com/api/docs/guides/tools-computer-use)
- [OpenAI computer-using agent overview](https://openai.com/index/computer-using-agent/)
- [OpenAI CUA sample application](https://github.com/openai/openai-cua-sample-app)

### 2.2 Anthropic Computer Use

Anthropic's Computer Use tool follows the same screenshot loop. The client declares the display dimensions, Claude emits actions with pixel coordinates, and the developer executes them. Anthropic provides the model and action schema, not a privileged desktop-control daemon.

Anthropic emphasizes that the execution environment should be isolated and that destructive or consequential actions should require confirmation. This matters because the model can encounter hostile instructions in web pages, email, documents, and chat messages.

Sources:

- [Anthropic Computer Use documentation](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)
- [Developing a computer-use model](https://www.anthropic.com/news/developing-computer-use)
- [Claude computer-use announcement](https://www.anthropic.com/news/3-5-models-and-computer-use)

### 2.3 Google Gemini Computer Use and Project Mariner

Google's computer-use work applies the same general loop to browsers. A notable design choice in Gemini's API documentation is the use of **normalized coordinates**, commonly represented on a 0–999 scale. The executor scales those values to the current image or viewport. This is more portable across resolutions than hard-coded pixels, although it does not remove visual grounding errors.

Project Mariner was browser-focused, reinforcing an important pattern: browser automation is easier to isolate and make reliable than arbitrary desktop automation. CDP, DOM, and browser accessibility information can be combined with screenshots.

Sources:

- [Gemini Computer Use documentation](https://ai.google.dev/gemini-api/docs/computer-use)
- [Google Project Mariner announcement](https://blog.google/innovation-and-ai/models-and-research/google-deepmind/google-gemini-ai-update-december-2024/)
- [Gemini Computer Use model overview](https://blog.google/innovation-and-ai/models-and-research/google-deepmind/gemini-computer-use-model/)

### 2.4 Lessons from the commercial systems

The reusable strategies are:

1. Keep model output structured and small.
2. Separate model reasoning from privileged execution.
3. Send a fresh observation after actions that can change layout.
4. Use a dedicated sandbox whenever possible.
5. Normalize coordinates or attach them to a specific captured frame.
6. Put approval rules in the executor, not only in the model prompt.
7. Treat everything shown by an application as untrusted data.
8. Prefer semantic application APIs over mouse simulation when available.

---

## 3. Useful open-source projects and what they teach

This is not a ranking by popularity. It groups projects by architecture and relevance.

### 3.1 Browser agents

#### browser-use

Repository: [browser-use/browser-use](https://github.com/browser-use/browser-use)

Strategy:

- Uses browser automation, commonly through Playwright.
- Gives the agent structured browser state as well as visual context.
- Executes browser-specific actions rather than controlling the whole desktop.

Lesson:

- Use the browser's own automation layer whenever a task is on the web. It is more deterministic and can work in a separate browser process without moving the desktop pointer.

#### Skyvern

Repository: [Skyvern-AI/skyvern](https://github.com/Skyvern-AI/skyvern)

Strategy:

- Combines browser automation with LLM/vision decisions.
- Avoids depending entirely on brittle CSS selectors.
- Focuses on workflows such as forms and web operations.

Lesson:

- Pure selectors and pure vision both fail in different ways. A robust browser agent combines them.

#### Playwright and Chrome DevTools Protocol

Sources:

- [Playwright](https://github.com/microsoft/playwright)
- [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/)

Strategy:

- Operate on page elements, tabs, network events, and browser input directly.
- Capture viewport or full-page screenshots without desktop screen capture.
- Create input events inside a browser target without moving the Wayland compositor's cursor.

Lesson:

- CDP/Playwright should be the first-choice action backend for web tasks.

### 3.2 General visual GUI agents

#### UI-TARS

Repositories:

- [bytedance/UI-TARS](https://github.com/bytedance/UI-TARS)
- [bytedance/UI-TARS-desktop](https://github.com/bytedance/UI-TARS-desktop)

Strategy:

- Uses a vision-language model trained to ground actions in screenshots.
- Produces coordinate-based mouse and keyboard actions.
- The model is platform-independent, but each OS still needs an input/capture bridge.

Lesson:

- A strong grounding model helps, but it does not solve operating-system integration, focus conflicts, or safety.

#### OmniParser

Repository: [microsoft/OmniParser](https://github.com/microsoft/OmniParser)

Strategy:

- Converts screenshots into candidate UI elements, bounding boxes, and descriptions.
- Acts as a perception layer for another model rather than a complete agent.

Lesson:

- Pre-parsing a screenshot can reduce the burden on the planning model and make candidate targets explicit. On Linux this can supplement incomplete AT-SPI data.

#### Microsoft UFO

Repository: [microsoft/UFO](https://github.com/microsoft/UFO)

Strategy:

- Combines screenshots with Windows accessibility/UI Automation information.
- Separates higher-level application selection/planning from control inside an application.

Lesson:

- Hierarchical planning and hybrid perception are useful, but its Windows integration is not directly portable to Hyprland.

### 3.3 Demonstration and replay

#### OpenAdapt

Repositories:

- [OpenAdaptAI/OpenAdapt](https://github.com/OpenAdaptAI/OpenAdapt)
- [OpenAdaptAI/openadapt-flow](https://github.com/OpenAdaptAI/openadapt-flow)

Strategy:

- Records a human demonstration.
- Tries to compile the demonstration into a repeatable workflow.
- Uses screenshots and accessibility information to verify replay.

Lesson:

- Repeated workflows should not require open-ended model reasoning every time. Record/compile/replay can be faster, cheaper, and safer, with the model used mainly for recovery.

### 3.4 Sandboxes and evaluation

#### OSWorld

Repositories:

- [xlang-ai/OSWorld](https://github.com/xlang-ai/OSWorld)
- [xlang-ai/OSWorld-V2](https://github.com/xlang-ai/OSWorld-V2)

Strategy:

- Provides real desktop tasks and controlled environments for evaluating agents.
- Measures end results rather than only whether a click looked reasonable.

Lesson:

- Build task-level verification. A successful click is not the same as a successfully completed task.

#### OpenHands

Repository: [OpenHands/OpenHands](https://github.com/OpenHands/OpenHands)

Strategy:

- Uses specialized tools for shell, files, code, and browser work inside an isolated environment.
- Does not force every operation through visual computer use.

Lesson:

- Give the agent the narrowest and most semantic tool for each job. Visual control should be the fallback, not the universal interface.

### 3.5 Linux input tools

#### ydotool

Repository: [ReimuNotMoe/ydotool](https://github.com/ReimuNotMoe/ydotool)

Strategy:

- Creates Linux input events through `uinput`.
- Works below X11/Wayland, so it can work with Hyprland.
- Requires access to `/dev/uinput` and normally a daemon.

Important limitation:

- Hyprland sees a virtual ydotool mouse much like a physical mouse. It moves the same seat cursor and can alter focus.

#### wtype

Repository: [atx/wtype](https://github.com/atx/wtype)

Strategy:

- Sends keyboard input through the Wayland virtual-keyboard protocol.
- Useful for text and key sequences, but not general pointer control.

#### wlr virtual pointer

Protocol: [wlr-virtual-pointer-unstable-v1](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1)

Strategy:

- Lets a trusted client create pointer motion and button events through a compositor protocol.
- Hyprland implements the relevant virtual pointer functionality independently of modern wlroots.

Important limitation:

- A virtual pointer is another input source for a seat. It does not automatically create an independently visible second seat cursor.

#### wayland-automation and Wayland MCP projects

Examples:

- [OTAKUWeBer/Wayland-automation](https://github.com/OTAKUWeBer/Wayland-automation)
- [cheonglol/wayland-mcp](https://github.com/cheonglol/wayland-mcp)

Strategy:

- Wrap screenshot, pointer, keyboard, and compositor-specific functions in an API suitable for automation or an AI tool server.

Lesson:

- They are useful implementation references, but their desktop input generally still affects the user's normal session. Their permissions and maintenance status should be reviewed before relying on them.

---

## 4. Hyprland integration options

### 4.1 Hyprland IPC is the control plane

Hyprland provides two Unix sockets under its runtime directory:

- a command socket used by `hyprctl`;
- an event socket that emits changes such as window creation, focus, title, workspace, and monitor events.

Useful queries include:

- `hyprctl monitors -j`
- `hyprctl clients -j`
- `hyprctl activewindow -j`
- `hyprctl cursorpos`
- `hyprctl workspaces -j`
- `hyprctl devices -j`

Useful dispatchers can focus a window, switch workspaces, move windows, and run compositor operations without pretending to be a mouse.

Recommended use:

- Subscribe to the event socket instead of polling continuously.
- Cache the current monitor/window map.
- Use JSON query output for resynchronization after missed events.
- Batch commands where possible because `hyprctl` calls are synchronous.
- Prefer a dispatcher such as `focuswindow` over clicking a title bar.

Source: [Hyprland IPC documentation](https://wiki.hypr.land/IPC/)

### 4.2 Coordinates and multiple monitors

Hyprland reports windows and the cursor in a global compositor coordinate space. Monitor origins may be negative, and scale factors may differ.

A safe coordinate pipeline should retain:

- output name;
- logical output origin and size;
- output scale and transform;
- capture image dimensions;
- captured region origin;
- target window geometry;
- the exact frame ID or timestamp on which an action was based.

Do not assume that screenshot pixels always equal global logical coordinates. A captured 2× scaled output, transformed output, cropped region, or normalized model coordinate requires conversion.

A robust action should look conceptually like:

```text
captured frame + output metadata
        -> model-local or normalized coordinate
        -> capture pixel coordinate
        -> output logical coordinate
        -> Hyprland global coordinate
```

Before clicking, reject the action if the target frame is stale because focus, workspace, window geometry, or monitor layout changed.

Sources:

- [Hyprland monitor configuration](https://wiki.hypr.land/Configuring/Basics/Monitors/)
- [Using hyprctl](https://wiki.hypr.land/Configuring/Advanced-and-Cool/Using-hyprctl/)

### 4.3 Screen capture

#### grim

Repository: [emersion/grim](https://gitlab.freedesktop.org/emersion/grim)

Good for:

- simple full-output or rectangular captures;
- prototypes;
- one screenshot per action.

Use Hyprland's monitor and active-window geometry to request only the needed region. Smaller stable captures reduce image cost and coordinate ambiguity.

#### PipeWire and xdg-desktop-portal-hyprland

Repository: [hyprwm/xdg-desktop-portal-hyprland](https://github.com/hyprwm/xdg-desktop-portal-hyprland)

Good for:

- a continuous screen stream;
- sandbox-friendly capture;
- selecting a window or output through a user-approved portal flow.

Tradeoff:

- Portal capture is deliberately permission-gated. This is safer, but less convenient for unattended control.

#### Direct compositor protocols

A trusted native client may use supported screencopy protocols. This can reduce process-launch overhead, but it couples the implementation more closely to compositor protocols and permissions.

### 4.4 Keyboard input

Use this preference order:

1. Semantic application action, such as Playwright filling a field.
2. Accessibility action, if reliable.
3. Hyprland dispatcher or application command.
4. Wayland virtual keyboard through a tool such as wtype.
5. `uinput` through ydotool when broader compatibility is needed.

Typing secrets through a model-controlled loop should be avoided. Credentials should be inserted by a trusted local component only after an explicit approval and only into a verified target.

### 4.5 Pointer input

Possible backends:

- application-level events through CDP/Playwright;
- Wayland virtual-pointer protocol;
- `uinput`/ydotool;
- portal RemoteDesktop after user consent.

For desktop-wide control, virtual pointer or uinput is practical, but both can interfere with the user. The executor should therefore support:

- an exclusive automation mode;
- a prominent active indicator;
- immediate pause on physical user input;
- an emergency stop shortcut;
- action rate limits;
- focus and target checks before clicks;
- no automatic destructive actions.

### 4.6 Linux accessibility through AT-SPI

AT-SPI exposes roles, names, states, text, and actions from applications that implement accessibility correctly. It communicates over D-Bus and is independent of Wayland's screen-isolation model.

Useful role in a hybrid agent:

- enumerate controls in the focused application;
- locate a named button or text field;
- read current field values and disabled states;
- invoke semantic actions when supported;
- cross-check screenshot/OCR targets.

Limitations:

- Coverage and quality vary by toolkit and application.
- It is not a complete compositor-wide scene graph.
- Screen geometry can be incomplete or inconsistent.
- XWayland and custom-rendered applications may expose poor trees.
- AT-SPI alone is not a general global input-injection solution.

Sources:

- [AT-SPI2 overview](https://www.freedesktop.org/wiki/Accessibility/AT-SPI2/)
- [at-spi2-core](https://gitlab.gnome.org/GNOME/at-spi2-core)

---

## 5. Can Hyprland have a second background cursor?

The answer depends on what “second cursor” means.

### 5.1 A fake visual cursor

A transparent overlay can draw an agent pointer without moving the real one.

What it can do:

- show where the agent intends to act;
- animate a preview;
- provide an approval UI.

What it cannot do:

- deliver clicks to the application beneath it;
- create independent application focus;
- automate a target by itself.

This is useful as feedback, not as an input mechanism.

### 5.2 A virtual mouse in the current seat

A wlr virtual pointer or uinput device can create pointer events. In the normal Hyprland session those events feed the existing seat and move its real cursor.

Result:

- technically easy enough;
- works across applications;
- interferes with the user;
- can steal focus;
- is not a background cursor.

### 5.3 A true second seat in the same Hyprland session

Wayland models input as seats. A seat groups pointer, keyboard, and touch capabilities and has its own focus. Multiple independent cursors require multiple seats with compositor support and correct device assignment.

Current Hyprland multi-seat support is the blocker. Public Hyprland issue/discussion records describe multi-seat support as requested work rather than a normal supported configuration. A virtual pointer by itself does not create a new independent seat.

Therefore, a reliable second independent cursor in the same current Hyprland desktop should be treated as **not available** unless Hyprland gains and documents full multi-seat support.

Sources:

- [Wayland `wl_seat` protocol](https://wayland.app/protocols/wayland#wl_seat)
- [Hyprland multiple logical seat issue #1731](https://github.com/hyprwm/Hyprland/issues/1731)
- [Hyprland multi-seat discussion #10336](https://github.com/hyprwm/Hyprland/discussions/10336)

### 5.4 Application-level background interaction

Some applications can be controlled without a desktop pointer:

- Chromium through CDP or Playwright;
- applications exposing a local RPC/API;
- accessible controls that support semantic activation;
- terminal applications through commands or a PTY;
- editors through plugins or command sockets.

This is the closest match to “background computer use” inside the same user session. It avoids pointer contention, but only for applications with suitable control interfaces. It may still change application state, so concurrency with the user must be managed.

For browsers, a dedicated browser profile/process is preferable. The agent can own that process while the user keeps using another browser window or profile.

### 5.5 Nested compositor, separate session, or VM

A nested compositor can run as a window inside Hyprland while managing its own clients, seat, focus, and cursor. Weston has a Wayland backend designed for nested operation. Cage and Gamescope may be useful for narrower kiosk or application-isolation cases, depending on input and capture requirements.

Options:

- nested Weston session;
- a kiosk compositor such as Cage for one application;
- Gamescope for an isolated graphical application;
- a separate user Wayland session;
- QEMU/KVM virtual machine;
- remote desktop session attached to a separate graphical environment.

Advantages:

- agent pointer does not move the main Hyprland pointer;
- clear security and focus boundary;
- predictable resolution and scaling;
- easy to record or reset;
- the user can watch the nested display without sharing input focus.

Disadvantages:

- more resource use;
- applications run in a different graphical session;
- clipboard, files, credentials, and notifications need explicit bridging;
- GPU acceleration and sandboxing need careful setup.

Sources:

- [Running Weston](https://wayland.pages.freedesktop.org/weston/toc/running-weston.html)
- [Cage](https://github.com/cage-kiosk/cage)
- [Gamescope](https://github.com/ValveSoftware/gamescope)
- [wayvnc](https://github.com/any1/wayvnc)

### 5.6 Practical conclusion on the reach goal

Ranked by feasibility:

1. **Dedicated browser controlled with CDP/Playwright** — best for web work; no desktop pointer interference.
2. **Nested Weston or separate graphical session** — best general solution for a truly independent visual desktop and pointer.
3. **VM** — strongest isolation and reset behavior, with the highest resource cost.
4. **Semantic per-application adapters** — excellent where APIs exist, but not universal.
5. **Current-seat virtual pointer/uinput** — useful only when the user accepts visible pointer/focus interference.
6. **True second cursor in the same Hyprland seat/session** — not currently a realistic general implementation target.

---

## 6. Recommended architecture for a Hyprland agent

### 6.1 Split the system into clear layers

#### Observer

Collects:

- screenshot or stream frame;
- monitor geometry and scale;
- Hyprland windows, workspace, and focus;
- AT-SPI tree when available;
- browser DOM/accessibility snapshot when applicable;
- recent compositor events.

#### Planner

Receives a compact, privacy-filtered observation and returns a structured intent, not an arbitrary shell command.

Example:

```json
{
  "action": "click",
  "target": {
    "frame_id": 1842,
    "output": "DP-1",
    "x_normalized": 734,
    "y_normalized": 412,
    "label": "Save"
  },
  "risk": "low",
  "expected_result": "The settings dialog closes"
}
```

#### Policy engine

Checks:

- whether the action is allowed;
- whether the frame is still current;
- whether the target application is approved;
- whether physical user activity is occurring;
- whether the action requires confirmation;
- whether text contains secrets;
- whether action and target match the user's instruction.

#### Executor

Chooses the narrowest backend:

1. direct application/API tool;
2. CDP/Playwright;
3. AT-SPI semantic action;
4. Hyprland dispatcher;
5. virtual keyboard/pointer;
6. ydotool/uinput fallback.

#### Verifier

Checks the expected result using fresh state. It should not assume that successful event injection means successful completion.

### 6.2 Use capability-based backends

Rather than one giant “control desktop” function, expose capabilities such as:

- `observe_output`
- `observe_window`
- `list_windows`
- `focus_window`
- `browser_click_element`
- `activate_accessible_control`
- `move_pointer`
- `click_pointer`
- `type_text`
- `press_keys`
- `wait_for_window_event`

This makes permissions and safety easier to reason about. A browser-only task should not automatically receive global uinput access.

### 6.3 Track user activity and avoid races

A main-session agent should pause if a real keyboard or pointer becomes active. Possible signals include libinput/evdev activity, Hyprland focus/cursor events, or a short inactivity lease controlled by the user.

A simple policy:

1. User explicitly starts an automation session.
2. Executor obtains a short exclusive-input lease.
3. Any physical input cancels the lease immediately.
4. Agent stops before its next event.
5. User can resume or move the work to an isolated session.

This does not create two cursors, but it prevents the agent and user from fighting over one cursor.

### 6.4 Prefer normalized targets plus metadata

Normalized coordinates are easier for models, but execution must retain exact image metadata. Store both:

- normalized point for model output;
- original screenshot dimensions and crop;
- global logical coordinate after conversion;
- target window identity and geometry.

Never replay a coordinate against a different frame without re-grounding it.

### 6.5 Use event-driven waits

Avoid fixed sleeps where possible. Wait for:

- a window title or focus event from Hyprland;
- a DOM state through Playwright;
- an accessibility state change;
- a meaningful screenshot difference;
- a network or page lifecycle event.

This improves speed and reduces clicks during animations.

---

## 7. Safety and privacy requirements

A computer-use tool is effectively a privileged local operator. The main risks are larger than ordinary chat or code generation.

### 7.1 Prompt injection

Anything visible in a web page, email, document, terminal, or chat is untrusted. It can tell the model to ignore the user and perform another action.

Defenses:

- Keep user instructions and observed content in separate channels.
- Never treat on-screen text as permission.
- Require approval for sending, deleting, purchasing, publishing, permission changes, and credential use.
- Restrict network destinations and applications where practical.
- Show the user a plain-language action summary before consequential steps.

### 7.2 Secrets

Screenshots can contain passwords, tokens, private messages, account numbers, or personal files.

Defenses:

- Capture only the required window or region.
- Redact known secret fields locally.
- Keep credential insertion in a trusted local component.
- Do not place secret text in model prompts or logs.
- Encrypt or avoid storing screenshots.

### 7.3 Privilege separation

Do not run the planning model process with unrestricted `/dev/uinput`, filesystem, shell, and network access.

Use separate processes:

- an unprivileged observer/planner;
- a small privileged executor with a strict action schema;
- an approval UI owned by the user;
- an audit log that stores action metadata but avoids sensitive screenshot contents.

### 7.4 Destructive actions

Always confirm actions such as:

- sending a message or email;
- deleting or permanently modifying data;
- purchases and financial transfers;
- changing passwords or permissions;
- posting publicly;
- accepting legal terms;
- installing software or granting privileged access.

The confirmation should name the exact target and effect. A generic “allow computer use” prompt is not enough.

---

## 8. Suggested prototype plan

### Phase 1: Safe browser agent

- Launch a dedicated Chromium profile.
- Control it with Playwright/CDP.
- Give the agent DOM/accessibility snapshots and screenshots.
- Add action validation and confirmations.
- Do not grant desktop-wide input access.

This proves the model loop, safety rules, logs, and verification with minimal Hyprland complexity.

### Phase 2: Read-only Hyprland observer

- Subscribe to Hyprland IPC events.
- Read monitor, window, workspace, and focus state.
- Capture a selected output/window with grim.
- Implement and test coordinate conversion.
- Add screenshot redaction and frame IDs.

### Phase 3: Controlled main-session input

- Add wtype for keyboard input.
- Add a virtual-pointer or ydotool backend behind explicit permission.
- Require an exclusive automation mode.
- Pause immediately on physical input.
- Add a visible overlay showing the planned target before clicking.

### Phase 4: Semantic native-app support

- Add AT-SPI inspection.
- Implement semantic activation for applications with reliable accessibility trees.
- Use screenshot parsing only when semantic data is missing.

### Phase 5: Independent graphical environment

- Prototype nested Weston at a fixed resolution.
- Run target applications inside it.
- Capture only that nested display.
- Route agent input only to the nested seat.
- Explicitly bridge files and clipboard rather than sharing everything.

This phase is the practical route to an agent that works visually in the background without moving the user's Hyprland cursor.

---

## 9. Recommended first technical spike

Build a small, non-AI harness before connecting a model:

1. Read `hyprctl monitors -j` and `hyprctl clients -j`.
2. Subscribe to the Hyprland event socket.
3. Capture one selected output and one selected window.
4. Display the captured frame with its exact logical and pixel dimensions.
5. Accept a normalized test point from a local file or CLI argument.
6. Convert it to a global Hyprland coordinate.
7. Draw a harmless preview overlay instead of clicking.
8. Reject the point when the window or frame becomes stale.
9. Add actual click injection only after coordinate tests pass.
10. Repeat the same test inside nested Weston and confirm that the main pointer never moves.

This isolates the difficult OS and coordinate work from model quality. It also creates a reusable executor for different models.

---

## 10. Final recommendations

For this project:

- **Use Hyprland IPC as the source of desktop structure and events.**
- **Use Playwright/CDP for browser tasks.** Do not waste vision-model calls on operations the browser can perform directly.
- **Use AT-SPI as an optional semantic layer for native applications.** Expect incomplete coverage.
- **Use screenshots as a universal fallback and for visual verification.**
- **Use normalized model coordinates, but bind every point to exact capture metadata.**
- **Keep input injection in a small, policy-controlled executor.**
- **Do not promise a second cursor in the current Hyprland session.** A virtual device will normally move the existing cursor.
- **Use a nested compositor or separate session for genuinely independent background computer use.**
- **Prototype with a dedicated browser first, then nested Weston.** These provide the best reliability-to-effort ratio.
- **Treat prompt injection, secrets, and destructive actions as core architecture concerns, not later polish.**

The main strategic lesson from existing systems is not a particular model or input library. It is the combination of:

1. semantic tools where possible;
2. visual grounding where necessary;
3. a small structured action language;
4. strict separation between planning and execution;
5. fresh verification after state changes;
6. isolation when the agent needs uninterrupted control.
