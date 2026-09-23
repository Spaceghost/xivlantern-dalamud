#!/usr/bin/env bash
# Wine <-> native: a Linux linkpearl (Alice) and a Windows linkpearl.exe under
# Wine (Bob) befriend each other, text both ways and share a channel, relays
# off, on one machine. Evidence that the Windows build's socket layer works
# under Wine and speaks to a native node; it is not the game's Wine prefix.
#
#   WINEPREFIX=... scripts/wine-interop.sh <dir with linkpearl and linkpearl.exe>
set -euo pipefail
BIN="$(cd "${1:?directory holding linkpearl and linkpearl.exe}" && pwd)"
export WINEDEBUG=-all
DIR="$(mktemp -d)"
trap 'exec 3>&- 4>&- 2>/dev/null || true; kill $(jobs -p) 2>/dev/null || true; rm -rf "$DIR"' EXIT
mkfifo "$DIR/alice.in" "$DIR/bob.in"
"$BIN/linkpearl" --db "$DIR/alice.sqlite" --name Alice --relay off <"$DIR/alice.in" >"$DIR/alice.out" 2>&1 &
( cd "$DIR" && wine "$BIN/linkpearl.exe" --db bob.sqlite --name BobOnWine --relay off <"$DIR/bob.in" >"$DIR/bob.out" 2>&1 ) &
exec 3>"$DIR/alice.in" 4>"$DIR/bob.in"
wait_for() {

  for _ in $(seq 1 300); do grep -qF -- "$2" "$DIR/$1.out" && return 0; sleep 0.2; done
  echo "timed out: $1 never printed: $2" >&2; cat "$DIR/alice.out" "$DIR/bob.out" >&2; exit 1
}
wait_for alice "ready "; wait_for bob "ready "
echo /invite >&4; wait_for bob "invite lpfriend"
echo "/accept $(grep -o 'lpfriend[a-z0-9]*' "$DIR/bob.out" | head -1)" >&3
wait_for alice "friend added: BobOnWine"; wait_for bob "friend added: Alice"
wait_for alice "* BobOnWine is online"; wait_for bob "* Alice is online"
echo "/msg BobOnWine hello from Linux" >&3; wait_for bob "<Alice> hello from Linux"; wait_for alice "delivered"
echo "/msg Alice hello from Wine" >&4; wait_for alice "<BobOnWine> hello from Wine"
echo "/channel new static" >&4; wait_for bob "joined #static"
echo "/channel invite Alice" >&4; wait_for alice "/channel join lproom"
grep -o '/channel join lproom[a-z0-9]*' "$DIR/alice.out" | head -1 >&3
wait_for bob "#static: Alice joined"
echo "pull in 5" >&4; wait_for alice "#static <BobOnWine> pull in 5"
echo /selftest >&4; wait_for bob '"direct":'
echo /quit >&3; echo /quit >&4; wait_for alice bye; wait_for bob bye
for who in alice bob; do
  echo "===== $who"
  sed -E 's/(lp(friend|room|node)[a-z0-9]{12})[a-z0-9]+/\1.../g' "$DIR/$who.out" | grep -v XDG_RUNTIME_DIR
done
