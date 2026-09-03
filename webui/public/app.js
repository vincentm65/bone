import { DraftStore, MAX_ATTACHMENTS, buildRevisionedCommand, buildSubmission, buildSynchronizationCommand, downloadText, fileToAttachment, requestJson } from "./ui-core.js";
import { escapeHtml, highlightCode, renderMarkdown } from "./markdown.js";
import { artifactText, parseDiff } from "./canvas-core.js";

// bone studio — browser client for the bone runtime protocol.
//
// Daemon → us over SSE (RuntimeEvent), us → daemon over POST (RuntimeCommand).
// Externally-tagged serde: unit events arrive as the bare string "turn_complete",
// data events as { tool_call: {...} }. normalize() flattens both to { type, ... }.
// Chat metadata is bridge-local; providers and settings come from the daemon's
// canonical config snapshot.

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
  asstFrame: null,
  reasonEl: null,
  reasonDetails: null,
  tools: new Map(),
  approvals: new Map(),
  answeredApprovals: new Set(),
  replyingApprovals: new Set(),
  keyId: null,
  answeredKeys: new Set(),
  replyingKeys: new Set(),
  connected: false,
  conversationId: null,
  providers: [],
  providerId: null,
  model: null,
  snapshot: {},
  toolDefs: [],
  commands: [],
  subagents: [],
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
  navigationGeneration: 0,
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
let nextSyncRequestId = Date.now();
const repair = { id: null, timer: null, active: false };

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

let conversations = [];
let routingQueue = Promise.resolve();
let pendingSubmitRequest = null;
const watchRequests = new Map();

function routeConversation(generation, command) {
  const pending = routingQueue.then(() => {
    if (generation !== state.navigationGeneration) return false;
    return send(command);
  });
  routingQueue = pending.catch(() => false);
  return pending;
}

function recoverNavigation(token) {
  if (state.awaitingLoad !== token) return false;
  const previous = Object.hasOwn(token, "from") ? token.from : state.conversationId;
  token.failed = true;
  state.awaitingLoad = null;
  state.conversationId = previous ?? null;
  desiredConversationId = previous ?? null;
  if (Object.hasOwn(token, "draftChat")) state.draftChat = token.draftChat;
  if (previous == null) sessionStorage.removeItem("bone-active-conversation");
  else sessionStorage.setItem("bone-active-conversation", String(previous));
  if (previous != null && Object.hasOwn(token, "from")) {
    state.runningConvs.delete(previous);
    unwatchConversation(previous);
  }
  renderChats();
  updateRunningIndicators();
  if (repair.active) requestSynchronization();
  return true;
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
    if (repair.active && desiredConversationId == null) requestSynchronization();
  }
  if (msg.status === "disconnected") {
    resetRepairRequest();
    setConnectionState("reconnecting");
    toast("Daemon disconnected — reconnecting…");
  }
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

function resetRepairRequest() {
  if (repair.timer != null) clearTimeout(repair.timer);
  repair.id = repair.timer = null;
}

function retryRepair(delay) {
  if (repair.timer != null) clearTimeout(repair.timer);
  repair.timer = setTimeout(() => {
    repair.id = repair.timer = null;
    requestSynchronization();
  }, delay);
}

async function requestSynchronization() {
  if (repair.id != null || !state.connected || state.awaitingLoad) return false;
  const requestId = ++nextSyncRequestId;
  repair.id = requestId;
  const ok = await send(buildSynchronizationCommand(requestId, true));
  if (repair.id !== requestId) return ok;
  if (!ok) repair.id = null;
  if (repair.active) retryRepair(ok ? 1500 : 1000);
  return ok;
}

function onStreamLagged(ev) {
  if (!repair.active) toast(`Event stream fell behind${ev.skipped ? ` (${ev.skipped} skipped)` : ""} — resynchronizing…`);
  repair.active = true;
  requestSynchronization();
}

function onStateSynchronized(ev) {
  if (ev.request_id !== repair.id) return;
  resetRepairRequest();
  if (state.awaitingLoad && !switchSatisfiedBy(ev.snapshot)) return;
  if (!ev.busy) repair.active = false;
  if (!ev.busy && Array.isArray(ev.messages)) {
    onConversationLoaded({ messages: ev.messages, snapshot: ev.snapshot, busy: false });
  } else {
    onSnapshot(ev.snapshot);
    setRunning(ev.busy);
  }
  if (ev.view) onViewSnapshot(ev.view);
  if (ev.busy) retryRepair(500);
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
  if (watchRequests.has(id)) return watchRequests.get(id);
  const request = (async () => {
    try {
      const response = await fetch(`/api/watch?session=${state.session}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ conversation_id: id }),
      });
      if (!response.ok) throw new Error(await response.text());
      state.watched.add(id);
      return true;
    } catch {
      return false;
    } finally {
      watchRequests.delete(id);
    }
  })();
  watchRequests.set(id, request);
  return request;
}

async function unwatchConversation(id) {
  if (id == null) return;
  if (watchRequests.has(id)) await watchRequests.get(id);
  if (!state.watched.has(id)) return;
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
  switch (ev.type) {
    case "conversation_loaded":
      if (ev.busy) {
        state.runningConvs.add(convId);
        markRunning(convId, true);
      } else {
        state.runningConvs.delete(convId);
        markRunning(convId, false);
      }
      updateRunningIndicators();
      return;
    case "started":
      state.runningConvs.add(convId);
      markRunning(convId, true);
      updateRunningIndicators();
      return;
    case "turn_complete":
      state.runningConvs.delete(convId);
      markRunning(convId, false);
      unwatchConversation(convId);
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
    default:
      return;
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
// against, `status` lets a failed switch recover, and stream/config/frontend
// events are connection-global.
function onEvent(ev) {
  const routing = ev.type === "conversation_loaded" || ev.type === "conversation_load_failed" ||
                  ev.type === "state_snapshot" || ev.type === "status" ||
                  ev.type === "state_synchronized" || ev.type === "stream_lagged" ||
                  ev.type === "config_snapshot" || ev.type === "config_changed" ||
                  ev.type === "config_mutation_rejected" || ev.type === "frontend_state";
  if (state.awaitingLoad && !routing) {
    // Frames queued by the actor we just left must not bleed into the target.
    // The daemon replays authoritative transcript/view/gates after attachment.
    return;
  }
  return dispatchEvent(ev);
}

function dispatchEvent(ev) {
  switch (ev.type) {
    case "frontend_state": return onFrontendState(ev);
    case "config_snapshot": return adoptConfig(ev.schema, ev.snapshot);
    case "config_changed": return adoptConfig(ev.schema, ev.snapshot);
    // Correlated browser mutations use a dedicated bridge connection that
    // reports their own failure. Ignore that same broadcast on the primary
    // stream so another tab/request cannot produce a misleading toast here.
    case "config_mutation_rejected":
      if (ev.request_id) return;
      toast(ev.error || "Configuration changed elsewhere");
      return loadConfig();
    case "state_snapshot": return onSnapshot(ev.snapshot);
    case "state_synchronized": return onStateSynchronized(ev);
    case "stream_lagged": return onStreamLagged(ev);
    case "view_snapshot": return onViewSnapshot(ev.view);
    case "conversation_loaded": return onConversationLoaded(ev);
    case "conversation_load_failed": return onConversationLoadFailed(ev);
    case "started":
      state.sawStarted = true;
      // A turn we didn't submit ourselves is a daemon-injected one — typically
      // background sub-agent results being handed to the model.
      if (!state.sending) {
        resolveBackgroundAgents();
        // Mirror the TUI display semantics: None -> show the raw task,
        // Some("") -> suppress the user row, Some(label) -> show the label.
        if (ev.display !== "") userMessage(ev.display ?? ev.task);
      }
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
  if (Array.isArray(ev.subagents)) state.subagents = ev.subagents;
  applyTheme(ev.settings?.theme);
  if ($("agents-fields")) renderAgents();
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
      line.innerHTML = renderMarkdown(output); $("thread").appendChild(line); scrollDown();
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

function cancelAssistantFrame() {
  if (state.asstFrame === null) return;
  cancelAnimationFrame(state.asstFrame);
  state.asstFrame = null;
}

function flushAssistantMarkdown(enhance = false) {
  cancelAssistantFrame();
  if (!state.asstEl) return;
  state.asstEl.classList.remove("streaming");
  state.asstEl.innerHTML = renderMarkdown(state.asstRaw);
  if (enhance) enhanceContent(state.asstEl);
}

function appendText(text) {
  hideThinking();
  // Remove thinking once prose starts — it's no longer relevant.
  if (state.reasonDetails) { state.reasonDetails.remove(); state.reasonDetails = null; state.reasonEl = null; }
  ensureAssistant();
  state.asstRaw += text;
  if (state.asstFrame !== null) return;
  state.asstFrame = requestAnimationFrame(() => {
    state.asstFrame = null;
    if (!state.asstEl) return;
    const caret = el("span", "caret");
    state.asstEl.classList.add("streaming");
    state.asstEl.replaceChildren(document.createTextNode(state.asstRaw), caret);
    state.asstEl.parentElement.appendChild(state.asstEl); // keep prose last
    scrollDown();
  });
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
  shell: "Run", bash: "Run", read_file: "Read", create_file: "Create", edit_file: "Edit",
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
    flushAssistantMarkdown();
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

  // File-writing tools get an "open in canvas" affordance. create_file content is
  // available right now; edit_file's diff arrives with the result — defer the
  // button until we have the diff so we never show "nothing to show yet".
  const path = ev.arguments && (ev.arguments.path || ev.arguments.file_path);
  if (path && ev.name === "create_file" && typeof ev.arguments.content === "string") {
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
  if (!card) {
    // A successful result without its call has no safe label or arguments to
    // display. Errors remain visible as a minimal diagnostic.
    if (!ev.is_error) return;
    const orphan = el("div", "tool orphan");
    const title = el("div", "tool-title");
    title.appendChild(el("span", "tool-verb", (ev.name || "tool").replace(/_/g, " ")));
    title.appendChild(el("span", "tool-summary", ev.is_error ? "Failed" : "Done"));
    orphan.appendChild(title);
    activeContainer().appendChild(orphan);
    scrollDown();
    return;
  }
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

// ── canvas: split-screen artifact / diff viewer ──────────────────────────────
//
// One artifact per file path. create_file → a live "doc" (markdown rendered) or
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

$("task-popup-toggle").addEventListener("click", (e) => { e.stopPropagation(); toggleTaskPopup(); });
$("task-popup-collapsed").addEventListener("click", () => toggleTaskPopup());

// ── inline approvals ────────────────────────────────────────────────────────

async function sendInteraction(kind, id, payload, beacon = false) {
  const approval = kind === "approval_reply";
  const answered = state[approval ? "answeredApprovals" : "answeredKeys"];
  const replying = state[approval ? "replyingApprovals" : "replyingKeys"];
  if (answered.has(id) || replying.has(id)) return null;
  const conversationId = state.conversationId;
  const command = { [kind]: { id, ...payload } };
  replying.add(id);
  const sent = beacon && navigator.sendBeacon
    ? navigator.sendBeacon(`/api/command?session=${state.session}`, JSON.stringify(command))
    : await send(command);
  replying.delete(id);
  if (state.conversationId !== conversationId) return null;
  if (!sent) return false;
  answered.add(id);
  return true;
}

function onApproval(ev) {
  // Synchronization can replay a still-live gate, and the conversation hub is
  // shared by every attached tab. Keep one card/reply per approval id.
  if (state.approvals.has(ev.id) || state.answeredApprovals.has(ev.id) ||
      state.replyingApprovals.has(ev.id)) return;
  // Danger mode (and policy-allowed calls) arrive pre-approved: the daemon's
  // gate marks `auto_allows` and leaves the decision to the client, exactly as
  // the TUI does. Approve immediately and skip the prompt — the tool call still
  // renders via its own tool events.
  if (ev.auto_allows) {
    sendInteraction("approval_reply", ev.id, { outcome: "approve" });
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

async function resolveApproval(id, outcome, card, label) {
  if (!await sendInteraction("approval_reply", id, { outcome })) return;
  state.approvals.delete(id);
  const approved = outcome === "approve";
  const guided = typeof outcome === "object";
  card.innerHTML = `<div class="approval-resolved ${approved ? "ok" : "no"}">
    <span>${approved ? "✓" : guided ? "✎" : "✗"}</span><span>${label}</span></div>`;
}

// Auto-deny every approval still awaiting a reply. Leaving one unanswered wedges
// the daemon's turn loop forever (the approval gate blocks on the reply), so we
// resolve them whenever the user abandons the turn (new chat, switch chat, stop,
// tab close). `beacon` uses sendBeacon so it still fires during page unload.
async function denyPending(beacon = false) {
  // beforeunload cannot wait for promises. sendInteraction's beacon path runs
  // synchronously, so start every reply before yielding to the event loop.
  if (beacon) {
    for (const id of [...state.approvals.keys()]) {
      sendInteraction("approval_reply", id, { outcome: "denied" }, true);
    }
    return;
  }
  for (const id of [...state.approvals.keys()]) {
    const card = state.approvals.get(id);
    if (!await sendInteraction("approval_reply", id, { outcome: "denied" })) continue;
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
      const token = state.awaitingLoad;
      recoverNavigation(token);
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
  if (state.keyId === ev.id || state.answeredKeys.has(ev.id) ||
      state.replyingKeys.has(ev.id)) return;
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
  replyKey(id, key);
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
  replyKey(id, key);
}
async function replyKey(id, key) {
  const sent = await sendInteraction("key_reply", id, { key });
  if (sent === false && state.keyId == null) state.keyId = id;
}

// ── interact pane (ask_user) rendering ───────────────────────────────────────

const interactState = {
  active: false, multi: false, queue: [], model: null, total: 0, hasCustom: false,
  identity: "", optionCache: new Map(),
};

function splitInteractLine(line) {
  if (typeof line === "string") {
    const at = line.indexOf("┃");
    if (at < 0) return { left: line, right: null };
    return { left: line.slice(0, at).trimEnd(), right: [{ text: line.slice(at + 1).replace(/^ /, "") }] };
  }
  if (!line || !line.spans) return { left: "", right: null };
  const left = [], right = [];
  let divided = false;
  for (const value of line.spans) {
    const text = value.text || "";
    const at = divided ? -1 : text.indexOf("┃");
    if (at >= 0) {
      if (at > 0) left.push({ ...value, text: text.slice(0, at).replace(/ $/, "") });
      const tail = text.slice(at + 1).replace(/^ /, "");
      if (tail) right.push({ ...value, text: tail });
      divided = true;
    } else if (divided) right.push(value);
    else left.push(value);
  }
  return {
    left: left.map((value) => value.text || "").join("").trimEnd(),
    right: divided ? right : null,
  };
}

// Parse the `interact` pane's styled lines back into a small semantic model so
// we can render real buttons instead of the TUI's cursor/checkbox glyphs.
function parseInteractPane(comp) {
  const model = { title: comp.title || "", question: "", options: [], custom: null, text: null,
                  multi: false, scrollAbove: 0, scrollBelow: 0, hint: "", notice: "", preview: null };
  let seenInteractive = false;
  let lastOption = null;
  for (const raw of (comp.lines || [])) {
    const split = splitInteractLine(raw);
    const t = split.left;
    if (split.right) {
      if (!model.preview) model.preview = { title: "", lines: [] };
      const previewText = split.right.map((value) => value.text || "").join("");
      if (!model.preview.title && previewText) model.preview.title = previewText.trim();
      else model.preview.lines.push(split.right);
    }
    if (!t) continue;
    let m = t.match(/^\s*↑\s+(\d+)\s+more\s*·\s*↓\s+(\d+)\s+more/);
    if (m) { model.scrollAbove = +m[1]; model.scrollBelow = +m[2]; continue; }
    m = t.match(/^\s*↑\s+(\d+)\s+more/);
    if (m) { model.scrollAbove = +m[1]; continue; }
    m = t.match(/^\s*↓\s+(\d+)\s+more/);
    if (m) { model.scrollBelow = +m[1]; continue; }
    if (/·/.test(t) && /(move|submit|select|cancel|toggle|switch pane|scroll)/i.test(t)) { model.hint = t.trim(); continue; }
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

function setInteractText(node, value) {
  if (node.textContent !== value) node.textContent = value;
}

function makeInteractOption(key, kind) {
  const row = el("button", `interact-opt interact-${kind}`);
  row.type = "button";
  row.dataset.key = key;
  row.setAttribute("role", "option");
  row.appendChild(el("span", "interact-choice"));
  const copy = el("span", "interact-opt-copy");
  copy.appendChild(el("span", "interact-opt-label"));
  copy.appendChild(el("span", "interact-opt-description"));
  row.appendChild(copy);
  return row;
}

function patchInteractOption(row, option, index, multi) {
  row.className = "interact-opt interact-option" + (option.selected ? " selected" : "") + (multi ? " multi" : "");
  row.dataset.index = index;
  row.setAttribute("aria-selected", String(option.selected));
  if (multi) row.setAttribute("aria-checked", String(option.checked));
  else row.removeAttribute("aria-checked");
  const choice = row.querySelector(".interact-choice");
  choice.className = "interact-choice" + (option.checked ? " checked" : "");
  setInteractText(choice, multi && option.checked ? "✓" : "");
  setInteractText(row.querySelector(".interact-opt-label"), option.label);
  const description = row.querySelector(".interact-opt-description");
  setInteractText(description, option.description || "");
  description.classList.toggle("hidden", !option.description);
  row.onclick = () => clickInteractOption(Number(row.dataset.index));
}

function patchInteractCustom(row, custom) {
  row.className = "interact-opt interact-custom" + (custom.selected ? " selected" : "");
  row.setAttribute("aria-selected", String(custom.selected));
  const choice = row.querySelector(".interact-choice");
  choice.className = "interact-choice custom-choice";
  setInteractText(choice, "+");
  const label = row.querySelector(".interact-opt-label");
  setInteractText(label, custom.value || "Type a custom answer…");
  label.classList.toggle("placeholder", !custom.value);
  const description = row.querySelector(".interact-opt-description");
  setInteractText(description, "Custom answer");
  description.classList.remove("hidden");
  row.onclick = clickInteractCustom;
}

function makeInteractMore(key, code) {
  const row = el("button", "interact-more");
  row.type = "button";
  row.dataset.key = key;
  row.onclick = () => enqueueKeys([K(code)]);
  return row;
}

function patchInteractOptions(model) {
  const opts = $("interact-options");
  opts.setAttribute("aria-multiselectable", String(model.multi));
  const existing = new Map(Array.from(opts.children, (node) => [node.dataset.key, node]));
  const rows = [];
  const use = (key, create) => {
    const node = existing.get(key) || create();
    existing.delete(key);
    rows.push(node);
    return node;
  };

  const selectedPosition = model.options.findIndex((option) => option.selected);
  const selectedIndex = selectedPosition >= 0 ? model.scrollAbove + selectedPosition : model.scrollAbove;
  for (let index = 0; index < interactState.total;) {
    const option = interactState.optionCache.get(index);
    if (option) {
      const key = `option-${index}`;
      const row = use(key, () => makeInteractOption(key, "option"));
      patchInteractOption(row, option, index, model.multi);
      index++;
      continue;
    }
    const start = index;
    while (index < interactState.total && !interactState.optionCache.has(index)) index++;
    const count = index - start;
    const above = start < selectedIndex;
    const key = `more-${start}-${index - 1}`;
    const row = use(key, () => makeInteractMore(key, above ? "PageUp" : "PageDown"));
    row.onclick = () => enqueueKeys([K(above ? "PageUp" : "PageDown")]);
    setInteractText(row, above ? `↑ ${count} earlier option${count === 1 ? "" : "s"}` : `${count} more option${count === 1 ? "" : "s"} ↓`);
  }
  if (model.custom) {
    const row = use("custom", () => makeInteractOption("custom", "custom"));
    patchInteractCustom(row, model.custom);
  }
  if (model.text) {
    const row = use("text", () => {
      const field = el("div", "interact-text");
      field.dataset.key = "text";
      field.setAttribute("role", "textbox");
      field.setAttribute("aria-label", "Your answer");
      field.appendChild(el("span", "interact-text-value"));
      field.appendChild(el("span", "interact-caret"));
      return field;
    });
    const value = row.querySelector(".interact-text-value");
    setInteractText(value, model.text.value || "Type your answer…");
    value.classList.toggle("placeholder", !model.text.value);
  }

  rows.forEach((node, index) => {
    if (opts.children[index] !== node) opts.insertBefore(node, opts.children[index] || null);
  });
  for (const node of existing.values()) node.remove();
  const selected = opts.querySelector(".selected");
  if (selected && !selected.matches(":hover") && selected.offsetParent) selected.scrollIntoView({ block: "nearest" });
}

function patchInteractPreview(model) {
  const preview = $("interact-preview");
  const hasPreview = !!(model.preview && model.preview.title);
  $("interact").classList.toggle("has-preview", hasPreview);
  $("interact-body").classList.toggle("previewing", hasPreview);
  preview.classList.toggle("hidden", !hasPreview);
  setInteractText($("interact-preview-title"), hasPreview ? model.preview.title : "");

  const content = $("interact-preview-content");
  const signature = hasPreview ? JSON.stringify(model.preview.lines) : "";
  if (content.dataset.signature === signature) return;
  content.dataset.signature = signature;
  const rows = [];
  if (hasPreview) {
    for (const values of model.preview.lines) {
      const row = el("div", "interact-preview-line");
      for (const value of values) {
        const valueSpan = el("span");
        valueSpan.textContent = value.text || "";
        if (value.fg) valueSpan.style.color = value.fg;
        const modifiers = value.modifiers || [];
        if (modifiers.includes("bold")) valueSpan.style.fontWeight = "700";
        if (modifiers.includes("dim")) valueSpan.style.opacity = "0.62";
        if (modifiers.includes("italic")) valueSpan.style.fontStyle = "italic";
        if (modifiers.includes("strike") || modifiers.includes("crossed_out")) valueSpan.style.textDecoration = "line-through";
        row.appendChild(valueSpan);
      }
      rows.push(row);
    }
  }
  content.replaceChildren(...rows);
}

function interactIdentity(model) {
  return JSON.stringify([model.title, model.question, model.multi, !!model.text]);
}

function renderInteractPane(model) {
  const identity = interactIdentity(model);
  if (interactState.identity !== identity) {
    interactState.identity = identity;
    interactState.optionCache.clear();
  }
  for (const option of interactState.optionCache.values()) option.selected = false;
  model.options.forEach((option, position) => {
    const index = model.scrollAbove + position;
    interactState.optionCache.set(index, { ...interactState.optionCache.get(index), ...option });
  });

  interactState.active = true;
  interactState.multi = model.multi;
  interactState.model = model;
  interactState.total = model.scrollAbove + model.options.length + model.scrollBelow;
  interactState.hasCustom = !!model.custom;

  const pane = $("interact");
  pane.classList.remove("hidden");
  setInteractText($("interact-kicker"), model.title || "Question");
  setInteractText($("interact-q"), model.question || "Choose an option");
  const notice = $("interact-notice");
  setInteractText(notice, model.notice || "");
  notice.classList.toggle("hidden", !model.notice);
  patchInteractOptions(model);
  patchInteractPreview(model);
  setInteractText($("interact-hint"), "Arrow keys move · Enter submits · Esc cancels");
}
function closeInteract() {
  interactState.active = false;
  interactState.queue = [];
  interactState.model = null;
  interactState.identity = "";
  interactState.optionCache.clear();
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
function clickInteractOption(index) {
  const model = interactState.model;
  if (!model) return;
  // A click only moves the cursor (multi also toggles the checkbox in place).
  // Committing is always an explicit Enter / Submit — never on selection.
  const keys = interactMoveKeys(interactSelectedIndex(model), index);
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
  flushAssistantMarkdown(true);
  finalizeTurn();
}
function onFailed(ev) {
  hideThinking();
  closeInteract();
  markRunning(state.conversationId, false);
  if (state.asstRaw) flushAssistantMarkdown(true);
  systemLine(ev.message || "turn failed", true);
  finalizeTurn();
  setRunning(false);
}
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
function finalizeTurn() {
  cancelAssistantFrame();
  state.asstEl = null;
  state.asstRaw = "";
  state.reasonEl = null;
  state.reasonDetails = null;
  state.tools.clear();
  state.toolInfo.clear();
}

function clearArtifacts() {
  artifacts.clear();
  activeArtifact = null;
  showingAllEdits = false;
  closeCanvas();
  $("canvas-toggle").classList.add("hidden");
  renderTabs();
}

function onConversationLoadFailed(ev) {
  const token = state.awaitingLoad;
  if (!token || token.mode !== "load" || token.id !== ev.id) return;
  recoverNavigation(token);
  systemLine(ev.message || `Failed to load conversation ${ev.id}`, true);
}

function onConversationLoaded(ev) {
  // A quick A→B double switch produces two loads (A then B); only the one we
  // last asked for is authoritative. Ignore a load for any other conversation so
  // it can't render over, or clear the gate ahead of, the target we want.
  if (state.awaitingLoad && !switchSatisfiedBy(ev.snapshot)) return;
  // The target conversation's view is now authoritative — stop dropping events.
  state.awaitingLoad = null;
  state.approvals.clear();
  state.answeredApprovals.clear();
  state.replyingApprovals.clear();
  state.keyId = null;
  state.answeredKeys.clear();
  state.replyingKeys.clear();
  state.sawStarted = false;
  $("thread").innerHTML = "";
  bgAgentRows = []; // rows live in the DOM we just discarded
  finalizeTurn();
  // Conversation routing is independent: the loaded actor's authoritative busy
  // state decides whether this tab's composer is available.
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
  const loadedId = ev.snapshot?.conversation_id ?? state.conversationId;
  const loadedRunning = typeof ev.busy === "boolean"
    ? ev.busy
    : state.runningConvs.has(loadedId);
  state.runningConvs.delete(loadedId);
  setRunning(loadedRunning);
  restoreDraft();
  clearTaskList();
  // An empty conversation (fresh chat) shows the welcome rather than a blank pane.
  if (!rendered) { $("thread").appendChild(buildWelcome()); }
  // Open on the latest exchange, not the first message.
  scrollToBottom();
  if (repair.active || loadedRunning) {
    // A late attachment missed the live stream head and may also have missed an
    // outstanding approval/key gate. Synchronize immediately and poll until the
    // actor is idle, just like explicit stream-lag recovery.
    repair.active = true;
    requestSynchronization();
  }
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
function applyViewHighlight(name, fg) {
  if (prefs.theme !== "auto" || name !== "accent") return;
  if (fg) document.documentElement.style.setProperty("--accent", fg);
  else document.documentElement.style.removeProperty("--accent");
}

function onViewSnapshot(view = {}) {
  const components = view.components || [];
  const interact = components.find((component) => component.id === "interact" && component.lines);
  const sameInteract = interactState.active && interact &&
    interactIdentity(parseInteractPane(interact)) === interactState.identity;
  taskState.active = false;
  taskState.items = [];
  renderTaskList();
  if (!sameInteract) closeInteract();
  if (prefs.theme === "auto") document.documentElement.style.removeProperty("--accent");
  for (const [name, fg] of Object.entries(view.highlights || {})) applyViewHighlight(name, fg);
  for (const component of components) renderViewDiff({ upsert: { component } });
}

function onViewDiff(diff) {
  renderViewDiff(diff);
}

function renderViewDiff(diff) {
  // Runtime-pushed accent colour: only honoured when the theme defers to it.
  if (diff?.set_highlight) applyViewHighlight(diff.set_highlight.name, diff.set_highlight.fg);

  // Task list pane (source="task_list") — render in sidebar. Theme-independent.
  if (diff && diff.upsert && diff.upsert.component) {
    const comp = diff.upsert.component;
    if (comp.id === "task_list" && comp.lines) {
      taskState.active = true;
      taskState.title = comp.title || "Tasks";
      taskState.items = parseTaskLines(comp.lines);
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
  line.innerHTML = renderMarkdown(text);
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
  const previousPending = state.awaitingLoad;
  const generation = ++state.navigationGeneration;
  if (id === state.conversationId && !previousPending) return;
  saveDraft();
  const leaving = state.conversationId;
  let leavingRunning = state.running || state.sending;
  const token = { mode: "load", id, from: leaving, draftChat: state.draftChat, generation };
  state.awaitingLoad = token;
  // A prompt POST and a navigation POST share the primary daemon link. Make sure
  // the prompt is written to the actor it came from before that link is repinned.
  if (state.sending && pendingSubmitRequest) {
    const delivered = await pendingSubmitRequest;
    if (generation !== state.navigationGeneration) return;
    if (!delivered && !state.running) leavingRunning = false;
  }
  // Start the old chat's watch before repinning the primary link. Issuing these
  // requests in this order closes the hand-off gap where neither socket would
  // be subscribed to the actor's broadcast.
  if (leavingRunning && leaving != null && leaving !== id) {
    state.runningConvs.add(leaving); // its dot/timer keep going while off-screen
    if (!await watchConversation(leaving)) {
      if (generation !== state.navigationGeneration) return;
      recoverNavigation(token);
      toast("Could not keep this running chat attached");
      return;
    }
  }
  if (generation !== state.navigationGeneration) return;
  await denyPending();
  if (!await routeConversation(generation, { load_conversation: { id } })) {
    if (generation === state.navigationGeneration) recoverNavigation(token);
    return;
  }
  if (generation !== state.navigationGeneration || token.failed) return;
  state.runningConvs.delete(id);
  unwatchConversation(id);
  if (state.awaitingLoad === token) state.conversationId = id;
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
  const generation = ++state.navigationGeneration;
  saveDraft();
  const leaving = state.conversationId;
  let leavingRunning = state.running || state.sending;
  const token = { mode: "new", from: leaving, draftChat: state.draftChat, generation };
  state.awaitingLoad = token;
  if (state.sending && pendingSubmitRequest) {
    const delivered = await pendingSubmitRequest;
    if (generation !== state.navigationGeneration) return;
    if (!delivered && !state.running) leavingRunning = false;
  }
  // Keep the chat we're leaving live in the background if it's still mid-turn.
  if (leavingRunning && leaving != null) {
    state.runningConvs.add(leaving);
    if (!await watchConversation(leaving)) {
      if (generation !== state.navigationGeneration) return;
      recoverNavigation(token);
      toast("Could not keep this running chat attached");
      return;
    }
  }
  if (generation !== state.navigationGeneration) return;
  await denyPending();
  if (!await routeConversation(generation, "new_conversation")) {
    if (generation === state.navigationGeneration) recoverNavigation(token);
    return;
  }
  if (generation !== state.navigationGeneration || token.failed) return;
  clearTaskList();
  $("thread").innerHTML = "";
  bgAgentRows = [];
  $("thread").appendChild(buildWelcome());
  finalizeTurn();
  setRunning(false);
  clearArtifacts();
  if (state.awaitingLoad === token) {
    state.conversationId = null;
    desiredConversationId = null;
    sessionStorage.removeItem("bone-active-conversation");
  }
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
  { key: "context_window_tokens", label: "Context window", placeholder: "Unknown", type: "number" },
  { key: "max_concurrency", label: "Max concurrency", placeholder: "Unlimited", type: "number" },
  { key: "reasoning_effort", label: "Reasoning effort", type: "effort" },
  { key: "fast_mode", label: "Fast mode", type: "checkbox", handlers: ["codex"] },
];

function providerUpdate(id, provider, fields = {}) {
  const merged = { ...provider, ...fields };
  return {
    id,
    label: merged.label || id,
    base_url: merged.base_url ?? "",
    model: merged.model ?? "",
    endpoint: merged.endpoint ?? "",
    handler: merged.handler ?? "",
    context_window_tokens: merged.context_window_tokens ?? null,
    max_concurrency: merged.max_concurrency ?? null,
    reasoning_effort: merged.reasoning_effort ?? "",
    fast_mode: merged.handler === "codex" && (merged.fast_mode ?? false),
    ...(Object.hasOwn(fields, "api_key") ? { api_key: fields.api_key } : {}),
  };
}

let _provExpanded = null;   // key of expanded card (null = collapsed)
let _provShowKey = null;    // key whose API key is revealed

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
      if (fd.handlers && !fd.handlers.includes(p.handler)) continue;
      const field = el("div", "prov-field");
      const lbl = el("label", null, fd.label);
      let input;
      if (fd.type === "select") {
        input = createProvSelect(p.key, fd.key, p[fd.key] ?? "", fd.options);
      } else if (fd.type === "effort") {
        input = createProvEffort(p.key, p[fd.key] ?? "");
      } else if (fd.type === "checkbox") {
        input = createProvCheckbox(p.key, fd.key, p[fd.key] === true);
      } else {
        input = createProvInput(p.key, fd.key, p[fd.key] ?? "", fd.placeholder, fd.type, fd.key === "api_key");
      }
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
  if (fieldKey === "max_concurrency") {
    input.min = "1";
    input.step = "1";
  }
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
  if (!options.includes(value)) {
    const unset = document.createElement("option");
    unset.value = value;
    unset.textContent = value || "Select…";
    sel.appendChild(unset);
  }
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

const REASONING_EFFORT_PRESETS = [
  ["Low", "low"],
  ["Med", "medium"],
  ["High", "high"],
  ["XHigh", "xhigh"],
];

function createProvEffort(providerKey, value) {
  const wrap = el("div", "prov-effort");
  const select = document.createElement("select");
  select.className = "prov-select";
  const isPreset = REASONING_EFFORT_PRESETS.some(([, preset]) => preset === value);
  for (const [label, preset] of REASONING_EFFORT_PRESETS) {
    const option = document.createElement("option");
    option.value = preset;
    option.textContent = label;
    option.selected = preset === value;
    select.appendChild(option);
  }
  const custom = document.createElement("option");
  custom.value = "__custom__";
  custom.textContent = "Custom";
  custom.selected = !isPreset;
  select.appendChild(custom);

  const input = document.createElement("input");
  input.className = "prov-input";
  input.value = isPreset ? "" : value;
  input.placeholder = "Custom value (for example ultra)";
  input.hidden = isPreset;
  input.onchange = () => saveProviderField(providerKey, "reasoning_effort", input.value.trim());
  input.onkeydown = (event) => { if (event.key === "Enter") input.blur(); };
  select.onchange = () => {
    if (select.value === "__custom__") {
      input.hidden = false;
      input.focus();
    } else {
      saveProviderField(providerKey, "reasoning_effort", select.value);
    }
  };
  wrap.append(select, input);
  return wrap;
}

function createProvCheckbox(providerKey, fieldKey, checked) {
  const input = document.createElement("input");
  input.className = "prov-check";
  input.type = "checkbox";
  input.checked = checked;
  input.onchange = () => saveProviderField(providerKey, fieldKey, input.checked);
  return input;
}

async function saveProviderField(providerKey, fieldKey, value) {
  const prov = state.providers.find((p) => p.key === providerKey);
  if (!prov) return;
  if (fieldKey === "max_concurrency") {
    if (value.trim() === "") {
      value = null;
    } else {
      value = Number(value);
      if (!Number.isInteger(value) || value < 1) {
        toast("Max concurrency must be blank or a positive integer");
        renderProviderPicker();
        return;
      }
    }
  }
  const oldVal = prov[fieldKey];
  const oldFastMode = prov.fast_mode;
  prov[fieldKey] = value;
  const fields = { [fieldKey]: value };
  if (fieldKey === "handler" && value !== "codex") {
    prov.fast_mode = false;
    fields.fast_mode = false;
  }
  if (providerKey === state.providerId && fieldKey === "model") state.model = value;
  renderModelLabel();
  renderProviderPicker();
  if (!$("model-pop").classList.contains("hidden")) positionModelPop();
  try {
    await writeRevisionedCommand("upsert_provider", {
      provider: providerUpdate(providerKey, prov, fields),
    });
    toast("Saved");
  } catch (error) {
    prov[fieldKey] = oldVal;
    prov.fast_mode = oldFastMode;
    toast(`Save failed: ${error.message}`);
    renderProviderPicker();
    await loadConfig();
  }
}

async function deleteProvider(key) {
  if (!confirm(`Delete provider "${key}"?`)) return;
  try {
    await writeRevisionedCommand("delete_provider", { id: key });
    state.providers = state.providers.filter((p) => p.key !== key);
    if (_provExpanded === key) _provExpanded = null;
    renderProviderPicker();
    if (!$("model-pop").classList.contains("hidden")) positionModelPop();
    toast("provider deleted");
  } catch (e) {
    toast("failed to delete: " + e.message);
    await loadConfig();
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
      <input class="prov-add-input" id="add-prov-model" placeholder="model" />
      <input class="prov-add-input" id="add-prov-url" placeholder="base URL" />
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
  const model = modelInput.value.trim();
  const base_url = urlInput.value.trim();
  try {
    await writeRevisionedCommand("upsert_provider", {
      provider: providerUpdate(key, {}, { label, model, base_url }),
    });
    keyInput.value = "";
    labelInput.value = "";
    modelInput.value = "";
    urlInput.value = "";
    toast(`added "${label}"`);
  } catch (e) {
    toast("failed to add: " + e.message);
    await loadConfig();
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

let configCache = { schema: { pages: [] }, snapshot: { revision: 0, values: {}, disabled_tools: [] } };

function adoptConfig(schema, snapshot) {
  if (!schema || !snapshot) return;
  configCache = { schema, snapshot };
  const providers = (snapshot.providers || []).map((provider) => ({
    key: provider.id,
    ...provider,
    api_key: "",
  }));
  const providersChanged = JSON.stringify(providers) !== JSON.stringify(state.providers);
  state.providers = providers;
  if (providersChanged) {
    if (prefs.providerId && providers.some((provider) => provider.key === prefs.providerId)) {
      state.providerId = prefs.providerId;
      state.model = providers.find((provider) => provider.key === prefs.providerId).model || state.model;
    }
    renderModelLabel();
    renderProviderPicker();
  }
  syncConfigState();
  renderBehavior();
  renderTools();
}

async function loadConfig() {
  clearError();
  try {
    const result = await requestJson("/api/config");
    adoptConfig(result.schema, result.snapshot);
  }
  catch (error) {
    adoptConfig(
      { pages: [] },
      { revision: 0, values: {}, disabled_tools: [] },
    );
    reportError("Could not load settings", error, loadConfig);
  }
}

function* schemaFields(pages = configCache.schema?.pages || []) {
  for (const page of pages) {
    yield* page.fields || [];
    yield* schemaFields(page.pages || []);
  }
}

function findField(path) {
  return [...schemaFields()].find((field) => field.path === path);
}

function configValue(field) {
  let value = configCache.snapshot?.values;
  for (const part of field.path.split(".")) {
    if (value == null || !Object.hasOwn(value, part)) {
      value = undefined;
      break;
    }
    value = value[part];
  }
  return value === undefined ? (field.value ?? field.default) : value;
}

function syncConfigState() {
  const approval = findField("general.approval");
  if (approval) renderMode(configValue(approval) === "danger");
  const reasoning = findField("general.show_reasoning");
  if (reasoning) document.body.classList.toggle("hide-thinking", !configValue(reasoning));
}

async function writeConfig(change) {
  try {
    const result = await mutateConfig("/api/config", { ...change, expected_revision: configCache.snapshot.revision });
    toast(result.restart_required ? "Saved — restart required" : "Saved");
  } catch (error) {
    toast(`Save failed: ${error.message}`);
    await loadConfig();
  }
}

async function mutateConfig(url, body) {
  const result = await requestJson(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  adoptConfig(result.schema, result.snapshot);
  return result;
}

function writeRevisionedCommand(kind, payload) {
  return mutateConfig("/api/config-command", buildRevisionedCommand(kind, payload, configCache.snapshot.revision));
}

async function mutateSubagent(kind, payload, success, failure, rerender = false) {
  try {
    await writeRevisionedCommand(kind, payload);
    await send("reload_extensions");
    if (success) toast(success);
  } catch (error) {
    toast(`${failure} failed: ${error.message}`);
    await loadConfig();
    if (rerender) renderAgents();
  }
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

function configControl(field) {
  const value = configValue(field);
  const save = (next) => {
    if (field.path === "general.approval") renderMode(next === "danger");
    writeConfig({ path: field.path, value: next });
  };
  if (field.type === "bool") return switchEl(Boolean(value), save);
  if (field.type === "enum") return enumEl(String(value ?? ""), field.options || [], save);

  const input = document.createElement("input");
  input.className = "set-num";
  input.type = field.type === "number" ? "number" : "text";
  if (field.min != null) input.min = String(field.min);
  if (field.max != null) input.max = String(field.max);
  if (field.integer) input.step = "1";
  input.value = String(value ?? "");
  const commit = () => save(field.type === "number" ? Number(input.value) : input.value);
  input.onblur = commit;
  input.onkeydown = (event) => { if (event.key === "Enter") input.blur(); };
  return input;
}

function renderConfigPage(wrap, page, heading = false) {
  if (heading && ((page.fields || []).length || (page.pages || []).length)) {
    wrap.appendChild(el("h3", "settings-section-title", page.title || page.namespace));
  }
  for (const field of page.fields || []) {
    wrap.appendChild(setRow(field.label || field.key, field.path, configControl(field)));
  }
  for (const child of page.pages || []) renderConfigPage(wrap, child, true);
}

function renderBehavior() {
  const wrap = $("behavior-fields");
  wrap.innerHTML = "";
  const pages = configCache.schema?.pages || [];
  const general = pages.find((page) => page.namespace === "general");
  if (general) renderConfigPage(wrap, general);
  const extensions = pages.find((page) => page.namespace === "extensions");
  if (extensions) renderConfigPage(wrap, extensions);

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
  { id: "nord", name: "Nord", dot: "#88c0d0" },
  { id: "dracula", name: "Dracula", dot: "#bd93f9" },
  { id: "gruvbox", name: "Gruvbox", dot: "#fabd2f" },
  { id: "solarized", name: "Solarized", dot: "#268bd2" },
  { id: "tokyo-night", name: "Tokyo Night", dot: "#7aa2f7" },
  { id: "catppuccin", name: "Catppuccin", dot: "#cba6f7" },
  { id: "rose-pine", name: "Rosé Pine", dot: "#c4a7e7" },
  { id: "one-dark", name: "One Dark", dot: "#61afef" },
  { id: "ayu", name: "Ayu", dot: "#ffb454" },
  { id: "everforest", name: "Everforest", dot: "#a7c080" },
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
  const status = (configCache.schema?.pages || []).find((page) => page.namespace === "status");
  if (status) renderConfigPage(wrap, status, true);
}

function agentInput(value, multiline = false) {
  const input = document.createElement(multiline ? "textarea" : "input");
  input.className = multiline ? "agent-textarea" : "set-input";
  input.value = value || "";
  return input;
}

function renderAgentEditor(agent = null) {
  const wrap = $("agents-fields");
  wrap.innerHTML = "";
  const draft = agent || { name: "", description: "", system_prompt: "", provider: "", model: "", approval: "safe", timeout_ms: "", enabled: true, source: "config" };
  const fields = {
    name: agentInput(draft.name),
    description: agentInput(draft.description),
    system_prompt: agentInput(draft.system_prompt, true),
    provider: agentInput(draft.provider),
    model: agentInput(draft.model),
    approval: enumEl(draft.approval || "safe", ["safe", "danger"], () => {}),
    timeout_ms: agentInput(draft.timeout_ms == null ? "" : String(draft.timeout_ms)),
    enabled: switchEl(draft.enabled !== false, () => {}),
  };
  if (agent) fields.name.disabled = true;
  for (const [key, label, desc] of [
    ["name", "Name", "Letters, digits, - and _"],
    ["description", "Description", "Shown to the parent agent"],
    ["system_prompt", "System prompt", "Optional role and operating instructions"],
    ["provider", "Provider", "Blank inherits the active provider"],
    ["model", "Model", "Blank inherits the active model"],
    ["approval", "Approval", "Safe asks before risky tools"],
    ["timeout_ms", "Timeout (ms)", "Optional, maximum 900000"],
    ["enabled", "Enabled", "Available to the delegation tool"],
  ]) wrap.appendChild(setRow(label, desc, fields[key]));

  const actions = el("div", "agent-actions");
  const cancel = el("button", "ghost-btn", "Cancel");
  cancel.onclick = renderAgents;
  const save = el("button", "btn", "Save agent");
  save.onclick = async () => {
    const name = fields.name.value.trim();
    const description = fields.description.value.trim();
    const timeoutText = fields.timeout_ms.value.trim();
    if (!/^[A-Za-z0-9_-]+$/.test(name)) return toast("Name may contain only letters, digits, - and _");
    if (!description) return toast("Description is required");
    const timeout = timeoutText ? Number(timeoutText) : null;
    if (timeout != null && (!Number.isInteger(timeout) || timeout < 1 || timeout > 900000)) return toast("Timeout must be 1–900000 ms");
    const definition = {
      name,
      description,
      system_prompt: fields.system_prompt.value.trim() || null,
      provider: fields.provider.value.trim() || null,
      model: fields.model.value.trim() || null,
      approval: fields.approval.value,
      timeout_ms: timeout,
      enabled: fields.enabled.querySelector("input").checked,
      source: "config",
    };
    await mutateSubagent("upsert_subagent", { agent: definition }, `saved ${name}`, "Save");
  };
  actions.append(cancel, save);
  wrap.appendChild(actions);
  fields.name.focus();
}

function renderAgents() {
  const wrap = $("agents-fields");
  if (!wrap) return;
  wrap.innerHTML = "";
  const head = el("div", "agent-actions");
  head.appendChild(el("div", "section-title", "Named sub-agents"));
  const add = el("button", "btn", "Add agent");
  add.onclick = () => renderAgentEditor();
  head.appendChild(add);
  wrap.appendChild(head);
  if (!state.subagents.length) wrap.appendChild(el("div", "set-desc", "No sub-agents configured."));
  for (const agent of state.subagents) {
    const control = el("div", "agent-row-actions");
    const edit = el("button", "ghost-btn", "Edit");
    edit.onclick = () => renderAgentEditor(agent);
    control.appendChild(edit);
    if (agent.source === "config") {
      const remove = el("button", "ghost-btn", "Delete");
      remove.onclick = async () => {
        if (!confirm(`Delete sub-agent “${agent.name}”?`)) return;
        await mutateSubagent("delete_subagent", { name: agent.name }, null, "Delete");
      };
      control.appendChild(remove);
    } else {
      control.appendChild(el("span", "agent-chip", "Lua"));
    }
    const toggle = switchEl(agent.enabled !== false, (enabled) =>
      mutateSubagent("set_subagent_enabled", { name: agent.name, enabled }, null, "Update", true));
    control.appendChild(toggle);
    wrap.appendChild(setRow(agent.name, agent.description, control));
  }
}

function renderTools() {
  const wrap = $("tools-fields");
  wrap.innerHTML = "";
  if (!state.toolDefs.length) { wrap.appendChild(el("div", "set-desc", "Tool list loads once connected.")); return; }
  const disabled = new Set(configCache.snapshot?.disabled_tools || []);
  for (const t of state.toolDefs) {
    const desc = (t.description || "").split("\n")[0].slice(0, 70);
    wrap.appendChild(setRow(t.name, desc, switchEl(!disabled.has(t.name), (enabled) => {
      writeConfig({ tool: t.name, enabled });
    })));
  }
}

// ── settings modal shell ──────────────────────────────────────────────────────

function openSettings() {
  renderBehavior();
  renderAgents();
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
function renderMode(d) {
  danger = d;
  const btn = $("mode-toggle");
  btn.classList.toggle("mode-safe", !danger);
  btn.classList.toggle("mode-danger", danger);
  $("mode-label").textContent = danger ? "Danger" : "Safe";
  const seg = $("behavior-approval-seg");
  if (seg) for (const b of seg.children) b.classList.toggle("active", b.dataset.mode === (danger ? "danger" : "safe"));
}

function setMode(d) {
  renderMode(d);
  return send({ set_approval_mode: { mode: danger ? "danger" : "safe" } });
}

// ── display prefs ─────────────────────────────────────────────────────────────

function loadPrefs() {
  let p = {};
  try { p = JSON.parse(localStorage.getItem("bone-studio-prefs") || "{}"); } catch {}
  return { expandTools: !!p.expandTools, showMeter: p.showMeter !== false, theme: p.theme || "codex-mono", sidebarW: clampSidebarW(p.sidebarW), canvasW: clampCanvasW(p.canvasW), providerId: p.providerId || null };
}
function savePrefs() { localStorage.setItem("bone-studio-prefs", JSON.stringify(prefs)); }
// Sidebar width is user-draggable; keep it within a sane range and fall back to
// the CSS default (280) when unset.
function clampSidebarW(w) { return w ? Math.max(240, Math.min(420, w)) : 0; }
function clampCanvasW(w) { return w ? Math.max(320, Math.min(innerWidth * .7, w)) : 0; }
function applyPrefs() {
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
  const request = send({ submit_prompt: submission });
  pendingSubmitRequest = request;
  const ok = await request;
  if (pendingSubmitRequest === request) pendingSubmitRequest = null;
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
  await denyPending();
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
$("interact-cancel").addEventListener("click", cancelInteract);
$("interact-submit").addEventListener("click", () => enqueueKeys([K("Enter")]));

applyPrefs();
autosize();
connect();
loadChats();
loadConfig();
setTimeout(() => send({ set_terminal_width: { width: 100 } }), 400);
