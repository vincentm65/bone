import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";
import { renderMarkdown } from "../public/markdown.js";

const [html, css, js, bridge, markdown, canvasCore] = await Promise.all([
  readFile(new URL("../public/index.html", import.meta.url), "utf8"),
  readFile(new URL("../public/styles.css", import.meta.url), "utf8"),
  readFile(new URL("../public/app.js", import.meta.url), "utf8"),
  readFile(new URL("../bridge.mjs", import.meta.url), "utf8"),
  readFile(new URL("../public/markdown.js", import.meta.url), "utf8"),
  readFile(new URL("../public/canvas-core.js", import.meta.url), "utf8"),
]);

const markdownContext = { render: renderMarkdown };

test("dialogs expose modal semantics and managed focus", () => {
  assert.match(html, /role="dialog" aria-modal="true"/);
  assert.match(js, /function trapDialogFocus/);
  assert.match(js, /dialogReturnFocus/);
});

test("mobile navigation behaves as a dismissible drawer", () => {
  assert.match(html, /id="sidebar-backdrop"/);
  assert.match(css, /mobile-sidebar-open/);
  assert.match(js, /closeMobileSidebar\(\)/);
});

test("sidebar is drag-resizable with a persisted, clamped width", () => {
  assert.match(html, /id="sidebar-resize"/);
  assert.match(css, /--sidebar-w:/);
  assert.match(css, /#sidebar \{[^}]*width: var\(--sidebar-w\)/);
  assert.match(js, /function clampSidebarW/);
  assert.match(js, /prefs\.sidebarW/);
  assert.match(js, /setProperty\("--sidebar-w"/);
});

test("ask_user interact pane renders and maps keys to the runtime", () => {
  assert.match(html, /id="interact"/);
  assert.match(html, /id="interact-options"/);
  assert.match(html, /id="interact-kicker"/);
  // The interact pane (source="interact") is rendered, not ignored.
  assert.match(js, /comp\.id === "interact"/);
  assert.match(js, /function parseInteractPane/);
  // Browser keys are translated to the runtime's crossterm-style code names.
  assert.match(js, /ArrowUp: "Up"/);
  assert.match(js, /Escape: "Esc"/);
  // Clicks drain through a key queue, one reply per key_request.
  assert.match(js, /function pumpKeyQueue/);
  assert.match(js, /interactState\.queue/);
  assert.match(css, /\.interact-card \{/);
});

test("ask_user parses option descriptions without consuming notices", () => {
  const source = js.slice(js.indexOf("function parseInteractPane"),
    js.indexOf("function renderInteractPane"));
  const context = {};
  vm.runInNewContext(`${source};globalThis.parse = parseInteractPane`, context);
  const model = context.parse({
    title: "Question 2 of 4",
    lines: [
      "Choose carefully",
      " > [x] Alpha",
      "     First description",
      "   [ ] Beta",
      "     Second description",
      "Select at least one option.",
      "↑↓ move · Enter submit · Esc cancel",
    ],
  });
  assert.equal(model.title, "Question 2 of 4");
  assert.equal(model.question, "Choose carefully");
  assert.equal(model.options[0].description, "First description");
  assert.equal(model.options[1].description, "Second description");
  assert.equal(model.notice, "Select at least one option.");
  assert.match(js, /\$\("interact-kicker"\)\.textContent = model\.title \|\| "Question"/);
  assert.match(js, /el\("span", "interact-opt-description"\)/);
  assert.match(css, /\.interact-opt-description \{/);
  assert.match(css, /\.interact-opt-copy \{/);
});

test("streaming conversations expose reading and recovery controls", () => {
  assert.match(html, /id="jump-latest"/);
  assert.match(js, /function showRetry/);
  assert.match(js, /function enhanceContent/);
  assert.match(css, /\.approval \{ position: sticky/);
});

test("chat rendering supports rich markdown and highlighted code", () => {
  assert.match(markdown, /function safeHref/);
  assert.match(markdown, /function highlightCode/);
  assert.match(markdown, /data-language=/);
  assert.match(markdown, /class="task-item"/);
  assert.match(markdown, /loading="lazy"/);
  assert.match(css, /\.tok-keyword/);
  assert.match(css, /\.code-language/);
  assert.match(css, /\.prose \.task-item/);

  const rendered = markdownContext.render("- [x] done\n\n~~old~~ and [safe](https://example.com)\n\n```js\nconst n = 42; // note\n```");
  assert.match(rendered, /class="task-item"/);
  assert.match(rendered, /<del>old<\/del>/);
  assert.match(rendered, /tok-keyword/);
  assert.match(rendered, /tok-number/);
  assert.match(rendered, /tok-comment/);
  assert.match(rendered, /data-language="js"/);
  assert.equal(markdownContext.render("[bad](javascript:alert(1))").includes('href="javascript:'), false);
  assert.equal(markdownContext.render("first\nsecond"), "<p>first<br/>second</p>");
  assert.equal(markdownContext.render("first<br>second<br />third"), "<p>first<br/>second<br/>third</p>");
  assert.equal(markdownContext.render("`<br>`"), "<p><code>&lt;br&gt;</code></p>");
  assert.match(markdownContext.render("<script>alert(1)</script>"), /&lt;script&gt;/);
});

test("thinking states are simple, animated, and motion-safe", () => {
  assert.match(js, /thinking-spinner/);
  assert.match(js, /setAttribute\("aria-label", "Thinking"\)/);
  assert.match(js, /<span>Thinking…<\/span>/);
  assert.match(js, /\^thinking\(\?:/);
  assert.doesNotMatch(js, /thinkingTimer/);
  assert.match(css, /@keyframes think-spin/);
  assert.match(css, /prefers-reduced-motion/);
  assert.match(css, /\.reasoning-preview/);
});

test("multiplexed chats retain and replay each in-flight turn", () => {
  assert.match(js, /const liveEventCache = new Map\(\)/);
  assert.match(js, /cacheLiveEvent\(convId, ev\)/);
  assert.match(js, /cacheLiveEvent\(state\.awaitingLoad\.from, ev\)/);
  assert.match(js, /replayLiveTail\(state\.conversationId\)/);
  assert.match(js, /liveEventCache\.delete\(convId\)/);
  assert.doesNotMatch(js, /its text\/tools are deliberately ignored/);
  assert.match(bridge, /kind: "watch", conversation_id: convId/);
  assert.match(bridge, /snapshot\.conversation_id === convId/);
  assert.match(js, /await watchConversation\(leaving\)/);
});

test("conversation management preserves transcript content", () => {
  assert.match(bridge, /CREATE TABLE IF NOT EXISTS webui_conversations/);
  assert.match(bridge, /COALESCE\(meta\.title, first_user\.content\)/);
  assert.doesNotMatch(bridge, /UPDATE messages SET content/);
  assert.match(js, /function renameConversation/);
  assert.match(js, /function archiveConversation/);
});

test("primary dynamic controls use native buttons", () => {
  assert.match(js, /el\("button", "chat-item"\)/);
  assert.match(js, /el\("button", "provider-row"\)/);
  assert.match(js, /el\("button", "suggestion"/);
  assert.match(css, /:focus-visible/);
});

test("subagent calls render as agent cards with live per-task status", () => {
  // Dedicated card path for the runtime's `subagent` tool.
  assert.match(js, /name === "subagent"/);
  assert.match(js, /function buildAgentRows/);
  assert.match(js, /function applySubagentResult/);
  // Background dispatches resolve when the daemon injects the results turn.
  assert.match(js, /function resolveBackgroundAgents/);
  assert.match(js, /!state\.sending && !replayingLiveEvents/);
  // Persisted injected results replay as a compact card, not a "You" bubble.
  assert.match(js, /BG_RESULTS_PREFIX/);
  assert.match(js, /function jobResultsCard/);
  // Registered agents surface in settings, parsed from the tool description.
  assert.match(js, /function registeredAgents/);
  assert.match(css, /\.agent-row \{/);
  assert.match(css, /\.tool-status\.bg/);

  // subagentSummary is pure — exercise it directly.
  const source = js.slice(js.indexOf("function subagentSummary"), js.indexOf("// One status row"));
  const context = {};
  vm.runInNewContext(`${source};globalThis.summary = subagentSummary`, context);
  assert.equal(context.summary({ action: "dispatch", tasks: [{}, {}] }), "dispatch · 2 tasks · background");
  assert.equal(context.summary({ action: "dispatch", tasks: [{}], wait: true }), "dispatch · 1 task");
  assert.equal(context.summary({ action: "wait", ids: ["job-1"] }), "wait · job-1");
  assert.equal(context.summary({}), "status");
});

test("edit canvas only exposes captured diffs and can show all edits", () => {
  assert.match(html, /id="canvas-all"/);
  assert.match(js, /path && captureDiff\(path, content\)/);
  assert.match(js, /function showAllEdits/);
  assert.match(canvasCore, /const hunk = raw\.match/);
});

test("composer supports persistent drafts and accessible attachments", () => {
  assert.match(html, /id="attachment-input"[^>]*multiple/);
  assert.match(html, /id="attachment-button"[^>]*aria-label=/);
  assert.match(js, /new DraftStore\(localStorage\)/);
  assert.match(js, /addEventListener\("paste"/);
  assert.match(js, /addEventListener\("drop"/);
  assert.match(js, /buildSubmission\(text, attachments\)/);
});

test("composer exposes capability-aware slash commands", () => {
  assert.match(html, /id="command-menu"[^>]*role="listbox"/);
  assert.match(html, /id="command-button"[^>]*aria-label="Open command menu"/);
  assert.match(js, /if \(Array\.isArray\(ev\.commands\)\) state\.commands = ev\.commands/);
  assert.match(js, /const NATIVE_COMMANDS = new Map/);
  assert.match(js, /const HIDDEN_COMMANDS = new Set/);
  assert.match(js, /\.sort\(\(a, b\) => a\.name\.localeCompare\(b\.name\)\)/);
  assert.match(js, /run_command: \{ name: command\.name, input: command\.input \}/);
  assert.match(js, /if \(!exact && highlighted\)/);
  assert.match(js, /if \(commands\[state\.commandIndex\]\).*selectCommand/s);
  assert.match(js, /case "command_complete": return onCommandComplete/);
  assert.match(css, /\.command-option\.active/);
});

test("canvas exposes search, download, full-file loading, and keyboard resizing", () => {
  assert.match(html, /id="divider"[^>]*role="separator"[^>]*tabindex="0"/);
  assert.match(html, /id="canvas-search"/);
  assert.match(html, /id="canvas-download"/);
  assert.match(html, /id="canvas-full-file"/);
  assert.match(html, /id="canvas-editor"/);
  assert.match(js, /function updateCanvasSearch/);
  assert.match(js, /function loadFullArtifact/);
  assert.match(js, /prefs\.canvasW/);
  assert.match(js, /\$\("divider"\)\.addEventListener\("keydown"/);
});

test("settings tabs expose selected state", () => {
  assert.match(html, /role="tab" aria-selected="true"/);
  assert.match(js, /setAttribute\("aria-selected", active\)/);
  assert.match(js, /e\.key !== "ArrowLeft" && e\.key !== "ArrowRight"/);
});

test("recoverable data failures expose a persistent retry action", () => {
  assert.match(html, /id="global-error"[^>]*role="alert"/);
  assert.match(html, /id="global-error-retry"/);
  assert.match(js, /function reportError/);
  assert.match(js, /reportError\("Could not load conversations"/);
});

test("renderTaskList escapes agent-controlled task text (no HTML injection)", () => {
  // Build minimal DOM stubs for the elements renderTaskList touches.
  const label = { textContent: "", innerHTML: "", childNodes: [], appendChild(c) { this.childNodes.push(c); return c; }, querySelector() { return null; } };
  const wrap = { classList: { add() {}, remove() {}, toggle() {}, contains() {} } };
  const collapsed = {};
  const titleEl = { textContent: "" };
  const countEl = { textContent: "" };
  const itemsEl = { innerHTML: "", childNodes: [], appendChild(c) { this.childNodes.push(c); } };
  const expanded = { querySelector: (s) => (s === ".task-list-title" ? titleEl : countEl) };

  const byId = new Map([
    ["task-popup-label", label],
    ["task-popup-wrap", wrap],
    ["task-popup-collapsed", collapsed],
    ["task-popup-expanded", expanded],
    ["task-list-items", itemsEl],
  ]);

  // el() that tracks querySelector-able children so expanded rows work.
  function elStub(tag, cls, html) {
    const el = { className: cls || "", textContent: "", childNodes: [], classList: { add() {}, remove() {}, toggle() {}, contains: () => cls && cls.includes("open") }, dataset: {}, querySelector(s) { return this.childNodes.find((c) => c.className === s.slice(1)) || null; }, appendChild(c) { this.childNodes.push(c); return c; } };
    let markup = "";
    Object.defineProperty(el, "innerHTML", {
      get() { return markup; },
      set(value) {
        markup = value;
        this.childNodes = [];
        for (const [, cn, text] of value.matchAll(/<span class="([^"]*)">(.*?)<\/span>/g)) {
          this.childNodes.push({ className: cn, textContent: text, tagName: "SPAN" });
        }
      },
    });
    el.innerHTML = html || "";
    return el;
  }

  const fnSrc = js.slice(js.indexOf("function renderTaskList"),
    js.indexOf("function toggleTaskPopup")) + ";renderTaskList();";

  const ctx = vm.createContext({
    taskState: {
      active: true,
      title: "Test",
      items: [
        { text: '<img src=x onerror=alert(1)>', status: "in_progress" },
        { text: "safe task", status: "pending" },
      ],
      expanded: false,
    },
    document: { createElement: (tag) => elStub(tag) },
    $: (id) => byId.get(id),
    el: elStub,
  });

  vm.runInContext(fnSrc, ctx);

  // The collapsed label must store agent-controlled content as text, not HTML.
  assert.equal(label.textContent, '<img src=x onerror=alert(1)>',
    "agent-controlled text must be assigned through textContent");

  // The progress span must exist as a DOM child (preserves layout behavior).
  assert.equal(label.childNodes.length, 1,
    "label must contain one child (the progress span)");
  assert.equal(label.childNodes[0].className, "task-progress");
  assert.equal(label.childNodes[0].textContent, " 1/2");

  // No HTML-injection surface in the expanded list either.
  assert.equal(itemsEl.childNodes.length, 2, "both tasks rendered");
  assert.equal(itemsEl.childNodes[0].querySelector(".task-text").textContent, "<img src=x onerror=alert(1)>",
    "task text in expanded list must be safe text");
  assert.equal(itemsEl.childNodes[1].querySelector(".task-text").textContent, "safe task",
    "second task text must be safe text");
});

test("stats charts have keyboard-accessible spoken values", () => {
  assert.match(js, /tabindex="0" role="img" aria-label=/);
});

test("restart-daemon kills only tracked child process, not arbitrary port listeners", () => {
  // The fuser-based port-kill that kills ANY process on the daemon port is removed.
  assert.doesNotMatch(bridge, /fuser/);
  assert.doesNotMatch(bridge, /spawn\("fuser"/);
  // restartDaemon checks daemonProc before killing — returns early if untracked.
  assert.match(bridge, /if \(!daemonProc\)/);
  assert.match(bridge, /return false/);
  // The /api/restart-daemon handler returns 503 with a clear error when untracked,
  // and the client preserves recovery UI when that request fails.
  assert.match(bridge, /res\.writeHead\(503/);
  assert.match(bridge, /daemon not managed by this bridge/);
  assert.match(js, /if \(!response\.ok\)/);
  assert.match(js, /body\.error \|\| "engine could not be restarted"/);
  // Self-healing is preserved for tracked daemons (setTimeout + ensureDaemon).
  assert.match(bridge, /setTimeout\(ensureDaemon, 700\)/);
  assert.match(bridge, /return true/);
});
