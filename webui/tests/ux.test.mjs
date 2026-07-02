import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const [html, css, js, bridge] = await Promise.all([
  readFile(new URL("../public/index.html", import.meta.url), "utf8"),
  readFile(new URL("../public/styles.css", import.meta.url), "utf8"),
  readFile(new URL("../public/app.js", import.meta.url), "utf8"),
  readFile(new URL("../bridge.mjs", import.meta.url), "utf8"),
]);

const markdownSource = js.slice(js.indexOf("function escapeHtml"), js.indexOf("function enhanceContent"));
const markdownContext = {};
vm.runInNewContext(`${markdownSource};globalThis.render = renderMarkdown`, markdownContext);

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

test("streaming conversations expose reading and recovery controls", () => {
  assert.match(html, /id="jump-latest"/);
  assert.match(js, /function showRetry/);
  assert.match(js, /function enhanceContent/);
  assert.match(css, /\.approval \{ position: sticky/);
});

test("chat rendering supports rich markdown and highlighted code", () => {
  assert.match(js, /function safeHref/);
  assert.match(js, /function highlightCode/);
  assert.match(js, /data-language=/);
  assert.match(js, /class="task-item"/);
  assert.match(js, /loading="lazy"/);
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
  assert.match(js, /const hunk = raw\.match/);
});
