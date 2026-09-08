#!/usr/bin/env python3
"""Probe fresh-socket event ordering on the daemon: default replay vs NewConversation.
Connects, immediately sends NewConversation, prints the event sequence."""
import json, socket, time, sys

ADDR = ("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 17878)

s = socket.create_connection(ADDR, timeout=5)

def read(timeout):
    s.settimeout(timeout)
    try:
        data = s.recv(65536)
        if not data:
            return None
        return data.decode(errors="replace")
    except socket.timeout:
        return "TIMEOUT"

def dump(buf, tag):
    for line in buf.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
            kind = next(iter(ev))
            payload = ev[kind]
            conv = None
            if isinstance(payload, dict):
                conv = payload.get("conversation_id") or (payload.get("snapshot") or {}).get("conversation_id")
            print(f"[{tag}] {time.monotonic():.3f} {kind} conv={conv} keys={sorted(payload.keys()) if isinstance(payload, dict) else ''}")
        except Exception as e:
            print(f"[{tag}] raw: {line[:120]!r} ({e})")

t0 = time.monotonic()
# 1. Wait briefly for any initial replay.
time.sleep(0.5)
print(f"--- after connect {time.monotonic()-t0:.3f}s, sending NewConversation")
s.sendall((json.dumps("new_conversation") + "\n").encode())
end = time.monotonic() + 4
while time.monotonic() < end:
    buf = read(1.0)
    if buf is None:
        print("EOF")
        break
    if buf == "TIMEOUT":
        continue
    dump(buf, time.monotonic() - t0)
s.close()
