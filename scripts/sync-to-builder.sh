#!/usr/bin/env bash
# Copy the working tree into the build container. Building never happens on the
# workstation: the game is running there and 16 GB does not survive rustc.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INCUS="${INCUS:-incus}"
TARGET="${LINKPEARL_BUILDER:-fedora:iroh-build}"
DEST="${LINKPEARL_BUILD_DIR:-/root/linkpearl}"

cd "$ROOT"
tar --exclude=.git --exclude=target --exclude=bin --exclude=obj -cz . \
  | "$INCUS" exec "$TARGET" -- bash -c "mkdir -p $DEST && tar -xz -C $DEST"
echo "synced $ROOT -> $TARGET:$DEST"
