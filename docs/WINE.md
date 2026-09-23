# Linkpearl under Wine

> Status: what is written here was observed, on the date given, under a
> **stock Wine in a container**, not under the game's Wine build and not in
> the game's prefix. It is evidence that the Windows build's socket layer
> works under Wine; it is not evidence that the plugin works in FFXIV. That
> needs `/linkpearl selftest` in game.

## Setup

* Built in the `fedora:iroh-build` container: `cargo build --release --target
  x86_64-pc-windows-gnu -p linkpearl-ffi -p linkpearl-cli` (mingw-w64 GCC
  16.1, Rust 1.98.1). `linkpearl.dll` imports only system DLLs (`WS2_32`,
  `bcrypt`, `iphlpapi`, `KERNEL32`, …); no libgcc or winpthread DLL.
* `tests/wine/abi_smoke.c`, built with `x86_64-w64-mingw32-gcc` against the
  DLL, loads it the way the plugin does, opens an in-memory node, mints an
  invite and runs `lp_selftest`.
* Run in the `fedora:ghostty-wine` container: Wine 11.0 (Staging), Fedora 44,
  a prefix of its own (`/root/linkpearl-wine/prefix`), never the game's.

## Observed 2026-09-23

`wine abi_smoke.exe off` (relays off):

```
abi 2, linkpearl 0.1.0 (iroh 1.2, iroh-gossip 0.101, iroh-blobs 0.103)
invite: ok (114 chars, ok)
selftest: {..."platform":"windows-x86_64","bound":["0.0.0.0:42076","[::]:57916"],
  "direct":{"ok":true,"ms":1,"detail":""},"relay":{"ok":false,"ms":0,"detail":"relays are off"}}
closed, exit 0
```

`wine abi_smoke.exe default` (n0's public relays):

```
selftest: {..."bound":["0.0.0.0:60016","[::]:53464"],"relay_url":"https://usw1-1.relay.n0.iroh.link./",
  "direct":{"ok":true,"ms":5,"detail":""},"relay":{"ok":true,"ms":174,"detail":""}}
closed, exit 0
```

So under this Wine: both the IPv4 and the IPv6 UDP socket bind, a QUIC
handshake completes on the direct path, the endpoint reaches a home relay, and
a node that knows only the relay URL reaches it through the relay.

`scripts/wine-interop.sh`: a native Linux `linkpearl` (Alice) and
`linkpearl.exe` under Wine (Bob), relays off, befriended each other through an
invite made on the Wine side, texted both ways with delivery receipts, shared
a channel Bob created, and Bob's `/selftest` passed the direct path.

## noq-udp and WSAEINVAL

`ghostty-multiagent` needed a patched `iroh-quinn-udp` 0.5.7 for iroh 0.35.
This tree's iroh 1.2 uses `noq-udp` 1.3.0, which already only asks
`IPV6_V6ONLY` of IPv6 sockets, treats `WSAEOPNOTSUPP`/`WSAENOPROTOOPT` on the
optional socket options as "unsupported", and turns `IP_PKTINFO` off under
Wine. Nothing failed with `WSAEINVAL` above, so nothing is vendored. If the
game's Wine does fail that way, `/linkpearl selftest` will show it, and the
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
incus exec fedora:iroh-build -- bash -lc 'cd /root/linkpearl &&
  cargo build --release --target x86_64-pc-windows-gnu -p linkpearl-ffi -p linkpearl-cli &&
  cargo build --release -p linkpearl-cli && d=target/x86_64-pc-windows-gnu/release &&
  x86_64-w64-mingw32-gcc -O2 -o $d/abi_smoke.exe tests/wine/abi_smoke.c -I include -L $d -l:linkpearl.dll'
incus exec fedora:iroh-build -- bash -c 'cd /root/linkpearl/target && tar cz -C x86_64-pc-windows-gnu/release \
    linkpearl.dll linkpearl.exe abi_smoke.exe -C ../release linkpearl' |
  incus exec fedora:ghostty-wine -- bash -c 'mkdir -p /root/linkpearl-wine/bin && tar xz -C /root/linkpearl-wine/bin'
incus exec fedora:ghostty-wine -- bash -lc 'cd /root/linkpearl-wine/bin &&
  WINEPREFIX=/root/linkpearl-wine/prefix wine abi_smoke.exe default'
```
