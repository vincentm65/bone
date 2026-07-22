#!/usr/bin/env node
// Zero-dependency bridge between the browser UI and a `bone serve` daemon.
//
//   browser  ──HTTP/SSE──▶  bridge  ──TCP (newline-JSON)──▶  bone serve
//
// The daemon speaks newline-delimited JSON: `RuntimeEvent`s out, `RuntimeCommand`s
// in (see core/src/rpc). The browser can't open a raw TCP socket, so the bridge
// gives it two HTTP endpoints instead:
//
//   GET  /api/events?session=ID   Server-Sent Events; opens a fresh daemon
//                                 connection and streams every RuntimeEvent.
//   POST /api/command?session=ID  body is one RuntimeCommand; written to the
//                                 daemon socket for that session.
//   POST /api/watch?session=ID    body { conversation_id }; opens an extra
//                                 read-only daemon socket pinned to a background
//                                 conversation so its live events keep flowing
//                                 while another chat is on screen. /api/unwatch
//                                 closes it. Events arrive tagged kind:"watch".
//
// Each browser tab gets its own daemon connection. The daemon's session manager
// routes that connection to one conversation actor and replays full state
// (frontend_state, state_snapshot, conversation_loaded) whenever it attaches.
// If nothing is listening on the daemon address, the bridge spawns `bone serve`.

import http from "node:http";
import net from "node:net";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join, extname, resolve, relative } from "node:path";
import { existsSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { randomUUID } from "node:crypto";

// ── usage stats ─────────────────────────────────────────────────────────────
//
// Reads `conversations.db` directly so the web UI can show the same stats
// dashboard as the TUI without going through the daemon. The SQL mirrors the
// queries in `core/src/session_db.rs` (porting the CTE-based bucket queries).

function openStatsDb() {
  if (!existsSync(DB_PATH)) return null;
  try {
    return new DatabaseSync(DB_PATH, { readOnly: true });
  } catch {
    return null;
  }
}

function readSummaryRow(row) {
  const v = Object.values(row);
  return {
    prompt_tokens: Number(v[0]),
    completion_tokens: Number(v[1]),
    cached_tokens: Number(v[2]),
    cost: Number(v[3]),
    request_count: Number(v[4]),
  };
}

function readProviderRow(row) {
  const v = Object.values(row);
  return {
    provider: v[0],
    model: v[1],
    prompt_tokens: Number(v[2]),
    completion_tokens: Number(v[3]),
    cached_tokens: Number(v[4]),
    cost: Number(v[5]),
    request_count: Number(v[6]),
  };
}

function readBucketRow(row) {
  const v = Object.values(row);
  return {
    label: v[0],
    prompt_tokens: Number(v[1]),
    completion_tokens: Number(v[2]),
    cached_tokens: Number(v[3]),
    cost: Number(v[4]),
    request_count: Number(v[5]),
  };
}

function readHourRow(row) {
  const v = Object.values(row);
  return {
    hour: Number(v[0]),
    prompt_tokens: Number(v[1]),
    completion_tokens: Number(v[2]),
    cached_tokens: Number(v[3]),
    request_count: Number(v[4]),
  };
}

const SUM_COLS = "COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0), COALESCE(SUM(cost),0.0), COUNT(*)";
const BUCKET_AGG_COLS = "COALESCE(SUM(prompt_tokens),0) AS prompt, COALESCE(SUM(completion_tokens),0) AS completion, COALESCE(SUM(cached_tokens),0) AS cached, COALESCE(SUM(cost),0.0) AS cost, COUNT(*) AS requests";
const BUCKET_PROJECTION = "COALESCE(usage.prompt,0), COALESCE(usage.completion,0), COALESCE(usage.cached,0), COALESCE(usage.cost,0.0), COALESCE(usage.requests,0)";

function timeWindowClause(window) {
  switch (window) {
    case "today":
      return { where: " WHERE date(created_at, 'localtime') = date('now', 'localtime')", params: [] };
    case "7d":
      return { where: " WHERE date(created_at, 'localtime') >= date('now', 'localtime', '-6 days')", params: [] };
    case "4w":
      return { where: " WHERE date(created_at, 'localtime') >= date('now', 'localtime', '-27 days')", params: [] };
    case "all":
    default:
      return { where: "", params: [] };
  }
}

function usageByModel(db, window) {
  const { where, params } = timeWindowClause(window);
  const sql = `SELECT provider, model, ${SUM_COLS} FROM usage_events${where} GROUP BY provider, model ORDER BY (COALESCE(SUM(prompt_tokens),0) + COALESCE(SUM(completion_tokens),0)) DESC`;
  try {
    return db.prepare(sql).all(...params).map(readProviderRow);
  } catch {
    return [];
  }
}

function usageTodayByHour(db) {
  const sql = `WITH RECURSIVE hours(hour) AS (
    VALUES(0) UNION ALL SELECT hour + 1 FROM hours WHERE hour < 23
  ), usage AS (
    SELECT CAST(strftime('%H', created_at, 'localtime') AS INTEGER) AS hour, ${BUCKET_AGG_COLS}
    FROM usage_events
    WHERE date(created_at, 'localtime') = date('now', 'localtime')
    GROUP BY hour
  )
  SELECT printf('%02d:00', hours.hour), ${BUCKET_PROJECTION}
  FROM hours LEFT JOIN usage ON usage.hour = hours.hour
  ORDER BY hours.hour ASC`;
  try {
    return db.prepare(sql).all().map(readBucketRow);
  } catch {
    return [];
  }
}

function usageRecentDays(db, days) {
  const modifier = `-${days - 1} days`;
  const sql = `WITH RECURSIVE series(n, day) AS (
    VALUES(0, date('now', 'localtime', '${modifier}'))
    UNION ALL SELECT n + 1, date(day, '+1 day') FROM series WHERE n + 1 < ${days}
  ), usage AS (
    SELECT date(created_at, 'localtime') AS day, ${BUCKET_AGG_COLS}
    FROM usage_events
    WHERE date(created_at, 'localtime') >= date('now', 'localtime', '${modifier}')
    GROUP BY day
  )
  SELECT series.day, ${BUCKET_PROJECTION}
  FROM series LEFT JOIN usage ON usage.day = series.day
  ORDER BY series.day ASC`;
  try {
    return db.prepare(sql).all().map(readBucketRow);
  } catch {
    return [];
  }
}

function usageRecentWeeks(db, weeks) {
  const firstLabelModifier = `-${(weeks - 1) * 7} days`;
  const usageModifier = `-${(weeks * 7 - 1)} days`;
  const sql = `WITH RECURSIVE series(n, week) AS (
    VALUES(0, strftime('%Y-W%W', date('now', 'localtime', '${firstLabelModifier}')))
    UNION ALL
    SELECT n + 1, strftime('%Y-W%W', date('now', 'localtime', printf('-%d days', (${weeks} - n - 2) * 7)))
    FROM series WHERE n + 1 < ${weeks}
  ), usage AS (
    SELECT strftime('%Y-W%W', created_at, 'localtime') AS week, ${BUCKET_AGG_COLS}
    FROM usage_events
    WHERE date(created_at, 'localtime') >= date('now', 'localtime', '${usageModifier}')
    GROUP BY week
  )
  SELECT series.week, ${BUCKET_PROJECTION}
  FROM series LEFT JOIN usage ON usage.week = series.week
  ORDER BY series.n ASC`;
  try {
    return db.prepare(sql).all().map(readBucketRow);
  } catch {
    return [];
  }
}

function usageAllMonths(db) {
  const sql = `WITH RECURSIVE bounds(first_month, current_month) AS (
    SELECT COALESCE(strftime('%Y-%m', MIN(created_at), 'localtime'),
                    strftime('%Y-%m', 'now', 'localtime')),
           strftime('%Y-%m', 'now', 'localtime')
    FROM usage_events
  ), series(month) AS (
    SELECT first_month FROM bounds
    UNION ALL
    SELECT strftime('%Y-%m', date(month || '-01', '+1 month'))
    FROM series, bounds WHERE month < current_month
  ), usage AS (
    SELECT strftime('%Y-%m', created_at, 'localtime') AS month, ${BUCKET_AGG_COLS}
    FROM usage_events
    GROUP BY month
  )
  SELECT series.month, ${BUCKET_PROJECTION}
  FROM series LEFT JOIN usage ON usage.month = series.month
  ORDER BY series.month ASC`;
  try {
    return db.prepare(sql).all().map(readBucketRow);
  } catch {
    return [];
  }
}

function usageByYear(db) {
  const sql = `SELECT strftime('%Y', created_at, 'localtime') AS year, ${SUM_COLS} FROM usage_events GROUP BY year ORDER BY year ASC`;
  try {
    return db.prepare(sql).all().map(readBucketRow);
  } catch {
    return [];
  }
}

function usageByHourSince(db, whereClause) {
  // Simplified: just get prompt/completion/cached without cost for hourly
  const sql2 = `SELECT CAST(strftime('%H', created_at, 'localtime') AS INTEGER), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(cached_tokens),0), COUNT(*) FROM usage_events${whereClause} GROUP BY 1 ORDER BY 1`;
  try {
    return db.prepare(sql2).all().map(readHourRow);
  } catch {
    return [];
  }
}

function loadStatsSnapshot() {
  const db = openStatsDb();
  if (!db) {
    return {
      started_at: null,
      ended_at: null,
      total: { prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0, request_count: 0 },
      by_model_today: [],
      by_model_7d: [],
      by_model_4w: [],
      by_model_all: [],
      daily: [],
      weekly: [],
      monthly: [],
      all_time: [],
      yearly: [],
      hourly_today: [],
      hourly_7d: [],
      hourly_4w: [],
      hourly_all: [],
      daily_activity: [],
    };
  }
  try {
    const vals = db.prepare(
      "SELECT datetime(MIN(created_at), 'localtime'), datetime(MAX(created_at), 'localtime') FROM usage_events"
    ).get();
    const started_at = vals ? Object.values(vals)[0] : null;
    const ended_at = vals ? Object.values(vals)[1] : null;

    const total = db.prepare(`SELECT ${SUM_COLS} FROM usage_events`).get();
    const totalParsed = total ? readSummaryRow(total) : { prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0, request_count: 0 };

    return {
      started_at,
      ended_at,
      total: totalParsed,
      by_model_today: usageByModel(db, "today"),
      by_model_7d: usageByModel(db, "7d"),
      by_model_4w: usageByModel(db, "4w"),
      by_model_all: usageByModel(db, "all"),
      daily: usageTodayByHour(db),
      weekly: usageRecentDays(db, 7),
      monthly: usageRecentWeeks(db, 4),
      all_time: usageAllMonths(db),
      yearly: usageByYear(db),
      hourly_today: usageByHourSince(db, " WHERE date(created_at, 'localtime') = date('now', 'localtime')"),
      hourly_7d: usageByHourSince(db, " WHERE date(created_at, 'localtime') >= date('now', 'localtime', '-6 days')"),
      hourly_4w: usageByHourSince(db, " WHERE date(created_at, 'localtime') >= date('now', 'localtime', '-27 days')"),
      hourly_all: usageByHourSince(db, ""),
      daily_activity: usageRecentDays(db, 730),
    };
  } catch (e) {
    console.error("stats query failed:", e.message);
    return {
      started_at: null,
      ended_at: null,
      total: { prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0, request_count: 0 },
      by_model_today: [],
      by_model_7d: [],
      by_model_4w: [],
      by_model_all: [],
      daily: [],
      weekly: [],
      monthly: [],
      all_time: [],
      yearly: [],
      hourly_today: [],
      hourly_7d: [],
      hourly_4w: [],
      hourly_all: [],
      daily_activity: [],
    };
  } finally {
    db.close();
  }
}

const HERE = dirname(fileURLToPath(import.meta.url));
const PUBLIC = join(HERE, "public");
const REPO = dirname(HERE);

const PORT = Number(process.env.PORT || 4577);
const [DAEMON_HOST, DAEMON_PORT] = (process.env.BONE_ADDR || "127.0.0.1:7878").split(":");

// bone's data lives under bone_dir() — mirror core/src/config::bone_dir().
function boneDir() {
  if (process.env.XDG_CONFIG_HOME) return join(process.env.XDG_CONFIG_HOME, "bone-rust");
  const home = process.env.HOME || process.env.USERPROFILE;
  return home ? join(home, ".bone-rust") : "/tmp/.bone-rust";
}
const DB_PATH = join(boneDir(), "data", "conversations.db");

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
};

// session id -> { sse: res, link, watches: Map<convId, link> }
const sessions = new Map();

// ── daemon lifecycle ────────────────────────────────────────────────────────

function findBoneBinary() {
  if (process.env.BONE_BIN) return { cmd: process.env.BONE_BIN, args: ["serve"] };
  const exe = process.platform === "win32" ? "bone.exe" : "bone";
  const packaged = join(REPO, "bin", exe);
  const release = join(REPO, "target", "release", exe);
  const debug = join(REPO, "target", "debug", exe);
  if (existsSync(packaged)) return { cmd: packaged, args: ["serve"] };
  if (existsSync(release)) return { cmd: release, args: ["serve"] };
  if (existsSync(debug)) return { cmd: debug, args: ["serve"] };
  return { cmd: "cargo", args: ["run", "-q", "-p", "bone", "--", "serve"] };
}

let daemonProc = null;
function ensureDaemon() {
  if (daemonProc) return;
  const { cmd, args } = findBoneBinary();
  log(`daemon not reachable — spawning: ${cmd} ${args.join(" ")}`);
  daemonProc = spawn(cmd, args, { cwd: REPO, stdio: ["ignore", "inherit", "inherit"] });
  daemonProc.on("error", (err) => {
    log(`failed to spawn daemon: ${err.message}`);
    daemonProc = null;
  });
  daemonProc.on("exit", (code) => {
    log(`daemon exited (code ${code})`);
    daemonProc = null;
  });
}

// Hard-restart the daemon. Used to recover when a turn wedges (e.g. an approval
// abandoned by another client leaves the runtime blocked forever). Killing the
// tracked child process is enough — every session's self-healing link redials
// and respawns it via ensureDaemon. The conversation survives in the SQLite
// history and can be reloaded.
// Returns true if a tracked daemon was killed, false if the daemon was not
// managed by this bridge (externally managed or already stopped).
function restartDaemon() {
  if (!daemonProc) { log("restart requested — no tracked daemon"); return false; }
  log("restart requested — killing daemon");
  try { daemonProc.kill("SIGKILL"); } catch {}
  daemonProc = null;
  setTimeout(ensureDaemon, 700);
  return true;
}

// A self-healing link to the daemon. Dials with backoff (spawning the daemon if
// nothing is listening yet), reconnects if the daemon restarts, and reports
// status transitions. Returns a stable handle whose `write` always targets the
// current socket — so a command sent right after first boot still lands.
//
//   onLine(line)        a newline-framed RuntimeEvent JSON string arrived
//   onStatus("connected"|"disconnected")
function createDaemonLink(onLine, onStatus) {
  let socket = null;
  let buffer = "";
  let connected = false;
  let closed = false;
  let attempt = 0;

  const dial = () => {
    if (closed) return;
    socket = net.createConnection({ host: DAEMON_HOST, port: Number(DAEMON_PORT) });
    socket.setEncoding("utf8");

    socket.on("connect", () => {
      attempt = 0;
      connected = true;
      log("→ daemon connected");
      onStatus("connected");
    });
    socket.on("data", (chunk) => {
      buffer += chunk;
      let nl;
      while ((nl = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, nl);
        buffer = buffer.slice(nl + 1);
        if (line.trim()) onLine(line);
      }
    });
    socket.on("error", (err) => {
      if (err.code === "ECONNREFUSED") ensureDaemon();
      else log(`daemon socket error: ${err.message}`);
    });
    // 'close' follows both a clean disconnect and a failed dial. Only surface a
    // user-visible drop if we were actually connected; otherwise keep retrying
    // quietly while the daemon boots.
    socket.on("close", () => {
      if (connected) {
        connected = false;
        onStatus("disconnected");
      }
      if (!closed && attempt < 120) {
        attempt++;
        setTimeout(dial, 400);
      }
    });
  };

  dial();

  return {
    write: (obj) => {
      if (socket && connected && !socket.destroyed) {
        socket.write(JSON.stringify(obj) + "\n");
        return true;
      }
      return false;
    },
    close: () => {
      closed = true;
      if (socket) socket.end();
    },
  };
}

// ── local data (chats) ──────────────────────────────────────────────────────
//
// The runtime protocol has no "list conversations" command,
// but the bridge is local: it reads bone's SQLite history directly so the UI
// can show a chat sidebar as a real widget.

function listConversations() {
  if (!existsSync(DB_PATH)) return [];
  const db = new DatabaseSync(DB_PATH);
  try {
    ensureWebuiMetadata(db);
    return db
      .prepare(
        `SELECT c.id AS id, c.provider AS provider, c.model AS model,
                c.started_at AS started_at, c.ended_at AS ended_at,
                COALESCE(meta.title, first_user.content) AS title,
                (SELECT COUNT(*) FROM messages WHERE conversation_id = c.id) AS n,
                (SELECT MAX(created_at) FROM messages WHERE conversation_id = c.id) AS last_at
         FROM conversations c
         LEFT JOIN webui_conversations meta ON meta.conversation_id = c.id
         JOIN messages first_user ON first_user.id = (
           SELECT m.id FROM messages m
           WHERE m.conversation_id = c.id AND m.role = 'user'
           ORDER BY m.seq ASC, m.id ASC LIMIT 1
         )
         WHERE first_user.content NOT LIKE 'unique-task-%'
           AND COALESCE(meta.archived, 0) = 0
         ORDER BY COALESCE(last_at, c.started_at) DESC, c.id DESC LIMIT 80`,
      )
      .all()
      .filter((r) => r.n > 0 && r.title)
      .map((r) => ({
        id: r.id,
        provider: r.provider,
        model: r.model,
        started_at: r.started_at,
        last_at: r.last_at || r.started_at,
        title: String(r.title).replace(/\s+/g, " ").trim().slice(0, 80),
      }));
  } finally {
    db.close();
  }
}

function ensureWebuiMetadata(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS webui_conversations (
    conversation_id INTEGER PRIMARY KEY,
    title TEXT,
    archived INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
  )`);
}

function updateConversation(id, changes) {
  if (!Number.isInteger(id) || id < 1) throw new Error("invalid conversation id");
  if (!existsSync(DB_PATH)) throw new Error("conversation database missing");
  const db = new DatabaseSync(DB_PATH);
  try {
    ensureWebuiMetadata(db);
    if (Object.hasOwn(changes, "title")) {
      const title = String(changes.title || "").replace(/\s+/g, " ").trim().slice(0, 80);
      db.prepare(`INSERT INTO webui_conversations(conversation_id, title, updated_at)
        VALUES (?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(conversation_id) DO UPDATE SET title=excluded.title, updated_at=CURRENT_TIMESTAMP`).run(id, title || null);
    }
    if (changes.archived === true) {
      db.prepare(`INSERT INTO webui_conversations(conversation_id, archived, updated_at)
        VALUES (?, 1, CURRENT_TIMESTAMP)
        ON CONFLICT(conversation_id) DO UPDATE SET archived=1, updated_at=CURRENT_TIMESTAMP`).run(id);
    }
  } finally { db.close(); }
}

function handleConversationWrite(req, res, id) {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    try {
      updateConversation(id, req.method === "DELETE" ? { archived: true } : JSON.parse(body));
      res.writeHead(204).end();
    } catch (e) { res.writeHead(400).end(String(e)); }
  });
}

async function daemonConfigCommand(command) {
  const requestId = typeof command === "object" ? randomUUID() : null;
  if (requestId) {
    const kind = Object.keys(command)[0];
    command = { [kind]: { ...command[kind], request_id: requestId } };
  }
  return new Promise((resolve, reject) => {
    let sent = false;
    let timer;
    let link;
    const finish = (callback, value) => {
      clearTimeout(timer);
      link.close();
      callback(value);
    };
    link = createDaemonLink(
      (line) => {
        let event;
        try { event = JSON.parse(line); } catch { return; }
        if (!requestId && event.config_snapshot) finish(resolve, event.config_snapshot);
        else if (!requestId && event.config_changed) finish(resolve, event.config_changed);
        else if (event.config_changed?.request_id === requestId) finish(resolve, event.config_changed);
        else if (event.config_mutation_rejected?.request_id === requestId)
          finish(reject, new Error(event.config_mutation_rejected.error));
      },
      (status) => {
        if (status === "connected" && !sent) {
          sent = true;
          if (!link.write(command)) finish(reject, new Error("daemon disconnected"));
        }
      },
    );
    timer = setTimeout(() => finish(reject, new Error("configuration request timed out")), 5000);
  });
}

async function daemonConfigSnapshot() {
  return (await daemonConfigCommand("get_config")).snapshot;
}

async function readProvidersFromDaemon() {
  return (await daemonConfigSnapshot()).providers.map((provider) => ({
    key: provider.id,
    ...provider,
    api_key: "",
  }));
}

async function getConfigFromDaemon() {
  return daemonConfigCommand("get_config");
}

async function sendJson(res, fn) {
  try {
    const data = await fn();
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify(data));
  } catch (e) {
    res.writeHead(500, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: String(e) }));
  }
}

// ── http server ─────────────────────────────────────────────────────────────

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);

  if (url.pathname === "/api/events") return handleEvents(url, req, res);
  if (url.pathname === "/api/command" && req.method === "POST") return handleCommand(url, req, res);
  if (url.pathname === "/api/conversations" && req.method === "GET") return sendJson(res, listConversations);
  const conversationMatch = url.pathname.match(/^\/api\/conversations\/(\d+)$/);
  if (conversationMatch && (req.method === "PATCH" || req.method === "DELETE"))
    return handleConversationWrite(req, res, Number(conversationMatch[1]));
  if (url.pathname === "/api/providers" && req.method === "GET") return sendJson(res, readProvidersFromDaemon);
  const providerMatch = url.pathname.match(/^\/api\/providers\/([^/]+)$/);
  if (providerMatch && req.method === "PATCH") return handleProviderPatch(req, res, providerMatch[1]);
  if (providerMatch && req.method === "DELETE") return handleProviderDelete(req, res, providerMatch[1]);
  if (url.pathname === "/api/providers" && req.method === "POST") return handleProviderPost(req, res);
  if (url.pathname === "/api/stats") return sendJson(res, loadStatsSnapshot);
  if (url.pathname === "/api/file" && req.method === "GET") return sendJson(res, async () => {
    const requested = url.searchParams.get("path");
    if (!requested) throw new Error("path is required");
    const root = process.cwd();
    const file = resolve(root, requested);
    const rel = relative(root, file);
    if (rel.startsWith("..") || rel === "") throw new Error("path must be a workspace file");
    return { path: rel, absolute_path: file, content: await readFile(file, "utf8") };
  });
  if (url.pathname === "/api/config" && req.method === "GET") return sendJson(res, getConfigFromDaemon);
  if (url.pathname === "/api/config" && req.method === "POST") return handleConfigWrite(req, res);
  if (url.pathname === "/api/restart-daemon" && req.method === "POST") {
    if (restartDaemon()) return res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ ok: true }));
    res.writeHead(503, { "content-type": "application/json" }).end(JSON.stringify({ error: "daemon not managed by this bridge; restart manually" }));
    return;
  }
  if (url.pathname === "/api/watch" && req.method === "POST") return handleWatch(url, req, res, true);
  if (url.pathname === "/api/unwatch" && req.method === "POST") return handleWatch(url, req, res, false);

  // static files
  let p = url.pathname === "/" ? "/index.html" : url.pathname;
  const file = join(PUBLIC, p.replace(/\.\./g, ""));
  try {
    const body = await readFile(file);
    // No caching: this is a local dev UI whose assets change often. Without
    // this the browser heuristically caches app.js/styles.css and silently
    // runs stale code after edits.
    res.writeHead(200, {
      "content-type": MIME[extname(file)] || "application/octet-stream",
      "cache-control": "no-cache, no-store, must-revalidate",
    });
    res.end(body);
  } catch {
    res.writeHead(404).end("not found");
  }
});

// POST /api/config — revision-checked daemon configuration mutation.
function handleConfigWrite(req, res) {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", async () => {
    try {
      const { path, value, tool, enabled, expected_revision } = JSON.parse(body);
      let command;
      if (tool) {
        command = { set_tool_enabled: { name: tool, enabled, expected_revision } };
      } else if (path) {
        command = { set_config_value: { path, value, expected_revision } };
      } else {
        throw new Error("path or tool is required");
      }
      const event = await daemonConfigCommand(command);
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(event));
    } catch (e) {
      res.writeHead(400, { "content-type": "text/plain" });
      res.end(String(e));
    }
  });
}

// ── provider endpoints ──────────────────────────────────────────────────────

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
    reasoning_effort: merged.reasoning_effort ?? "",
    ...(Object.hasOwn(fields, "api_key") ? { api_key: fields.api_key } : {}),
  };
}

// PATCH /api/providers/:key — body { field, value } or { fields: { ... } }
function handleProviderPatch(req, res, key) {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", async () => {
    try {
      const data = JSON.parse(body);
      const snapshot = await daemonConfigSnapshot();
      const provider = snapshot.providers.find((entry) => entry.id === key);
      if (!provider) throw new Error(`provider "${key}" not found`);
      const fields = data.fields || { [data.field]: data.value };
      await daemonConfigCommand({ upsert_provider: {
        provider: providerUpdate(key, provider, fields),
        expected_revision: snapshot.revision,
      } });
      res.writeHead(204).end();
    } catch (e) {
      res.writeHead(400, { "content-type": "text/plain" });
      res.end(String(e));
    }
  });
}

// DELETE /api/providers/:key
async function handleProviderDelete(_req, res, key) {
  try {
    const snapshot = await daemonConfigSnapshot();
    await daemonConfigCommand({ delete_provider: { id: key, expected_revision: snapshot.revision } });
    res.writeHead(204).end();
  } catch (e) {
    res.writeHead(400, { "content-type": "text/plain" }).end(String(e));
  }
}

// POST /api/providers — body { key, label, ...field values }
function handleProviderPost(req, res) {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", async () => {
    try {
      const data = JSON.parse(body);
      const snapshot = await daemonConfigSnapshot();
      await daemonConfigCommand({ upsert_provider: {
        provider: providerUpdate(data.key, {}, data),
        expected_revision: snapshot.revision,
      } });
      res.writeHead(201).end();
    } catch (e) {
      res.writeHead(400, { "content-type": "text/plain" });
      res.end(String(e));
    }
  });
}

function handleEvents(url, req, res) {
  const id = url.searchParams.get("session") || Math.random().toString(36).slice(2);
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
    connection: "keep-alive",
  });

  const send = (obj) => {
    if (!res.writableEnded) res.write(`data: ${JSON.stringify(obj)}\n\n`);
  };

  const link = createDaemonLink(
    (line) => {
      try {
        send({ kind: "event", payload: JSON.parse(line) });
      } catch {
        /* skip malformed frame */
      }
    },
    (status) => send({ kind: "bridge", status }),
  );

  const sess = { sse: res, link, watches: new Map() };
  sessions.set(id, sess);

  // First thing the browser learns is its assigned session id.
  send({ kind: "bridge", session: id });

  const ping = setInterval(() => {
    if (!res.writableEnded) res.write(": ping\n\n");
  }, 15000);

  req.on("close", () => {
    clearInterval(ping);
    link.close();
    for (const w of sess.watches.values()) w.link.close();
    sess.watches.clear();
    sessions.delete(id);
    log(`session ${id} closed`);
  });
}

// A background "watch" link: a second daemon connection pinned to a conversation
// the browser isn't currently viewing, so that conversation's live events (task
// list, turn lifecycle) keep flowing while another chat is on screen. The daemon
// runs an independent actor per conversation and accepts many connections, so
// this simply attaches to that actor's broadcast. Watch links are read-only — the
// browser never routes commands to them — so they can never wedge the turn loop.
// Events are tagged with the conversation id (the bridge knows it, since it chose
// which conversation to pin) because the wire protocol itself carries none.
function openWatch(sess, convId) {
  const existing = sess.watches.get(convId);
  if (existing) return existing.ready;
  const send = (obj) => { if (!sess.sse.writableEnded) sess.sse.write(`data: ${JSON.stringify(obj)}\n\n`); };
  let resolveReady;
  const ready = new Promise((resolve) => { resolveReady = resolve; });
  let link;
  link = createDaemonLink(
    (line) => {
      try {
        const payload = JSON.parse(line);
        send({ kind: "watch", conversation_id: convId, payload });
        // Do not acknowledge /api/watch until the daemon confirms this socket is
        // attached to the requested actor. This makes the subsequent primary
        // repin a lossless hand-off rather than a best-effort race.
        const body = payload && typeof payload === "object"
          ? (payload.state_snapshot || payload.conversation_loaded)
          : null;
        const snapshot = body && (body.snapshot || body);
        if (snapshot && snapshot.conversation_id === convId) resolveReady(true);
      } catch {}
    },
    // A fresh connection attaches to the daemon's latest conversation; pin it to
    // the one we want to watch. Re-pins automatically after a reconnect too.
    (status) => { if (status === "connected") link.write({ load_conversation: { id: convId } }); },
  );
  sess.watches.set(convId, { link, ready });
  return ready;
}

function closeWatch(sess, convId) {
  const watch = sess.watches.get(convId);
  if (watch) { watch.link.close(); sess.watches.delete(convId); }
}

// POST /api/watch|/api/unwatch — body { conversation_id }. Open/close a background
// watch link for the given conversation on this SSE session.
function handleWatch(url, req, res, on) {
  const id = url.searchParams.get("session");
  const sess = sessions.get(id);
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", async () => {
    if (!sess) { res.writeHead(409).end("no session"); return; }
    let convId;
    try { convId = JSON.parse(body).conversation_id; } catch { res.writeHead(400).end("bad body"); return; }
    if (!Number.isInteger(convId)) { res.writeHead(400).end("bad conversation_id"); return; }
    if (!on) { closeWatch(sess, convId); res.writeHead(204).end(); return; }
    const attached = await Promise.race([
      openWatch(sess, convId),
      new Promise((resolve) => setTimeout(() => resolve(false), 5000)),
    ]);
    if (attached) res.writeHead(204).end();
    else { closeWatch(sess, convId); res.writeHead(504).end("watch attach timed out"); }
  });
}

function handleCommand(url, req, res) {
  const id = url.searchParams.get("session");
  const sess = sessions.get(id);
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    if (!sess) {
      res.writeHead(409).end("no session");
      return;
    }
    try {
      const cmd = JSON.parse(body);
      if (sess.link.write(cmd)) res.writeHead(204).end();
      else res.writeHead(409).end("daemon not connected");
    } catch (e) {
      res.writeHead(400).end(String(e));
    }
  });
}

function log(msg) {
  const t = new Date().toLocaleTimeString();
  console.log(`\x1b[2m[${t}]\x1b[0m ${msg}`);
}

server.listen(PORT, () => {
  console.log(`\n  \x1b[1mbone studio\x1b[0m`);
  console.log(`  ▸ ui      http://localhost:${PORT}`);
  console.log(`  ▸ daemon  ${DAEMON_HOST}:${DAEMON_PORT}\n`);
});
