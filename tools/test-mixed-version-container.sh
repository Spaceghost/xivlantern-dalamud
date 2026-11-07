#!/usr/bin/env bash
# Compare completed libraries in the isolated builder, never a game process.
set -euo pipefail
builder=${LANTERN_TEST_CONTAINER:-ghostty-vote-lantern-build}
legacy=${1:-/out/pre-tags/linux/liblantern.so}
current=${2:-/out/activity-tags/linux/liblantern.so}
podman exec -i "$builder" bash -s -- "$legacy" "$current" <<'CONTAINER'
set -euo pipefail
test -f "$1"
test -f "$2"
work=$(mktemp -d /tmp/lantern-mixed-version.XXXXXXXX)
trap 'rm -rf "$work"' EXIT
gcc -std=gnu11 -O2 -Wall -Wextra -Werror -o "$work/check" \
    /source/tests/native/mixed_version.c -I /source/include -ldl
timeout --kill-after=5s 75s "$work/check" "$1" "$2"
CONTAINER
