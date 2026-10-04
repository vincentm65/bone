#!/usr/bin/env python3
"""Load every catalog core half through a real Bone 3 headless server."""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent
binary = Path(os.getenv("BONE_BIN", str(ROOT.parent / "target/debug/bone")))
if not binary.exists():
    raise SystemExit(f"missing {binary}; run cargo build -p bone first")

with tempfile.TemporaryDirectory() as raw:
    config = Path(raw)
    shutil.copytree(ROOT / "plugins", config / "plugins")
    (config / "core.lua").write_text(
        'bone.config.providers.fake = { base_url = "http://127.0.0.1:1/v1", model = "catalog-check" }\n'
        'bone.config.provider = "fake"\n'
    )
    env = dict(os.environ, BONE_CONFIG_DIR=str(config))
    process = subprocess.Popen([str(binary), "--headless"], stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, env=env)
    def call(request):
        assert process.stdin and process.stdout
        process.stdin.write(json.dumps(request) + "\n")
        process.stdin.flush()
        return json.loads(process.stdout.readline())
    initialized = call({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {"protocol_version": 0, "client_name": "catalog-check"}})
    if "error" in initialized:
        raise SystemExit(initialized["error"])
    result = call({"jsonrpc": "2.0", "id": 2, "method": "plugin/list", "params": {}})
    if "error" in result:
        raise SystemExit(result["error"])
    expected = sorted(p.name for p in (ROOT / "plugins").iterdir()
                      if p.is_dir() and (p / "core.lua").exists())
    loaded = sorted(item["name"] for item in result["result"] if item.get("core") and item.get("loaded"))
    if loaded != expected:
        raise SystemExit(f"core plugin mismatch: expected {expected}, got {loaded}")
    call({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}})
    process.wait(timeout=5)
    print("loaded", len(loaded), "catalog core plugins")
