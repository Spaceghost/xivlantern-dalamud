#!/usr/bin/env bash
# Build the native library the plugin loads, and put it where the plugin build
# picks it up. Never run this on the machine the game runs on; use the builder
# (scripts/sync-to-builder.sh) or CI.
#
#   tools/build-native.sh            # lantern.dll (x86_64-pc-windows-gnu) + liblantern.so
#
# Output (LANTERN_NATIVE_OUT overrides): ../xiv-lantern-build/native/
#   lantern.dll        for the game (Windows PE; runs under Wine)
#   linux/liblantern.so for the binding's tests on a Linux host
#
# Needs cargo, the x86_64-pc-windows-gnu target and mingw-w64 gcc.
# Exit codes: 0 done, 1 a build failed, 127 a missing tool.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${LANTERN_NATIVE_OUT:-$ROOT/../xiv-lantern-build/native}"
for tool in cargo x86_64-w64-mingw32-gcc; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 127; }
done
cd "$ROOT"
cargo build --release --locked -p lantern-ffi --target x86_64-pc-windows-gnu
cargo build --release --locked -p lantern-ffi
mkdir -p "$OUT/linux"
cp target/x86_64-pc-windows-gnu/release/lantern.dll "$OUT/lantern.dll"
cp target/release/liblantern.so "$OUT/linux/liblantern.so"
( cd "$OUT" && sha256sum lantern.dll linux/liblantern.so )
