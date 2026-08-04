import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import test from "node:test";
import { promisify } from "node:util";
import vm from "node:vm";
import { isSafeImageUrl, isSafeLinkUrl } from "../public/markdown.js";

const execFileAsync = promisify(execFile);

const [html, css, js, bridge, markdown, canvasCore, dompurify] = await Promise.all([
  readFile(new URL("../public/index.html", import.meta.url), "utf8"),
  readFile(new URL("../public/styles.css", import.meta.url), "utf8"),
  readFile(new URL("../public/app.js", import.meta.url), "utf8"),
  readFile(new URL("../bridge.mjs", import.meta.url), "utf8"),
  readFile(new URL("../public/markdown.js", import.meta.url), "utf8"),
  readFile(new URL("../public/canvas-core.js", import.meta.url), "utf8"),
  readFile(new URL("../public/dompurify-3.4.12.min.js", import.meta.url), "utf8"),
]);

async function renderMarkdownInBrowser(inputs, t) {
  const encodedInputs = encodeURIComponent(JSON.stringify(inputs));
  const page = `<!doctype html><title>pending</title>
    <script src="/dompurify.js"></script>
    <script type="module">
      import { renderMarkdown } from "/markdown.js";
      const inputs = JSON.parse(decodeURIComponent("${encodedInputs}"));
      document.title = encodeURIComponent(JSON.stringify(inputs.map((input) => renderMarkdown(input))));
    </script>`;
  const server = createServer((request, response) => {
    if (request.url === "/dompurify.js") {
      response.writeHead(200, { "content-type": "text/javascript" });
      response.end(dompurify);
    } else if (request.url === "/markdown.js") {
      response.writeHead(200, { "content-type": "text/javascript" });
      response.end(markdown);
    } else {
      response.writeHead(200, { "content-type": "text/html" });
      response.end(page);
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  try {
    const { port } = server.address();
    let stdout;
    try {
      ({ stdout } = await execFileAsync("chromium", [
        "--headless=new", "--no-sandbox", "--disable-gpu", "--disable-background-networking",
        "--virtual-time-budget=1000", "--dump-dom", `http://127.0.0.1:${port}/`,
      ], { maxBuffer: 2 * 1024 * 1024 }));
    } catch (error) {
      if (error.code === "ENOENT") {
        t.skip("chromium is not installed");
        return null;
      }
      throw error;
    }
    const title = stdout.match(/<title>([^<]*)<\/title>/)?.[1];
    assert.ok(title && title !== "pending", "browser completed Markdown rendering");
    return JSON.parse(decodeURIComponent(title));
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

test("daemon config response remains canonical", async () => {
  const source = bridge.slice(
    bridge.indexOf("async function getConfigFromDaemon"),
    bridge.indexOf("async function sendJson"),
  );
  const canonical = {
    schema: {
      pages: [{
        namespace: "general",
        fields: [
          { key: "approval", path: "general.approval", type: "enum" },
          { key: "show_reasoning", path: "general.show_reasoning", type: "bool" },
        ],
        pages: [],
      }],
    },
    snapshot: {
      revision: 1,
      values: { general: { approval: "danger", show_reasoning: true } },
      disabled_tools: [],
    },
  };
  const context = { daemonConfigCommand: async () => canonical };
  vm.runInNewContext(`${source};globalThis.load = getConfigFromDaemon`, context);

  assert.deepEqual(await context.load(), canonical);
  assert.doesNotMatch(source, /approval_mode|show_thinking|toolsDisabled/);
});

test("web config resolves recursive canonical paths with snapshot precedence", () => {
  const source = js.slice(
    js.indexOf("let configCache"),
    js.indexOf("function syncConfigState"),
  );
  const context = {};
  vm.runInNewContext(`${source};globalThis.setConfig = (value) => { configCache = value; }; globalThis.lookup = findField; globalThis.value = configValue`, context);
  context.setConfig({
    schema: { pages: [{ namespace: "extensions", pages: [{ namespace: "demo", fields: [
      { path: "extensions.demo.count", key: "count", type: "number", value: 5, default: 3 },
      { path: "extensions.demo.label", key: "label", type: "string", default: "fallback" },
    ] }] }] },
    snapshot: { revision: 9, values: { extensions: { demo: { count: 8 } } }, disabled_tools: ["shell"] },
  });

  assert.equal(context.value(context.lookup("extensions.demo.count")), 8);
  assert.equal(context.value(context.lookup("extensions.demo.label")), "fallback");
  assert.match(js, /expected_revision: configCache\.snapshot\.revision/);
  assert.match(js, /configCache\.snapshot\?\.disabled_tools/);
  assert.doesNotMatch(js, /["']approval_mode["']|["']show_thinking["']|toolsDisabled|set_setting/);
});

test("concurrent config mutations resolve only their correlated response", async () => {
  const source = bridge.slice(
    bridge.indexOf("async function daemonConfigCommand"),
    bridge.indexOf("async function daemonConfigSnapshot"),
  );
  const links = [];
  let nextId = 0;
  const context = {
    Error,
    clearTimeout,
    setTimeout,
    randomUUID: () => `request-${++nextId}`,
    createDaemonLink(onLine, onStatus) {
      const link = {
        command: null,
        closed: false,
        close() { this.closed = true; },
        write(command) { this.command = command; return true; },
        emit(event) { onLine(JSON.stringify(event)); },
      };
      links.push(link);
      queueMicrotask(() => onStatus("connected"));
      return link;
    },
  };
  vm.runInNewContext(`${source};globalThis.send = daemonConfigCommand`, context);

  let firstSettled = false;
  const first = context.send({ set_tool_enabled: { name: "shell", enabled: false } })
    .finally(() => { firstSettled = true; });
  const second = context.send({ set_tool_enabled: { name: "shell", enabled: true } });
  await new Promise(queueMicrotask);

  const firstId = links[0].command.set_tool_enabled.request_id;
  const secondId = links[1].command.set_tool_enabled.request_id;
  assert.notEqual(firstId, secondId);
  links[0].emit({ config_changed: { request_id: secondId, revision: 2 } });
  await new Promise(queueMicrotask);
  assert.equal(firstSettled, false);

  links[1].emit({ config_changed: { request_id: secondId, revision: 2 } });
  links[0].emit({ config_changed: { request_id: firstId, revision: 3 } });
  assert.equal((await second).request_id, secondId);
  assert.equal((await first).request_id, firstId);
  assert.equal(links[0].closed, true);
  assert.equal(links[1].closed, true);
});

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

test("ask_user updates stable controls instead of rebuilding the picker", () => {
  const renderSource = js.slice(js.indexOf("function setInteractText"), js.indexOf("function closeInteract"));
  assert.match(html, /id="interact-notice"/);
  assert.match(html, /id="interact-cancel"/);
  assert.match(html, /id="interact-submit"/);
  assert.match(renderSource, /const existing = new Map/);
  assert.match(renderSource, /interactState\.optionCache\.set/);
  assert.match(renderSource, /for \(let index = 0; index < interactState\.total;\)/);
  assert.match(renderSource, /opts\.children\[index\] !== node/);
  assert.match(renderSource, /content\.dataset\.signature === signature/);
  assert.doesNotMatch(renderSource, /opts\.innerHTML\s*=/);
  assert.doesNotMatch(css, /\.interact-opt[^\n{]*\{[^}]*animation:/);
});

test("ask_user parses option descriptions without consuming notices", () => {
  const source = js.slice(js.indexOf("function splitInteractLine"),
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
  assert.match(js, /setInteractText\(\$\("interact-kicker"\), model\.title \|\| "Question"\)/);
  assert.match(js, /el\("span", "interact-opt-description"\)/);
  assert.match(css, /\.interact-opt-description \{/);
  assert.match(css, /\.interact-opt-copy \{/);
});

test("ask_user preview panes preserve diagrams and styled spans", () => {
  const source = js.slice(js.indexOf("function splitInteractLine"),
    js.indexOf("function renderInteractPane"));
  const context = {};
  vm.runInNewContext(`${source};globalThis.parse = parseInteractPane`, context);
  const model = context.parse({
    title: "Menu",
    lines: [
      "Choose an architecture",
      { spans: [
        { text: " > Sessions" }, { text: "                 " }, { text: " ┃ " },
        { text: "Architecture", fg: "white", modifiers: ["bold"] },
      ] },
      { spans: [
        { text: "   Tokens" }, { text: "                   " }, { text: " ┃ " },
        { text: "Browser  ──▶  API", fg: "#78B373" },
      ] },
      { spans: [
        { text: "                             " }, { text: " ┃ " },
        { text: "   │             │", fg: "gray" },
      ] },
      "    ↑ 2 more · ↓ 5 more",
      "↑↓/j/k move · Tab switch pane · Enter select · Esc cancel",
    ],
  });
  assert.equal(model.options.length, 2);
  assert.equal(model.options[0].label, "Sessions");
  assert.equal(model.preview.title, "Architecture");
  assert.equal(model.preview.lines[0][0].text, "Browser  ──▶  API");
  assert.equal(model.preview.lines[0][0].fg, "#78B373");
  assert.equal(model.preview.lines[1][0].text, "   │             │");
  assert.equal(model.scrollAbove, 2);
  assert.equal(model.scrollBelow, 5);
  assert.match(html, /id="interact-preview"/);
  assert.match(css, /\.interact-body\.previewing/);
  assert.match(css, /white-space: pre/);
});

test("streaming conversations expose reading and recovery controls", () => {
  assert.match(html, /id="jump-latest"/);
  assert.match(js, /function showRetry/);
  assert.match(js, /function enhanceContent/);
  assert.match(css, /\.approval \{ position: sticky/);
});

test("chat rendering supports Markdown and escapes raw HTML", async (t) => {
  assert.match(html, /dompurify-3\.4\.12\.min\.js[^]*type="module" src="\/app\.js"/);
  assert.match(css, /\.tok-keyword/);

  const [rendered, inlineHtml, blockHtml] = await renderMarkdownInBrowser([
    "- [x] done\n\n~~old~~ and [safe](https://example.com)\n\n```js\nconst n = 42; // note\n```",
    "Inline <kbd>Ctrl</kbd> and <mark>text</mark>.",
    "<section><h4>Title</h4><p>Body</p></section>",
  ], t) || [];
  if (!rendered) return;
  assert.match(rendered, /class="task-item"/);
  assert.match(rendered, /<del>old<\/del>/);
  assert.match(rendered, /tok-keyword/);
  assert.match(rendered, /tok-number/);
  assert.match(rendered, /tok-comment/);
  assert.match(rendered, /data-language="js"/);
  assert.match(rendered, /href="https:\/\/example\.com"/);
  assert.match(rendered, /target="_blank"/);
  assert.match(rendered, /rel="noopener noreferrer"/);
  assert.equal(inlineHtml, "<p>Inline &lt;kbd&gt;Ctrl&lt;/kbd&gt; and &lt;mark&gt;text&lt;/mark&gt;.</p>");
  assert.equal(blockHtml, "<p>&lt;section&gt;&lt;h4&gt;Title&lt;/h4&gt;&lt;p&gt;Body&lt;/p&gt;&lt;/section&gt;</p>");
});

test("Markdown resources reject unsafe URLs", async (t) => {
  assert.equal(isSafeLinkUrl("javascript:alert(1)"), false);
  assert.equal(isSafeLinkUrl("java\nscript:alert(1)"), false);
  assert.equal(isSafeLinkUrl("//evil.example/path"), false);
  assert.equal(isSafeLinkUrl("https://example.com"), true);
  assert.equal(isSafeLinkUrl("/local"), true);
  assert.equal(isSafeImageUrl("http://example.com/image.png"), false);
  assert.equal(isSafeImageUrl("data:image/png;base64,AAAA"), false);
  assert.equal(isSafeImageUrl("https://example.com/image.png"), true);

  const [rendered] = await renderMarkdownInBrowser([
    `[bad](javascript:alert) [good](https://example.com/ok)\n\n![bad](http://example.com/insecure.png) ![good](https://example.com/safe.png)`,
  ], t) || [];
  if (!rendered) return;
  assert.match(rendered, /<a>bad<\/a>/);
  assert.match(rendered, /<a href="https:\/\/example\.com\/ok" target="_blank" rel="noopener noreferrer">good<\/a>/);
  assert.doesNotMatch(rendered, /http:\/\/example\.com\/insecure/);
  assert.match(rendered, /<img src="https:\/\/example\.com\/safe\.png" alt="good" loading="lazy" decoding="async" referrerpolicy="no-referrer">/);
});

test("streaming is frame-buffered text before final sanitized rendering", async (t) => {
  const appendSource = js.slice(js.indexOf("function appendText"), js.indexOf("function appendReasoning"));
  assert.match(appendSource, /requestAnimationFrame/);
  assert.match(appendSource, /replaceChildren\(document\.createTextNode\(state\.asstRaw\), caret\)/);
  assert.doesNotMatch(appendSource, /renderMarkdown|innerHTML/);
  assert.match(js, /function flushAssistantMarkdown\(enhance = false\)[^]*state\.asstEl\.innerHTML = renderMarkdown\(state\.asstRaw\)/);
  assert.match(js, /function cancelAssistantFrame\(\)[^]*cancelAnimationFrame\(state\.asstFrame\)/);
  assert.match(js, /function onFinished\(\)[^]*flushAssistantMarkdown\(true\)/);
  assert.match(js, /function onToolCall\(ev\)[^]*flushAssistantMarkdown\(\)/);
  assert.match(css, /\.prose\.streaming \{[^}]*white-space: pre-wrap/);
  assert.match(js, /renderStoredMessage[^]*el\("div", "prose", renderMarkdown\(m\.content\)\)/);
  const outputs = await renderMarkdownInBrowser([
    "<scr",
    "<script>alert(1)",
    "<script>alert(1)</script><p>safe</p>",
  ], t);
  if (!outputs) return;
  assert.equal(outputs[0], "<p>&lt;scr</p>");
  assert.equal(outputs[1], "<p>&lt;script&gt;alert(1)</p>");
  assert.equal(outputs[2], "<p>&lt;script&gt;alert(1)&lt;/script&gt;&lt;p&gt;safe&lt;/p&gt;</p>");
});

test("streaming batches deltas and cancels stale frames on flush", () => {
  const source = js.slice(js.indexOf("function cancelAssistantFrame"), js.indexOf("function appendReasoning"));
  const frames = new Map();
  const classes = new Set();
  let nextFrame = 0;
  let enhanced = 0;
  const parent = { appendChild() {} };
  const asstEl = {
    classList: { add: (name) => classes.add(name), remove: (name) => classes.delete(name) },
    parentElement: parent,
    replaceChildren(...children) { this.children = children; },
    innerHTML: "",
  };
  const context = {
    state: { asstEl, asstRaw: "", asstFrame: null, reasonDetails: null, reasonEl: null },
    requestAnimationFrame(callback) {
      const id = ++nextFrame;
      frames.set(id, () => { frames.delete(id); callback(); });
      return id;
    },
    cancelAnimationFrame(id) { frames.delete(id); },
    document: { createTextNode: (text) => ({ text }) },
    el: (tag, cls) => ({ tag, className: cls }),
    hideThinking() {},
    ensureAssistant() {},
    scrollDown() {},
    renderMarkdown: (text) => `<p>${text}</p>`,
    enhanceContent: () => { enhanced++; },
  };
  vm.runInNewContext(`${source};globalThis.append = appendText;globalThis.flush = flushAssistantMarkdown`, context);

  context.append("<di");
  context.append("v>");
  assert.equal(frames.size, 1);
  assert.equal(asstEl.children, undefined);
  frames.get(context.state.asstFrame)();
  assert.equal(asstEl.children[0].text, "<div>");
  assert.equal(asstEl.children[1].className, "caret");
  assert.equal(classes.has("streaming"), true);

  context.append("tail");
  assert.equal(frames.size, 1);
  context.flush(true);
  assert.equal(frames.size, 0);
  assert.equal(asstEl.innerHTML, "<p><div>tail</p>");
  assert.equal(classes.has("streaming"), false);
  assert.equal(enhanced, 1);
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

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

function navigationHarness() {
  const route = js.slice(js.indexOf("function routeConversation"), js.indexOf("function recoverNavigation"));
  const open = js.slice(js.indexOf("async function openChat"), js.indexOf("function highlightActiveChat"));
  const fresh = js.slice(js.indexOf("async function newChat"), js.indexOf("// ── providers"));
  const commands = [];
  const watches = [];
  const storage = new Map();
  const state = {
    awaitingLoad: null,
    conversationId: 1,
    draftChat: false,
    navigationGeneration: 0,
    running: true,
    sending: false,
    runningConvs: new Set(),
  };
  const context = {
    state,
    Promise,
    bgAgentRows: [],
    sessionStorage: {
      setItem: (key, value) => storage.set(key, value),
      removeItem: (key) => storage.delete(key),
    },
    send: async (command) => { commands.push(command); return true; },
    watchConversation: () => { const wait = deferred(); watches.push(wait); return wait.promise; },
    unwatchConversation() {},
    saveDraft() {},
    denyPending() {},
    cacheTasks() {},
    recoverNavigation(token) { if (state.awaitingLoad === token) state.awaitingLoad = null; },
    toast() {},
    renderChats() {},
    updateRunningIndicators() {},
    closeMobileSidebar() {},
    clearTaskList() {},
    buildWelcome: () => ({}),
    finalizeTurn() {},
    setRunning() {},
    clearArtifacts() {},
    restoreDraft() {},
    $: () => ({ innerHTML: "", appendChild() {} }),
  };
  vm.runInNewContext(`let routingQueue = Promise.resolve(); let pendingSubmitRequest = null; let desiredConversationId = 1; ${route}${open}${fresh};globalThis.open = openChat;globalThis.fresh = newChat;globalThis.setPendingSubmit = (request) => { pendingSubmitRequest = request; }`, context);
  return { context, state, commands, watches };
}

test("rapid chat navigation cannot route an older load after the newer intent", async () => {
  const { context, state, commands, watches } = navigationHarness();
  const first = context.open(2);
  const second = context.open(3);
  assert.equal(watches.length, 2);

  watches[1].resolve(true);
  await second;
  watches[0].resolve(true);
  await first;

  assert.equal(commands.length, 1);
  assert.equal(commands[0].load_conversation.id, 3);
  assert.equal(state.conversationId, 3);
  assert.equal(state.awaitingLoad.id, 3);
});

test("switching immediately after send delivers the prompt before repinning", async () => {
  const { context, state, commands, watches } = navigationHarness();
  const delivery = deferred();
  state.running = false;
  state.sending = true;
  context.setPendingSubmit(delivery.promise);

  const navigation = context.open(2);
  assert.equal(commands.length, 0);
  assert.equal(watches.length, 0);

  delivery.resolve(true);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(watches.length, 1);
  watches[0].resolve(true);
  await navigation;

  assert.equal(commands.length, 1);
  assert.equal(commands[0].load_conversation.id, 2);
  assert.equal(state.runningConvs.has(1), true);
});

test("new chat racing with open chat preserves the later open intent", async () => {
  const { context, state, commands, watches } = navigationHarness();
  const first = context.fresh();
  const second = context.open(9);

  watches[1].resolve(true);
  await second;
  watches[0].resolve(true);
  await first;

  assert.equal(commands.length, 1);
  assert.equal(commands[0].load_conversation.id, 9);
  assert.equal(state.conversationId, 9);
  assert.equal(state.draftChat, false);
});

test("duplicate watch requests share readiness and both observe failure", async () => {
  const source = js.slice(js.indexOf("async function watchConversation"), js.indexOf("// Events from a background"));
  const response = deferred();
  let fetches = 0;
  const context = {
    state: { session: "session-1", watched: new Set() },
    fetch: () => { fetches++; return response.promise; },
  };
  vm.runInNewContext(`const watchRequests = new Map(); ${source};globalThis.watch = watchConversation`, context);

  const first = context.watch(7);
  const second = context.watch(7);
  assert.equal(fetches, 1);
  response.resolve({ ok: false, text: async () => "attach failed" });

  assert.equal(await first, false);
  assert.equal(await second, false);
  assert.equal(context.state.watched.has(7), false);
});

test("reattaching to a busy conversation restores its active running state", () => {
  const source = js.slice(js.indexOf("function onConversationLoaded"), js.indexOf("function renderStoredMessage"));
  const state = { awaitingLoad: null, conversationId: 4, runningConvs: new Set([8]) };
  let running = false;
  const thread = { innerHTML: "", appendChild() {} };
  const context = {
    state,
    switchSatisfiedBy: () => true,
    $: () => thread,
    finalizeTurn() {},
    clearArtifacts() {},
    onSnapshot: (snapshot) => { state.conversationId = snapshot.conversation_id; },
    setRunning: (value) => { running = value; },
    restoreDraft() {},
    restoreTasks() {},
    buildWelcome: () => ({}),
    replayLiveTail() {},
    scrollToBottom() {},
  };
  vm.runInNewContext(`let bgAgentRows = []; ${source};globalThis.loaded = onConversationLoaded`, context);

  context.loaded({ messages: [], snapshot: { conversation_id: 8 }, busy: true });

  assert.equal(state.conversationId, 8);
  assert.equal(state.runningConvs.has(8), false);
  assert.equal(running, true);
});

test("conversation load failure only clears and restores its correlated navigation", () => {
  const recover = js.slice(js.indexOf("function recoverNavigation"), js.indexOf("const LIVE_EVENT_TYPES"));
  const failed = js.slice(js.indexOf("function onConversationLoadFailed"), js.indexOf("function onConversationLoaded"));
  const token = { mode: "load", id: 8, from: 4, draftChat: true };
  const state = { awaitingLoad: token, conversationId: 8, draftChat: false, runningConvs: new Set([4]) };
  const errors = [];
  const context = {
    state,
    sessionStorage: { setItem() {}, removeItem() {} },
    unwatchConversation() {},
    renderChats() {},
    updateRunningIndicators() {},
    systemLine: (message, isError) => errors.push({ message, isError }),
  };
  vm.runInNewContext(`let desiredConversationId = 8; ${recover}${failed};globalThis.fail = onConversationLoadFailed`, context);

  context.fail({ id: 7, message: "wrong failure" });
  assert.equal(state.awaitingLoad, token);
  assert.equal(state.conversationId, 8);

  context.fail({ id: 8, message: "database unavailable" });
  assert.equal(state.awaitingLoad, null);
  assert.equal(state.conversationId, 4);
  assert.equal(state.draftChat, true);
  assert.deepEqual(errors, [{ message: "database unavailable", isError: true }]);
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

test("provider editor supports optional max concurrency", () => {
  const providerEditor = js.slice(js.indexOf("const PROVIDER_FIELDS"), js.indexOf("// Inline add-provider form"));
  assert.match(providerEditor, /key: "max_concurrency"/);
  assert.match(providerEditor, /Max concurrency must be blank or a positive integer/);
  assert.match(bridge, /max_concurrency: merged\.max_concurrency \?\? null/);
});

test("provider editor supports custom effort and Codex-only fast mode", () => {
  const providerEditor = js.slice(js.indexOf("const PROVIDER_FIELDS"), js.indexOf("// Inline add-provider form"));
  assert.match(providerEditor, /\["Med", "medium"\]/);
  assert.match(providerEditor, /\["XHigh", "xhigh"\]/);
  assert.match(providerEditor, /Custom value \(for example ultra\)/);
  assert.match(providerEditor, /key: "fast_mode"[^\n]+handlers: \["codex"\]/);
  assert.match(bridge, /fast_mode: merged\.handler === "codex"/);
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
  // Registered agents surface as structured daemon state with CRUD controls.
  assert.match(html, /data-tab="agents"[^>]*>Agents</);
  assert.match(html, /data-pane="agents"/);
  assert.match(js, /function renderAgents/);
  assert.match(js, /upsert_subagent/);
  assert.match(js, /set_subagent_enabled/);
  const agentEditor = js.slice(js.indexOf("function renderAgentEditor"), js.indexOf("function renderAgents"));
  assert.doesNotMatch(agentEditor, /max_concurrency/);
  assert.doesNotMatch(js, /Lua · read-only/);
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

test("orphan tool errors remain visible without exposing result content", () => {
  assert.match(js, /if \(!card\) \{[\s\S]*tool orphan[\s\S]*activeContainer\(\)\.appendChild\(orphan\)/);
  assert.doesNotMatch(
    js.slice(js.indexOf("if (!card) {", js.indexOf("function onToolResult")), js.indexOf("card.classList.remove", js.indexOf("function onToolResult"))),
    /ev\.content|ev\.arguments/,
  );
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
