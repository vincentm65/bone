import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [html, css, js, bridge] = await Promise.all([
  readFile(new URL("../public/index.html", import.meta.url), "utf8"),
  readFile(new URL("../public/styles.css", import.meta.url), "utf8"),
  readFile(new URL("../public/app.js", import.meta.url), "utf8"),
  readFile(new URL("../bridge.mjs", import.meta.url), "utf8"),
]);

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

test("edit canvas only exposes captured diffs and can show all edits", () => {
  assert.match(html, /id="canvas-all"/);
  assert.match(js, /path && captureDiff\(path, content\)/);
  assert.match(js, /function showAllEdits/);
  assert.match(js, /const hunk = raw\.match/);
});
