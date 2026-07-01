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

test("streaming conversations expose reading and recovery controls", () => {
  assert.match(html, /id="jump-latest"/);
  assert.match(js, /function showRetry/);
  assert.match(js, /function enhanceContent/);
  assert.match(css, /\.approval \{ position: sticky/);
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
