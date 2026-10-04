#!/usr/bin/env python3
"""Cheap package validation that does not require a running Bone server.

Bone runs LuaJIT (Lua 5.1), so files are checked with `luajit`, not a newer
`luac` that accepts syntax bone cannot load."""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
errors = []
for path in sorted((ROOT / "plugins").rglob("*.lua")):
    result = subprocess.run(["luajit", "-bl", str(path)], capture_output=True, text=True)
    if result.returncode:
        errors.append(f"{path}: {result.stderr.strip()}")
if errors:
    print("\n".join(errors), file=sys.stderr)
    raise SystemExit(1)
print("validated", len(list((ROOT / "plugins").rglob("*.lua"))), "Lua files")
