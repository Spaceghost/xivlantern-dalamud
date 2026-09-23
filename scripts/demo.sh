#!/usr/bin/env bash
# Two lantern CLIs on one machine, relays off: Alice invites Bob, they chat
# 1:1, Alice opens a channel and invites Bob into it. Prints both transcripts.
#
# Run it where the tree is built (the Incus builder, never the game machine):
#   scripts/sync-to-builder.sh
#   incus exec fedora:iroh-build -- bash -lc 'cd /root/xiv-lantern && scripts/demo.sh'
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
cargo build -q -p lantern-cli
BIN="${LANTERN_BIN:-$ROOT/target/debug/lantern}"

DIR="$(mktemp -d)"
trap 'exec 3>&- 4>&- 2>/dev/null || true; kill $(jobs -p) 2>/dev/null || true; rm -rf "$DIR"' EXIT
mkfifo "$DIR/alice.in" "$DIR/bob.in"
"$BIN" --db "$DIR/alice.sqlite" --name Alice --relay off <"$DIR/alice.in" >"$DIR/alice.out" 2>&1 &
"$BIN" --db "$DIR/bob.sqlite" --name Bob --relay off <"$DIR/bob.in" >"$DIR/bob.out" 2>&1 &
exec 3>"$DIR/alice.in" 4>"$DIR/bob.in"

# wait_for <who> <text>: block until the transcript contains the text.
wait_for() {
  local file="$DIR/$1.out"
  for _ in $(seq 1 150); do
    grep -qF -- "$2" "$file" && return 0
    sleep 0.2
  done
  echo "timed out: $1 never printed: $2" >&2
  cat "$file" >&2
  exit 1
}
alice() { echo "$*" >&3; }
bob() { echo "$*" >&4; }

wait_for alice "ready "; wait_for bob "ready "
alice /invite
wait_for alice "invite ltfriend"
bob "/accept $(grep -o 'ltfriend[a-z0-9]*' "$DIR/alice.out" | head -1)"
wait_for bob "friend added: Alice"; wait_for alice "* Bob is online"
bob /msg Alice hello from the other terminal
wait_for alice "<Bob> hello from the other terminal"; wait_for bob "delivered"
alice /status away crafting
wait_for bob '* Alice is away "crafting"'
alice /channel new static
wait_for alice "joined #static"
alice /channel invite Bob
wait_for bob "/channel join ltroom"
bob "$(grep -o '/channel join ltroom[a-z0-9]*' "$DIR/bob.out" | head -1)"
wait_for alice "#static: Bob joined"
alice pull in 5
wait_for bob "#static <Alice> pull in 5"
bob /say ready
wait_for alice "#static <Bob> ready"
alice /friends
bob /quit; wait_for bob "bye"
wait_for alice "* Bob is offline"
alice /quit; wait_for alice "bye"

# Tickets are long; shorten them for reading.
for who in alice bob; do
  echo "===== $who"
  sed -E 's/(lt(friend|room|node)[a-z0-9]{12})[a-z0-9]+/\1.../g' "$DIR/$who.out"
done
