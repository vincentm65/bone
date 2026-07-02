import assert from "node:assert/strict";
import test from "node:test";
import { DraftStore, buildSubmission } from "../public/ui-core.js";
import { artifactText, parseDiff } from "../public/canvas-core.js";

class MemoryStorage {
  data = new Map();
  getItem(key) { return this.data.get(key) ?? null; }
  setItem(key, value) { this.data.set(key, String(value)); }
  removeItem(key) { this.data.delete(key); }
}

test("drafts persist independently by conversation and can move from a new chat", () => {
  const drafts = new DraftStore(new MemoryStorage());
  drafts.set(null, "new draft"); drafts.set(7, "existing draft");
  assert.equal(drafts.get(null), "new draft");
  assert.equal(drafts.get(7), "existing draft");
  drafts.move(null, 8);
  assert.equal(drafts.get(null), "");
  assert.equal(drafts.get(8), "new draft");
});

test("submission maps images and embeds text attachments", () => {
  const result = buildSubmission("Review these", [
    { kind: "image", media_type: "image/png", data: "abc" },
    { kind: "text", name: "sample.rs", text: "fn main() {}" },
  ]);
  assert.deepEqual(result.images, [{ media_type: "image/png", data: "abc" }]);
  assert.match(result.text, /<attached_file name="sample.rs">/);
  assert.match(result.text, /fn main\(\) \{\}/);
});

test("attachment names cannot inject attachment markup", () => {
  const result = buildSubmission("", [{ kind: "text", name: '"><bad>', text: "safe" }]);
  assert.doesNotMatch(result.text, /name=""><bad>"/);
});

test("canvas diff parsing supports unified diffs and plain text export", () => {
  const parsed = parseDiff("@@ -1,2 +1,2 @@\n-old\n+new\n same");
  assert.equal(parsed.add, 1); assert.equal(parsed.del, 1);
  assert.deepEqual(parsed.lines.map((line) => line.type), ["del", "add", "ctx"]);
  assert.equal(artifactText({ kind: "diff", lines: parsed.lines }), "old\nnew\nsame");
  assert.equal(artifactText({ kind: "file", content: "hello" }), "hello");
});
