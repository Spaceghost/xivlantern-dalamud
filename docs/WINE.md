# Lantern under Wine

> Status: what is written here was observed, on the date given, under a
> **stock Wine in a container**, not under the game's Wine build and not in
> the game's prefix. It is evidence that the Windows build's socket layer
> works under Wine; it is not evidence that the plugin works in FFXIV. That
> needs `/lantern selftest` in game.

## Setup

* Built in the `fedora:iroh-build` container: `cargo build --release --target
  x86_64-pc-windows-gnu -p lantern-ffi -p lantern-cli` (mingw-w64 GCC
  16.1, Rust 1.98.1). `lantern.dll` imports only system DLLs (`WS2_32`,
  `bcrypt`, `iphlpapi`, `KERNEL32`, …); no libgcc or winpthread DLL.
* `tests/wine/abi_smoke.c`, built with `x86_64-w64-mingw32-gcc` against the
  DLL, loads it the way the plugin does, opens an in-memory node, mints an
  invite and runs `lt_selftest`.
* Run in the `fedora:ghostty-wine` container: Wine 11.0 (Staging),
  Fedora 43, a prefix of its own (`/root/xiv-lantern-wine/prefix`), never the game's.

## Observed 2026-09-23

Run again after the rename to Lantern, on `lantern.dll` sha256
`818d9c1c676a6db38867e948d8a1feed579a483cd10a8831e20ef59210602148` (the same
code as the Linkpearl-named DLL that passed earlier the same day, under its
new names, ALPNs and ticket prefixes).

`wine abi_smoke.exe off` (relays off):

```
abi 2, lantern 0.1.0 (iroh 1.2, iroh-gossip 0.101, iroh-blobs 0.103)
invite: ok (114 chars, ok)
selftest: {..."platform":"windows-x86_64","bound":["0.0.0.0:50847","[::]:43337"],
  "direct":{"ok":true,"ms":1,"detail":""},"relay":{"ok":false,"ms":0,"detail":"relays are off"}}
closed, exit 0
```

`wine abi_smoke.exe default` (n0's public relays):

```
selftest: {..."bound":["0.0.0.0:48157","[::]:42342"],"relay_url":"https://use1-1.relay.n0.iroh.link./",
  "direct":{"ok":true,"ms":3,"detail":""},"relay":{"ok":true,"ms":420,"detail":""}}
closed, exit 0
```

So under this Wine: both the IPv4 and the IPv6 UDP socket bind, a QUIC
handshake completes on the direct path, the endpoint reaches a home relay, and
a node that knows only the relay URL reaches it through the relay.

`scripts/wine-interop.sh`: a native Linux `lantern` (Alice) and `lantern.exe`
under Wine (Bob), relays off, befriended each other through an `ltfriend…`
invite made on the Wine side, texted both ways with delivery receipts, shared
a channel Bob created, and Bob's `/selftest` passed the direct path.

## noq-udp and WSAEINVAL

`ghostty-multiagent` needed a patched `iroh-quinn-udp` 0.5.7 for iroh 0.35.
This tree's iroh 1.2 uses `noq-udp` 1.3.0, which already only asks
`IPV6_V6ONLY` of IPv6 sockets, treats `WSAEOPNOTSUPP`/`WSAENOPROTOOPT` on the
optional socket options as "unsupported", and turns `IP_PKTINFO` off under
Wine. Nothing failed with `WSAEINVAL` above, so nothing is vendored. If the
game's Wine does fail that way, `/lantern selftest` will show it, and the
fix is to vendor `noq-udp` into `crates/patched/` treating `WSAEINVAL` like
the other two.

## Found along the way

The smoke test's first run found a bug that was not about Wine at all: a node
opened with no database (in-memory, what a throwaway selftest node uses)
panicked because the in-memory blob store was created outside the runtime.
Fixed, with `a_node_with_no_database_opens_in_memory` in `tests/two_node.rs`.

## Reproduce

```sh
scripts/sync-to-builder.sh
incus exec fedora:iroh-build -- bash -lc 'cd /root/xiv-lantern &&
  cargo build --release --target x86_64-pc-windows-gnu -p lantern-ffi -p lantern-cli &&
  cargo build --release -p lantern-cli && d=target/x86_64-pc-windows-gnu/release &&
  x86_64-w64-mingw32-gcc -O2 -o $d/abi_smoke.exe tests/wine/abi_smoke.c -I include -L $d -l:lantern.dll'
incus exec fedora:iroh-build -- bash -c 'cd /root/xiv-lantern/target && tar cz -C x86_64-pc-windows-gnu/release \
    lantern.dll lantern.exe abi_smoke.exe -C /root/xiv-lantern/target/release lantern' |
  incus exec fedora:ghostty-wine -- bash -c 'mkdir -p /root/xiv-lantern-wine/bin && tar xz -C /root/xiv-lantern-wine/bin'
incus exec fedora:ghostty-wine -- bash -lc 'cd /root/xiv-lantern-wine/bin &&
  WINEPREFIX=/root/xiv-lantern-wine/prefix wine abi_smoke.exe default'
```
