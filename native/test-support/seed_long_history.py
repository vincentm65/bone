#!/usr/bin/env python3
"""Seed a long, varied history conversation (id 7) for virtualization smoke tests.

Inserts into target/desktop-validation/data/conversations.db a NEW conversation
with `turns` user/assistant pairs (620 messages by default), mirroring the real
PersistedMessageV2 payload rows written by the daemon (see core/src/session_db.rs).
Conversations 1-6 and all other tables are left untouched; running again reseeds
conversation 7 only.

Usage:
    python3 native/test-support/seed_long_history.py [--turns N] [--db PATH]
"""
import argparse
import json
import sqlite3
import sys
from datetime import datetime, timedelta, timezone

MARKER_FIRST = "FIRSTREQUEST-LONGHISTORY"
MARKER_LAST = "LASTANSWER-LONGHISTORY"

BULLETS = (
    "summary of the change",
    "files touched",
    "test coverage added",
    "edge cases handled",
)

# Pool of short realistic assistant paragraphs to vary row heights.
PARA_POOL = [
    "I traced the request through the RPC layer and confirmed the session "
    "database replays every durable message in sequence order before the first "
    "frame is painted, so nothing depends on transcript length.",
    "The variable-height cache keeps measured row heights stable across frames; "
    "only the visible band plus a small overscan is re-measured on each render, "
    "which is what keeps long histories warm.",
    "One caveat: a width change invalidates the whole layout because wrapping "
    "depends on the available width, so that frame legitimately re-measures "
    "every row once.",
    "I verified the undo path does not touch persisted rows and that "
    "cancellation scopes to the requesting tab only.",
    "Error rows keep their payload so retry can resubmit the exact same "
    "request without reconstructing the message.",
    "The fixture returns plain text for now; structured tool events are a "
    "separate milestone in the plan.",
    "Markdown headings and code fences change row height, which is exactly the "
    "kind of variance this test corpus wants.",
    "I would keep the cache keyed by (row index, width, zoom) and evict parsed "
    "markdown only outside the slack window so scrolling stays smooth.",
]


def payload(role, content, created_at, is_last=False):
    if role == "user":
        message = {"role": "user", "content": content}
        return {"version": 2, "message": message}
    output = [{"type": "text", "value": content}]
    message = {"role": "assistant", "content": content, "created_at": created_at}
    return {"version": 2, "message": message, "output_sequence": output}


def build_rows(turns, start):
    rows = []
    seq = 1
    ts = start
    step = timedelta(seconds=120)

    # Turn 0: user opener carrying the top marker.
    rows.append(
        (
            "user",
            MARKER_FIRST
            + " open the project and walk through the long-history rendering path.",
            ts,
        )
    )
    ts += step
    seq += 1
    for t in range(turns):
        assistant_is_last = t == turns - 1
        if t % 3 == 0:
            body = "\n\n".join(
                f"{b}:\n- {MARKER_FIRST if (t == 0) else 'point'} {t}-{i}"
                for i, b in enumerate(BULLETS)
            )
        elif t % 3 == 1:
            body = (
                "```rust\n"
                "fn warm(p95: f64) -> bool { p95 < 16.7 }\n"
                "```\n\n" + PARA_POOL[t % len(PARA_POOL)]
            )
        else:
            body = "\n\n".join(
                PARA_POOL[(t + i) % len(PARA_POOL)] for i in range(2 + t % 3)
            )
        if assistant_is_last:
            body = MARKER_LAST + "\n\n" + body
        rows.append(("assistant", body, ts))
        ts += step
        seq += 1

        if t < turns - 1:
            rows.append(
                (
                    "user",
                    f"Question {t + 1}: what changed next and does it stay cached?",
                    ts,
                )
            )
            ts += step
            seq += 1
    return rows, seq - 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--turns", type=int, default=310)
    ap.add_argument(
        "--db",
        default="target/desktop-validation/data/conversations.db",
    )
    args = ap.parse_args()

    conn = sqlite3.connect(args.db)
    cur = conn.cursor()

    existing = cur.execute(
        "SELECT id FROM conversations WHERE id = 7"
    ).fetchone()
    if existing:
        cur.execute("DELETE FROM messages WHERE conversation_id = 7")
        cur.execute("DELETE FROM conversation_context_checkpoints WHERE conversation_id = 7")
        cur.execute("DELETE FROM conversations WHERE id = 7")
        print("reseed: removed previous conversation 7")

    start = datetime(2026, 9, 6, 8, 0, 0, tzinfo=timezone.utc)
    rows, last_seq = build_rows(args.turns, start)

    cur.execute(
        "INSERT INTO conversations (id, started_at, ended_at, provider, model)"
        " VALUES (7, ?, NULL, 'desktop-fixture', 'desktop-fixture')",
        (start.strftime("%Y-%m-%dT%H:%M:%SZ"),),
    )
    for seq, (role, content, ts) in enumerate(rows, start=1):
        created_at = ts.strftime("%Y-%m-%dT%H:%M:%SZ")
        payload_json = json.dumps(payload(role, content, created_at))
        cur.execute(
            "INSERT INTO messages (conversation_id, role, content, is_error,"
            " payload_json, seq, created_at)"
            " VALUES (7, ?, ?, 0, ?, ?, ?)",
            (role, content, payload_json, seq, created_at),
        )
    conn.commit()

    count = cur.execute(
        "SELECT count(*), min(seq), max(seq) FROM messages WHERE conversation_id = 7"
    ).fetchone()
    first = cur.execute(
        "SELECT content FROM messages WHERE conversation_id = 7 AND seq = 1"
    ).fetchone()[0]
    last = cur.execute(
        "SELECT content FROM messages WHERE conversation_id = 7 AND seq = ?",
        (count[2],),
    ).fetchone()[0]
    print(f"seeded conversation 7: {count[0]} messages (seq {count[1]}..{count[2]})")
    print(f"first starts: {first[:40]!r}")
    print(f"last starts:  {last[:40]!r}")
    conn.close()


if __name__ == "__main__":
    main()
