#!/usr/bin/env bash
# Build a release tarball: dist/bone-<version>-<target>.tar.gz containing the
# binary (with its Lua runtime built in), docs.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
cargo build --release --locked -p bone
version="$(target/release/bone --version | awk '{print $2}')"
target="$(rustc -vV | sed -n 's/^host: //p')"
name="bone-$version-$target"
stage="dist/$name"
rm -rf "$stage"
mkdir -p "$stage"
cp target/release/bone README.md "$stage/"
cp -r docs "$stage/docs"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "$stage"
echo "dist/$name.tar.gz"
