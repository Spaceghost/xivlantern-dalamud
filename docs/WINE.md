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

For the isolated Podman vote-build container, use
`bash tools/test-wine-container.sh`. It expects the repository mounted read-only
at `/source`, the completed Windows release library in
`/build/target/x86_64-pc-windows-gnu/release`, and Wine, Xvfb and MinGW installed in
the container. Wine setup windows use a private virtual display with TCP
listening disabled; no host display is connected. `LANTERN_TEST_CONTAINER` can select another disposable builder
with the same layout. The script compiles the C ABI smoke test with warnings
treated as errors, creates a fresh prefix under the container's `/tmp`, runs
with relays disabled and a time limit, then removes the prefix. It never runs
Wine on the host or uses a game prefix. A successful smoke test verifies the
native endpoint; it does not verify Dalamud UI, idle-input sampling or audio.

The older Incus setup used these commands:

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

The current channel-history source passes 98 default and 109 optional native
workspace tests plus nine actual C# binding tests against debug and optimized
Linux libraries. Its expanded Windows smoke fixture checks direct socket
reachability, signed metadata, byte-exact saved-history delivery and acknowledged
local clearing. It passes against the current optimized Windows DLL under both
isolated Fedora 43 and Fedora 44 Wine 11.0 Staging.

The separate Fedora 43 prefix refresh timed out at 60 seconds with RPC/service
setup errors and no OOM counters, but direct execution in that disposable prefix
passed (`native/channel-history-wine43-direct-smoke.log`). The reproduce harness
now bounds prefix refresh at 60 seconds, records its result, stops wineserver
when refresh fails, then executes the actual ABI smoke independently. The
executable's exit code remains the gate: missing libraries, native failures or
timeouts still fail. This revised harness passes in the original Fedora 44
builder too (`native/channel-history-wine44-smoke.log`). No host/game prefix is
used. This proves current native Windows ABI behavior, not Dalamud UI or game
integration.

Optimized artifacts are in `native/channel-history/`; their build log is
`native/channel-history-optimized-build.log`. The checked local
`vote-preview-history/latest.zip` contains those Windows bytes and the current
managed assemblies. It is not installed or published. In-game verification,
including settings/mute/privacy behavior and loading transitions, remains open.

The subsequent delivery/privacy revision passes all 102 default and 113 optional
workspace tests, ten actual optimized C# binding tests (including 256-record
queue pressure) and optimized legacy-library interoperability. Its optimized
Windows DLL passes the same expanded ABI smoke using the revised Fedora 44
harness (`native/channel-history-delivery-wine-smoke.log`). Current optimized
libraries are under `native/channel-history-delivery/`; the checked local
preview is `vote-preview-history-delivery/latest.zip`. These results include
blocked-author queue purging, cache invalidation and rejection of late metadata
for left channels. The package remains uninstalled/unpublished and game
verification is still required.

The schema-5 remembered-provider revision also passes the actual Windows ABI
smoke in the isolated Fedora 44 builder
(`native/channel-history-peer-hint-wine-smoke.log`), including signed metadata,
exact saved history and acknowledged local clear. Prefix refresh times out;
the actual executable succeeds afterward. Optimized libraries are under
`native/channel-history-peers/`; the byte-checked local
`vote-preview-history-peers/latest.zip` is uninstalled and unpublished. Ten
optimized C# binding tests include identity-preserving restart and catch-up
through a previously met provider using the offline creator's original ticket.
This is not proof of first-contact discovery or in-game behavior.

The name-card revision passes the expanded Windows ABI smoke
(`native/name-card-wine-smoke.log`) with optional exports required. It resolves
all three entry points, checks stable local public identity and rejects invalid
TTL/relay inputs without scheduling a connection. Existing signed metadata,
saved history and acknowledged clear checks also pass. Prefix refresh still
times out; the actual executable succeeds afterward. Optimized native artifacts
are in `native/name-card-invites/`; the six-file byte-checked local
`vote-preview-name-cards/latest.zip` remains uninstalled/unpublished. Game and
actual framework/worker/unload/UI verification remain open.

The optimized name-card limits revision also passes the expanded smoke with
optional exports required (`native/name-card-limits-wine-smoke.log`). Prefix
refresh times out; the actual executable succeeds afterward. Eleven C# tests
and actual legacy interoperability pass against these exact optimized libraries.
The final plugin build has zero warnings/errors, and the six-file byte-checked
`vote-preview-name-card-limits/latest.zip` includes privacy, rate/event/block
limits and preparation cancellation. It is uninstalled/unpublished; actual
framework/unload/UI and game verification remain open.

The isolated wrapper also accepts `LANTERN_TEST_FILE_TRANSFER_REQUIRED=1`.
It builds `tests/wine/file_transfer.c` with MinGW warnings treated as errors,
then runs it in the same disposable prefix against the exact Windows DLL.
The fixture creates two throwaway private identities with public relays off;
checks authenticated friend metadata, no automatic fetch, explicit byte-exact
save, refusal to overwrite existing bytes, scoped withdrawal and stale-handle
rejection; then queues a 16 MiB transfer for cancellation and unfriend.
A terminal failure must leave no partial destination; a completion that wins
cancellation/removal must retain all exact bytes. This does not force a cancel
at a particular filesystem-write boundary or cover Dalamud/framework behavior.
The wrapper gives this executable a separate 150-second timeout and cleans
its own temporary prefix/files. Do not use a host or game Wine prefix.

On the current isolated Wine installation, direct selftests succeed but endpoint
interface discovery produces address-free invites (native InviteFailed says
“No addressing information available”). The Windows transfer fixture therefore
uses explicit reciprocal localhost address hints, not production discovery.
Build the offline `wine_address_hint` example inside the capped native builder
before enabling this fixture. It serializes NodeTickets from only the synthetic
public ids and bound UDP ports using the real ticket codec; no bearer invite,
private key or network socket enters the helper. Its temporary request/reply
files live in the wrapper's private directory and are removed with the prefix.
Passing this mode proves a Windows DLL transfer with explicit private routes;
it does not prove Wine automatic address discovery, NAT or relay behavior.

### Sealed optimized candidate (2026-10-01)

The locked optimized Windows build with nostr enabled passes the disposable
Wine wrapper with LANTERN_TEST_SEALED_REQUIRED=1 plus name-card, bounded-file,
friend-file and actual friend-file transfer checks. The new sealed check
feature-detects four exports, creates a sealed room, verifies ticket prefix,
imports encrypted bytes on the native worker, reseals with a distinct ticket
and refuses reusing the old encrypted file in the new room. It makes no
remote sealed-channel contact. Existing ordinary friend-file transfer still
uses explicit localhost hints and verifies exact download/overwrite/withdrawal
and 16 MiB cancellation/unfriend terminal integrity.

The Wine prefix refresh timed out at 60 seconds as before; actual ABI and
transfer executables passed and the wrapper exited zero. Prefix/display/helper
cleanup completed. This is neither actual Windows-kernel certification nor
a sealed Windows peer-to-peer file/history transfer proof or game proof.
Log: ../xiv-lantern-build/native/sealed-native-wine-tests.log
Windows DLL SHA256: 614bdefe44502df29f9cb07dcd7f5076981c7ca5fa7f7583b2bf05ef1dfab421

### Actual sealed Windows peer exchange (2026-10-01)

With the current optimized Windows DLL and LANTERN_TEST_SEALED_REQUIRED=1,
the expanded private file fixture now creates a sealed channel across two
Windows DLL instances. Explicit test-only localhost hints avoid Wine's
address discovery limitation. The receiver obtains an encrypted history
snapshot whose original record/signature matches the creator's, then
downloads/decrypts 65537 exact bytes. Generic ciphertext export and overwrite
are refused; withdrawal invalidates the offer and preserves saved bytes.
A fresh-key room refuses the old encrypted file. A 16 MiB encrypted download
is canceled and ends with intact completion or no partial file. Existing
ordinary friend transfer/cancellation/unfriend checks also pass.

Wrapper exit zero: ../xiv-lantern-build/native/sealed-peer-wine-tests.log.
All identities/keys/files are synthetic and confined to a disposable prefix.
The native DLL is unchanged from the optimized build already recorded.
This proves actual Windows-binary peer exchange under Wine with explicit
loopback routes, not automatic discovery, Internet traversal, actual Windows
kernel behavior, forced mid-write cancellation or Dalamud/game lifecycle.
