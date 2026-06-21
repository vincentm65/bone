#!/usr/bin/env bash
# Regenerate catalog.json from the tools/ and commands/ .lua files.
#
# Each entry: { name, kind, description, version, sha256 }.
#  - description: prefer a `description = "..."` field, else the first `--`
#    comment line (mirrors `extract_description` in src/ext/mod.rs).
#  - version: preserved from the existing catalog.json if present, else 1.
#    Bump by hand to push a refresh to installed users.
#  - sha256: over the file bytes; the client verifies after download.
#
# Usage: catalog/gen-index.sh   (run from the repo root or the catalog dir)
set -euo pipefail

cd "$(dirname "$0")"

prev="catalog.json"
out="catalog.json.tmp"

# Pull the previous version for a given file name (so versions are sticky).
prev_version() {
  local name="$1"
  if [[ -f "$prev" ]]; then
    # crude but dependency-free: find the object with this name, read its version
    python3 - "$prev" "$name" <<'PY' 2>/dev/null || echo 1
import json, sys
try:
    data = json.load(open(sys.argv[1]))
except Exception:
    print(1); sys.exit()
for e in data:
    if e.get("name") == sys.argv[2]:
        print(e.get("version", 1)); break
else:
    print(1)
PY
  else
    echo 1
  fi
}

# Extract a one-line description from a Lua file.
extract_desc() {
  local file="$1"
  # Prefer `description = "..."`.
  local d
  d=$(grep -oE 'description[[:space:]]*=[[:space:]]*"[^"]*"' "$file" | head -1 \
        | sed -E 's/.*description[[:space:]]*=[[:space:]]*"([^"]*)".*/\1/')
  if [[ -n "$d" ]]; then printf '%s' "$d"; return; fi
  # Else the first non-empty `--` comment line.
  grep -m1 -E '^\s*--' "$file" | sed -E 's/^\s*-+\s*//'
}

json_escape() { python3 -c 'import json,sys; print(json.dumps(sys.stdin.read().rstrip("\n")))'; }

echo "[" > "$out"
first=1
for kind in tools commands; do
  [[ -d "$kind" ]] || continue
  singular=${kind%s}
  for file in "$kind"/*.lua; do
    [[ -e "$file" ]] || continue
    name=$(basename "$file")
    desc=$(extract_desc "$file" | json_escape)
    sha=$(sha256sum "$file" | cut -d' ' -f1)
    ver=$(prev_version "$name")
    if [[ $first -eq 0 ]]; then echo "," >> "$out"; fi
    first=0
    printf '  { "name": %s, "kind": "%s", "description": %s, "version": %s, "sha256": "%s" }' \
      "\"$name\"" "$singular" "$desc" "$ver" "$sha" >> "$out"
  done
done
echo "" >> "$out"
echo "]" >> "$out"

mv "$out" "$prev"
echo "wrote $prev"
