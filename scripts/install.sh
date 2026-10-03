#!/usr/bin/env bash
# Build bone in release mode and install it.
#
#   scripts/install.sh                 install as ~/.local/bin/bone3
#   scripts/install.sh --name bone     install as `bone` (replaces what is there)
#   scripts/install.sh --dir /usr/local/bin
#   scripts/install.sh --link          symlink to target/release instead of copying
set -euo pipefail

name=bone3
dir="$HOME/.local/bin"
link=0
while [ $# -gt 0 ]; do
  case "$1" in
    --name) name="$2"; shift 2 ;;
    --dir) dir="$2"; shift 2 ;;
    --link) link=1; shift ;;
    -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build --release --locked -p bone --manifest-path "$root/Cargo.toml"
bin="$root/target/release/bone"
dest="$dir/$name"
mkdir -p "$dir"

if [ -e "$dest" ] || [ -L "$dest" ]; then
  current="$(readlink -f "$dest" || true)"
  if [ "$current" != "$(readlink -f "$bin")" ]; then
    echo "replacing $dest (was: ${current:-a file})"
  fi
  rm -f "$dest"
fi
if [ "$link" = 1 ]; then
  ln -s "$bin" "$dest"
else
  install -m 755 "$bin" "$dest"
fi
echo "installed $("$dest" --version) as $dest"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "note: $dir is not on your PATH" ;;
esac
[ -e "${BONE_CONFIG_DIR:-$HOME/.bone}/core.lua" ] || echo "next: $name --init"
