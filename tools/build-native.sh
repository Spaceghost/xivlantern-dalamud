#!/usr/bin/env bash
# Build the native library the plugin loads, and put it where the plugin build
# picks it up. Never run this on the machine the game runs on; use the builder
# (scripts/sync-to-builder.sh) or CI.
#
#   tools/build-native.sh            # linkpearl.dll (x86_64-pc-windows-gnu) + liblinkpearl.so
#
# Output (LINKPEARL_NATIVE_OUT overrides): ../ffxiv-linkpearl-build/native/
#   linkpearl.dll        for the game (Windows PE; runs under Wine)
#   linux/liblinkpearl.so for the binding's tests on a Linux host
#
# Needs cargo, the x86_64-pc-windows-gnu target and mingw-w64 gcc.
# Exit codes: 0 done, 1 a build failed, 127 a missing tool.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${LINKPEARL_NATIVE_OUT:-$ROOT/../ffxiv-linkpearl-build/native}"
for tool in cargo x86_64-w64-mingw32-gcc; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 127; }
done
cd "$ROOT"
cargo build --release --locked -p linkpearl-ffi --target x86_64-pc-windows-gnu
cargo build --release --locked -p linkpearl-ffi
mkdir -p "$OUT/linux"
cp target/x86_64-pc-windows-gnu/release/linkpearl.dll "$OUT/linkpearl.dll"
cp target/release/liblinkpearl.so "$OUT/linux/liblinkpearl.so"
( cd "$OUT" && sha256sum linkpearl.dll linux/liblinkpearl.so )
