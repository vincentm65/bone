// bone studio — browser client for the bone runtime protocol.
//
// Daemon → us over SSE (RuntimeEvent), us → daemon over POST (RuntimeCommand).
// Externally-tagged serde: unit events arrive as the bare string "turn_complete",
// data events as { tool_call: {...} }. normalize() flattens both to { type, ... }.
// Chat list / providers / config come from the bridge's local-data endpoints.

const $ = (id) => document.getElementById(id);
const el = (tag, cls, html) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (html != null) n.innerHTML = html;
  return n;
};

const prefs = loadPrefs();
const storedConversationId = Number(sessionStorage.getItem("bone-active-conversation"));
let desiredConversationId = Number.isInteger(storedConversationId) && storedConversationId > 0
  ? storedConversationId
  : null;

const state = {
  session: null,
  running: false,
  asstEl: null,
  asstRaw: "",
  reasonEl: null,
  reasonDetails: null,
  tools: new Map(),
  approvals: new Map(),
  conversationId: null,
  providers: [],
  providerId: null,
  model: null,
  snapshot: {},
  toolDefs: [],
  toolInfo: new Map(),   // call id -> { name, arguments }
  // The conversation switch in flight, or null when none. Each browser tab
  // multiplexes one daemon connection across conversations, so the previous
  // actor's in-flight events can still be buffered in the socket when we switch.
  // We drop those strays until the *target* conversation is established. The
  // token records which target so we only resolve on it (not on a stray snapshot
  // from the actor we just left, nor on an out-of-order load from a quick A→B
  // double switch):
  //   { mode: "load", id }   — waiting for conversation `id`.
  //   { mode: "new", from }  — waiting for a fresh conversation, any id != `from`.
  awaitingLoad: null,
};

// Does this snapshot/conversation_loaded satisfy the pending switch? With nothing
// pending, everything passes. A specific load resolves only on its own id. A
// new-chat request resolves on the fresh conversation, which the daemon either
// mints under a new id or — when we were already on an empty chat — reuses under
// the same id; either way it is empty, so resolve on a different id OR an empty
// transcript. A stray snapshot from the non-empty actor we left (same id,
// transcript_len > 0) is still ignored.
function switchSatisfiedBy(snapshot) {
  const w = state.awaitingLoad;
  if (!w) return true;
  const cid = snapshot ? snapshot.conversation_id : null;
  if (cid == null) return false;
  if (w.mode === "load") return cid === w.id;
  return cid !== w.from || !(snapshot.transcript_len > 0);
}

// Per-conversation task lists, keyed by conversation id. The daemon emits a
// conversation's task pane as live `view_diff`s during its turn and never
// replays them on re-attach, so switching away would lose the list. We cache
// each conversation's latest list here and restore it on switch/return.
const taskCache = new Map();

// ── connection ──────────────────────────────────────────────────────────────

function connect() {
  const es = new EventSource("/api/events");
  es.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.kind === "bridge") return onBridge(msg);
    if (msg.kind === "event") return onEvent(normalize(msg.payload));
  };
  es.onerror = () => setConn(false);
}

function onBridge(msg) {
  if (msg.session) state.session = msg.session;
  if (msg.status === "connected") {
    setConn(true);
    clearRecovery();
    // A reconnect creates a fresh TCP connection, which initially attaches to
    // the daemon's latest conversation. Restore this tab's own selection.
    if (desiredConversationId != null) {
      state.awaitingLoad = { mode: "load", id: desiredConversationId };
      send({ load_conversation: { id: desiredConversationId } });
    }
  }
  if (msg.status === "disconnected") { setConn(false); toast("daemon disconnected — retrying…"); }
}

function setConn(online) {
  const dot = $("conn-dot");
  dot.classList.toggle("online", online);
  dot.classList.toggle("offline", !online);
}

function normalize(payload) {
  if (typeof payload === "string") return { type: payload };
  const type = Object.keys(payload)[0];
  return { type, ...payload[type] };
}

async function send(command) {
  if (!state.session) return;
  await fetch(`/api/command?session=${state.session}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(command),
  }).catch(() => toast("command failed"));
}

// ── event handling ───────────────────────────────────────────────────────────

// Streaming/turn events belong to whichever actor this connection is currently
// attached to. While a switch is in flight (`awaitingLoad`), they may still be
// strays from the conversation we just left — drop them until the target is
// established. Routing/identity events pass through so the switch can resolve:
// `state_snapshot` and `conversation_loaded` carry the conversation id we match
// against, `status` lets a failed switch recover, and `frontend_state` is global.
function onEvent(ev) {
  const routing = ev.type === "conversation_loaded" || ev.type === "state_snapshot" ||
                  ev.type === "status" || ev.type === "frontend_state";
  if (state.awaitingLoad && !routing) return;
  switch (ev.type) {
    case "frontend_state": return onFrontendState(ev);
    case "state_snapshot": return onSnapshot(ev.snapshot);
    case "conversation_loaded": return onConversationLoaded(ev);
    case "started": return setRunning(true);
    case "status": return onStatus(ev.message);
    case "notice": return systemLine(ev.message);
    case "reasoning_delta": return appendReasoning(ev.text);
    case "text_delta": return appendText(ev.text);
    case "tool_call": return onToolCall(ev);
    case "tool_result": return onToolResult(ev);
    case "token_usage": return onTokenUsage(ev);
    case "approval_request": return onApproval(ev);
    case "key_request": return onKeyRequest(ev);
    case "finished": return onFinished(ev);
    case "failed": return onFailed(ev);
    case "turn_complete": return onTurnComplete();
    case "view_diff": return onViewDiff(ev.diff);
    default: return;
  }
}

function onFrontendState(ev) {
  if (Array.isArray(ev.tool_defs)) state.toolDefs = ev.tool_defs;
  applyTheme(ev.theme);
}

function onSnapshot(s) {
  if (!s) return;
  // While switching, only the target conversation's snapshot is authoritative;
  // a snapshot from the actor we just left would clobber state.conversationId.
  // The matching snapshot resolves the switch — this is the only signal a fresh
  // conversation produces (NewConversation emits no `conversation_loaded`).
  if (state.awaitingLoad) {
    if (!switchSatisfiedBy(s)) return;
    state.awaitingLoad = null;
  }
  state.snapshot = s;
  state.model = s.provider_model || state.model;
  state.providerId = s.provider_id || state.providerId;
  if (s.conversation_id != null) {
    const changed = state.conversationId !== s.conversation_id;
    state.conversationId = s.conversation_id;
    desiredConversationId = s.conversation_id;
    sessionStorage.setItem("bone-active-conversation", String(s.conversation_id));
    if (changed) highlightActiveChat();
  }
  renderModelLabel();
  updateMeter(s.context_length, s.sent, s.received, s.cost);
  renderSettingsStats();
}

function renderModelLabel() {
  const prov = state.providers.find((p) => p.key === state.providerId);
  const name = prov ? prov.label : state.providerId || "model";
  $("model-label").textContent = state.model ? `${name} · ${state.model}` : name;
}

function onTokenUsage(ev) { updateMeter(ev.context_length, ev.sent, ev.received, null); }

let lastCost = 0;
function updateMeter(contextLen, sent, received, cost) {
  if (cost != null) lastCost = cost;
  sent = sent || 0; received = received || 0;
  const total = sent + received;
  const ctx = contextLen || total || 0;
  $("meter-fill").style.width = Math.min(100, (ctx / 200000) * 100) + "%";
  const costStr = lastCost > 0 ? ` · $${lastCost.toFixed(4)}` : "";
  $("meter-text").textContent = `${fmt(ctx)} tok${costStr}`;
  // Composer readout: context · in / out / total.
  $("composer-tokens").innerHTML =
    `<span class="ct-ctx">${fmt(ctx)} ctx</span>` +
    `<span class="ct-sep">·</span><span class="ct-in">↑${fmt(sent)}</span>` +
    `<span class="ct-out">↓${fmt(received)}</span>` +
    `<span class="ct-sep">·</span><span class="ct-tot">${fmt(total)} tot</span>`;
}

function fmt(n) {
  if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(n >= 10_000_000_000 ? 0 : 1) + "B";
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1) + "M";
  if (n >= 1000) return (n / 1000).toFixed(n >= 10000 ? 0 : 1) + "k";
  return String(n);
}

// ── conversation rendering ─────────────────────────────────────────────────

function clearWelcome() { const w = $("welcome"); if (w) w.remove(); }

function turn(role) {
  clearWelcome();
  const t = el("div", `turn msg-${role}`);
  $("thread").appendChild(t);
  return t;
}

function userMessage(text) {
  const t = turn("user");
  t.appendChild(el("div", "role-tag", "You"));
  t.appendChild(el("div", "bubble")).textContent = text;
  scrollDown();
  return t;
}

function ensureAssistant() {
  // Streaming output implies a live turn. When we re-attach to a chat that is
  // already mid-turn we may miss its `started` event, so infer running here to
  // keep the Stop button (and composer state) correct.
  if (!state.running) setRunning(true);
  if (state.asstEl) return;
  const t = turn("assistant");
  t.appendChild(el("div", "role-tag", "bone"));
  state.asstEl = el("div", "prose");
  state.asstRaw = "";
  t.appendChild(state.asstEl);
}

// Where to drop tool / approval cards: inside the active assistant turn.
function activeContainer() {
  ensureAssistant();
  return state.asstEl.parentElement;
}

function appendText(text) {
  // Remove thinking once prose starts — it's no longer relevant.
  if (state.reasonDetails) { state.reasonDetails.remove(); state.reasonDetails = null; state.reasonEl = null; }
  ensureAssistant();
  state.asstRaw += text;
  state.asstEl.innerHTML = renderMarkdown(state.asstRaw) + '<span class="caret"></span>';
  state.asstEl.parentElement.appendChild(state.asstEl); // keep prose last
  scrollDown();
}

function appendReasoning(text) {
  ensureAssistant();
  if (!state.reasonEl) {
    const d = el("details", "reasoning");
    d.appendChild(el("summary", null, "Thinking"));
    const body = el("div", "body");
    d.appendChild(body);
    state.asstEl.parentElement.insertBefore(d, state.asstEl);
    state.reasonDetails = d;
    state.reasonEl = body;
  }
  state.reasonEl.textContent += text;
  // Live one-line preview in the summary — user clicks to expand.
  const raw = state.reasonEl.textContent;
  const preview = raw.replace(/\n/g, " ").slice(0, 72);
  const dots = raw.length > 72 ? "…" : "";
  state.reasonDetails.querySelector("summary").textContent = "Thinking: " + preview + dots;
  // Never auto-scroll for reasoning tokens — user may be reading above.
}

// ── tool cards ──────────────────────────────────────────────────────────────

const TOOL_VERBS = {
  shell: "Run", bash: "Run", read_file: "Read", write_file: "Write", edit_file: "Edit",
  apply_patch: "Patch", search: "Search", grep: "Search", list: "List", ls: "List",
  glob: "Find", web: "Fetch", fetch: "Fetch", web_search: "Search",
};

// Keys whose value is the "script" of a call (a shell command, file content,
// patch, …). We render these raw — with real newlines — so an expanded tool
// shows the entire batch script as written, not a single escaped JSON line.
const SCRIPT_KEYS = ["command", "cmd", "script", "content", "input", "patch", "code"];

// Populate a tool card's body with its full arguments. The primary script
// renders raw under its own label; any remaining args follow as compact JSON.
// Long bodies are capped + scrollable via CSS (.tool-body pre).
function fillToolArgs(body, args) {
  if (!args || !Object.keys(args).length) return;
  const rest = { ...args };
  let script = null, scriptKey = null;
  for (const k of SCRIPT_KEYS) {
    if (typeof rest[k] === "string") { script = rest[k]; scriptKey = k; delete rest[k]; break; }
  }
  if (script != null) {
    body.appendChild(el("div", "tool-section-label", scriptKey));
    body.appendChild(el("pre", "args")).textContent = script;
  }
  if (Object.keys(rest).length) {
    body.appendChild(el("div", "tool-section-label", "Arguments"));
    body.appendChild(el("pre", "args")).textContent = JSON.stringify(rest, null, 2);
  }
}

function toolMeta(name, args) {
  args = args || {};
  const verb = TOOL_VERBS[name] || name.replace(/_/g, " ");
  const argKeys = ["command", "cmd", "path", "file_path", "file", "query", "pattern", "url", "name"];
  let arg = "";
  for (const k of argKeys) if (typeof args[k] === "string") { arg = args[k]; break; }
  if (!arg) { const v = Object.values(args).find((x) => typeof x === "string"); if (v) arg = v; }
  return { verb, arg };
}

function onToolCall(ev) {
  // Snapshot any text accumulated so far — it belongs chronologically
  // before this tool call. Start a fresh prose segment for text that
  // comes after.
  let hadText = false;
  if (state.asstEl && state.asstRaw) {
    state.asstEl.innerHTML = renderMarkdown(state.asstRaw);
    state.asstRaw = "";
    hadText = true;
  }
  const cont = activeContainer();
  state.toolInfo.set(ev.id, { name: ev.name, arguments: ev.arguments });
  const { verb, arg } = toolMeta(ev.name, ev.arguments);
  const card = el("div", "tool running" + (prefs.expandTools ? " open" : ""));
  card.innerHTML = `
    <div class="tool-head">
      <div class="tool-main">
        <div class="tool-title"><span class="tool-verb"></span> <span class="tool-arg"></span></div>
      </div>
      <button class="ghost-btn tool-open hidden" title="Open in canvas">
        <svg viewBox="0 0 24 24"><path d="M14 3h7v7M21 3l-9 9M10 5H5a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-5"/></svg>
      </button>
      <span class="tool-status running"></span>
      <svg class="tool-chevron" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg>
    </div>
    <div class="tool-body"></div>`;
  card.querySelector(".tool-verb").textContent = verb;
  card.querySelector(".tool-arg").textContent = arg;
  const body = card.querySelector(".tool-body");
  fillToolArgs(body, ev.arguments);
  card.querySelector(".tool-head").onclick = () => card.classList.toggle("open");

  // File-writing tools get an "open in canvas" affordance. write_file content is
  // available right now; edit_file's diff arrives with the result — defer the
  // button until we have the diff so we never show "nothing to show yet".
  const path = ev.arguments && (ev.arguments.path || ev.arguments.file_path);
  if (path && ev.name === "write_file" && typeof ev.arguments.content === "string") {
    const open = card.querySelector(".tool-open");
    open.classList.remove("hidden");
    open.onclick = (e) => { e.stopPropagation(); focusArtifact(path); };
    captureDoc(path, ev.arguments.content);
  }
  cont.appendChild(card);
  // New prose segment for text after this tool call.
  if (hadText) {
    state.asstEl = el("div", "prose");
    cont.appendChild(state.asstEl);
  } else if (state.asstEl) {
    // No text yet — keep prose after the tool card.
    cont.appendChild(state.asstEl);
  }
  state.tools.set(ev.id, card);
  scrollDown();
}

function onToolResult(ev) {
  const card = state.tools.get(ev.call_id);
  if (!card) return;
  card.classList.remove("running");
  const status = card.querySelector(".tool-status");
  status.classList.remove("running");
  status.classList.add(ev.is_error ? "error" : "done");
  const content = (ev.content || "").trim();
  if (content) {
    const lines = content.split("\n").length;
    card.querySelector(".tool-body").appendChild(
      el("div", "tool-section-label", (ev.is_error ? "Error" : "Output") + ` · ${lines} line${lines === 1 ? "" : "s"}`),
    );
    const pre = el("pre", ev.is_error ? "err" : null);
    pre.textContent = formatToolOutput(content);
    card.querySelector(".tool-body").appendChild(pre);
  }
  // Surface an edit's diff in the canvas. The result content embeds bone's
  // numbered unified diff (see core/src/tools/edit_file/diff.rs).
  const info = state.toolInfo.get(ev.call_id);
  if (info && info.name === "edit_file" && !ev.is_error) {
    const path = info.arguments && (info.arguments.path || info.arguments.file_path);
    if (path) {
      captureDiff(path, content);
      // Reveal the "Open in canvas" button now that we have the diff.
      const open = card.querySelector(".tool-open");
      if (open) {
        open.classList.remove("hidden");
        open.onclick = (e) => { e.stopPropagation(); focusArtifact(path); };
      }
    }
  }

  if (state.asstEl) state.asstEl.parentElement.appendChild(state.asstEl);
  scrollDown();
}

function formatToolOutput(s) {
  const t = s.trim();
  if ((t.startsWith("{") && t.endsWith("}")) || (t.startsWith("[") && t.endsWith("]"))) {
    try { return JSON.stringify(JSON.parse(t), null, 2); } catch { /* not json */ }
  }
  return s;
}

// ── canvas: split-screen artifact / diff viewer ──────────────────────────────
//
// One artifact per file path. write_file → a live "doc" (markdown rendered) or
// "file" (plain) view; edit_file → a colour-coded "diff" parsed from the result.
// The canvas opens automatically with the latest artifact and keeps a tab strip
// so you can step back through what the agent has written this turn.

const artifacts = new Map(); // path -> { path, name, kind, content, lines, add, del }
let activeArtifact = null;

function baseName(p) { return String(p).split("/").pop() || p; }

function captureDoc(path, content) {
  const kind = /\.(md|markdown|mdx)$/i.test(path) ? "doc" : "file";
  upsertArtifact({ path, name: baseName(path), kind, content, add: content.split("\n").length, del: 0 });
}

function captureDiff(path, resultContent) {
  const { lines, add, del } = parseDiff(resultContent);
  if (!lines.length) return; // "no changes" — nothing to show
  upsertArtifact({ path, name: baseName(path), kind: "diff", lines, add, del });
}

// Parse bone's numbered unified diff. Lines look like:
//   "   12   context"   "   13 - removed"   "   13 + added"
function parseDiff(text) {
  const lines = [];
  let add = 0, del = 0, prevNum = null;
  for (const raw of String(text).split("\n")) {
    const m = raw.match(/^\s*(\d+)\s([-+ ])\s(.*)$/);
    if (!m) continue;
    const num = Number(m[1]), sign = m[2], txt = m[3];
    // A drop in line number between hunks marks a gap; show a separator.
    if (prevNum != null && num < prevNum) lines.push({ type: "hunk" });
    if (sign === "+") { lines.push({ type: "add", ln: num, text: txt }); add++; }
    else if (sign === "-") { lines.push({ type: "del", ln: num, text: txt }); del++; }
    else lines.push({ type: "ctx", ln: num, text: txt });
    prevNum = num;
  }
  return { lines, add, del };
}

function upsertArtifact(art) {
  artifacts.set(art.path, { ...(artifacts.get(art.path) || {}), ...art });
  activeArtifact = art.path;
  $("canvas-toggle").classList.remove("hidden");
  openCanvas();
  renderTabs();
  renderArtifact();
}

function focusArtifact(path) {
  if (!artifacts.has(path)) { toast("nothing to show yet"); return; }
  activeArtifact = path;
  openCanvas();
  renderTabs();
  renderArtifact();
}

function closeArtifact(path) {
  artifacts.delete(path);
  if (activeArtifact === path) activeArtifact = [...artifacts.keys()].pop() || null;
  if (!artifacts.size) { closeCanvas(); $("canvas-toggle").classList.add("hidden"); }
  renderTabs();
  renderArtifact();
}

function openCanvas() { $("canvas").classList.remove("hidden"); $("divider").classList.remove("hidden"); }
function closeCanvas() { $("canvas").classList.add("hidden"); $("divider").classList.add("hidden"); }
function toggleCanvas() {
  if (!artifacts.size) return;
  $("canvas").classList.contains("hidden") ? openCanvas() : closeCanvas();
}

const KIND_LABEL = { doc: "md", file: "file", diff: "diff" };

function renderTabs() {
  const tabs = $("canvas-tabs");
  tabs.innerHTML = "";
  for (const a of artifacts.values()) {
    const tab = el("div", "canvas-tab" + (a.path === activeArtifact ? " active" : ""));
    tab.title = a.path;
    tab.innerHTML = `<span class="ct-kind"></span><span class="ct-name"></span>
      <span class="ct-x"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></span>`;
    tab.querySelector(".ct-kind").textContent = KIND_LABEL[a.kind] || "file";
    tab.querySelector(".ct-name").textContent = a.name;
    tab.onclick = (e) => { if (e.target.closest(".ct-x")) return; focusArtifact(a.path); };
    tab.querySelector(".ct-x").onclick = (e) => { e.stopPropagation(); closeArtifact(a.path); };
    tabs.appendChild(tab);
  }
}

function artifactMeta(a) {
  const meta = el("div", "canvas-meta");
  const path = el("span", "cm-path");
  path.textContent = a.path;
  meta.appendChild(path);
  if (a.kind === "diff") {
    meta.appendChild(el("span", "cm-add", `+${a.add}`));
    meta.appendChild(el("span", "cm-del", `−${a.del}`));
  } else {
    meta.appendChild(el("span", null, `${(a.content || "").split("\n").length} lines`));
  }
  return meta;
}

function renderArtifact() {
  const body = $("canvas-body");
  body.innerHTML = "";
  const a = artifacts.get(activeArtifact);
  if (!a) { body.appendChild(el("div", "canvas-empty", "Nothing open")); return; }
  body.appendChild(artifactMeta(a));
  if (a.kind === "doc") {
    body.appendChild(el("div", "prose", renderMarkdown(a.content || "")));
  } else if (a.kind === "diff") {
    body.appendChild(renderDiffView(a.lines));
  } else {
    body.appendChild(renderCodeView(a.content || ""));
  }
  body.scrollTop = 0;
}

function renderDiffView(lines) {
  const wrap = el("div", "diffview");
  for (const l of lines) {
    if (l.type === "hunk") { wrap.appendChild(el("div", "diff-hunk", "⋯")); continue; }
    const row = el("div", "diff-line " + l.type);
    const sign = l.type === "add" ? "+" : l.type === "del" ? "−" : "";
    row.innerHTML = `<span class="ln"></span><span class="sign"></span><span class="lt"></span>`;
    row.querySelector(".ln").textContent = l.ln ?? "";
    row.querySelector(".sign").textContent = sign;
    row.querySelector(".lt").textContent = l.text;
    wrap.appendChild(row);
  }
  return wrap;
}

function renderCodeView(content) {
  const wrap = el("div", "codeview");
  const lines = content.split("\n");
  lines.forEach((text, i) => {
    const row = el("div", "code-line");
    row.innerHTML = `<span class="ln"></span><span class="lt"></span>`;
    row.querySelector(".ln").textContent = i + 1;
    row.querySelector(".lt").textContent = text;
    wrap.appendChild(row);
  });
  return wrap;
}

// ── task list panel (sidebar) ─────────────────────────────────────────
// Receives ViewDiff::Upsert from the daemon for source="task_list". The pane
// carries { title, lines: [{ spans: [{ text, fg, modifiers }] }] }. We render
// each line as a task item with status-derived styling (pending/in_progress/done).

const taskState = { active: false, title: "", items: [], expanded: false };

function renderTaskList() {
  const wrap = $("task-popup-wrap");
  const collapsed = $("task-popup-collapsed");
  const expanded = $("task-popup-expanded");
  const label = $("task-popup-label");
  const titleEl = expanded.querySelector(".task-list-title");
  const countEl = expanded.querySelector(".task-list-count");
  const itemsEl = $("task-list-items");

  if (!taskState.active || taskState.items.length === 0) {
    wrap.classList.add("hidden");
    return;
  }

  wrap.classList.remove("hidden");

  // Collapsed bar: "Refactor auth module  3/7"
  const done = taskState.items.filter((t) => t.status === "done").length;
  const inProg = taskState.items.filter((t) => t.status === "in_progress");
  const activeTask = inProg.length ? inProg[0].text : (taskState.items[taskState.items.length - 1]?.text || "");
  const progressIdx = taskState.items.findIndex((t) => t.status === "in_progress");
  const progressLabel = progressIdx >= 0
    ? ` ${progressIdx + 1}/${taskState.items.length}`
    : ` ${done}/${taskState.items.length}`;
  label.innerHTML = `${activeTask}<span class="task-progress">${progressLabel}</span>`;

  // Expanded: full list
  titleEl.textContent = taskState.title || "Tasks";
  countEl.textContent = `${done}/${taskState.items.length} done`;

  itemsEl.innerHTML = "";
  for (const item of taskState.items) {
    const t = el("div", `task-item ${item.status || "pending"}`);
    const icon = item.status === "done" ? "✓" : item.status === "in_progress" ? "◐" : "○";
    t.innerHTML = `<span class="task-icon">${icon}</span><span class="task-text"></span>`;
    t.querySelector(".task-text").textContent = item.text;
    itemsEl.appendChild(t);
  }
}

function toggleTaskPopup() {
  taskState.expanded = !taskState.expanded;
  $("task-popup-wrap").classList.toggle("expanded", taskState.expanded);
  $("task-popup-expanded").classList.toggle("hidden", !taskState.expanded);
}

// Reset the sidebar task list — called when creating a fresh chat so no stale
// tasks linger.
function clearTaskList() {
  taskState.active = false;
  taskState.items = [];
  taskState.expanded = false;
  $("task-popup-expanded").classList.add("hidden");
  $("task-popup-wrap").classList.remove("expanded");
  renderTaskList();
}

// Persist the live task list under a conversation id so it survives a switch.
// A conversation's actor emits its task pane only as live diffs and never
// replays them on re-attach, so without this cache the list vanishes the moment
// you look at another chat.
function cacheTasks(convId) {
  if (convId == null) return;
  if (taskState.active && taskState.items.length) {
    taskCache.set(convId, { title: taskState.title, items: taskState.items.map((t) => ({ ...t })) });
  } else {
    taskCache.delete(convId);
  }
}

// Restore (or clear) the sidebar task list for the conversation now in view.
function restoreTasks(convId) {
  const cached = convId != null ? taskCache.get(convId) : null;
  if (cached) {
    taskState.active = true;
    taskState.title = cached.title;
    taskState.items = cached.items.map((t) => ({ ...t }));
  } else {
    taskState.active = false;
    taskState.items = [];
  }
  taskState.expanded = false;
  $("task-popup-expanded").classList.add("hidden");
  $("task-popup-wrap").classList.remove("expanded");
  renderTaskList();
}

$("task-popup-toggle").addEventListener("click", (e) => { e.stopPropagation(); toggleTaskPopup(); });
$("task-popup-collapsed").addEventListener("click", () => toggleTaskPopup());

// ── inline approvals ────────────────────────────────────────────────────────

function onApproval(ev) {
  // Danger mode (and policy-allowed calls) arrive pre-approved: the daemon's
  // gate marks `auto_allows` and leaves the decision to the client, exactly as
  // the TUI does. Approve immediately and skip the prompt — the tool call still
  // renders via its own tool events.
  if (ev.auto_allows) {
    send({ approval_reply: { id: ev.id, outcome: "approve" } });
    return;
  }
  const cont = activeContainer();
  const card = el("div", "approval");
  card.innerHTML = `
    <div class="approval-top">
      <span class="approval-badge">⚠</span>
      <div>
        <div class="approval-kicker">Approval needed</div>
        <div class="approval-tool"></div>
      </div>
    </div>
    <div class="approval-detail"></div>
    <pre class="approval-args hidden"></pre>
    <div class="approval-guide hidden"><input placeholder="Tell the agent what to do instead…" /></div>
    <div class="approval-actions">
      <button class="btn btn-deny">Deny</button>
      <button class="btn btn-block">Guide…</button>
      <span class="grow"></span>
      <button class="btn btn-approve">Approve</button>
    </div>`;
  card.querySelector(".approval-tool").textContent = ev.name;
  card.querySelector(".approval-detail").textContent = ev.summary || "The agent wants to run this tool.";
  if (ev.arguments && Object.keys(ev.arguments).length) {
    const pre = card.querySelector(".approval-args");
    pre.textContent = JSON.stringify(ev.arguments, null, 2);
    pre.classList.remove("hidden");
  }
  const guide = card.querySelector(".approval-guide");
  const guideInput = guide.querySelector("input");
  card.querySelector(".btn-approve").onclick = () => resolveApproval(ev.id, "approve", card, "Approved");
  card.querySelector(".btn-deny").onclick = () => resolveApproval(ev.id, "denied", card, "Denied");
  card.querySelector(".btn-block").onclick = () => {
    if (guide.classList.contains("hidden")) { guide.classList.remove("hidden"); guideInput.focus(); }
    else resolveApproval(ev.id, { blocked: guideInput.value.trim() || "Please reconsider this step." }, card, "Guided");
  };
  guideInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") resolveApproval(ev.id, { blocked: guideInput.value.trim() || "Please reconsider." }, card, "Guided");
  });
  cont.appendChild(card);
  state.approvals.set(ev.id, card);
  scrollDown();
}

function resolveApproval(id, outcome, card, label) {
  send({ approval_reply: { id, outcome } });
  state.approvals.delete(id);
  const ok = outcome === "approve";
  const guided = typeof outcome === "object";
  card.innerHTML = `<div class="approval-resolved ${ok ? "ok" : "no"}">
    <span>${ok ? "✓" : guided ? "✎" : "✗"}</span><span>${label}</span></div>`;
}

// Auto-deny every approval still awaiting a reply. Leaving one unanswered wedges
// the daemon's turn loop forever (the approval gate blocks on the reply), so we
// resolve them whenever the user abandons the turn (new chat, switch chat, stop,
// tab close). `beacon` uses sendBeacon so it still fires during page unload.
function denyPending(beacon) {
  for (const id of [...state.approvals.keys()]) {
    const card = state.approvals.get(id);
    const body = JSON.stringify({ approval_reply: { id, outcome: "denied" } });
    if (beacon && navigator.sendBeacon) navigator.sendBeacon(`/api/command?session=${state.session}`, body);
    else send({ approval_reply: { id, outcome: "denied" } });
    if (card) card.innerHTML = `<div class="approval-resolved no"><span>✗</span><span>Dismissed</span></div>`;
    state.approvals.delete(id);
  }
}

// Daemon status lines. Most are transient chatter; a few matter:
//  - "busy: a turn is in progress" — this conversation already has a turn
//    running (possibly from another tab attached to the same chat).
//  - "ignored (idle)" — internal no-op acks; never surface them.
//  - "running <tool>: …" — the driver's per-tool-call status; the tool_call event
//    renders a richer card for the same call, so the grey line would just be a
//    raw-text duplicate of the card. Drop it here (the TUI uses it as a transient
//    status bar, which is why the runtime still emits it).
function onStatus(message) {
  if (!message) return;
  // A switch that can't complete resolves here instead of via `conversation_loaded`
  // (the daemon reports load/create failures as a Status). Clear the pending gate
  // so the tab recovers rather than silently dropping every later event.
  if (state.awaitingLoad) {
    if (/failed to (load|create) conversation/i.test(message)) {
      state.awaitingLoad = null;
      return systemLine(message, true);
    }
    // Other statuses mid-switch are strays from the actor we left; don't bleed
    // them into the chat we're opening.
    return;
  }
  if (message.startsWith("busy:")) return onBusy();
  if (message.startsWith("ignored (idle)")) return;
  if (message.startsWith("running ")) return;
  systemLine(message);
}

function onBusy() {
  // Put the rejected message back in the composer and drop its orphaned bubble.
  if (state.lastBubble) { state.lastBubble.remove(); state.lastBubble = null; }
  if (state.lastText && !input.value.trim()) { input.value = state.lastText; autosize(); }
  showRecovery();
}

// A banner offering recovery when the engine is wedged by another session.
function showRecovery() {
  if ($("recovery")) return;
  const bar = el("div", "recovery");
  bar.id = "recovery";
  bar.innerHTML = `<span class="rec-msg">This chat already has a running turn — another tab may be waiting for approval.</span>
    <button class="btn rec-restart">Restart engine</button>
    <button class="ghost-btn rec-close"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button>`;
  bar.querySelector(".rec-restart").onclick = restartEngine;
  bar.querySelector(".rec-close").onclick = () => bar.remove();
  $("composer-wrap").prepend(bar);
}
function clearRecovery() { const b = $("recovery"); if (b) b.remove(); }

async function restartEngine() {
  toast("restarting engine…");
  clearRecovery();
  await fetch("/api/restart-daemon", { method: "POST" }).catch(() => {});
  // The SSE link reconnects automatically; resend the pending prompt once back.
  const text = state.lastText;
  state.lastText = null;
  setTimeout(() => {
    if (text && !state.running) { input.value = text; autosize(); toast("engine restarted — press send"); }
  }, 1800);
}

function onKeyRequest(ev) { state.keyId = ev.id; toast("press any key…"); }
function captureKey(e) {
  if (state.keyId == null) return;
  e.preventDefault();
  const key = { code: e.key.length === 1 ? "Char" : e.key, char: e.key.length === 1 ? e.key : null, ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey };
  send({ key_reply: { id: state.keyId, key } });
  state.keyId = null;
}

// ── turn lifecycle ──────────────────────────────────────────────────────────

function onFinished() {
  if (state.asstEl) state.asstEl.innerHTML = renderMarkdown(state.asstRaw);
  finalizeTurn();
}
function onFailed(ev) { systemLine(ev.message || "turn failed", true); finalizeTurn(); setRunning(false); }
function onTurnComplete() { setRunning(false); loadChats(); }
function finalizeTurn() { state.asstEl = null; state.asstRaw = ""; state.reasonEl = null; state.reasonDetails = null; state.tools.clear(); state.toolInfo.clear(); }

function clearArtifacts() {
  artifacts.clear();
  activeArtifact = null;
  closeCanvas();
  $("canvas-toggle").classList.add("hidden");
  renderTabs();
}

function onConversationLoaded(ev) {
  // A quick A→B double switch produces two loads (A then B); only the one we
  // last asked for is authoritative. Ignore a load for any other conversation so
  // it can't render over, or clear the gate ahead of, the target we want.
  if (state.awaitingLoad && !switchSatisfiedBy(ev.snapshot)) return;
  // The target conversation's view is now authoritative — stop dropping events.
  state.awaitingLoad = null;
  $("thread").innerHTML = "";
  finalizeTurn();
  // Conversation routing is independent: leaving a running chat must not keep
  // this tab's composer disabled while another chat continues in its actor.
  setRunning(false);
  clearArtifacts();
  // The DB stores each LLM round as its own assistant message, but a single
  // turn often spans several tool-call rounds. Group consecutive assistant
  // messages into one visual turn (one "bone" tag) to match the live layout.
  let asstTurn = null;
  let rendered = 0;
  for (const m of ev.messages || []) {
    if (m.role === "user") asstTurn = null;
    asstTurn = renderStoredMessage(m, asstTurn);
    rendered++;
  }
  if (ev.snapshot) onSnapshot(ev.snapshot);
  // Restore this conversation's task list from the per-chat cache (the actor
  // won't re-emit it on attach). Uses the id from the snapshot we just applied.
  restoreTasks(state.conversationId);
  // An empty conversation (fresh chat) shows the welcome rather than a blank pane.
  if (!rendered) { $("thread").appendChild(buildWelcome()); }
  scrollDown();
}

function renderStoredMessage(m, asstTurn) {
  if (m.role === "user") { userMessage(m.content); return null; }
  if (m.role === "assistant") {
    const t = asstTurn || turn("assistant");
    if (!asstTurn) t.appendChild(el("div", "role-tag", "bone"));
    // Only emit a prose block when there's actual text — empty assistant
    // messages (tool-call-only rounds) shouldn't add blank separation.
    if ((m.content || "").trim()) t.appendChild(el("div", "prose", renderMarkdown(m.content)));
    for (const tc of m.tool_calls || []) {
      const { verb, arg } = toolMeta(tc.name, tc.arguments);
      const card = el("div", "tool");
      card.innerHTML = `<div class="tool-head">
        <div class="tool-main"><div class="tool-title"><span class="tool-verb"></span> <span class="tool-arg"></span></div></div>
        <span class="tool-status done"></span>
        <svg class="tool-chevron" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></div>
        <div class="tool-body"></div>`;
      card.querySelector(".tool-verb").textContent = verb;
      card.querySelector(".tool-arg").textContent = arg;
      fillToolArgs(card.querySelector(".tool-body"), tc.arguments);
      card.querySelector(".tool-head").onclick = () => card.classList.toggle("open");
      t.appendChild(card);
    }
    return t;
  }
  return asstTurn;
}

// The runtime may push an accent colour, but an explicit theme choice wins;
// only "auto" defers to the runtime.
function onViewDiff(diff) {
  // Runtime-pushed accent colour: only honoured when the theme defers to it.
  if (prefs.theme === "auto" && diff && diff.set_highlight &&
      diff.set_highlight.name === "accent" && diff.set_highlight.fg)
    document.documentElement.style.setProperty("--accent", diff.set_highlight.fg);

  // Task list pane (source="task_list") — render in sidebar. Theme-independent.
  if (diff && diff.upsert && diff.upsert.component) {
    const comp = diff.upsert.component;
    if (comp.id === "task_list" && comp.lines) {
      taskState.active = true;
      taskState.title = comp.title || "Tasks";
      taskState.items = parseTaskLines(comp.lines);
      cacheTasks(state.conversationId);
      renderTaskList();
      return;
    }
  }
  // Task list removed (empty pane → Remove diff).
  if (diff && diff.remove && diff.remove.id === "task_list") {
    taskState.active = false;
    taskState.items = [];
    cacheTasks(state.conversationId);
    renderTaskList();
  }
}

// Parse a pane's styled lines into { text, status } task items.
// Lines are PaneLineSpec::Spans with up to two spans: icon + text.
// Modifiers like "strike" signal done; colour hints help but we infer status
// from the icon span text (✓/◐/○) emitted by the Lua tool.
function parseTaskLines(lines) {
  const items = [];
  for (const line of lines) {
    if (typeof line === "string") {
      items.push({ text: line, status: "pending" });
      continue;
    }
    if (!line.spans || !line.spans.length) continue;
    // Concatenate span text; infer status from the first span (the icon).
    const text = line.spans.map((s) => s.text || "").join("");
    const icon = (line.spans[0].text || "").trim();
    let status = "pending";
    if (icon === "✓" || line.spans.some((s) => s.modifiers && s.modifiers.includes("strike"))) status = "done";
    else if (icon === "◐") status = "in_progress";
    // Strip the icon prefix from the display text.
    const display = text.replace(/^[○◐✓]\s*/, "");
    items.push({ text: display || text, status });
  }
  return items;
}
function applyTheme(theme) {
  if (prefs.theme !== "auto" || !theme) return;
  const hi = theme.highlights || theme;
  const accent = hi.tool_call?.fg || hi.accent?.fg;
  if (typeof accent === "string" && /^#/.test(accent)) document.documentElement.style.setProperty("--accent", accent);
}

function systemLine(text, isError) {
  clearWelcome();
  const line = el("div", "system-line" + (isError ? " error" : ""));
  line.textContent = text;
  $("thread").appendChild(line);
  scrollDown();
}
function scrollDown() {
  const t = $("thread");
  const atBottom = t.scrollHeight - t.scrollTop - t.clientHeight < 160;
  if (atBottom) t.scrollTop = t.scrollHeight;
}

// ── chat sidebar ────────────────────────────────────────────────────────────

async function loadChats() {
  const chats = await fetch("/api/conversations").then((r) => r.json()).catch(() => []);
  const list = $("chat-list");
  list.innerHTML = "";
  for (const c of chats) {
    const item = el("div", "chat-item");
    item.dataset.id = c.id;
    item.innerHTML = `<div class="chat-title"></div>
      <div class="chat-meta"><span>${c.provider}</span><span>${relTime(c.started_at)}</span></div>`;
    item.querySelector(".chat-title").textContent = c.title || "Untitled";
    item.onclick = () => openChat(c.id);
    list.appendChild(item);
  }
  highlightActiveChat();
}
function openChat(id) {
  if (id === state.conversationId) return;
  denyPending();
  // Stash the chat we're leaving so its task list is there when we come back,
  // then ask the daemon to switch. Strays from the old actor are gated out
  // until the target's `conversation_loaded` lands (see `awaitingLoad`).
  cacheTasks(state.conversationId);
  state.awaitingLoad = { mode: "load", id };
  send({ load_conversation: { id } });
  state.conversationId = id;
  desiredConversationId = id;
  sessionStorage.setItem("bone-active-conversation", String(id));
  highlightActiveChat();
}
function highlightActiveChat() {
  for (const item of document.querySelectorAll(".chat-item"))
    item.classList.toggle("active", Number(item.dataset.id) === state.conversationId);
}
function relTime(iso) {
  if (!iso) return "";
  const then = new Date(iso.endsWith("Z") || iso.includes("+") ? iso : iso + "Z").getTime();
  const s = (Date.now() - then) / 1000;
  if (s < 60) return "now";
  if (s < 3600) return Math.floor(s / 60) + "m";
  if (s < 86400) return Math.floor(s / 3600) + "h";
  if (s < 604800) return Math.floor(s / 86400) + "d";
  return new Date(then).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
function newChat() {
  denyPending();
  cacheTasks(state.conversationId);
  clearTaskList();
  state.awaitingLoad = { mode: "new", from: state.conversationId };
  send("new_conversation");
  $("thread").innerHTML = "";
  $("thread").appendChild(buildWelcome());
  finalizeTurn();
  setRunning(false);
  clearArtifacts();
  state.conversationId = null;
  desiredConversationId = null;
  sessionStorage.removeItem("bone-active-conversation");
  highlightActiveChat();
}

// ── providers / model picker ─────────────────────────────────────────────────

async function loadProviders() {
  state.providers = await fetch("/api/providers").then((r) => r.json()).catch(() => []);
  renderModelLabel();
  renderProviderPicker();
}
function renderProviderPicker() {
  const list = $("provider-list");
  list.innerHTML = "";
  for (const p of state.providers) {
    const row = el("div", "provider-row");
    row.dataset.key = p.key;
    row.innerHTML = `<span class="pr-check">✓</span><span class="pr-name"></span><span class="pr-model"></span>`;
    row.querySelector(".pr-name").textContent = p.label;
    row.querySelector(".pr-model").textContent = p.model;
    row.onclick = () => pickProvider(p.key);
    list.appendChild(row);
  }
  markActiveProvider();
}
function markActiveProvider() {
  for (const r of document.querySelectorAll(".provider-row")) r.classList.toggle("active", r.dataset.key === state.providerId);
}
function pickProvider(key) {
  if (key !== state.providerId) {
    send({ switch_provider: { provider_id: key } });
    state.providerId = key;
    const p = state.providers.find((x) => x.key === key);
    if (p && p.model) state.model = p.model;
    renderModelLabel();
    markActiveProvider();
    toast(`switched to ${p ? p.label : key}`);
  }
  $("model-pop").classList.add("hidden");
}
function toggleModelPop() {
  const pop = $("model-pop");
  const hidden = pop.classList.contains("hidden");
  pop.classList.toggle("hidden", !hidden);
  if (hidden) {
    // Anchor the dropdown just under the model chip that triggered it.
    const r = $("model-chip").getBoundingClientRect();
    pop.style.top = `${r.bottom + 6}px`;
    pop.style.left = `${r.left}px`;
    markActiveProvider();
  }
}

// ── settings: behavior / display / tools ─────────────────────────────────────

let configCache = { general: [], toolsDisabled: [] };

async function loadConfig() {
  configCache = await fetch("/api/config").then((r) => r.json()).catch(() => ({ general: [], toolsDisabled: [] }));
  // Sync approval mode from persisted config so the UI matches the daemon's
  // actual state (the daemon may have been toggled before a page refresh).
  const am = findField("approval_mode");
  if (am.value) setMode(am.value === "danger");
  renderBehavior();
  renderTools();
}

function findField(key) { return configCache.general.find((f) => f.key === key) || {}; }

async function writeConfig(namespace, key, value, type, reload) {
  await fetch("/api/config", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ namespace, key, value, type }),
  }).catch(() => {});
  if (reload) { send("reload_extensions"); toast("applied — reloading"); }
}

function setRow(label, desc, control) {
  const row = el("div", "set-row");
  const info = el("div", "set-info");
  info.appendChild(el("div", "set-label", label));
  if (desc) info.appendChild(el("div", "set-desc", desc));
  row.appendChild(info);
  const c = el("div", "set-control");
  c.appendChild(control);
  row.appendChild(c);
  return row;
}

function switchEl(checked, onChange) {
  const wrap = el("label", "switch");
  const input = document.createElement("input");
  input.type = "checkbox";
  input.checked = checked;
  input.onchange = () => onChange(input.checked);
  const track = el("span", "track");
  track.appendChild(el("span", "thumb"));
  wrap.appendChild(input);
  wrap.appendChild(track);
  return wrap;
}

function renderBehavior() {
  const wrap = $("behavior-fields");
  wrap.innerHTML = "";

  // approval mode (runtime + persisted)
  const seg = el("div", "seg");
  for (const m of ["safe", "danger"]) {
    const b = el("button", "seg-btn" + (danger === (m === "danger") ? " active" : ""));
    b.dataset.mode = m;
    b.textContent = m === "safe" ? "Safe" : "Danger";
    b.onclick = () => { setMode(m === "danger"); writeConfig("general", "approval_mode", m, "enum", false); };
    seg.appendChild(b);
  }
  seg.id = "behavior-approval-seg";
  wrap.appendChild(setRow("Tool approval", "Confirm each tool call, or let the agent run freely.", seg));

  // stream reasoning (show_thinking) — persist + drive client visibility
  const st = findField("show_thinking");
  wrap.appendChild(setRow("Show reasoning", "Display the model's thinking as it streams.",
    switchEl(prefs.showThinking, (on) => {
      prefs.showThinking = on; savePrefs(); applyPrefs();
      writeConfig("general", "show_thinking", on, "bool", false);
    })));

  // auto-compact tokens
  wrap.appendChild(setRow("Auto-compact at", "Summarise the conversation once it passes this many tokens.",
    numEl(findField("auto_compact_tokens").value || "", "tokens", (v) =>
      writeConfig("general", "auto_compact_tokens", v, "string", true))));

  // keep messages on compact
  wrap.appendChild(setRow("Keep recent messages", "How many recent messages to preserve when compacting.",
    numEl(findField("auto_compact_keep_messages").value || "", "msgs", (v) =>
      writeConfig("general", "auto_compact_keep_messages", v, "string", true))));

  // render the display pane too (shares this load)
  renderDisplay();
}

function numEl(value, suffix, onCommit) {
  const input = document.createElement("input");
  input.className = "set-num";
  input.type = "number";
  input.value = value;
  input.placeholder = suffix;
  const commit = () => onCommit(input.value.trim());
  input.onblur = commit;
  input.onkeydown = (e) => { if (e.key === "Enter") { input.blur(); } };
  return input;
}

const THEMES = [
  { id: "codex-mono", name: "Codex Mono", dot: "#ececec" },
  { id: "teal", name: "Teal", dot: "#2dd4bf" },
  { id: "green", name: "Terminal", dot: "#4ec98c" },
  { id: "slate", name: "Slate", dot: "#5b9dff" },
  { id: "purple", name: "Purple", dot: "#8b7bff" },
  { id: "auto", name: "Auto", dot: "linear-gradient(135deg, #8b7bff, #2dd4bf)" },
];
function themePicker() {
  const wrap = el("div", "theme-swatches");
  for (const t of THEMES) {
    const b = el("button", "swatch" + (prefs.theme === t.id ? " active" : ""));
    const dot = el("span", "dot");
    dot.style.background = t.dot;
    b.appendChild(dot);
    b.appendChild(document.createTextNode(t.name));
    b.onclick = () => { prefs.theme = t.id; savePrefs(); applyThemePref(); renderDisplay(); };
    wrap.appendChild(b);
  }
  return wrap;
}

function renderDisplay() {
  const wrap = $("display-fields");
  wrap.innerHTML = "";
  wrap.appendChild(setRow("Theme", "Accent and surface palette for the interface.", themePicker()));
  wrap.appendChild(setRow("Expand tool calls", "Open tool cards automatically instead of collapsed.",
    switchEl(prefs.expandTools, (on) => { prefs.expandTools = on; savePrefs(); })));
  wrap.appendChild(setRow("Context meter", "Show the token/cost meter in the header.",
    switchEl(prefs.showMeter, (on) => { prefs.showMeter = on; savePrefs(); applyPrefs(); })));
}

function renderTools() {
  const wrap = $("tools-fields");
  wrap.innerHTML = "";
  if (!state.toolDefs.length) { wrap.appendChild(el("div", "set-desc", "Tool list loads once connected.")); return; }
  const disabled = new Set(configCache.toolsDisabled || []);
  for (const t of state.toolDefs) {
    const desc = (t.description || "").split("\n")[0].slice(0, 70);
    wrap.appendChild(setRow(t.name, desc, switchEl(!disabled.has(t.name), (on) => {
      writeConfig("tools", t.name, on, "bool", true);
    })));
  }
}

// ── settings modal shell ──────────────────────────────────────────────────────

function openSettings() {
  renderBehavior();
  renderTools();
  renderSettingsStats();
  $("settings-overlay").classList.remove("hidden");
}
function closeSettings() { $("settings-overlay").classList.add("hidden"); }

function switchTab(tab) {
  for (const b of document.querySelectorAll(".stab")) b.classList.toggle("active", b.dataset.tab === tab);
  for (const p of document.querySelectorAll(".settings-pane")) p.classList.toggle("hidden", p.dataset.pane !== tab);
}

function renderSettingsStats() {
  const s = state.snapshot || {};
  const kv = $("settings-stats");
  if (!kv) return;
  kv.innerHTML = `
    <div class="k">Conversation</div><div class="v">${s.conversation_id ?? "—"}</div>
    <div class="k">Messages</div><div class="v">${s.transcript_len ?? 0}</div>
    <div class="k">Tokens sent</div><div class="v">${fmt(s.sent || 0)}</div>
    <div class="k">Tokens received</div><div class="v">${fmt(s.received || 0)}</div>
    <div class="k">Requests</div><div class="v">${s.request_count ?? 0}</div>
    <div class="k">Cost</div><div class="v">$${(s.cost || 0).toFixed(4)}</div>`;
}

// ── approval mode (composer pill + behavior seg) ─────────────────────────────

let danger = false;
function setMode(d) {
  danger = d;
  const btn = $("mode-toggle");
  btn.classList.toggle("mode-safe", !danger);
  btn.classList.toggle("mode-danger", danger);
  $("mode-label").textContent = danger ? "Danger" : "Safe";
  send({ set_approval_mode: { mode: danger ? "danger" : "safe" } });
  const seg = $("behavior-approval-seg");
  if (seg) for (const b of seg.children) b.classList.toggle("active", b.dataset.mode === (danger ? "danger" : "safe"));
}

// ── display prefs ─────────────────────────────────────────────────────────────

function loadPrefs() {
  let p = {};
  try { p = JSON.parse(localStorage.getItem("bone-studio-prefs") || "{}"); } catch {}
  return { showThinking: p.showThinking !== false, expandTools: !!p.expandTools, showMeter: p.showMeter !== false, theme: p.theme || "codex-mono" };
}
function savePrefs() { localStorage.setItem("bone-studio-prefs", JSON.stringify(prefs)); }
function applyPrefs() {
  document.body.classList.toggle("hide-thinking", !prefs.showThinking);
  document.body.classList.toggle("hide-meter", !prefs.showMeter);
  applyThemePref();
}
function applyThemePref() {
  // Drop any inline accent the runtime may have set, then hand off to the CSS
  // palette. "auto" keeps the legacy purple base and re-accepts runtime accents.
  document.documentElement.style.removeProperty("--accent");
  if (prefs.theme && prefs.theme !== "auto") document.documentElement.dataset.theme = prefs.theme;
  else delete document.documentElement.dataset.theme;
}

// ── running state ──────────────────────────────────────────────────────────

function setRunning(on) {
  state.running = on;
  $("stop").classList.toggle("hidden", !on);
  $("send").classList.toggle("hidden", on);
}

// ── composer ───────────────────────────────────────────────────────────────

const input = $("input");
function autosize() {
  input.style.height = "auto";
  input.style.height = Math.min(input.scrollHeight, 240) + "px";
  $("send").disabled = !input.value.trim();
}
function submit() {
  const text = input.value.trim();
  if (!text || state.running) return;
  // Remember the message so we can restore it if the daemon rejects it as busy.
  state.lastBubble = userMessage(text);
  state.lastText = text;
  send({ submit_prompt: { text, images: [] } });
  input.value = "";
  autosize();
}

// ── welcome / suggestions ────────────────────────────────────────────────────

const SUGGESTIONS = [
  { title: "Explore this codebase", sub: "Map the project structure", text: "Give me a high-level tour of this codebase." },
  { title: "Find and fix a bug", sub: "Investigate then patch", text: "Look for a likely bug and propose a fix." },
  { title: "Write a test", sub: "Cover an existing function", text: "Add a unit test for an important function." },
  { title: "Explain a file", sub: "Walk through the logic", text: "Pick an interesting file and explain how it works." },
];
function buildWelcome() {
  const w = el("div", "welcome");
  w.id = "welcome";
  w.innerHTML = `<div class="welcome-mark">⠿</div><h1>bone studio</h1>
    <p>A calm, elegant front-end for your bone agent.</p><div class="suggestions"></div>`;
  const wrap = w.querySelector(".suggestions");
  for (const s of SUGGESTIONS) {
    const card = el("div", "suggestion", `<div class="s-title">${s.title}</div><div class="s-sub">${s.sub}</div>`);
    card.onclick = () => { input.value = s.text; autosize(); input.focus(); };
    wrap.appendChild(card);
  }
  return w;
}

// ── toast ──────────────────────────────────────────────────────────────────

let toastTimer;
function toast(msg) {
  const t = $("toast");
  t.textContent = msg;
  t.classList.remove("hidden");
  requestAnimationFrame(() => t.classList.add("show"));
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.classList.remove("show"); setTimeout(() => t.classList.add("hidden"), 250); }, 2200);
}

// ── markdown (compact, escaped-first) ────────────────────────────────────────

function escapeHtml(s) { return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;"); }
function inlineMd(s) {
  const t = escapeHtml(s);
  return t
    .replace(/`([^`]+)`/g, (_, c) => `<code>${c}</code>`)
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*])\*([^*]+)\*/g, "$1<em>$2</em>")
    .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noreferrer">$1</a>');
}
function renderMarkdown(src) {
  const lines = src.split("\n");
  let html = "", i = 0, listType = null;
  const closeList = () => { if (listType) { html += `</${listType}>`; listType = null; } };
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^\s*```(\w*)/);
    if (fence) { closeList(); const buf = []; i++; while (i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i++]); i++; html += `<pre><code>${escapeHtml(buf.join("\n"))}</code></pre>`; continue; }
    if (/^\s*$/.test(line)) { closeList(); i++; continue; }
    const h = line.match(/^(#{1,3})\s+(.*)/);
    if (h) { closeList(); html += `<h${h[1].length}>${inlineMd(h[2])}</h${h[1].length}>`; i++; continue; }
    const ul = line.match(/^\s*[-*]\s+(.*)/);
    if (ul) { if (listType !== "ul") { closeList(); html += "<ul>"; listType = "ul"; } html += `<li>${inlineMd(ul[1])}</li>`; i++; continue; }
    const ol = line.match(/^\s*\d+\.\s+(.*)/);
    if (ol) { if (listType !== "ol") { closeList(); html += "<ol>"; listType = "ol"; } html += `<li>${inlineMd(ol[1])}</li>`; i++; continue; }
    const bq = line.match(/^\s*>\s?(.*)/);
    if (bq) { closeList(); html += `<blockquote>${inlineMd(bq[1])}</blockquote>`; i++; continue; }
    if (/^\s*([-*_])\1\1+\s*$/.test(line)) { closeList(); html += "<hr/>"; i++; continue; }

    // tables: a `| … |` header row followed by a `|---|---|` separator
    if (/^\s*\|.*\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|[\s:|-]*$/.test(lines[i + 1])) {
      closeList();
      const cells = (r) => r.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
      const head = cells(line);
      i += 2;
      let body = "";
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) {
        body += "<tr>" + cells(lines[i]).map((c) => `<td>${inlineMd(c)}</td>`).join("") + "</tr>";
        i++;
      }
      html += `<table><thead><tr>${head.map((c) => `<th>${inlineMd(c)}</th>`).join("")}</tr></thead><tbody>${body}</tbody></table>`;
      continue;
    }
    closeList();
    const para = [line]; i++;
    while (i < lines.length && lines[i].trim() && !/^\s*(#{1,3}\s|[-*]\s|\d+\.\s|>|```)/.test(lines[i])) para.push(lines[i++]);
    html += `<p>${inlineMd(para.join("<br/>"))}</p>`;
  }
  closeList();
  return html;
}

// ── wiring ──────────────────────────────────────────────────────────────────

input.addEventListener("input", autosize);
input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); submit(); } });
$("send").onclick = submit;
$("stop").onclick = () => { denyPending(); send("cancel"); };
  window.addEventListener("keydown", (e) => { if (e.key === "Escape") { $("model-pop").classList.add("hidden"); closeSettings(); } });
  // ── Stats ───────────────────────────────────────────────────────────────────

const statsState = {
  open: false,
  mode: "today",
  data: null,
  loaded: null,
};

const MODE_LABELS = { today: "Today", "7d": "7 days", "4w": "4 weeks", yearly: "Yearly", all: "All time" };

async function loadStats() {
  $("stats-body").classList.add("loading");
  const refreshedEl = $("stats-refreshed");
  if (refreshedEl) refreshedEl.textContent = "loading…";
  try {
    const res = await fetch("/api/stats");
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    statsState.data = await res.json();
    statsState.loaded = new Date();
    renderStats();
  } catch (e) {
    console.error("stats load failed:", e);
    toast("failed to load stats");
  } finally {
    $("stats-body").classList.remove("loading");
  }
}

// Map a view mode to the snapshot keys for the time-series chart, the
// model breakdown, and the by-hour-of-day distribution. "yearly" reuses the
// all-time model/hourly slices (no per-year breakdown is stored).
function modeKeys(mode) {
  const m = mode === "yearly" ? "all" : mode;
  return {
    buckets: mode === "today" ? "daily" : mode === "7d" ? "weekly" : mode === "4w" ? "monthly" : mode === "yearly" ? "yearly" : "all_time",
    models: `by_model_${m}`,
    hourly: `hourly_${m}`,
  };
}

function chartLabel(mode, b) {
  const s = b.label || "";
  if (mode === "today") return s.slice(0, 2); // "00:00" -> "00"
  if (mode === "7d") return s.slice(5);        // "2025-06-29" -> "06-29"
  if (mode === "all") return s.slice(2);       // "2025-06" -> "25-06"
  return s;                                    // week ("2025-W26") / year ("2025")
}

function money(x) {
  return x >= 0.01 ? "$" + x.toFixed(2) : "$" + x.toFixed(4);
}

// Vertical column chart: each bar stacks completion (accent) over prompt (dim).
// `rows` may be time buckets or hourly rows; both carry prompt/completion tokens.
function renderColChart(rows, { height = 150, labelFn } = {}) {
  if (!rows || !rows.length) return '<div class="stats-empty">No data</div>';
  const totals = rows.map((r) => r.prompt_tokens + r.completion_tokens);
  const max = Math.max(...totals, 1);
  const step = Math.max(1, Math.ceil(rows.length / 10));
  const cls = height !== 150 ? "stats-chart stats-chart-sm" : "stats-chart";
  return `<div class="${cls}" style="height:${height}px">${rows.map((r, i) => {
    const total = totals[i];
    const pct = (total / max) * 100;
    const pr = r.prompt_tokens, cp = r.completion_tokens;
    const lbl = labelFn ? labelFn(r, i) : r.label;
    return `<div class="stats-col" title="${escapeHtml(lbl)} · ${fmt(total)} (prompt ${fmt(pr)} · completion ${fmt(cp)})">
      <div class="stats-col-stack" style="height:${pct}%">
        ${cp > 0 ? `<div class="stats-col-seg seg-comp" style="flex-grow:${cp}"></div>` : ""}
        ${pr > 0 ? `<div class="stats-col-seg seg-prompt" style="flex-grow:${pr}"></div>` : ""}
      </div>
      <div class="stats-col-label">${i % step === 0 ? escapeHtml(String(lbl)) : ""}</div>
    </div>`;
  }).join("")}</div>`;
}

function renderModelsTable(models, total) {
  const head = `<div class="stats-row stats-table-head">
    <span class="provider">Provider / Model</span>
    <span class="num">Prompt</span>
    <span class="num">Completion</span>
    <span class="num cost">Cost</span>
  </div>`;
  const rows = models.map((m) => {
    const cached = m.cached_tokens > 0 ? `<span style="color:var(--text-faint);font-size:11px"> +${fmt(m.cached_tokens)} cached</span>` : '';
    return `<div class="stats-row stats-table-row">
    <span class="provider"><span class="prov-badge">${escapeHtml(m.provider)}</span><span class="prov-model" title="${escapeHtml(m.model)}">${escapeHtml(m.model)}</span></span>
    <span class="num" title="${fmt(m.prompt_tokens)} prompt${m.cached_tokens ? ' · ' + fmt(m.cached_tokens) + ' cached' : ''}">${fmt(m.prompt_tokens)}${cached}</span>
    <span class="num">${fmt(m.completion_tokens)}</span>
    <span class="num cost">${money(m.cost)}</span>
  </div>`;
  }).join("");
  const foot = `<div class="stats-row stats-table-foot">
    <span class="provider"><span class="prov-badge">Total</span></span>
    <span class="num">${fmt(total.prompt_tokens)}</span>
    <span class="num">${fmt(total.completion_tokens)}</span>
    <span class="num cost">${money(total.cost)}</span>
  </div>`;
  return `<div class="stats-table">${head}${rows}${foot}</div>`;
}

function renderStats() {
  const d = statsState.data;
  if (!d) return;
  const mode = statsState.mode;
  const keys = modeKeys(mode);

  // KPI cards + summary are derived from the model breakdown for this window,
  // so the cards, summary line and model table always agree with each other.
  const models = d[keys.models] || [];
  const t = models.reduce((a, m) => ({
    prompt_tokens: a.prompt_tokens + m.prompt_tokens,
    completion_tokens: a.completion_tokens + m.completion_tokens,
    cached_tokens: a.cached_tokens + m.cached_tokens,
    cost: a.cost + m.cost,
    request_count: a.request_count + m.request_count,
  }), { prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0, request_count: 0 });

  // Summary line
  const since = d.started_at ? d.started_at.slice(0, 10) : "—";
  $("stats-range").innerHTML =
    `<b>${fmt(t.request_count)}</b> requests · <b>${money(t.cost)}</b> · ` +
    `<b>${models.length}</b> model${models.length === 1 ? "" : "s"} · since ${escapeHtml(since)}`;

  // KPI cards — hero row (cost + total tokens) + metric row
  const tokens = t.prompt_tokens + t.completion_tokens;
  const cachePct = t.prompt_tokens > 0 ? Math.round((t.cached_tokens / t.prompt_tokens) * 100) : 0;
  $("stats-cards").innerHTML =
    `<div class="stats-card-item hero">
      <div class="stats-card-label">Total cost</div>
      <div class="stats-card-value">${money(t.cost)}</div>
      <div class="stats-card-sub">${fmt(t.request_count)} requests · ${fmt(tokens)} tokens</div>
    </div>
    <div class="stats-card-item hero">
      <div class="stats-card-label">Total tokens</div>
      <div class="stats-card-value">${fmt(tokens)}</div>
      <div class="stats-card-sub">${fmt(t.prompt_tokens)} prompt · ${fmt(t.completion_tokens)} completion</div>
    </div>`;
  $("stats-cards-row").innerHTML =
    `<div class="stats-card-item"><div class="stats-card-value">${fmt(t.request_count)}</div><div class="stats-card-label">Requests</div></div>
    <div class="stats-card-item"><div class="stats-card-value">${fmt(t.prompt_tokens)}</div><div class="stats-card-label">Prompt tokens</div></div>
    <div class="stats-card-item"><div class="stats-card-value">${fmt(t.completion_tokens)}</div><div class="stats-card-label">Completion</div></div>
    <div class="stats-card-item"><div class="stats-card-value">${fmt(t.cached_tokens)}<span style="font-size:12px;color:var(--text-faint);font-weight:400;margin-left:4px">${cachePct}%</span></div><div class="stats-card-label">Cached</div></div>`;

  // Time-series chart
  const buckets = d[keys.buckets] || [];
  $("stats-chart-sub").textContent = `· ${MODE_LABELS[mode]}`;
  $("stats-chart").innerHTML = renderColChart(buckets, { labelFn: (b) => chartLabel(mode, b) });

  // Models table
  $("stats-models").innerHTML = models.length
    ? renderModelsTable(models, t)
    : '<div class="stats-empty">No model data</div>';

  // By hour of day — redundant with today's per-hour main chart, so hide it there.
  const hourlySection = $("stats-hourly-section");
  if (mode === "today") {
    hourlySection.classList.add("hidden");
  } else {
    hourlySection.classList.remove("hidden");
    $("stats-hourly").innerHTML = renderColChart(d[keys.hourly] || [], {
      height: 96,
      labelFn: (h) => `${String(h.hour).padStart(2, "0")}h`,
    });
  }

  const refreshedEl = $("stats-refreshed");
  if (refreshedEl && statsState.loaded) {
    refreshedEl.textContent = `updated ${Math.round((Date.now() - statsState.loaded.getTime()) / 1000)}s ago`;
  }
}

function openStats() {
  statsState.open = true;
  $("stats-overlay").classList.remove("hidden");
  loadStats();
}

function closeStats() {
  statsState.open = false;
  $("stats-overlay").classList.add("hidden");
}

function toggleStats() {
  statsState.open ? closeStats() : openStats();
}

// Stats event listeners
$("stats-btn").onclick = openStats;
$("stats-close").onclick = closeStats;
$("stats-refresh").onclick = () => loadStats();
$("stats-overlay").addEventListener("click", (e) => { if (e.target === $("stats-overlay")) closeStats(); });
for (const b of document.querySelectorAll(".stats-mode")) {
  b.onclick = () => {
    statsState.mode = b.dataset.mode;
    document.querySelectorAll(".stats-mode").forEach((m) => m.classList.toggle("active", m === b));
    renderStats();
  };
}

// Keyboard shortcuts for stats
window.addEventListener("keydown", (e) => {
  if (!statsState.open) return;
  if (e.key === "q" || e.key === "Escape") { closeStats(); return; }
  if (e.key === "r") { loadStats(); return; }
  const modeMap = { "1": "today", "2": "7d", "3": "4w", "4": "yearly", "5": "all" };
  if (modeMap[e.key]) {
    statsState.mode = modeMap[e.key];
    document.querySelectorAll(".stats-mode").forEach((m) => m.classList.toggle("active", m.dataset.mode === statsState.mode));
    renderStats();
  }
});
window.addEventListener("beforeunload", () => denyPending(true));
$("new-chat").onclick = newChat;
$("settings-btn").onclick = openSettings;
$("settings-close").onclick = closeSettings;
$("model-chip").onclick = toggleModelPop;
$("mode-toggle").onclick = () => setMode(!danger);
$("collapse-btn").onclick = () => { $("app").classList.add("sidebar-hidden"); $("show-sidebar").classList.remove("hidden"); };
$("show-sidebar").onclick = () => { $("app").classList.remove("sidebar-hidden"); $("show-sidebar").classList.add("hidden"); };
$("canvas-toggle").onclick = toggleCanvas;
$("canvas-close").onclick = closeCanvas;
for (const b of document.querySelectorAll(".stab")) b.onclick = () => switchTab(b.dataset.tab);

// Draggable divider: resize the canvas by dragging its left edge.
$("divider").addEventListener("mousedown", (e) => {
  e.preventDefault();
  const divider = $("divider");
  const work = $("work");
  divider.classList.add("dragging");
  document.body.style.cursor = "col-resize";
  const onMove = (ev) => {
    const rect = work.getBoundingClientRect();
    const w = Math.max(320, Math.min(rect.width * 0.7, rect.right - ev.clientX));
    document.documentElement.style.setProperty("--canvas-w", w + "px");
  };
  const onUp = () => {
    divider.classList.remove("dragging");
    document.body.style.cursor = "";
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
  };
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
});

document.addEventListener("click", (e) => {
  const pop = $("model-pop");
  if (!pop.classList.contains("hidden") && !pop.contains(e.target) && !e.target.closest("#model-chip")) pop.classList.add("hidden");
});
document.addEventListener("keydown", (e) => { if (e.key === "Escape") { $("model-pop").classList.add("hidden"); closeSettings(); } });
$("settings-overlay").addEventListener("click", (e) => { if (e.target === $("settings-overlay")) closeSettings(); });
window.addEventListener("keydown", captureKey, true);

applyPrefs();
autosize();
connect();
loadChats();
loadProviders();
loadConfig();
setTimeout(() => send({ set_terminal_width: { width: 100 } }), 400);
