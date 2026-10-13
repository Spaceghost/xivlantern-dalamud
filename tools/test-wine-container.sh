#!/usr/bin/env bash
# Run only inside the disposable native builder. Never use a host/game prefix.
set -euo pipefail

builder=${LANTERN_TEST_CONTAINER:-ghostty-vote-lantern-build}
podman exec -i "$builder" bash -s -- "${LANTERN_TEST_NOSTR_REQUIRED:-0}" "${LANTERN_TEST_FILES_REQUIRED:-0}" "${LANTERN_TEST_FRIEND_FILES_REQUIRED:-0}" "${LANTERN_TEST_FILE_TRANSFER_REQUIRED:-0}" "${LANTERN_TEST_SEALED_REQUIRED:-0}" <<'CONTAINER'
set -euo pipefail
wine_bin=$(command -v wine || command -v wine64 || true)
if [[ -z "$wine_bin" ]]; then
    echo 'Wine is required inside the isolated builder.' >&2
    exit 2
fi
dll=/build/target/x86_64-pc-windows-gnu/release/lantern.dll
test -f "$dll"
work=$(mktemp -d /tmp/lantern-wine-check.XXXXXXXX)
hint_pid=
display_pid=
export WINEPREFIX="$work/prefix" WINEDEBUG=err+all WINEARCH=win64 XDG_RUNTIME_DIR="$work/runtime"
mkdir -m 700 "$XDG_RUNTIME_DIR"
cleanup() {
    if [[ -n "$hint_pid" ]]; then kill "$hint_pid" 2>/dev/null || true; wait "$hint_pid" 2>/dev/null || true; fi
    if command -v wineserver >/dev/null; then wineserver -k || true; fi
    if [[ -n "$display_pid" ]]; then kill "$display_pid" 2>/dev/null || true; wait "$display_pid" 2>/dev/null || true; fi
    rm -rf "$work"
}
trap cleanup EXIT
x86_64-w64-mingw32-gcc -O2 -Wall -Wextra -Werror \
    -o "$work/abi_smoke.exe" /source/tests/wine/abi_smoke.c \
    -I /source/include -L /build/target/x86_64-pc-windows-gnu/release -llantern
if [[ "$4" == 1 ]]; then
    test -x /build/target/debug/examples/wine_address_hint || {
        echo 'Build the wine_address_hint example inside the isolated builder first.' >&2
        exit 2
    }
    x86_64-w64-mingw32-gcc -O2 -Wall -Wextra -Werror \
        -o "$work/file_transfer.exe" /source/tests/wine/file_transfer.c \
        -I /source/include -L /build/target/x86_64-pc-windows-gnu/release -llantern
fi
cp "$dll" "$work/lantern.dll"
cd "$work"
# No host display is mounted. Wine's setup windows use a private framebuffer.
unset DISPLAY WAYLAND_DISPLAY
command -v Xvfb >/dev/null || { echo 'Xvfb is required inside the isolated builder.' >&2; exit 2; }
mkdir -p /tmp/.X11-unix
chmod 1777 /tmp/.X11-unix
Xvfb -displayfd 3 -screen 0 1024x768x24 -nolisten tcp 3>"$work/display" >"$work/xvfb.log" 2>&1 &
display_pid=$!
for ((attempt=0; attempt<200; attempt++)); do
    [[ -s "$work/display" ]] && break
    kill -0 "$display_pid" 2>/dev/null || { cat "$work/xvfb.log" >&2; exit 2; }
    sleep 0.1
done
IFS= read -r display_number < "$work/display"
[[ "$display_number" =~ ^[0-9]+$ ]] || { echo 'Virtual display did not start.' >&2; exit 2; }
export DISPLAY=":$display_number"
echo 'Initializing disposable Wine prefix'
# Some isolated Wine installations hang during service refresh even when
# direct console applications work. The actual ABI executable is the gate.
if timeout --kill-after=10s 60s "$wine_bin" wineboot.exe -u; then
    echo 'Disposable prefix initialization finished'
else
    lantern_boot_status=$?
    echo "Wine prefix refresh exited $lantern_boot_status; checking the actual ABI executable"
    wineserver -k || true
fi
echo 'Running Windows native ABI selftest (relays disabled)'
timeout --kill-after=10s 120s "$wine_bin" ./abi_smoke.exe off "$1" "$2" "$3" "$5"
if [[ "$4" == 1 ]]; then
    echo 'Running actual Windows private friend-file transfer/cancellation fixture with explicit localhost hints'
    /build/target/debug/examples/wine_address_hint "$work" &
    hint_pid=$!
    timeout --kill-after=10s 150s "$wine_bin" ./file_transfer.exe "$5"
    wait "$hint_pid"
    hint_pid=
fi
CONTAINER
