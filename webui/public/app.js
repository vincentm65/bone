import { DraftStore, MAX_ATTACHMENTS, buildSubmission, downloadText, fileToAttachment, requestJson } from "./ui-core.js";
import { escapeHtml, highlightCode, renderMarkdown } from "./markdown.js";
import { artifactText, parseDiff } from "./canvas-core.js";

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
const drafts = new DraftStore(localStorage);
let attachments = [];
const storedConversationId = Number(sessionStorage.getItem("bone-active-conversation"));
let desiredConversationId = Number.isInteger(storedConversationId) && storedConversationId > 0
  ? storedConversationId
  : null;

const state = {
  session: null,
  running: false,
  sending: false,
  asstEl: null,
  asstRaw: "",
  reasonEl: null,
  reasonDetails: null,
  tools: new Map(),
  approvals: new Map(),
  connected: false,
  conversationId: null,
  providers: [],
  providerId: null,
  model: null,
  snapshot: {},
  toolDefs: [],
  commands: [],
  commandIndex: -1,
  commandRunning: false,
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
  // Background conversations kept live while we view another (see watch links in
  // bridge.mjs): `watched` is the set the bridge holds an extra socket for,
  // `runningConvs` the subset still mid-turn (drives the sidebar "running" dot).
  watched: new Set(),
  runningConvs: new Set(),
  pendingWorkElapsed: null,
  // conversation id -> Date.now() when its current turn started; drives the
  // live elapsed timer next to each running chat in the sidebar.
  runStart: new Map(),
  // Did we observe this turn's `started`? A page refresh mid-response reconnects
  // partway through a turn: the DB replay only reaches the last user message and
  // the streamed head is already gone, so we catch only the tail. The daemon
  // persists the whole turn before `turn_complete`, so when we join mid-turn we
  // reload from the DB on completion to recover the full response.
  sawStarted: false,
  // A "New chat" was clicked but not yet used. Shows an ephemeral placeholder row
  // at the top of the sidebar as a visual hint; cleared when the chat gains
  // messages (becomes a real listed conversation) or the user opens another chat.
  draftChat: false,
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
// In-flight runtime events are not persisted until a turn completes. Keep the
// complete live tail per conversation so navigating away and back can rebuild
// the transcript from: DB replay + this tail. Completed turns drop their tail;
// the next load then comes entirely from the authoritative database.
const liveEventCache = new Map();
let replayingLiveEvents = false;
let conversations = [];

const LIVE_EVENT_TYPES = new Set([
  "started", "notice", "reasoning_delta", "text_delta", "tool_call",
  "tool_result", "tool_output", "token_usage", "approval_request", "key_request",
  "finished", "failed", "work_elapsed", "view_diff",
]);

function cacheLiveEvent(convId, ev) {
  if (replayingLiveEvents || convId == null || !LIVE_EVENT_TYPES.has(ev.type)) return;
  if (ev.type === "started") liveEventCache.set(convId, []);
  const events = liveEventCache.get(convId) || [];
  events.push(ev);
  liveEventCache.set(convId, events);
}

function replayLiveTail(convId) {
  const events = liveEventCache.get(convId);
  if (!events || !events.length) return;
  state.sawStarted = false;
  replayingLiveEvents = true;
  try { for (const ev of events) dispatchEvent(ev); }
  finally { replayingLiveEvents = false; }
}

function dropCachedApproval(convId, approvalId) {
  const events = liveEventCache.get(convId);
  if (!events) return;
  liveEventCache.set(convId, events.filter(
    (ev) => ev.type !== "approval_request" || ev.id !== approvalId,
  ));
}

// ── connection ──────────────────────────────────────────────────────────────

function connect() {
  setConnectionState("connecting");
  const es = new EventSource("/api/events");
  es.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.kind === "bridge") return onBridge(msg);
    if (msg.kind === "watch") return onWatchEvent(msg.conversation_id, normalize(msg.payload));
    if (msg.kind === "event") return onEvent(normalize(msg.payload));
  };
  es.onerror = () => setConnectionState("reconnecting");
}

function onBridge(msg) {
  if (msg.session) state.session = msg.session;
  if (msg.status === "connected") {
    setConnectionState("connected");
    clearRecovery();
    // A reconnect creates a fresh TCP connection, which initially attaches to
    // the daemon's latest conversation. Restore this tab's own selection.
    if (desiredConversationId != null) {
      state.awaitingLoad = { mode: "load", id: desiredConversationId };
      send({ load_conversation: { id: desiredConversationId } });
    }
    // A reconnect is a fresh bridge session with no watch links — re-open one for
    // each background conversation still running (except the one now in view).
    state.watched.clear();
    for (const id of state.runningConvs)
      if (id !== state.conversationId) watchConversation(id);
  }
  if (msg.status === "disconnected") { setConnectionState("reconnecting"); toast("Daemon disconnected — reconnecting…"); }
}

function setConnectionState(status) {
  state.connected = status === "connected";
  const dot = $("conn-dot");
  dot.classList.toggle("online", status === "connected");
  dot.classList.toggle("offline", status === "offline");
  dot.classList.toggle("connecting", status === "connecting" || status === "reconnecting");
  const label = status[0].toUpperCase() + status.slice(1);
  $("conn-label").textContent = label;
  $("model-chip").title = `${label} · Change model`;
  announce(label);
}

function normalize(payload) {
  if (typeof payload === "string") return { type: payload };
  const type = Object.keys(payload)[0];
  return { type, ...payload[type] };
}

async function send(command) {
  try {
    if (!state.session || !state.connected) return false;
    const response = await fetch(`/api/command?session=${state.session}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(command),
    });
    if (!response.ok) throw new Error((await response.text()) || "Command failed");
    return true;
  } catch (error) {
    toast(error.message || "Command failed");
    return false;
  }
}

// ── background watches ────────────────────────────────────────────────────────
//
// Each tab multiplexes one primary daemon connection for the chat on screen. To
// keep a chat we've navigated away from live (its task list updating, its running
// dot lit), we ask the bridge to hold an extra read-only socket pinned to it. The
// bridge tags those events `kind:"watch"` with the conversation id; onWatchEvent
// folds them into the sidebar/cache only — they never touch the on-screen thread.

async function watchConversation(id) {
  if (id == null || !state.session) return false;
  if (state.watched.has(id)) return true;
  state.watched.add(id);
  try {
    const response = await fetch(`/api/watch?session=${state.session}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ conversation_id: id }),
    });
    if (!response.ok) throw new Error(await response.text());
    return true;
  } catch {
    state.watched.delete(id);
    return false;
  }
}

async function unwatchConversation(id) {
  if (id == null || !state.watched.has(id)) return;
  state.watched.delete(id);
  await fetch(`/api/unwatch?session=${state.session}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ conversation_id: id }),
  }).catch(() => {});
}

// Events from a background conversation are retained as its live, unpersisted
// tail. They never mutate the visible thread until that conversation is opened.
function onWatchEvent(convId, ev) {
  if (convId == null || (convId === state.conversationId && !state.awaitingLoad)) return;
  cacheLiveEvent(convId, ev);
  switch (ev.type) {
    case "started":
      state.runningConvs.add(convId);
      markRunning(convId, true);
      updateRunningIndicators();
      return;
    case "turn_complete":
      state.runningConvs.delete(convId);
      markRunning(convId, false);
      unwatchConversation(convId);
      liveEventCache.delete(convId);
      updateRunningIndicators();
      loadChats();
      return;
    case "failed":
      state.runningConvs.delete(convId);
      markRunning(convId, false);
      unwatchConversation(convId);
      updateRunningIndicators();
      loadChats();
      return;
    case "view_diff":
      cacheWatchDiff(convId, ev.diff);
      return;
    default:
      return;
  }
}

// Fold a background conversation's task-pane diff straight into taskCache so
// restoreTasks() shows the up-to-date list the moment we switch back to it.
function cacheWatchDiff(convId, diff) {
  if (!diff) return;
  const up = diff.upsert && diff.upsert.component;
  if (up && up.id === "task_list" && up.lines) {
    taskCache.set(convId, { title: up.title || "Tasks", items: parseTaskLines(up.lines) });
  } else if (diff.remove && diff.remove.id === "task_list") {
    taskCache.delete(convId);
  }
}

// A conversation is "running" if it's the active turn or a watched background turn.
function isConvRunning(id) {
  return state.runningConvs.has(id) || (state.running && id === state.conversationId);
}
// Record/clear the turn-start time that the sidebar elapsed timer counts from.
function markRunning(convId, on) {
  if (convId == null) return;
  if (on) { if (!state.runStart.has(convId)) state.runStart.set(convId, Date.now()); }
  else state.runStart.delete(convId);
}
function updateRunningIndicators() {
  for (const item of document.querySelectorAll(".chat-item")) {
    const id = Number(item.dataset.id);
    const running = isConvRunning(id);
    item.classList.toggle("running", running);
    // A chat we rejoined mid-turn has no recorded start; count from now so the
    // timer shows something sensible rather than staying blank.
    if (running && !state.runStart.has(id)) state.runStart.set(id, Date.now());
  }
  tickRunningTimers();
}
function formatElapsed(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(s / 60), sec = String(s % 60).padStart(2, "0");
  if (m < 60) return `${m}:${sec}`;
  return `${Math.floor(m / 60)}:${String(m % 60).padStart(2, "0")}:${sec}`;
}
// Refresh every running row's elapsed timer; ticked once a second.
function tickRunningTimers() {
  for (const item of document.querySelectorAll(".chat-item.running")) {
    const timer = item.querySelector(".chat-timer");
    if (!timer) continue;
    const start = state.runStart.get(Number(item.dataset.id));
    timer.textContent = start ? formatElapsed(Date.now() - start) : "";
  }
}
setInterval(tickRunningTimers, 1000);

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
  if (state.awaitingLoad && !routing) {
    // Frames already queued by the actor we just left still belong to its live
    // tail. Preserve them instead of either bleeding them into the target or
    // losing them during the hand-off to its watch connection.
    cacheLiveEvent(state.awaitingLoad.from, ev);
    return;
  }
  if (!routing) cacheLiveEvent(state.conversationId, ev);
  return dispatchEvent(ev);
}

function dispatchEvent(ev) {
  switch (ev.type) {
    case "frontend_state": return onFrontendState(ev);
    case "state_snapshot": return onSnapshot(ev.snapshot);
    case "conversation_loaded": return onConversationLoaded(ev);
    case "started":
      state.sawStarted = true;
      // A turn we didn't submit ourselves is a daemon-injected one — typically
      // background sub-agent results being handed to the model.
      if (!state.sending && !replayingLiveEvents) resolveBackgroundAgents();
      markRunning(state.conversationId, true); setRunning(true); showThinking(); return;
    case "status": return onStatus(ev.message);
    case "notice": return systemLine(ev.message);
    case "reasoning_delta": return appendReasoning(ev.text);
    case "text_delta": return appendText(ev.text);
    case "tool_call": return onToolCall(ev);
    case "tool_result": return onToolResult(ev);
    case "tool_output": return onToolOutput(ev);
    case "token_usage": return onTokenUsage(ev);
    case "approval_request": return onApproval(ev);
    case "key_request": return onKeyRequest(ev);
    case "finished": return onFinished(ev);
    case "failed": return onFailed(ev);
    case "work_elapsed": state.pendingWorkElapsed = ev.elapsed_ms; return;
    case "turn_complete": return onTurnComplete();
    case "view_diff": return onViewDiff(ev.diff);
    case "command_complete": return onCommandComplete(ev);
    default: return;
  }
}

function onFrontendState(ev) {
  if (Array.isArray(ev.tool_defs)) state.toolDefs = ev.tool_defs;
  if (Array.isArray(ev.commands)) state.commands = ev.commands;
  applyTheme(ev.settings?.theme);
}

function plainTerminalText(text) {
  return String(text || "").replace(/\x1b\[[0-?]*[ -\/]*[@-~]/g, "");
}

async function applyCommandAction(action) {
  if (!action) return;
  if (action.conversation_replace) {
    await send({ replace_conversation: { messages: action.conversation_replace } });
  }
  if (action.conversation_load?.conversation_id != null) {
    await openChat(action.conversation_load.conversation_id);
  }
  const config = action.config_action;
  if (config === "reload_tools") await send("reload_extensions");
  else if (config === "apply" || config === "apply_restart_required") {
    await send("reload_settings");
    await send({ switch_provider: { provider_id: state.providerId } });
  }
  else if (config?.switch_provider?.id) await send({ switch_provider: { provider_id: config.switch_provider.id } });
}

async function onCommandComplete(ev) {
  setCommandRunning(false);
  state.lastBubble = null;
  await applyCommandAction(ev.action);
  const output = plainTerminalText(ev.output).trim();
  // Submitting commands feed their output to the model; the daemon's normal
  // started/delta events render that turn. Display-only commands need a result.
  if (output && !ev.submit) {
    if (ev.display_role === "assistant") {
      const t = turn("assistant");
      const prose = el("div", "prose");
      prose.innerHTML = renderMarkdown(output); t.appendChild(prose); enhanceContent(t); scrollDown();
    } else {
      clearWelcome();
      const line = el("div", "system-line command-result");
      line.textContent = output; $("thread").appendChild(line); scrollDown();
    }
  }
  autosize();
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
    const previousConversationId = state.conversationId;
    const changed = state.conversationId !== s.conversation_id;
    state.conversationId = s.conversation_id;
    desiredConversationId = s.conversation_id;
    sessionStorage.setItem("bone-active-conversation", String(s.conversation_id));
    if (previousConversationId == null && drafts.get(null) && !drafts.get(s.conversation_id)) drafts.move(null, s.conversation_id);
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

function userMessage(text, images = []) {
  const t = turn("user");
  t.appendChild(el("div", "role-tag", "You"));
  t.appendChild(el("div", "bubble")).textContent = text;
  if (images.length) {
    const gallery = el("div", "message-images");
    for (const image of images) {
      const img = document.createElement("img");
      img.src = image.preview || `data:${image.media_type};base64,${image.data}`;
      img.alt = image.name || "Attached image"; img.loading = "lazy"; gallery.appendChild(img);
    }
    t.appendChild(gallery);
  }
  scrollDown();
  return t;
}

// A lightweight "bone is working" placeholder shown from the moment a turn starts
// until the first real output (prose, a tool call, or visible reasoning) lands, so
// there's never a silent gap. Kept distinct from the reasoning block: when
// reasoning is hidden by preference this is the only sign the agent is thinking.
function showThinking() {
  if ($("thinking")) return;
  clearWelcome();
  const t = el("div", "turn msg-assistant thinking-turn");
  t.id = "thinking";
  t.setAttribute("aria-label", "Thinking");
  t.innerHTML = `<div class="thinking"><span class="thinking-spinner" aria-hidden="true"></span><span>Thinking…</span></div>`;
  $("thread").appendChild(t);
  scrollDown();
}
function hideThinking() {
  const n = $("thinking"); if (n) n.remove();
}

function ensureAssistant() {
  // Streaming output implies a live turn. When we re-attach to a chat that is
  // already mid-turn we may miss its `started` event, so infer running here to
  // keep the Stop button (and composer state) correct.
  if (!state.running) setRunning(true);
  if (state.asstEl) return;
  const t = turn("assistant");
  t.appendChild(el("div", "role-tag", ""));
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
  hideThinking();
  // Remove thinking once prose starts — it's no longer relevant.
  if (state.reasonDetails) { state.reasonDetails.remove(); state.reasonDetails = null; state.reasonEl = null; }
  ensureAssistant();
  state.asstRaw += text;
  state.asstEl.innerHTML = renderMarkdown(state.asstRaw) + '<span class="caret"></span>';
  state.asstEl.parentElement.appendChild(state.asstEl); // keep prose last
  scrollDown();
}

function appendReasoning(text) {
  // The reasoning block is itself a thinking indicator — retire the generic one,
  // unless reasoning is hidden by preference (then the generic one is all we have).
  if (!document.body.classList.contains("hide-thinking")) hideThinking();
  ensureAssistant();
  if (!state.reasonEl) {
    const d = el("details", "reasoning");
    d.appendChild(el("summary", null, `<span class="reasoning-spark" aria-hidden="true"></span><span class="reasoning-title">Thinking</span><span class="reasoning-preview"></span><svg class="reasoning-chevron" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg>`));
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
  state.reasonDetails.querySelector(".reasoning-preview").textContent = preview + dots;
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
  if (name === "subagent") return { verb: "Agents", arg: subagentSummary(args) };
  const verb = TOOL_VERBS[name] || name.replace(/_/g, " ");
  const argKeys = ["command", "cmd", "path", "file_path", "file", "query", "pattern", "url", "name"];
  let arg = "";
  for (const k of argKeys) if (typeof args[k] === "string") { arg = args[k]; break; }
  if (!arg) { const v = Object.values(args).find((x) => typeof x === "string"); if (v) arg = v; }
  return { verb, arg };
}

function onToolCall(ev) {
  hideThinking();
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
  state.toolInfo.set(ev.id, { name: ev.name, arguments: ev.arguments, startedAt: performance.now() });
  const { verb, arg } = toolMeta(ev.name, ev.arguments);
  const card = el("div", "tool running" + (prefs.expandTools ? " open" : ""));
  card.innerHTML = `
    <div class="tool-head" role="button" tabindex="0" aria-expanded="${prefs.expandTools ? "true" : "false"}">
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
  if (ev.name === "subagent") {
    const rows = buildAgentRows(ev.arguments, false);
    if (rows.childElementCount) card.insertBefore(rows, body);
  }
  const head = card.querySelector(".tool-head");
  const toggleTool = () => { card.classList.toggle("open"); head.setAttribute("aria-expanded", card.classList.contains("open")); };
  head.onclick = toggleTool;
  head.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggleTool(); } };

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
    const isCompletion = card.dataset.liveOutput;
    card.querySelector(".tool-body").appendChild(
      el("div", "tool-section-label", (isCompletion ? "Completion" : (ev.is_error ? "Error" : "Output")) + ` · ${lines} line${lines === 1 ? "" : "s"}`),
    );
    const pre = el("pre", ev.is_error ? "err" : null);
    pre.textContent = formatToolOutput(content);
    card.querySelector(".tool-body").appendChild(pre);
  }
  // Surface an edit's diff in the canvas. The result content embeds bone's
  // numbered unified diff (see core/src/tools/edit_file/diff.rs).
  const info = state.toolInfo.get(ev.call_id);
  const elapsed = info?.startedAt ? Math.max(0, performance.now() - info.startedAt) : null;
  const summary = el("span", "tool-summary");
  summary.textContent = `${ev.is_error ? "Failed" : "Done"}${elapsed == null ? "" : ` · ${elapsed < 1000 ? Math.round(elapsed) + "ms" : (elapsed / 1000).toFixed(1) + "s"}`}`;
  card.querySelector(".tool-title").appendChild(summary);
  if (info && info.name === "subagent" && !ev.is_error) applySubagentResult(card, content);
  if (info && info.name === "edit_file" && !ev.is_error) {
    const path = info.arguments && (info.arguments.path || info.arguments.file_path);
    if (path && captureDiff(path, content)) {
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

function onToolOutput(ev) {
  const card = state.tools.get(ev.call_id);
  if (!card || !ev.content) return;
  const pre = card.querySelector(".tool-live-output") || el("pre", "tool-live-output");
  if (!pre.parentNode) card.querySelector(".tool-body").appendChild(pre);
  card.dataset.liveOutput = "1";
  pre.textContent += ev.content;
  scrollDown();
}

function formatToolOutput(s) {
  const t = s.trim();
  if ((t.startsWith("{") && t.endsWith("}")) || (t.startsWith("[") && t.endsWith("]"))) {
    try { return JSON.stringify(JSON.parse(t), null, 2); } catch { /* not json */ }
  }
  return s;
}

// ── sub-agents ────────────────────────────────────────────────────────────────
//
// The runtime's `subagent` tool dispatches tasks to agents registered via
// bone.subagent.register in init.lua. There is no dedicated protocol: calls
// arrive as ordinary tool_call/tool_result events, and results of background
// (non-blocking) dispatches are injected by the daemon as an automated turn.
// We give the call a dedicated card — one row per dispatched task with a live
// status dot — and resolve each row from the result text (blocking dispatch /
// wait) or when the injected results turn begins (background dispatch).

// Rows from non-blocking dispatches whose jobs are still running in the
// background. Cleared on conversation switch (the thread DOM is rebuilt).
let bgAgentRows = [];

// Compact head-line summary for a subagent call.
function subagentSummary(args) {
  const action = (args && args.action) || "status";
  if (action === "dispatch") {
    const n = ((args && args.tasks) || []).length;
    return `dispatch · ${n} task${n === 1 ? "" : "s"}${args.wait ? "" : " · background"}`;
  }
  const ids = (args && args.ids) || [];
  return ids.length ? `${action} · ${ids.join(", ")}` : action;
}

// One status row per dispatched task. `resolved` renders neutral done dots for
// transcript replay, where the per-job outcome isn't stored with the call.
function buildAgentRows(args, resolved) {
  const rows = el("div", "agent-rows");
  for (const t of (args && args.tasks) || []) {
    const row = el("div", "agent-row");
    row.innerHTML = `<span class="tool-status ${resolved ? "done" : "running"}"></span><span class="agent-name"></span><span class="agent-task"></span>`;
    row.dataset.agent = t.agent || "";
    if (resolved) row.dataset.resolved = "1";
    row.querySelector(".agent-name").textContent = t.agent || "agent";
    row.querySelector(".agent-task").textContent = t.title || t.task || "";
    rows.appendChild(row);
  }
  return rows;
}

function markAgentRow(row, cls) {
  row.dataset.resolved = "1";
  row.querySelector(".tool-status").className = "tool-status " + cls;
}

// Resolve a subagent card's rows from the tool result text.
function applySubagentResult(card, content) {
  const rows = [...card.querySelectorAll(".agent-row")];
  // Per-task dispatch lines are only listed when something was rejected; they
  // map 1:1 to the tasks (and therefore rows) in order (see subagent.lua).
  const lines = content.split("\n");
  if (/^Dispatched \d+, rejected [1-9]/.test(lines[0] || "")) {
    rows.forEach((row, i) => { if (/^REJECTED/.test(lines[i + 1] || "")) markAgentRow(row, "error"); });
  }
  // Blocking dispatch/wait results carry one "## agent (job-N) — done|ERROR"
  // section per finished job. Resolve this card's rows first, then any rows
  // still in the background from an earlier non-blocking dispatch (a later
  // `wait` call returns those jobs' results).
  for (const m of content.matchAll(/^## (.+?) \([^)]*\) — (done|ERROR)/gm)) {
    const row = rows.find((r) => !r.dataset.resolved && r.dataset.agent === m[1])
      || bgAgentRows.find((r) => !r.dataset.resolved && r.dataset.agent === m[1]);
    if (row) markAgentRow(row, m[2] === "done" ? "done" : "error");
  }
  bgAgentRows = bgAgentRows.filter((r) => !r.dataset.resolved && r.isConnected);
  // Anything left on a dispatch card runs in the background; the daemon injects
  // its result as an automated turn later (see resolveBackgroundAgents).
  for (const row of rows) {
    if (!row.dataset.resolved) {
      row.querySelector(".tool-status").className = "tool-status bg";
      row.title = "Running in background — results are delivered automatically";
      bgAgentRows.push(row);
    }
  }
}

// The daemon injects finished background job results as an automated turn (see
// rpc's next_background_prompt). No dedicated event exists, but an injected
// turn is the only turn this client didn't submit itself — use that to flip
// lingering background rows to done. (A bone.submit prompt also matches;
// resolving on it is harmless since those rows' jobs report via injection too.)
function resolveBackgroundAgents() {
  if (!bgAgentRows.length) return;
  for (const row of bgAgentRows) if (row.isConnected) markAgentRow(row, "done");
  bgAgentRows = [];
  systemLine("Sub-agent results delivered — agent continuing");
}

// Injected background-results turns are persisted as user messages with a
// recognizable header (jobs.rs format_results_for_injection). On replay,
// render them as a compact agent-results card instead of a giant "You" bubble.
const BG_RESULTS_PREFIX = "[automated message] Results from background jobs";

function jobResultsCard(content) {
  clearWelcome();
  const card = el("div", "tool agent-results");
  card.innerHTML = `<div class="tool-head" role="button" tabindex="0" aria-expanded="false">
      <div class="tool-main"><div class="tool-title"><span class="tool-verb">Agents</span> <span class="tool-arg">results delivered</span></div></div>
      <span class="tool-status done"></span>
      <svg class="tool-chevron" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></div>
    <div class="tool-body"></div>`;
  const rows = el("div", "agent-rows");
  // Sections look like "## agent (job-N) — ✓|✗|◑" (glyphs from status_sym).
  for (const m of content.matchAll(/^## (.+?) \(([^)]*)\) — (✓|✗|◑|done|ERROR)/gm)) {
    const ok = m[3] === "✓" || m[3] === "done";
    const row = el("div", "agent-row");
    row.innerHTML = `<span class="tool-status ${ok ? "done" : m[3] === "◑" ? "bg" : "error"}"></span><span class="agent-name"></span><span class="agent-task"></span>`;
    row.querySelector(".agent-name").textContent = m[1];
    row.querySelector(".agent-task").textContent = m[2];
    rows.appendChild(row);
  }
  const body = card.querySelector(".tool-body");
  if (rows.childElementCount) card.insertBefore(rows, body);
  body.appendChild(el("div", "tool-section-label", "Results"));
  body.appendChild(el("pre", null)).textContent = content;
  const head = card.querySelector(".tool-head");
  const toggle = () => { card.classList.toggle("open"); head.setAttribute("aria-expanded", card.classList.contains("open")); };
  head.onclick = toggle;
  head.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } };
  $("thread").appendChild(card);
}

// Registered sub-agents, parsed from the subagent tool's dynamic description
// ("Registered agents:" list) so no extra endpoint is needed.
function registeredAgents() {
  const def = state.toolDefs.find((t) => t.name === "subagent");
  if (!def) return [];
  const out = [];
  let inList = false;
  for (const line of (def.description || "").split("\n")) {
    if (/^Registered agents:/.test(line)) { inList = true; continue; }
    if (!inList) continue;
    const m = line.match(/^\s+-\s+([^:]+):\s+(.*)$/);
    if (!m) break;
    out.push({ name: m[1], description: m[2].replace(/\s*\[[^\]]*\]$/, "") });
  }
  return out;
}

// ── canvas: split-screen artifact / diff viewer ──────────────────────────────
//
// One artifact per file path. write_file → a live "doc" (markdown rendered) or
// "file" (plain) view; edit_file → a colour-coded "diff" parsed from the result.
// The canvas opens automatically with the latest artifact and keeps a tab strip
// so you can step back through what the agent has written this turn.

const artifacts = new Map(); // path -> { path, name, kind, content, lines, add, del }
let activeArtifact = null;
let showingAllEdits = false;

function baseName(p) { return String(p).split("/").pop() || p; }

function captureDoc(path, content) {
  const kind = /\.(md|markdown|mdx)$/i.test(path) ? "doc" : "file";
  upsertArtifact({ path, name: baseName(path), kind, content, add: content.split("\n").length, del: 0 });
}

function captureDiff(path, resultContent) {
  const { lines, add, del } = parseDiff(resultContent);
  if (!lines.length) return false; // "no changes" or an unrecognised result
  upsertArtifact({ path, name: baseName(path), kind: "diff", lines, add, del });
  return true;
}

// Parse bone's numbered unified diff. Lines look like:
//   "   12   context"   "   13 - removed"   "   13 + added"
function upsertArtifact(art) {
  artifacts.set(art.path, { ...(artifacts.get(art.path) || {}), ...art });
  activeArtifact = art.path;
  showingAllEdits = false;
  $("canvas-toggle").classList.remove("hidden");
  openCanvas();
  renderTabs();
  renderArtifact();
}

function focusArtifact(path) {
  if (!artifacts.has(path)) { toast("nothing to show yet"); return; }
  activeArtifact = path;
  showingAllEdits = false;
  openCanvas();
  renderTabs();
  renderArtifact();
}

function closeArtifact(path) {
  artifacts.delete(path);
  if (activeArtifact === path) activeArtifact = [...artifacts.keys()].pop() || null;
  if (showingAllEdits && [...artifacts.values()].filter((a) => a.kind === "diff").length < 2) {
    showingAllEdits = false;
    activeArtifact = [...artifacts.keys()].pop() || null;
  }
  if (!artifacts.size) { closeCanvas(); $("canvas-toggle").classList.add("hidden"); }
  renderTabs();
  renderArtifact();
}

function openCanvas() { $("canvas").classList.remove("hidden"); $("divider").classList.remove("hidden"); $("canvas-toggle").setAttribute("aria-expanded", "true"); }
function closeCanvas() { $("canvas").classList.add("hidden"); $("divider").classList.add("hidden"); $("canvas-toggle").setAttribute("aria-expanded", "false"); }
function toggleCanvas() {
  if (!artifacts.size) return;
  $("canvas").classList.contains("hidden") ? openCanvas() : closeCanvas();
}

function showAllEdits() {
  if (![...artifacts.values()].some((a) => a.kind === "diff")) return;
  showingAllEdits = true;
  activeArtifact = null;
  openCanvas();
  renderTabs();
  renderArtifact();
}

const KIND_LABEL = { doc: "md", file: "file", diff: "diff" };

function renderTabs() {
  const tabs = $("canvas-tabs");
  tabs.innerHTML = "";
  for (const a of artifacts.values()) {
    const tab = el("div", "canvas-tab" + (a.path === activeArtifact ? " active" : ""));
    tab.title = a.path;
    tab.innerHTML = `<span class="ct-kind"></span><span class="ct-name"></span>
      <button type="button" class="ct-x" aria-label="Close ${escapeHtml(a.name)}"><svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg></button>`;
    tab.querySelector(".ct-kind").textContent = KIND_LABEL[a.kind] || "file";
    tab.querySelector(".ct-name").textContent = a.name;
    tab.tabIndex = 0; tab.setAttribute("role", "tab"); tab.setAttribute("aria-selected", String(a.path === activeArtifact));
    tab.onclick = (e) => { if (e.target.closest(".ct-x")) return; focusArtifact(a.path); };
    tab.onkeydown = (e) => { if ((e.key === "Enter" || e.key === " ") && !e.target.closest(".ct-x")) { e.preventDefault(); focusArtifact(a.path); } };
    tab.querySelector(".ct-x").onclick = (e) => { e.stopPropagation(); closeArtifact(a.path); };
    tabs.appendChild(tab);
  }
  const diffCount = [...artifacts.values()].filter((a) => a.kind === "diff").length;
  $("canvas-all").classList.toggle("hidden", diffCount < 2);
  $("canvas-all").classList.toggle("active", showingAllEdits);
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
  if (showingAllEdits) {
    for (const a of artifacts.values()) {
      if (a.kind !== "diff") continue;
      const section = el("section", "canvas-edit-section");
      section.appendChild(artifactMeta(a));
      section.appendChild(renderDiffView(a.lines));
      body.appendChild(section);
    }
    body.scrollTop = 0;
    return;
  }
  const a = artifacts.get(activeArtifact);
  if (!a) { body.appendChild(el("div", "canvas-empty", "Nothing open")); return; }
  body.appendChild(artifactMeta(a));
  if (a.kind === "doc") {
    body.appendChild(el("div", "prose", renderMarkdown(a.content || "")));
  } else if (a.kind === "diff") {
    body.appendChild(renderDiffView(a.lines));
  } else {
    body.appendChild(renderCodeView(a.content || "", a.path));
  }
  body.scrollTop = 0;
  updateCanvasSearch();
}

function updateCanvasSearch() {
  const query = $("canvas-search").value.trim().toLocaleLowerCase();
  const rows = [...$("canvas-body").querySelectorAll(".lt, .prose p, .prose li")];
  let count = 0;
  for (const row of rows) {
    const hit = !!query && row.textContent.toLocaleLowerCase().includes(query);
    row.classList.toggle("search-hit", hit); if (hit) count++;
  }
  $("canvas-match").textContent = query ? `${count} match${count === 1 ? "" : "es"}` : "";
  if (count) $("canvas-body").querySelector(".search-hit")?.scrollIntoView({ block: "center" });
}
function downloadArtifact() {
  const a = artifacts.get(activeArtifact);
  if (!a) return;
  downloadText(a.name, artifactText(a));
}
async function loadFullArtifact() {
  const a = artifacts.get(activeArtifact);
  if (!a) return;
  const button = $("canvas-full-file"); button.disabled = true;
  try {
    const file = await requestJson(`/api/file?path=${encodeURIComponent(a.path)}`);
    upsertArtifact({ path: a.path, absolutePath: file.absolute_path, name: a.name, kind: /\.(md|markdown|mdx)$/i.test(a.path) ? "doc" : "file", content: file.content, add: 0, del: 0 });
    toast("Loaded current workspace file");
  } catch (error) { toast(`Could not load file: ${error.message}`); }
  finally { button.disabled = false; }
}
async function openArtifactInEditor() {
  const a = artifacts.get(activeArtifact); if (!a) return;
  try {
    const file = a.absolutePath ? { absolute_path: a.absolutePath } : await requestJson(`/api/file?path=${encodeURIComponent(a.path)}`);
    location.href = `vscode://file/${file.absolute_path}`;
  } catch (error) { toast(`Could not open editor: ${error.message}`); }
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

function renderCodeView(content, path = "") {
  const wrap = el("div", "codeview");
  const lines = content.split("\n");
  lines.forEach((text, i) => {
    const row = el("div", "code-line");
    row.innerHTML = `<span class="ln"></span><span class="lt"></span>`;
    row.querySelector(".ln").textContent = i + 1;
    row.querySelector(".lt").innerHTML = highlightCode(text, path.split(".").pop() || "");
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
  label.textContent = activeTask;
  let ps = label.querySelector(".task-progress");
  if (!ps) { ps = document.createElement("span"); ps.className = "task-progress"; label.appendChild(ps); }
  ps.textContent = progressLabel;

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
  hideThinking();
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
  // Prefer the daemon-computed edit_file unified diff (same body as the TUI);
  // fall back to raw JSON arguments when preview is absent.
  const pre = card.querySelector(".approval-args");
  if (ev.preview) {
    pre.textContent = String(ev.preview).replace(/^\n/, "");
    pre.classList.remove("hidden");
  } else if (ev.arguments && Object.keys(ev.arguments).length) {
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
  dropCachedApproval(state.conversationId, id);
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
    dropCachedApproval(state.conversationId, id);
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
  state.sending = false;
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
  try {
    const response = await fetch("/api/restart-daemon", { method: "POST" });
    if (!response.ok) {
      const body = await response.json().catch(() => ({}));
      toast(body.error || "engine could not be restarted");
      return;
    }
  } catch {
    toast("engine could not be restarted");
    return;
  }
  clearRecovery();
  // The SSE link reconnects automatically; resend the pending prompt once back.
  const text = state.lastText;
  state.lastText = null;
  setTimeout(() => {
    if (text && !state.running) { input.value = text; autosize(); toast("engine restarted — press send"); }
  }, 1800);
}

// ── interactive key input (ask_user / any ctx.ui.key pane) ───────────────────
//
// The runtime blocks a tool on `ctx.ui.key()` and emits a `key_request`; we
// reply with a `key_reply` carrying a KeyEvent. The Lua menu is keyboard-driven
// (Up/Down/Enter/Space/Tab/Esc/Char), so `interact` panes (see onViewDiff) also
// render clickable controls that translate into the same keystrokes: clicks push
// keys onto `interactState.queue`, which drains one-per-`key_request` since each
// keystroke makes the tool re-render and ask for the next key.

// Browser `e.key` → the code names the runtime uses (crossterm-style, see the
// TUI's stream key encoder). Anything else printable becomes a `Char`.
const KEY_CODE_MAP = {
  ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right",
  Escape: "Esc", Enter: "Enter", Tab: "Tab", Backspace: "Backspace",
  Delete: "Delete", Insert: "Insert", Home: "Home", End: "End",
  PageUp: "PageUp", PageDown: "PageDown",
};
function mapBrowserKey(e) {
  if (KEY_CODE_MAP[e.key]) return keyEvent(KEY_CODE_MAP[e.key], null, e);
  if (e.key && e.key.length === 1) return keyEvent("Char", e.key, e);
  return null; // modifier-only / F-keys / unknown: ignore
}
function keyEvent(code, char, e) {
  return { code, char, ctrl: !!(e && e.ctrlKey), alt: !!(e && e.altKey), shift: !!(e && e.shiftKey) };
}
const K = (code, char = null) => ({ code, char, ctrl: false, alt: false, shift: false });

function onKeyRequest(ev) {
  state.keyId = ev.id;
  if (interactState.queue.length) return pumpKeyQueue();
  if (!interactState.active) toast("press any key…");
}
// Send the next queued (click-derived) key, if the tool is currently waiting.
function pumpKeyQueue() {
  if (state.keyId == null || !interactState.queue.length) return;
  const key = interactState.queue.shift();
  const id = state.keyId;
  state.keyId = null;
  send({ key_reply: { id, key } });
}
function enqueueKeys(keys) {
  if (!keys || !keys.length) return;
  interactState.queue.push(...keys);
  pumpKeyQueue();
}
function captureKey(e) {
  if (state.keyId == null) return;
  // While a click-driven burst is still draining, swallow raw keystrokes so they
  // don't interleave with the queued sequence.
  if (interactState.queue.length) { e.preventDefault(); return; }
  const key = mapBrowserKey(e);
  if (!key) return;
  e.preventDefault();
  const id = state.keyId;
  state.keyId = null;
  send({ key_reply: { id, key } });
}

// ── interact pane (ask_user) rendering ───────────────────────────────────────

const interactState = { active: false, multi: false, queue: [], model: null, total: 0, hasCustom: false };

// Parse the `interact` pane's styled lines back into a small semantic model so
// we can render real buttons instead of the TUI's cursor/checkbox glyphs.
function parseInteractPane(comp) {
  const model = { title: comp.title || "", question: "", options: [], custom: null, text: null,
                  multi: false, scrollAbove: 0, scrollBelow: 0, hint: "", notice: "" };
  let seenInteractive = false;
  let lastOption = null;
  for (const raw of (comp.lines || [])) {
    const t = paneLineText(raw);
    if (!t) continue;
    let m = t.match(/^\s*↑\s+(\d+)\s+more/);
    if (m) { model.scrollAbove = +m[1]; continue; }
    m = t.match(/^\s*↓\s+(\d+)\s+more/);
    if (m) { model.scrollBelow = +m[1]; continue; }
    if (/·/.test(t) && /(move|submit|select|cancel|toggle)/i.test(t)) { model.hint = t.trim(); continue; }
    // Interactive rows: " > label" / "   label" (space, cursor, space, then a
    // non-space so wrapped continuation lines are excluded).
    m = t.match(/^ ([ >]) (\S.*)$/);
    if (m) {
      seenInteractive = true;
      const selected = m[1] === ">";
      const rest = m[2];
      const cm = rest.match(/^Custom:\s?(.*)$/);
      if (cm) { model.custom = { value: cm[1].replace(/█$/, ""), selected }; lastOption = null; continue; }
      const chk = rest.match(/^\[([ x])\]\s(.*)$/);
      if (chk) {
        model.multi = true;
        lastOption = { label: chk[2], checked: chk[1] === "x", selected };
        model.options.push(lastOption);
        continue;
      }
      lastOption = { label: rest, checked: false, selected };
      model.options.push(lastOption);
      continue;
    }
    // ui.menu emits an option's description as the immediately following line,
    // indented five spaces to align below its label.
    m = t.match(/^ {5}(\S.*)$/);
    if (m && lastOption) { lastOption.description = m[1]; continue; }
    lastOption = null;
    // text_input value line: "> value█"
    m = t.match(/^> (.*)$/);
    if (m && !seenInteractive) { model.text = { value: m[1].replace(/█$/, "") }; continue; }
    // First remaining line is the question; any further one is a transient notice.
    if (!model.question) model.question = t.trim();
    else model.notice = t.trim();
  }
  return model;
}
function paneLineText(line) {
  if (typeof line === "string") return line;
  if (!line || !line.spans) return "";
  return line.spans.map((s) => s.text || "").join("");
}

function renderInteractPane(model) {
  interactState.active = true;
  interactState.multi = model.multi;
  interactState.model = model;
  interactState.total = model.scrollAbove + model.options.length + model.scrollBelow;
  interactState.hasCustom = !!model.custom;

  $("interact").classList.remove("hidden");
  $("interact-kicker").textContent = model.title || "Question";
  $("interact-q").textContent = model.question || "Choose an option";

  const opts = $("interact-options");
  opts.innerHTML = "";
  if (model.scrollAbove) opts.appendChild(moreRow("↑ " + model.scrollAbove + " more", K("PageUp")));
  model.options.forEach((o, p) => {
    const b = el("button", "interact-opt" + (o.selected ? " selected" : ""));
    if (model.multi) b.appendChild(el("span", "interact-check" + (o.checked ? " on" : ""), o.checked ? "✓" : ""));
    const copy = el("span", "interact-opt-copy");
    const lbl = el("span", "interact-opt-label");
    lbl.textContent = o.label;
    copy.appendChild(lbl);
    if (o.description) {
      const description = el("span", "interact-opt-description");
      description.textContent = o.description;
      copy.appendChild(description);
    }
    b.appendChild(copy);
    b.onclick = () => clickInteractOption(p);
    opts.appendChild(b);
  });
  if (model.scrollBelow) opts.appendChild(moreRow("↓ " + model.scrollBelow + " more", K("PageDown")));

  if (model.custom) {
    const b = el("button", "interact-opt interact-custom" + (model.custom.selected ? " selected" : ""));
    const lbl = el("span", "interact-opt-label");
    lbl.textContent = model.custom.value ? model.custom.value : "Type a custom answer…";
    if (!model.custom.value) lbl.classList.add("placeholder");
    b.appendChild(lbl);
    if (model.custom.selected) b.appendChild(el("span", "interact-caret"));
    b.onclick = clickInteractCustom;
    opts.appendChild(b);
  }
  if (model.text) {
    const t = el("div", "interact-text");
    if (model.text.value) t.textContent = model.text.value;
    else { t.textContent = "Type your answer…"; t.classList.add("placeholder"); }
    t.appendChild(el("span", "interact-caret"));
    opts.appendChild(t);
  }

  // Submit is always explicit (Enter commits the highlighted option / checked
  // set / typed answer); a click never auto-submits.
  const foot = $("interact-foot");
  foot.innerHTML = "";
  const cancel = el("button", "btn interact-cancel", "Cancel");
  cancel.onclick = cancelInteract;
  foot.appendChild(cancel);
  const submit = el("button", "btn btn-approve interact-submit", "Submit");
  submit.onclick = () => enqueueKeys([K("Enter")]);
  foot.appendChild(submit);

  $("interact-hint").textContent = model.hint || "";
}
function moreRow(label, key) {
  const d = el("div", "interact-more");
  d.textContent = label;
  d.onclick = () => enqueueKeys([key]);
  return d;
}
function closeInteract() {
  interactState.active = false;
  interactState.queue = [];
  interactState.model = null;
  $("interact").classList.add("hidden");
}
function cancelInteract() {
  interactState.queue = [];
  enqueueKeys([K("Esc")]);
}

// The cyclic list the Lua menu walks with Up/Down: options first, then the
// custom row (when present). Absolute index of the currently-selected row.
function interactSelectedIndex(model) {
  const vis = model.options.findIndex((o) => o.selected);
  if (vis >= 0) return model.scrollAbove + vis;
  if (model.custom && model.custom.selected) return interactState.total;
  return model.scrollAbove;
}
// Fewest Up/Down presses to move the cursor from → to around the cyclic list.
function interactMoveKeys(from, to) {
  const L = interactState.total + (interactState.hasCustom ? 1 : 0);
  if (L <= 0 || from === to) return [];
  const down = (((to - from) % L) + L) % L;
  const up = (((from - to) % L) + L) % L;
  const keys = [];
  const [code, n] = down <= up ? ["Down", down] : ["Up", up];
  for (let i = 0; i < n; i++) keys.push(K(code));
  return keys;
}
function clickInteractOption(p) {
  const model = interactState.model;
  if (!model) return;
  // A click only moves the cursor (multi also toggles the checkbox in place).
  // Committing is always an explicit Enter / Submit — never on selection.
  const keys = interactMoveKeys(interactSelectedIndex(model), model.scrollAbove + p);
  if (model.multi) keys.push(K("Char", " "));
  enqueueKeys(keys);
}
function clickInteractCustom() {
  const model = interactState.model;
  if (!model) return;
  enqueueKeys(interactMoveKeys(interactSelectedIndex(model), interactState.total));
}

// ── turn lifecycle ──────────────────────────────────────────────────────────

function showWorkElapsed() {
  const ms = state.pendingWorkElapsed;
  state.pendingWorkElapsed = null;
  if (typeof ms !== "number") return;
  systemLine(`worked for ${formatElapsed(ms)}`);
}

function onFinished() {
  hideThinking();
  if (state.asstEl) {
    state.asstEl.innerHTML = renderMarkdown(state.asstRaw);
    enhanceContent(state.asstEl);
  }
  finalizeTurn();
}
function onFailed(ev) { hideThinking(); closeInteract(); markRunning(state.conversationId, false); systemLine(ev.message || "turn failed", true); finalizeTurn(); setRunning(false); }
function onTurnComplete() {
  hideThinking();
  showWorkElapsed();
  // The turn is over — stop this conversation's elapsed timer.
  markRunning(state.conversationId, false);
  setRunning(false);
  // If we joined this turn after it began (e.g. a mid-response page refresh), the
  // rendered thread is missing the streamed head. The full turn is now persisted,
  // so reload the conversation from the DB to render the authoritative transcript.
  const joinedMidTurn = !state.sawStarted;
  state.sawStarted = false;
  liveEventCache.delete(state.conversationId);
  if (joinedMidTurn && state.conversationId != null && !state.awaitingLoad) {
    reloadActiveFromDb();
  }
  loadChats();
}

// Re-fetch the active conversation from the DB and re-render it. Used to recover
// the full transcript after joining a turn partway through; the `awaitingLoad`
// gate makes the incoming `conversation_loaded` authoritative over any strays.
function reloadActiveFromDb() {
  const id = state.conversationId;
  if (id == null) return;
  state.awaitingLoad = { mode: "load", id, from: id };
  send({ load_conversation: { id } });
}
function finalizeTurn() { state.asstEl = null; state.asstRaw = ""; state.reasonEl = null; state.reasonDetails = null; state.tools.clear(); state.toolInfo.clear(); }

function clearArtifacts() {
  artifacts.clear();
  activeArtifact = null;
  showingAllEdits = false;
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
  bgAgentRows = []; // rows live in the DOM we just discarded
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
  restoreDraft();
  // Restore this conversation's task list from the per-chat cache (the actor
  // won't re-emit it on attach). Uses the id from the snapshot we just applied.
  restoreTasks(state.conversationId);
  // An empty conversation (fresh chat) shows the welcome rather than a blank pane.
  if (!rendered) { $("thread").appendChild(buildWelcome()); }
  // A running turn's assistant/tool output is absent from the DB replay until
  // commit. Re-apply the cached live tail after the persisted transcript.
  replayLiveTail(state.conversationId);
  // Open on the latest exchange, not the first message.
  scrollToBottom();
}

function renderStoredMessage(m, asstTurn) {
  if (m.role === "user") {
    // Daemon-injected background job results — render as an agent card, not a
    // wall-of-text "You" bubble the user never typed.
    if ((m.content || "").startsWith(BG_RESULTS_PREFIX)) { jobResultsCard(m.content); return null; }
    userMessage(m.content, m.images || []);
    return null;
  }
  if (m.role === "assistant") {
    const t = asstTurn || turn("assistant");
    if (!asstTurn) t.appendChild(el("div", "role-tag", ""));
    // Only emit a prose block when there's actual text — empty assistant
    // messages (tool-call-only rounds) shouldn't add blank separation.
    if ((m.content || "").trim()) t.appendChild(el("div", "prose", renderMarkdown(m.content)));
    for (const tc of m.tool_calls || []) {
      const { verb, arg } = toolMeta(tc.name, tc.arguments);
      const card = el("div", "tool");
      card.innerHTML = `<div class="tool-head" role="button" tabindex="0" aria-expanded="false">
        <div class="tool-main"><div class="tool-title"><span class="tool-verb"></span> <span class="tool-arg"></span></div></div>
        <span class="tool-status done"></span>
        <svg class="tool-chevron" viewBox="0 0 24 24"><path d="M9 6l6 6-6 6"/></svg></div>
        <div class="tool-body"></div>`;
      card.querySelector(".tool-verb").textContent = verb;
      card.querySelector(".tool-arg").textContent = arg;
      fillToolArgs(card.querySelector(".tool-body"), tc.arguments);
      if (tc.name === "subagent") {
        // Per-job outcomes aren't stored with the call; show neutral done rows.
        const rows = buildAgentRows(tc.arguments, true);
        if (rows.childElementCount) card.insertBefore(rows, card.querySelector(".tool-body"));
      }
      const head = card.querySelector(".tool-head");
      const toggle = () => { card.classList.toggle("open"); head.setAttribute("aria-expanded", card.classList.contains("open")); };
      head.onclick = toggle;
      head.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggle(); } };
      t.appendChild(card);
    }
    enhanceContent(t);
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
    // Interactive question pane (source="interact", e.g. the ask_user tool).
    if (comp.id === "interact" && comp.lines) {
      renderInteractPane(parseInteractPane(comp));
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
  // Interact pane cleared (menu.clear / answered / cancelled → Remove diff).
  if (diff && diff.remove && diff.remove.id === "interact") closeInteract();
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
  const hi = theme.highlights || {};
  const color = (value) => typeof value === "string" ? value : value?.fg;
  const accent = color(hi.tool_call) || color(theme.tool_call) || theme.palette?.accent;
  if (typeof accent === "string" && /^#/.test(accent)) document.documentElement.style.setProperty("--accent", accent);
}

function systemLine(text, isError) {
  // The active turn already has a spinner and label. Avoid duplicating runtime
  // "thinking" notices as a second, centered status line in the thread.
  if (!isError && /^thinking(?:\.{3}|…)?$/i.test((text || "").trim())) return;
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
  updateJumpLatest();
}
// Unconditional jump to the newest message — used when opening a conversation so
// it lands on the latest exchange rather than the first (scrollDown would refuse
// because a freshly-rendered thread is scrolled to the top, not near the bottom).
function scrollToBottom() {
  const t = $("thread");
  t.scrollTop = t.scrollHeight;
  updateJumpLatest();
}
function updateJumpLatest() {
  const t = $("thread");
  const away = t.scrollHeight - t.scrollTop - t.clientHeight > 220;
  $("jump-latest").classList.toggle("hidden", !away);
}
function jumpToLatest() {
  const t = $("thread");
  t.scrollTo({ top: t.scrollHeight, behavior: "smooth" });
  $("jump-latest").classList.add("hidden");
}
function openMobileSidebar() {
  if (window.matchMedia("(max-width: 760px)").matches) $("app").classList.add("mobile-sidebar-open");
  else { $("app").classList.remove("sidebar-hidden"); $("show-sidebar").classList.add("hidden"); }
}
function closeMobileSidebar() { $("app").classList.remove("mobile-sidebar-open"); }

// ── chat sidebar ────────────────────────────────────────────────────────────

async function loadChats() {
  clearError();
  try { conversations = await requestJson("/api/conversations"); }
  catch (error) { conversations = []; reportError("Could not load conversations", error, loadChats); }
  renderChats();
}
function renderChats() {
  const query = $("chat-search").value.trim().toLowerCase();
  const chats = conversations.filter((c) => !query || `${c.title} ${c.provider} ${c.model || ""}`.toLowerCase().includes(query));
  const list = $("chat-list");
  list.innerHTML = "";

  // Ephemeral "New chat" placeholder — a visual hint that you're in a fresh,
  // unsent conversation. Shown only while the draft hasn't become a real (listed)
  // conversation yet; it vanishes once the chat gains messages and is listed, or
  // when the user opens another chat (openChat clears the flag).
  const draftListed = state.conversationId != null && conversations.some((c) => c.id === state.conversationId);
  const showDraft = state.draftChat && !draftListed && !query;
  if (showDraft) list.appendChild(buildDraftRow());

  for (const c of chats) {
    const row = el("div", "chat-row");
    const item = el("button", "chat-item");
    item.type = "button";
    item.dataset.id = c.id;
    item.innerHTML = `<div class="chat-title-row"><span class="chat-run-dot" aria-hidden="true"></span><div class="chat-title"></div><span class="chat-timer" aria-hidden="true"></span></div>
      <div class="chat-meta"><span>${c.provider}</span><span>${relTime(c.last_at || c.started_at)}</span></div>`;
    item.querySelector(".chat-title").textContent = c.title || "Untitled";
    item.onclick = () => openChat(c.id);
    const menu = el("button", "ghost-btn chat-menu-btn", "•••");
    menu.type = "button";
    menu.setAttribute("aria-label", `Actions for ${c.title || "Untitled"}`);
    menu.onclick = () => toggleChatActions(row, c);
    row.append(item, menu);
    list.appendChild(row);
  }
  if (!chats.length && !showDraft) list.appendChild(el("div", "chat-empty", query ? "No matching chats" : "No conversations yet"));
  highlightActiveChat();
  updateRunningIndicators();
}
// The unsent "New chat" hint row. It has no conversation id (nothing to open,
// rename, or archive yet); clicking it just focuses the composer.
function buildDraftRow() {
  const row = el("div", "chat-row");
  const item = el("button", "chat-item draft active");
  item.type = "button";
  item.innerHTML = `<div class="chat-title-row"><span class="chat-draft-mark" aria-hidden="true">+</span><div class="chat-title">New chat</div></div>
    <div class="chat-meta"><span>Draft — send a message to save</span></div>`;
  item.onclick = () => input.focus();
  row.appendChild(item);
  return row;
}
function toggleChatActions(row, conversation) {
  const existing = row.querySelector(".chat-actions");
  document.querySelectorAll(".chat-actions").forEach((n) => n.remove());
  if (existing) return;
  const actions = el("div", "chat-actions");
  const rename = el("button", null, "Rename");
  const archive = el("button", "danger", "Archive");
  rename.onclick = () => renameConversation(conversation);
  archive.onclick = () => archiveConversation(conversation);
  actions.append(rename, archive);
  row.appendChild(actions);
  rename.focus();
}
async function renameConversation(conversation) {
  const title = window.prompt("Conversation title", conversation.title || "");
  if (title == null || !title.trim()) return;
  const response = await fetch(`/api/conversations/${conversation.id}`, { method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify({ title }) });
  if (!response.ok) return toast("Could not rename conversation");
  toast("Conversation renamed");
  loadChats();
}
async function archiveConversation(conversation) {
  if (!window.confirm(`Archive “${conversation.title || "Untitled"}”?`)) return;
  const response = await fetch(`/api/conversations/${conversation.id}`, { method: "DELETE" });
  if (!response.ok) return toast("Could not archive conversation");
  if (conversation.id === state.conversationId) newChat();
  toast("Conversation archived");
  loadChats();
}
async function openChat(id) {
  if (id === state.conversationId) return;
  saveDraft();
  const leaving = state.conversationId;
  const leavingRunning = state.running;
  denyPending();
  // Stash the chat we're leaving so its task list is there when we come back,
  // then ask the daemon to switch. Strays from the old actor are gated out
  // until the target's `conversation_loaded` lands (see `awaitingLoad`).
  cacheTasks(state.conversationId);
  state.awaitingLoad = { mode: "load", id, from: leaving };
  // Start the old chat's watch before repinning the primary link. Issuing these
  // requests in this order closes the hand-off gap where neither socket would
  // be subscribed to the actor's broadcast.
  if (leavingRunning && leaving != null && leaving !== id) {
    state.runningConvs.add(leaving); // its dot/timer keep going while off-screen
    if (!await watchConversation(leaving)) {
      state.runningConvs.delete(leaving);
      state.awaitingLoad = null;
      toast("Could not keep this running chat attached");
      updateRunningIndicators();
      return;
    }
  }
  state.runningConvs.delete(id);
  unwatchConversation(id);
  send({ load_conversation: { id } });
  state.conversationId = id;
  desiredConversationId = id;
  sessionStorage.setItem("bone-active-conversation", String(id));
  // Leaving the fresh chat unused — drop its placeholder hint.
  state.draftChat = false;
  renderChats();
  closeMobileSidebar();
}
function highlightActiveChat() {
  for (const item of document.querySelectorAll(".chat-item")) {
    if (item.classList.contains("draft")) continue; // draft owns its own active state
    item.classList.toggle("active", Number(item.dataset.id) === state.conversationId);
  }
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
async function newChat() {
  saveDraft();
  const leaving = state.conversationId;
  const leavingRunning = state.running;
  denyPending();
  cacheTasks(state.conversationId);
  clearTaskList();
  state.awaitingLoad = { mode: "new", from: state.conversationId };
  // Keep the chat we're leaving live in the background if it's still mid-turn.
  if (leavingRunning && leaving != null) {
    state.runningConvs.add(leaving);
    if (!await watchConversation(leaving)) {
      state.runningConvs.delete(leaving);
      state.awaitingLoad = null;
      toast("Could not keep this running chat attached");
      updateRunningIndicators();
      return;
    }
  }
  send("new_conversation");
  $("thread").innerHTML = "";
  bgAgentRows = [];
  $("thread").appendChild(buildWelcome());
  finalizeTurn();
  setRunning(false);
  clearArtifacts();
  state.conversationId = null;
  desiredConversationId = null;
  sessionStorage.removeItem("bone-active-conversation");
  // Surface the ephemeral placeholder row for the fresh chat.
  state.draftChat = true;
  restoreDraft();
  renderChats();
  closeMobileSidebar();
}

// ── providers / model picker ─────────────────────────────────────────────────

const PROVIDER_FIELDS = [
  { key: "label",    label: "Label",       placeholder: "Display name",   type: "text" },
  { key: "base_url", label: "Base URL",    placeholder: "https://...",    type: "text" },
  { key: "model",    label: "Model",       placeholder: "gpt-4o-mini",    type: "text" },
  { key: "api_key",  label: "API Key",     placeholder: "sk-...",         type: "text" },
  { key: "endpoint", label: "Endpoint",    placeholder: "/chat/completions", type: "text" },
  { key: "handler",  label: "Handler",     placeholder: "openai",         type: "select", options: ["openai", "anthropic", "codex", "grok_build"] },
  { key: "reasoning_effort", label: "Reasoning effort", placeholder: "Default", type: "select", options: ["default", "none", "minimal", "low", "medium", "high", "xhigh", "max"], effortHandlers: ["codex", "openai", "grok_build"] },
];

let _provExpanded = null;   // key of expanded card (null = collapsed)
let _provShowKey = null;    // key whose API key is revealed

async function loadProviders() {
  clearError();
  try { state.providers = await requestJson("/api/providers"); }
  catch (error) { state.providers = []; reportError("Could not load providers", error, loadProviders); }
  // Restore last-selected provider if it still exists in the provider list.
  if (prefs.providerId && state.providers.some((p) => p.key === prefs.providerId)) {
    state.providerId = prefs.providerId;
    const p = state.providers.find((x) => x.key === prefs.providerId);
    if (p && p.model) state.model = p.model;
  }
  renderModelLabel();
  renderProviderPicker();
}

function renderProviderPicker() {
  const list = $("provider-list");
  list.innerHTML = "";

  for (const p of state.providers) {
    const expanded = p.key === _provExpanded;
    const card = el("div", "prov-card" + (p.key === state.providerId ? " prov-active" : "") + (expanded ? " prov-expanded" : ""));
    card.dataset.key = p.key;

    // Compact row
    const rowWrap = el("div", "prov-row-wrap");
    const row = el("button", "provider-row");
    row.classList.add("prov-row");
    row.type = "button";
    row.setAttribute("aria-label", `Switch to ${p.label || p.key}`);

    // Expand chevron
    const chev = el("button", "prov-chevron", "");
    chev.innerHTML = expanded ? "▾" : "▸";
    chev.type = "button";
    chev.title = expanded ? "Collapse provider settings" : "Edit provider settings";
    chev.setAttribute("aria-label", chev.title);
    chev.onclick = (e) => { e.stopPropagation(); toggleProvExpand(p.key); };
    rowWrap.appendChild(chev);

    // Label
    const title = el("span", "prov-title");
    title.textContent = p.label || p.key;
    row.appendChild(title);

    // Model
    const model = el("span", "prov-model");
    model.textContent = p.model || "No model configured";
    row.appendChild(model);

    // Handler badge
    if (p.handler) {
      const badge = el("span", "prov-badge", p.handler);
      row.appendChild(badge);
    }

    rowWrap.appendChild(row);
    card.appendChild(rowWrap);

    // Expanded editor (hidden by default)
    const editor = el("div", "prov-editor");
    for (const fd of PROVIDER_FIELDS) {
      if (fd.effortHandlers && !fd.effortHandlers.includes(p.handler)) continue;
      const field = el("div", "prov-field");
      const lbl = el("label", null, fd.label);
      const input = fd.type === "select"
        ? createProvSelect(p.key, fd.key, p[fd.key] || fd.options[0], fd.options)
        : createProvInput(p.key, fd.key, p[fd.key] || "", fd.placeholder, fd.type, fd.key === "api_key");
      field.appendChild(lbl);
      field.appendChild(input);
      editor.appendChild(field);
    }
    card.appendChild(editor);

    // Delete button (only on expanded)
    if (p.key !== "_last_provider") {
      const del = el("button", "prov-del-btn");
      del.type = "button";
      del.textContent = "Delete";
      del.onclick = (e) => { e.stopPropagation(); deleteProvider(p.key); };
      card.appendChild(del);
    }

    // Click row to select (not chevron)
    row.onclick = () => pickProvider(p.key);

    list.appendChild(card);
  }

  // Add provider form (inline)
  renderAddForm(list);
}

function toggleProvExpand(key) {
  _provExpanded = _provExpanded === key ? null : key;
  _provShowKey = null;
  renderProviderPicker();
  if (!$("model-pop").classList.contains("hidden")) positionModelPop();
}

function createProvInput(providerKey, fieldKey, value, placeholder, type, isApiKey) {
  const wrap = el("div", "prov-input-wrap");
  const input = document.createElement("input");
  input.className = "prov-input";
  input.type = isApiKey ? "password" : type || "text";
  input.value = value;
  input.placeholder = placeholder;
  input.onchange = () => saveProviderField(providerKey, fieldKey, input.value);
  input.onkeydown = (e) => { if (e.key === "Enter") input.blur(); };
  wrap.appendChild(input);

  // Reveal/hide API key toggle
  if (isApiKey) {
    const toggle = el("button", "prov-key-toggle");
    toggle.type = "button";
    toggle.textContent = "show";
    toggle.title = "Reveal API key";
    toggle.onclick = () => {
      input.type = input.type === "password" ? "text" : "password";
      toggle.title = input.type === "password" ? "Reveal API key" : "Hide API key";
      toggle.textContent = input.type === "password" ? "show" : "hide";
    };
    wrap.appendChild(toggle);
  }
  return wrap;
}

function createProvSelect(providerKey, fieldKey, value, options) {
  const sel = document.createElement("select");
  sel.className = "prov-select";
  for (const opt of options) {
    const o = document.createElement("option");
    o.value = opt;
    o.textContent = opt;
    if (opt === value) o.selected = true;
    sel.appendChild(o);
  }
  sel.onchange = () => saveProviderField(providerKey, fieldKey, sel.value);
  return sel;
}

async function saveProviderField(providerKey, fieldKey, value) {
  const prov = state.providers.find((p) => p.key === providerKey);
  if (!prov) return;
  const oldVal = prov[fieldKey];
  prov[fieldKey] = value;
  if (providerKey === state.providerId && fieldKey === "model") state.model = value;
  renderModelLabel();
  renderProviderPicker();
  if (!$("model-pop").classList.contains("hidden")) positionModelPop();
  try { await requestJson(`/api/providers/${providerKey}`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ field: fieldKey, value }),
  }); toast("Saved"); } catch (error) {
    prov[fieldKey] = oldVal;
    toast(`Save failed: ${error.message}`);
    renderProviderPicker();
  }
}

async function deleteProvider(key) {
  if (!confirm(`Delete provider "${key}"?`)) return;
  try {
    await requestJson(`/api/providers/${key}`, { method: "DELETE" });
    state.providers = state.providers.filter((p) => p.key !== key);
    if (_provExpanded === key) _provExpanded = null;
    renderProviderPicker();
    if (!$("model-pop").classList.contains("hidden")) positionModelPop();
    toast("provider deleted");
  } catch (e) {
    toast("failed to delete: " + e.message);
  }
}

// Inline add-provider form
function renderAddForm(list) {
  const form = el("div", "prov-add-form");
  form.innerHTML = `
    <div class="prov-add-label">Add provider</div>
    <div class="prov-add-row">
      <input class="prov-add-input" id="add-prov-key" placeholder="key" />
      <input class="prov-add-input" id="add-prov-label" placeholder="label" />
    </div>
    <div class="prov-add-row">
      <input class="prov-add-input" id="add-prov-model" placeholder="model" value="gpt-4o-mini" />
      <input class="prov-add-input" id="add-prov-url" placeholder="base URL" value="https://api.openai.com/v1" />
    </div>
    <div class="prov-add-actions">
      <button class="prov-add-submit" id="add-prov-submit">Add</button>
    </div>`;
  form.querySelector("#add-prov-submit").onclick = submitAddProvider;
  // Enter key submits
  form.querySelectorAll(".prov-add-input").forEach((inp) => {
    inp.onkeydown = (e) => { if (e.key === "Enter") submitAddProvider(); };
  });
  list.appendChild(form);
}

async function submitAddProvider() {
  const keyInput = $("add-prov-key");
  const labelInput = $("add-prov-label");
  const modelInput = $("add-prov-model");
  const urlInput = $("add-prov-url");
  const key = keyInput.value.trim();
  if (!key || !/^[a-zA-Z0-9_-]+$/.test(key)) {
    toast("key must be alphanumeric (a-z, 0-9, -, _)");
    return;
  }
  const label = labelInput.value.trim() || key;
  const model = modelInput.value.trim() || "gpt-4o-mini";
  const base_url = urlInput.value.trim() || "https://api.openai.com/v1";
  try {
    await requestJson("/api/providers", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ key, label, model, base_url }),
    });
    keyInput.value = "";
    labelInput.value = "";
    modelInput.value = "gpt-4o-mini";
    urlInput.value = "https://api.openai.com/v1";
    await loadProviders();
    toast(`added "${label}"`);
  } catch (e) {
    toast("failed to add: " + e.message);
  }
}

function markActiveProvider() {
  for (const c of document.querySelectorAll(".prov-card")) {
    c.classList.toggle("prov-active", c.dataset.key === state.providerId);
  }
}

function pickProvider(key) {
  if (key !== state.providerId) {
    send({ switch_provider: { provider_id: key } });
    state.providerId = key;
    prefs.providerId = key;
    savePrefs();
    const p = state.providers.find((x) => x.key === key);
    if (p && p.model) state.model = p.model;
    renderModelLabel();
    markActiveProvider();
    toast(`switched to ${p ? p.label : key}`);
  }
  closeModelPop();
}

function toggleModelPop() {
  const pop = $("model-pop");
  const hidden = pop.classList.contains("hidden");
  hidden ? openModelPop() : closeModelPop();
}

function openModelPop() {
  const pop = $("model-pop");
  pop.classList.remove("hidden");
  $("model-chip").setAttribute("aria-expanded", "true");
  positionModelPop();
  markActiveProvider();
  requestAnimationFrame(() => pop.querySelector("button, input, select")?.focus());
}

function closeModelPop() {
  $("model-pop").classList.add("hidden");
  $("model-chip").setAttribute("aria-expanded", "false");
}

function positionModelPop() {
  const pop = $("model-pop");
  const composer = $("composer").getBoundingClientRect();
  const gap = 10;
  const margin = 10;
  const width = Math.min(740, window.innerWidth - margin * 2, Math.max(320, composer.width));
  const left = Math.min(window.innerWidth - width - margin, Math.max(margin, composer.left + (composer.width - width) / 2));
  pop.style.width = `${width}px`;
  pop.style.left = `${left}px`;
  pop.style.top = "auto";
  pop.style.bottom = `${Math.max(margin, window.innerHeight - composer.top + gap)}px`;
}

// ── settings: behavior / display / tools ─────────────────────────────────────

let configCache = { general: [], toolsDisabled: [] };

async function loadConfig() {
  clearError();
  try { configCache = await requestJson("/api/config"); }
  catch (error) { configCache = { general: [], toolsDisabled: [] }; reportError("Could not load settings", error, loadConfig); }
  // Sync approval mode from persisted config so the UI matches the daemon's
  // actual state (the daemon may have been toggled before a page refresh).
  const am = findField("approval_mode");
  if (am.value) setMode(am.value === "danger");
  renderBehavior();
  renderTools();
}

function findField(key) { return configCache.general.find((f) => f.key === key) || {}; }

async function writeConfig(namespace, key, value, type, reload) {
  try { await requestJson("/api/config", {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ namespace, key, value, type }),
  });
    if (reload) { send("reload_extensions"); toast("Applied — reloading"); }
    else toast("Saved");
  } catch (error) { toast(`Save failed: ${error.message}`); await loadConfig(); }
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

  const triggerMode = findField("compact_trigger_mode").value || "absolute";
  wrap.appendChild(setRow("Compaction trigger", "Use a fixed token threshold or a percentage of model context capacity.",
    enumEl(triggerMode, ["absolute", "percentage"], (v) =>
      writeConfig("general", "compact_trigger_mode", v, "enum", true))));

  wrap.appendChild(setRow("Fixed threshold", "Auto-compact at this context size. Blank disables automatic compaction in absolute mode.",
    numEl(findField("auto_compact_tokens").value || "", "tokens", (v) =>
      writeConfig("general", "auto_compact_tokens", v, "string", true))));

  wrap.appendChild(setRow("Capacity threshold", "Auto-compact at this percentage when percentage mode is selected.",
    numEl(findField("compact_trigger_percentage").value || "80", "%", (v) =>
      writeConfig("general", "compact_trigger_percentage", v, "string", true))));

  wrap.appendChild(setRow("Context capacity", "Override model context capacity when it is not reported automatically.",
    numEl(findField("compact_context_window_tokens").value || "", "tokens", (v) =>
      writeConfig("general", "compact_context_window_tokens", v, "string", true))));

  wrap.appendChild(setRow("Keep recent context", "Token budget preserved verbatim at complete turn boundaries.",
    numEl(findField("compact_keep_tokens").value || "12000", "tokens", (v) =>
      writeConfig("general", "compact_keep_tokens", v, "string", true))));

  wrap.appendChild(setRow("Checkpoint input", "Maximum summarizer input per incremental folding pass.",
    numEl(findField("compact_input_tokens").value || "30000", "tokens", (v) =>
      writeConfig("general", "compact_input_tokens", v, "string", true))));

  wrap.appendChild(setRow("Checkpoint output", "Maximum size of the structured context checkpoint.",
    numEl(findField("compact_summary_tokens").value || "2500", "tokens", (v) =>
      writeConfig("general", "compact_summary_tokens", v, "string", true))));

  wrap.appendChild(setRow("Safety reserve", "Capacity held back from percentage-triggered compaction.",
    numEl(findField("compact_safety_tokens").value || "8000", "tokens", (v) =>
      writeConfig("general", "compact_safety_tokens", v, "string", true))));

  // render the display pane too (shares this load)
  renderDisplay();
}

function enumEl(value, options, onCommit) {
  const select = document.createElement("select");
  select.className = "set-num";
  for (const option of options) {
    const item = document.createElement("option");
    item.value = option;
    item.textContent = option[0].toUpperCase() + option.slice(1);
    item.selected = option === value;
    select.appendChild(item);
  }
  select.onchange = () => onCommit(select.value);
  return select;
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
  // Registered sub-agents (from the subagent tool's dynamic description).
  // Read-only here — they're defined in init.lua via bone.subagent.register;
  // the subagent entry in the tool list below toggles the whole feature.
  const agents = registeredAgents();
  if (agents.length) {
    wrap.appendChild(el("div", "set-group-label", "Sub-agents"));
    for (const a of agents) wrap.appendChild(setRow(a.name, a.description, el("span", "agent-chip", "agent")));
    wrap.appendChild(el("div", "set-group-label", "Tools"));
  }
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
  openDialog("settings-overlay", ".settings-card");
}
function closeSettings() { closeDialog("settings-overlay"); }

function switchTab(tab) {
  for (const b of document.querySelectorAll(".stab")) { const active = b.dataset.tab === tab; b.classList.toggle("active", active); b.setAttribute("aria-selected", active); }
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
  return { showThinking: p.showThinking !== false, expandTools: !!p.expandTools, showMeter: p.showMeter !== false, theme: p.theme || "codex-mono", sidebarW: clampSidebarW(p.sidebarW), canvasW: clampCanvasW(p.canvasW), providerId: p.providerId || null };
}
function savePrefs() { localStorage.setItem("bone-studio-prefs", JSON.stringify(prefs)); }
// Sidebar width is user-draggable; keep it within a sane range and fall back to
// the CSS default (280) when unset.
function clampSidebarW(w) { return w ? Math.max(240, Math.min(420, w)) : 0; }
function clampCanvasW(w) { return w ? Math.max(320, Math.min(innerWidth * .7, w)) : 0; }
function applyPrefs() {
  document.body.classList.toggle("hide-thinking", !prefs.showThinking);
  document.body.classList.toggle("hide-meter", !prefs.showMeter);
  if (prefs.sidebarW) document.documentElement.style.setProperty("--sidebar-w", prefs.sidebarW + "px");
  if (prefs.canvasW) document.documentElement.style.setProperty("--canvas-w", prefs.canvasW + "px");
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
  state.sending = false;
  $("stop").classList.toggle("hidden", !on);
  $("send").classList.toggle("hidden", on);
  $("send").disabled = on || !input.value.trim();
  // NB: the elapsed timer is deliberately NOT driven from here. setRunning(false)
  // fires transiently on every chat switch (onConversationLoaded), so clearing
  // runStart here would reset the timer each time you click into a running chat.
  // The timer is tied to the turn lifecycle instead (started → turn_complete).
  updateRunningIndicators();
  announce(on ? "Agent is responding" : "Agent is ready");
}

// ── composer ───────────────────────────────────────────────────────────────

const input = $("input");
const NATIVE_COMMANDS = new Map([
  ["history", { description: "Open conversation history", run: () => { openMobileSidebar(); $("chat-search").focus(); } }],
  ["clear", { description: "Clear this chat", run: newChat }],
  ["new", { description: "Start a new chat", run: newChat }],
  ["usage", { description: "Show usage for this session", run: () => { switchTab("session"); openSettings(); } }],
  ["stats", { description: "Open token statistics", run: openStats }],
  ["model", { description: "Choose a model", run: openModelPop }],
  ["provider", { description: "Choose a provider", run: openModelPop }],
  ["config", { description: "Open settings", run: openSettings }],
  ["tools", { description: "Configure tools", run: () => { switchTab("tools"); openSettings(); } }],
  ["help", { description: "Show available commands", run: () => openCommandMenu(true) }],
]);
const HIDDEN_COMMANDS = new Set(["quit", "exit", "edit", "e", "setup", "catalog", "update"]);

function availableCommands() {
  const commands = new Map(NATIVE_COMMANDS);
  for (const item of state.commands) {
    const name = Array.isArray(item) ? item[0] : item?.name;
    const description = Array.isArray(item) ? item[1] : item?.description;
    if (!name || HIDDEN_COMMANDS.has(name) || commands.has(name)) continue;
    commands.set(name, { description: description || "Custom command", remote: true });
  }
  return [...commands]
    .map(([name, command]) => ({ name, ...command }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

function commandQuery() {
  const match = input.value.match(/^\/([^\s]*)$/);
  return match ? match[1].toLowerCase() : null;
}

function matchingCommands() {
  const query = commandQuery();
  if (query == null) return [];
  return availableCommands().filter((c) =>
    c.name.toLowerCase().includes(query) || c.description.toLowerCase().includes(query));
}

function renderCommandMenu(force = false) {
  const menu = $("command-menu");
  const query = force ? "" : commandQuery();
  if (query == null) return closeCommandMenu();
  const matches = matchingCommands();
  state.commandIndex = matches.length ? Math.max(0, Math.min(state.commandIndex, matches.length - 1)) : -1;
  menu.innerHTML = "";
  for (const [index, command] of matches.entries()) {
    const option = el("button", "command-option" + (index === state.commandIndex ? " active" : ""));
    option.type = "button";
    option.setAttribute("role", "option");
    option.setAttribute("aria-selected", index === state.commandIndex ? "true" : "false");
    option.innerHTML = `<span class="command-name"></span><span class="command-desc"></span>`;
    option.querySelector(".command-name").textContent = `/${command.name}`;
    option.querySelector(".command-desc").textContent = command.description;
    option.onmousedown = (e) => e.preventDefault();
    option.onclick = () => selectCommand(command);
    menu.appendChild(option);
  }
  if (!matches.length) menu.appendChild(el("div", "command-empty", "No matching commands"));
  menu.classList.remove("hidden");
  $("command-button").setAttribute("aria-expanded", "true");
}

function openCommandMenu(resetInput = false) {
  closeModelPop();
  if (resetInput || commandQuery() == null) input.value = "/";
  state.commandIndex = 0;
  autosize(); saveDraft(); renderCommandMenu(); input.focus();
}

function closeCommandMenu() {
  $("command-menu").classList.add("hidden");
  $("command-button").setAttribute("aria-expanded", "false");
  state.commandIndex = -1;
}

function selectCommand(command) {
  input.value = `/${command.name} `;
  closeCommandMenu(); autosize(); saveDraft(); input.focus();
}

function moveCommandSelection(delta) {
  const options = [...$("command-menu").querySelectorAll(".command-option")];
  if (!options.length) return;
  state.commandIndex = (state.commandIndex + delta + options.length) % options.length;
  options.forEach((option, index) => {
    option.classList.toggle("active", index === state.commandIndex);
    option.setAttribute("aria-selected", index === state.commandIndex ? "true" : "false");
  });
  options[state.commandIndex].scrollIntoView({ block: "nearest" });
}

function parseCommand(text) {
  const match = text.match(/^\/([^\s]+)(?:\s+([\s\S]*))?$/);
  if (!match) return null;
  const name = match[1];
  const native = NATIVE_COMMANDS.get(name);
  if (native) return { name, input: match[2] || "", ...native };
  const remote = availableCommands().find((c) => c.remote && c.name === name);
  return remote ? { ...remote, input: match[2] || "" } : null;
}

async function runComposerCommand(command, sourceText) {
  closeCommandMenu();
  input.value = ""; drafts.set(state.conversationId, ""); autosize();
  if (command.run) { await command.run(command.input); return true; }
  setCommandRunning(true);
  state.lastBubble = userMessage(sourceText);
  state.lastText = sourceText;
  $("send").disabled = true;
  const ok = await send({ run_command: { name: command.name, input: command.input } });
  if (!ok) {
    setCommandRunning(false);
    state.lastBubble?.remove(); state.lastBubble = null;
    input.value = sourceText; saveDraft(); autosize();
  }
  return ok;
}

function setCommandRunning(on) {
  state.commandRunning = on;
  if (!state.running) {
    $("stop").classList.toggle("hidden", !on);
    $("send").classList.toggle("hidden", on);
  }
  if (!on) autosize();
}

function autosize() {
  input.style.height = "auto";
  input.style.height = Math.min(input.scrollHeight, 240) + "px";
  $("send").disabled = state.sending || (!input.value.trim() && !attachments.length);
}
function saveDraft() { drafts.set(state.conversationId, input.value); }
function restoreDraft() { input.value = drafts.get(state.conversationId); autosize(); }

function renderAttachments() {
  const host = $("attachment-list");
  host.innerHTML = "";
  for (const item of attachments) {
    const chip = el("div", "attachment-chip");
    if (item.preview) { const img = document.createElement("img"); img.src = item.preview; img.alt = ""; chip.appendChild(img); }
    const name = el("span", "attachment-chip-name"); name.textContent = item.name; chip.appendChild(name);
    const remove = el("button", "attachment-remove", "×"); remove.type = "button"; remove.setAttribute("aria-label", `Remove ${item.name}`);
    remove.onclick = () => { attachments = attachments.filter((a) => a.id !== item.id); renderAttachments(); autosize(); };
    chip.appendChild(remove); host.appendChild(chip);
  }
  announce(attachments.length ? `${attachments.length} attachment${attachments.length === 1 ? "" : "s"} selected` : "Attachments cleared");
}

async function addFiles(files) {
  for (const file of files) {
    if (attachments.length >= MAX_ATTACHMENTS) { toast(`Up to ${MAX_ATTACHMENTS} attachments are allowed`); break; }
    try { attachments.push(await fileToAttachment(file)); }
    catch (error) { toast(error.message); }
  }
  renderAttachments(); autosize();
}
async function submit(textOverride) {
  // Guard: wired as both `send.onclick` (receives a PointerEvent) and a direct
  // call with a string. Only honour a string override; anything else uses the
  // composer's value.
  const text = (typeof textOverride === "string" ? textOverride : input.value).trim();
  if ((!text && !attachments.length) || state.running || state.sending || state.commandRunning) return;
  const command = attachments.length ? null : parseCommand(text);
  if (command) return runComposerCommand(command, text);
  const submission = buildSubmission(text, attachments);
  state.sending = true;
  // Remember the message so we can restore it if the daemon rejects it as busy.
  state.lastBubble = userMessage(text || attachments.map((a) => a.name).join(", "), attachments.filter((a) => a.kind === "image"));
  state.lastText = text;
  input.value = "";
  drafts.set(state.conversationId, "");
  autosize();
  $("send").disabled = true;
  $("app-status").textContent = "Sending message";
  const sentAttachments = attachments;
  attachments = []; renderAttachments();
  const ok = await send({ submit_prompt: submission });
  if (!ok) {
    state.sending = false;
    if (state.lastBubble) { state.lastBubble.remove(); state.lastBubble = null; }
    input.value = text;
    attachments = sentAttachments; renderAttachments(); saveDraft();
    autosize();
    showRetry(text);
  }
}

function showRetry(text) {
  $("retry-bar")?.remove();
  const bar = el("div", "retry-bar");
  bar.id = "retry-bar";
  bar.innerHTML = `<span>Message wasn’t sent. Your draft has been restored.</span><button class="btn">Retry</button>`;
  bar.querySelector("button").onclick = () => { bar.remove(); submit(text); };
  $("composer-wrap").prepend(bar);
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
  w.innerHTML = `<h1>bone studio</h1>
    <p>A calm, elegant front-end for your bone agent.</p><div class="suggestions"></div>`;
  const wrap = w.querySelector(".suggestions");
  for (const s of SUGGESTIONS) {
    const card = el("button", "suggestion", `<div class="s-title">${s.title}</div><div class="s-sub">${s.sub}</div>`);
    card.type = "button";
    card.onclick = () => { input.value = s.text; autosize(); input.focus(); };
    wrap.appendChild(card);
  }
  return w;
}

// ── toast ──────────────────────────────────────────────────────────────────

let toastTimer;
let dialogReturnFocus = null;
function toast(msg) {
  const t = $("toast");
  t.textContent = msg;
  t.classList.remove("hidden");
  requestAnimationFrame(() => t.classList.add("show"));
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.classList.remove("show"); setTimeout(() => t.classList.add("hidden"), 250); }, 2200);
}
let errorRetry = null;
function reportError(context, error, retry) {
  $("global-error-text").textContent = `${context}: ${error.message || error}`;
  errorRetry = retry || null;
  $("global-error-retry").classList.toggle("hidden", !retry);
  $("global-error").classList.remove("hidden");
  announce(`${context} failed`);
}
function clearError() { $("global-error").classList.add("hidden"); errorRetry = null; }
function announce(message) { $("app-status").textContent = message; }
function openDialog(overlayId, cardSelector) {
  dialogReturnFocus = document.activeElement;
  const overlay = $(overlayId);
  overlay.classList.remove("hidden");
  overlay.setAttribute("aria-hidden", "false");
  requestAnimationFrame(() => overlay.querySelector(cardSelector)?.focus());
}
function closeDialog(overlayId) {
  const overlay = $(overlayId);
  if (overlay.classList.contains("hidden")) return;
  overlay.classList.add("hidden");
  overlay.setAttribute("aria-hidden", "true");
  dialogReturnFocus?.focus?.();
  dialogReturnFocus = null;
}
function trapDialogFocus(e) {
  if (e.key !== "Tab") return;
  const dialog = document.querySelector('.overlay:not(.hidden) [role="dialog"]');
  if (!dialog) return;
  const focusable = [...dialog.querySelectorAll('button:not([disabled]), input:not([disabled]), [href], [tabindex]:not([tabindex="-1"])')];
  if (!focusable.length) return;
  const first = focusable[0], last = focusable.at(-1);
  if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
}

// ── markdown (compact, escaped-first) ────────────────────────────────────────

function enhanceContent(root) {
  for (const pre of root.querySelectorAll("pre:not([data-enhanced])")) {
    pre.dataset.enhanced = "true";
    const language = pre.dataset.language;
    if (language) pre.prepend(el("span", "code-language", language));
    const button = el("button", "copy-btn", "Copy");
    button.type = "button";
    button.setAttribute("aria-label", "Copy code");
    button.onclick = async () => {
      const text = pre.querySelector("code")?.textContent || pre.textContent.replace(/^Copy/, "");
      await navigator.clipboard.writeText(text);
      button.textContent = "Copied";
      setTimeout(() => (button.textContent = "Copy"), 1200);
    };
    pre.prepend(button);
  }
  const turnEl = root.classList?.contains("msg-assistant") ? root : root.closest?.(".msg-assistant");
  if (turnEl && !turnEl.querySelector(":scope > .response-copy")) {
    const button = el("button", "response-copy", "Copy response");
    button.type = "button";
    button.onclick = async () => {
      const text = [...turnEl.querySelectorAll(":scope > .prose")].map((n) => n.innerText).join("\n\n");
      await navigator.clipboard.writeText(text);
      button.textContent = "Copied";
      setTimeout(() => (button.textContent = "Copy response"), 1200);
    };
    turnEl.appendChild(button);
  }
}

// ── wiring ──────────────────────────────────────────────────────────────────

input.addEventListener("input", () => { autosize(); state.commandIndex = 0; renderCommandMenu(); });
input.addEventListener("input", saveDraft);
input.addEventListener("keydown", (e) => {
  const menuOpen = !$("command-menu").classList.contains("hidden");
  if (menuOpen && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
    e.preventDefault(); moveCommandSelection(e.key === "ArrowDown" ? 1 : -1); return;
  }
  if (menuOpen && e.key === "Tab") {
    const commands = matchingCommands();
    if (commands[state.commandIndex]) { e.preventDefault(); selectCommand(commands[state.commandIndex]); }
    return;
  }
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    const exact = parseCommand(input.value.trim());
    const highlighted = menuOpen ? matchingCommands()[state.commandIndex] : null;
    if (!exact && highlighted) {
      const text = `/${highlighted.name}`;
      const command = parseCommand(text);
      if (command) runComposerCommand(command, text);
    } else submit();
  }
  if (e.key === "Escape" && menuOpen) { e.preventDefault(); closeCommandMenu(); }
});
$("attachment-button").onclick = () => $("attachment-input").click();
$("command-button").onclick = () => $("command-menu").classList.contains("hidden") ? openCommandMenu() : closeCommandMenu();
$("attachment-input").onchange = (e) => { addFiles(e.target.files); e.target.value = ""; };
input.addEventListener("paste", (e) => {
  const files = [...e.clipboardData.files];
  if (files.length) { e.preventDefault(); addFiles(files); }
});
for (const type of ["dragenter", "dragover"]) $("composer").addEventListener(type, (e) => { e.preventDefault(); $("composer").classList.add("drag-over"); });
for (const type of ["dragleave", "drop"]) $("composer").addEventListener(type, (e) => { e.preventDefault(); $("composer").classList.remove("drag-over"); });
$("composer").addEventListener("drop", (e) => addFiles(e.dataTransfer.files));
$("send").onclick = submit;
$("stop").onclick = async () => {
  denyPending();
  $("stop").disabled = true;
  announce("Canceling response");
  await send("cancel");
  if (state.commandRunning) setCommandRunning(false);
  $("stop").disabled = false;
};
window.addEventListener("keydown", (e) => { if (e.key === "Escape") { closeModelPop(); closeSettings(); } });
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
// Per-bar detail is surfaced through a shared hover tooltip (see showStatsTip),
// keyed off the data-* attributes rather than a native `title`.
function renderColChart(rows, { height = 150, labelFn, axis = false } = {}) {
  if (!rows || !rows.length) return '<div class="stats-empty">No data</div>';
  const totals = rows.map((r) => r.prompt_tokens + r.completion_tokens);
  const max = Math.max(...totals, 1);
  const step = Math.max(1, Math.ceil(rows.length / 10));
  const cls = height !== 150 ? "stats-chart stats-chart-sm" : "stats-chart";
  const cols = rows.map((r, i) => {
    const total = totals[i];
    // Floor non-zero buckets to a visible sliver so a busy period sitting next
    // to a large spike still reads as activity instead of a hairline.
    const MIN_BAR_PCT = 4;
    const pct = total > 0 ? Math.max(MIN_BAR_PCT, (total / max) * 100) : 0;
    const pr = r.prompt_tokens, cp = r.completion_tokens;
    const lbl = labelFn ? labelFn(r, i) : r.label;
    const data = `data-label="${escapeHtml(String(lbl))}" data-total="${total}" data-prompt="${pr}" data-comp="${cp}" data-cached="${r.cached_tokens || 0}"`;
    const spoken = `${lbl}: ${total} tokens, ${pr} prompt, ${cp} completion`;
    return `<div class="stats-col" ${data} tabindex="0" role="img" aria-label="${escapeHtml(spoken)}">
      <div class="stats-col-stack" style="height:${pct}%">
        ${cp > 0 ? `<div class="stats-col-seg seg-comp" style="flex-grow:${cp}"></div>` : ""}
        ${pr > 0 ? `<div class="stats-col-seg seg-prompt" style="flex-grow:${pr}"></div>` : ""}
      </div>
      <div class="stats-col-label">${i % step === 0 ? escapeHtml(String(lbl)) : ""}</div>
    </div>`;
  }).join("");
  // A single faint peak label anchors the scale without cluttering the plot.
  const axisEl = axis ? `<div class="stats-axis-max">${fmt(max)}</div>` : "";
  return `<div class="${cls}" style="height:${height}px">${axisEl}${cols}</div>`;
}

function renderModelsTable(models, total) {
  const totalTokens = (total.prompt_tokens + total.completion_tokens) || 1;
  const head = `<div class="stats-row stats-table-head">
    <span class="provider">Provider / Model</span>
    <span class="num">Requests</span>
    <span class="num">Prompt</span>
    <span class="num">Completion</span>
    <span class="num cost">Cost</span>
  </div>`;
  const rows = models.map((m) => {
    // Faint background fill = this model's share of total tokens for the window.
    const share = ((m.prompt_tokens + m.completion_tokens) / totalTokens) * 100;
    const cached = m.cached_tokens > 0 ? `<span class="stats-cached"> +${fmt(m.cached_tokens)} cached</span>` : '';
    return `<div class="stats-row stats-table-row" style="--share:${share.toFixed(1)}%">
    <span class="provider"><span class="prov-badge">${escapeHtml(m.provider)}</span><span class="prov-model" title="${escapeHtml(m.model)}">${escapeHtml(m.model)}</span></span>
    <span class="num">${fmt(m.request_count)}</span>
    <span class="num" title="${fmt(m.prompt_tokens)} prompt${m.cached_tokens ? ' · ' + fmt(m.cached_tokens) + ' cached' : ''}">${fmt(m.prompt_tokens)}${cached}</span>
    <span class="num">${fmt(m.completion_tokens)}</span>
    <span class="num cost">${money(m.cost)}</span>
  </div>`;
  }).join("");
  const foot = `<div class="stats-row stats-table-foot">
    <span class="provider"><span class="prov-badge">Total</span></span>
    <span class="num">${fmt(total.request_count)}</span>
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

  // KPI cards — hero row (tokens + requests) + metric row. Cost lives as a plain
  // metric card (and in the summary line), not a hero, since it's frequently $0.
  const tokens = t.prompt_tokens + t.completion_tokens;
  const cachePct = t.prompt_tokens > 0 ? Math.round((t.cached_tokens / t.prompt_tokens) * 100) : 0;
  const perReq = fmt(Math.round(tokens / (t.request_count || 1)));
  $("stats-cards").innerHTML =
    `<div class="stats-card-item hero">
      <div class="stats-card-label">Total tokens</div>
      <div class="stats-card-value">${fmt(tokens)}</div>
      <div class="stats-card-sub">${fmt(t.prompt_tokens)} prompt · ${fmt(t.completion_tokens)} completion</div>
    </div>
    <div class="stats-card-item hero">
      <div class="stats-card-label">Requests</div>
      <div class="stats-card-value">${fmt(t.request_count)}</div>
      <div class="stats-card-sub">${perReq} tokens / request</div>
    </div>`;
  $("stats-cards-row").innerHTML =
    `<div class="stats-card-item"><div class="stats-card-value">${fmt(t.prompt_tokens)}</div><div class="stats-card-label">Prompt tokens</div></div>
    <div class="stats-card-item"><div class="stats-card-value">${fmt(t.completion_tokens)}</div><div class="stats-card-label">Completion</div></div>
    <div class="stats-card-item"><div class="stats-card-value">${fmt(t.cached_tokens)}<span style="font-size:12px;color:var(--text-faint);font-weight:400;margin-left:4px">${cachePct}%</span></div><div class="stats-card-label">Cached</div></div>
    <div class="stats-card-item"><div class="stats-card-value">${money(t.cost)}</div><div class="stats-card-label">Cost</div></div>`;

  // Time-series chart
  const buckets = d[keys.buckets] || [];
  $("stats-chart-sub").textContent = `· ${MODE_LABELS[mode]}`;
  $("stats-chart").innerHTML = renderColChart(buckets, { axis: true, labelFn: (b) => chartLabel(mode, b) });

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
  openDialog("stats-overlay", ".stats-card");
  loadStats();
}

function closeStats() {
  statsState.open = false;
  hideStatsTip();
  closeDialog("stats-overlay");
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

// Shared hover tooltip for the usage charts — one element reused across every
// bar, positioned next to the cursor and flipped near the viewport edges.
let statsTipEl = null;
let statsTipKey = null;  // cache: skip DOM update when data hasn't changed
function hideStatsTip() { if (statsTipEl && statsTipEl.style.display !== "none") statsTipEl.style.display = "none"; statsTipKey = null; }
function showStatsTip(col, x, y) {
  const d = col.dataset;
  const total = +d.total;
  if (!total) return hideStatsTip();
  // Build a stable key from the bar's data attributes.
  const key = `${d.label}|${d.prompt}|${d.comp}|${d.cached}|${d.total}`;
  if (!statsTipEl) { statsTipEl = el("div", "stats-tip"); document.body.appendChild(statsTipEl); }
  if (key === statsTipKey) {
    // Content unchanged — only reposition.
    let left = x + 14, top = y + 14;
    if (left + statsTipEl.offsetWidth + 12 > innerWidth) left = x - statsTipEl.offsetWidth - 14;
    if (top + statsTipEl.offsetHeight + 12 > innerHeight) top = y - statsTipEl.offsetHeight - 14;
    statsTipEl.style.left = Math.max(8, left) + "px";
    statsTipEl.style.top = Math.max(8, top) + "px";
    return;
  }
  statsTipKey = key;
  const row = (k, v) => `<div class="stats-tip-row"><span>${k}</span><b>${fmt(v)}</b></div>`;
  statsTipEl.innerHTML =
    `<div class="stats-tip-head">${escapeHtml(d.label)}</div>` +
    row("Prompt", +d.prompt) + row("Completion", +d.comp) +
    (+d.cached ? row("Cached", +d.cached) : "") +
    `<div class="stats-tip-row total"><span>Total</span><b>${fmt(total)}</b></div>`;
  statsTipEl.style.display = "block";
  const r = statsTipEl.getBoundingClientRect();
  let left = x + 14, top = y + 14;
  if (left + r.width + 12 > innerWidth) left = x - r.width - 14;
  if (top + r.height + 12 > innerHeight) top = y - r.height - 14;
  statsTipEl.style.left = Math.max(8, left) + "px";
  statsTipEl.style.top = Math.max(8, top) + "px";
}
for (const id of ["stats-chart", "stats-hourly"]) {
  const host = $(id);
  if (!host) continue;
  host.addEventListener("mousemove", (e) => {
    const col = e.target.closest(".stats-col");
    col ? showStatsTip(col, e.clientX, e.clientY) : hideStatsTip();
  });
  host.addEventListener("mouseleave", hideStatsTip);
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
$("chat-search").addEventListener("input", renderChats);
$("thread").addEventListener("scroll", updateJumpLatest, { passive: true });
$("jump-latest").onclick = jumpToLatest;
$("settings-btn").onclick = openSettings;
$("settings-close").onclick = closeSettings;
$("global-error-retry").onclick = () => { const retry = errorRetry; clearError(); retry?.(); };
$("global-error-close").onclick = clearError;
$("model-chip").onclick = toggleModelPop;
$("mode-toggle").onclick = () => setMode(!danger);
$("collapse-btn").onclick = () => { $("app").classList.add("sidebar-hidden"); $("show-sidebar").classList.remove("hidden"); };
$("show-sidebar").onclick = openMobileSidebar;
$("sidebar-backdrop").onclick = closeMobileSidebar;
$("canvas-toggle").onclick = toggleCanvas;
$("canvas-all").onclick = showAllEdits;
$("canvas-search").addEventListener("input", updateCanvasSearch);
$("canvas-full-file").onclick = loadFullArtifact;
$("canvas-editor").onclick = openArtifactInEditor;
$("canvas-download").onclick = downloadArtifact;
$("canvas-close").onclick = closeCanvas;
for (const b of document.querySelectorAll(".stab")) {
  b.onclick = () => switchTab(b.dataset.tab);
  b.onkeydown = (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault(); const tabs = [...document.querySelectorAll(".stab")];
    const next = tabs[(tabs.indexOf(b) + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length];
    switchTab(next.dataset.tab); next.focus();
  };
}

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
    prefs.canvasW = clampCanvasW(parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--canvas-w")));
    savePrefs();
  };
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
});
$("divider").addEventListener("keydown", (e) => {
  if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
  e.preventDefault();
  const current = $("canvas").getBoundingClientRect().width;
  prefs.canvasW = clampCanvasW(current + (e.key === "ArrowLeft" ? 24 : -24));
  document.documentElement.style.setProperty("--canvas-w", prefs.canvasW + "px"); savePrefs();
});

// Draggable sidebar edge: resize the sidebar by dragging its right border.
// Double-click resets to the CSS default width.
$("sidebar-resize").addEventListener("mousedown", (e) => {
  e.preventDefault();
  const handle = $("sidebar-resize");
  const sidebar = $("sidebar");
  handle.classList.add("dragging");
  document.body.style.cursor = "col-resize";
  const onMove = (ev) => {
    const w = clampSidebarW(ev.clientX - sidebar.getBoundingClientRect().left);
    document.documentElement.style.setProperty("--sidebar-w", w + "px");
  };
  const onUp = (ev) => {
    handle.classList.remove("dragging");
    document.body.style.cursor = "";
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    prefs.sidebarW = clampSidebarW(ev.clientX - sidebar.getBoundingClientRect().left);
    savePrefs();
  };
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
});
$("sidebar-resize").addEventListener("dblclick", () => {
  document.documentElement.style.removeProperty("--sidebar-w");
  prefs.sidebarW = 0;
  savePrefs();
});
$("sidebar-resize").tabIndex = 0;
$("sidebar-resize").setAttribute("aria-label", "Resize conversation sidebar");
$("sidebar-resize").addEventListener("keydown", (e) => {
  if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
  e.preventDefault();
  prefs.sidebarW = clampSidebarW($("sidebar").getBoundingClientRect().width + (e.key === "ArrowRight" ? 24 : -24));
  document.documentElement.style.setProperty("--sidebar-w", prefs.sidebarW + "px"); savePrefs();
});

document.addEventListener("click", (e) => {
  const pop = $("model-pop");
  if (!pop.classList.contains("hidden") && !pop.contains(e.target) && !e.target.closest("#model-chip")) closeModelPop();
  const commands = $("command-menu");
  if (!commands.classList.contains("hidden") && !commands.contains(e.target) && !e.target.closest("#command-button") && e.target !== input) closeCommandMenu();
});
window.addEventListener("resize", () => {
  if (!$("model-pop").classList.contains("hidden")) positionModelPop();
});
document.addEventListener("keydown", (e) => { if (e.key === "Escape") { closeModelPop(); closeSettings(); } });
document.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") { e.preventDefault(); openCommandMenu(true); }
});
document.addEventListener("keydown", trapDialogFocus);
$("settings-overlay").addEventListener("click", (e) => { if (e.target === $("settings-overlay")) closeSettings(); });
window.addEventListener("keydown", captureKey, true);

applyPrefs();
autosize();
connect();
loadChats();
loadProviders();
loadConfig();
setTimeout(() => send({ set_terminal_width: { width: 100 } }), 400);
