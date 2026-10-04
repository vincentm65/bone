#!/usr/bin/env python3
"""Generate catalog.json and per-package file hashes."""
from __future__ import annotations

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PLUGINS = ROOT / "plugins"

def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main() -> None:
    entries = []
    for package in sorted(p for p in PLUGINS.iterdir() if p.is_dir()):
        manifest_path = package / "manifest.json"
        manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
        files = []
        for path in sorted(p for p in package.rglob("*") if p.is_file() and p.name != "manifest.json"):
            files.append({"path": path.relative_to(package).as_posix(), "sha256": sha256(path)})
        entry = {
            "name": package.name,
            "kind": "plugin",
            "version": manifest.get("version", "1.0.0"),
            "description": manifest.get("description", ""),
            "min_bone_version": manifest.get("min_bone_version", "0.1.0"),
            "capabilities": manifest.get("capabilities", []),
            "files": files,
        }
        entries.append(entry)
    (ROOT / "catalog.json").write_text(json.dumps(entries, indent=2) + "\n")

if __name__ == "__main__":
    main()
