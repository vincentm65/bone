#!/usr/bin/env python3
"""Install a verified catalog package into a Bone 3 config directory."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent

def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("name")
    parser.add_argument("--config-dir", default=os.getenv("BONE_CONFIG_DIR", str(Path.home() / ".bone")))
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    if not args.name.replace("_", "").replace("-", "").isalnum():
        raise SystemExit("invalid package name")
    entries = {item["name"]: item for item in json.loads((ROOT / "catalog.json").read_text())}
    entry = entries.get(args.name)
    source = ROOT / "plugins" / args.name
    if not entry or not source.is_dir():
        raise SystemExit(f"unknown catalog package: {args.name}")
    for item in entry.get("files", []):
        path = source / item["path"]
        if not path.is_file() or digest(path) != item["sha256"]:
            raise SystemExit(f"catalog hash mismatch: {item['path']}")
    destination = Path(args.config_dir).expanduser() / "plugins" / args.name
    if destination.exists() and not args.force:
        raise SystemExit(f"{destination} exists; use --force to replace it")
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = Path(tempfile.mkdtemp(prefix=f".{args.name}.", dir=str(destination.parent)))
    try:
        # Only what the index lists (and was just verified), plus the manifest.
        staged = temporary / args.name
        listed = [item["path"] for item in entry.get("files", [])]
        if (source / "manifest.json").is_file():
            listed.append("manifest.json")
        for rel in listed:
            target = staged / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source / rel, target)
        if destination.exists():
            shutil.rmtree(destination)
        os.replace(staged, destination)
    finally:
        shutil.rmtree(temporary, ignore_errors=True)
    print(f"installed {args.name} {entry.get('version', 'unknown')} to {destination}")

if __name__ == "__main__":
    main()
