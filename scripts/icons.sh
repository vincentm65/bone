#!/usr/bin/env bash
# Render the app icon from crates/bone-gui/assets/icon.svg: icon-512.png
# (the macOS Dock icon, built into bone-gui) and crates/bone/assets/icon.ico
# (the Windows icon, built into bone.exe). Needs rsvg-convert and python3.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
svg="$root/crates/bone-gui/assets/icon.svg"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
rsvg-convert -w 512 -h 512 "$svg" -o "$root/crates/bone-gui/assets/icon-512.png"
sizes=(16 24 32 48 64 128 256)
for s in "${sizes[@]}"; do
  rsvg-convert -w "$s" -h "$s" "$svg" -o "$tmp/$s.png"
done
mkdir -p "$root/crates/bone/assets"
# Every size is stored as PNG inside the .ico (Vista and later read it).
python3 - "$root/crates/bone/assets/icon.ico" "$tmp" "${sizes[@]}" <<'PY'
import struct, sys
out, tmp, sizes = sys.argv[1], sys.argv[2], [int(s) for s in sys.argv[3:]]
pngs = [open(f"{tmp}/{s}.png", "rb").read() for s in sizes]
head = struct.pack("<HHH", 0, 1, len(pngs))
offset = 6 + 16 * len(pngs)
entries = b""
for s, data in zip(sizes, pngs):
    entries += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
open(out, "wb").write(head + entries + b"".join(pngs))
PY
