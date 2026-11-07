# Lantern design

> Status: this is the required direction, not a claim that it works. Each
> section says whether it is **implemented** (code in this tree, with the test
> that exercises it named), a **spike**, or **design** (nothing built yet).
> Nothing in this document has been observed inside the game or under Wine;
> every test named here runs on a Linux host (the Incus builder) with no
> display, and passes inside a network namespace that has only `lo`.

The owner's brief: "a sort of friend list and our own comms net with calls and
video and text — maybe even nostr", for FFXIV players, driven from a Dalamud
plugin. Lantern is the transport and state underneath that plugin. It does not
talk to the game, read game memory, or send game commands, and it never will
(see [Threat model](#threat-model-and-privacy)).

## Status at a glance

| Piece | State | Evidence |
| --- | --- | --- |
| Device identity (iroh key in SQLite) | implemented | `identity_survives_a_restart`, `store::tests` |
| Gossip rooms, room presence, blobs, direct connections | implemented (scaffold) | `two_nodes_meet_talk_and_share_a_file` |
| Friend invites, friend list, presence heartbeat, 1:1 text | implemented | `tests/friends.rs` |
| Group channels over iroh-gossip, channel invites | implemented | `tests/friends.rs`, `tests/demo.rs` |
| C ABI for the above, `include/lantern.h` in sync | implemented | `tests/abi_friends.rs`, `the_header_matches_the_exports_and_constants` |
| C# binding for the above | implemented | `Lantern.Tests/FriendTests.cs` against `liblantern.so` |
| CLI demo (`lantern`) | implemented | `crates/lantern-cli/tests/demo.rs`, `scripts/demo.sh` |
| nostr: device list, invites over relays (feature `nostr`) | implemented | `tests/nostr.rs` against an in-process relay |
| nostr: NIP-17 offline mailbox for texts, NIP-05 names | design | — |
| nostr in the C ABI | design | — |
| Call signalling (feature `calls`) | spike | `tests/calls.rs` |
| Call media (moq over iroh, sidecar) | design | — |
| `lantern.dll` for `x86_64-pc-windows-gnu` | builds; selftest passes under stock Wine 11 | [WINE.md](WINE.md) |
| Blocking, per-peer rate limits, size limits | implemented | `tests/safety.rs` |
| Author channel (signed announcements), support messages | implemented | `tests/safety.rs`, `crates/lantern-cli/tests/demo.rs` |
| Dalamud plugin (XivLantern) | builds; host tests only | `tests/XivLantern.Core.Tests` |
| Anything in the game, or in the game's Wine prefix | not observed | — |

This table is kept current by the commits that change it.

### Try it

Builds and tests run in the Incus builder, never on the machine the game runs
on:

```sh
scripts/sync-to-builder.sh
incus exec fedora:iroh-build -- bash -lc 'cd /root/xiv-lantern && cargo test --workspace'
incus exec fedora:iroh-build -- bash -lc 'cd /root/xiv-lantern && scripts/demo.sh'
# everything, including nostr and the calls spike:
incus exec fedora:iroh-build -- bash -lc 'cd /root/xiv-lantern && cargo test --workspace \
    --features lantern-core/nostr,lantern-core/calls,lantern-cli/nostr'
```

By hand, two terminals on one machine (relays off, so only direct paths):

```text
A$ lantern --db alice.sqlite --name Alice --relay off
B$ lantern --db bob.sqlite --name Bob --relay off
A> /invite                      -> invite ltfriend...
B> /accept ltfriend...          -> friend added: Alice
B> /msg alice hello             A sees: <Bob> hello; B sees: delivered [...]
A> /status away crafting        B sees: * Alice is away "crafting"
A> /channel new static
A> /channel invite Bob          B sees: /channel join ltroom...
B> /channel join ltroom...
A> pull in 5                    B sees: #static <Alice> pull in 5
```

With `--relay off` a friend's stored address is only as good as their port;
`--port` pins it so a restarted node is still where friends last saw it. With
the default relay mode, iroh's n0 relays and DNS lookup find a NodeId wherever
it is — that path is not exercised by any test here, because the tests run
without a network.

### Known gaps in what is implemented

* Texts are addressed to a device, not a person; fan-out to all of a
  friend's devices is not built.
* Rate limits are per peer, not per network: many nodes can still each
  send up to their own limit. Blocking is per node id; a new id is a new
  stranger (invites are still needed to become a friend).
* Channels have no history for late joiners, no ordering and no encryption
  beyond each QUIC hop; anyone holding the ticket can read.
* `lt_stats.relayed_conns` is still always 0 (scaffold); friend links are not
  counted in `lt_stats` at all.
* nostr and call signalling are not in the C ABI; if a build enables them,
  their events reach `lt_poll` as `LT_EV_LOG` lines.
* The author channel shows each member's node id (and IP, on direct paths)
  to the neighbours it connects through; that is gossip. The plugin says so.
* The C# binding is tested against `liblantern.so` on Linux only. The
  plugin (XivLantern) compiles against Dalamud 15 but has never been
  loaded by it, and `lantern.dll` has never run in the game's process.

## Processes and layers

```
 FFXIV (Wine) ─ Dalamud ─ plugin (C#, ImGui)
                              │ P/Invoke, bindings/csharp
                              ▼
                        lantern.dll  (include/lantern.h, lantern-ffi)
                              │
                        lantern-core (tokio, iroh, gossip, blobs, SQLite)
                              │ QUIC (iroh), hole punching, optional relay
                              ▼
                friends' lantern nodes ── optional nostr relays
                              │
          (design) lantern-media sidecar: moq over iroh, audio/video
```

The C ABI is poll-driven: the plugin calls `lt_poll` once per frame and gets
`lt_event`s. No callback ever crosses into the plugin, for the same reason as
in `ghostty-multiagent/docs/IROH.md`: a Rust thread holding a function pointer
into an unloaded plugin is the crash to design against.

## Identity

**Device identity (implemented).** Every install is an iroh endpoint whose
ed25519 secret lives in the `identity` table of the node's SQLite file
(`Store::identity`, test `identity_survives_a_restart`). The public half is the
device's NodeId. It is what friends store, what QUIC authenticates on every
connection, and what signs every gossip frame (`proto::encode`). Two installs —
the Windows PC and the Steam Deck — are two NodeIds. Losing the file loses the
identity; there is deliberately no recovery path at this layer.

**User identity (implemented behind feature `nostr`).** A person is optionally a nostr keypair
(secp256k1, NIP-01). It is portable across devices and never required. The
user key publishes a **device list**: a parameterised-replaceable event (NIP-78
application data, kind 30078, `d` = `xivlantern/devices`) whose content
lists the user's NodeIds. Each entry carries the device's own ed25519 signature
over `"lantern-device-claim/1" || nostr_pubkey || node_id`, so the list proves
both directions: the user claims the device, and the device consented. Without
the device signature anyone could list somebody else's NodeId as theirs.

A friend who knows your nostr pubkey can therefore find all your devices, and a
new device joins your friend list everywhere by being added to the device list,
not by re-inviting every friend.

The nostr secret is stored beside the device key (the `profile` table, keys
`nostr_secret`/`nostr_pubkey`), generated or imported (`nostr_keys`,
`nostr_import`). A NIP-46 remote signer is the right answer for people who
already keep their nsec elsewhere; that is later work.

## Friend list

**Model.** A friend is a person with one or more devices:

```sql
friends        (id INTEGER PRIMARY KEY, name TEXT, nostr_pubkey BLOB UNIQUE NULL, added_at)
friend_devices (node_id BLOB PRIMARY KEY, friend_id REFERENCES friends ON DELETE CASCADE,
                addr BLOB NULL,            -- last known iroh EndpointAddr (postcard)
                added_at, last_seen)
invites        (secret BLOB PRIMARY KEY, created_at, expires_at, redeemed_by, redeemed_at)
messages       (peer, outgoing, msg_id, body, sent_at, delivered_at)  -- 1:1 history + outbox
profile        (key, value)                                           -- name, status, note, nostr
```

Without nostr, one invite yields one friend with one device. With nostr, a
friend is keyed by pubkey and their devices come from the verified device list.

**Implemented** (`crates/lantern-core/src/node/friends.rs`, store schema 2).

**Adding a friend is mutual and needs an invite ticket.** There is no directory
and no "add by name" without nostr.

1. Alice calls `invite_create(ttl)`. The ticket text is `ltfriend` + lowercase
   base32 of a postcard struct: protocol version, Alice's `EndpointAddr`, a
   128-bit random single-use secret, an expiry, Alice's display name, and
   (optionally) her nostr pubkey. The secret is kept in the `invites` table.
2. Alice gives the ticket to Bob out of band (a tell, Discord, a QR code).
3. Bob calls `invite_accept(ticket)`. His node dials Alice on the friend ALPN
   (`<app_id>/friend/1`) and sends `Redeem { secret, name, addr }`.
4. The QUIC handshake has already authenticated Bob's NodeId. Alice checks the
   secret is known, unexpired and unused, marks it used by Bob's NodeId, stores
   Bob, and answers `Welcome { name, addr }`. Bob stores Alice. Both raise
   `FriendAdded`. Anything else is answered with `Refused` and the connection
   is closed.

The ticket is a bearer credential until it is redeemed once; its expiry bounds
the window. Stale or reused tickets produce `InviteFailed` on the accepting side.

**Only friends get a friend link.** The friend ALPN refuses any connection whose
NodeId is not a stored friend device, except for a single `Redeem`. Removing a
friend sends `Unfriend`, drops the link and deletes the device on both sides;
refusing a NodeId at the transport is the block list.

## Presence

Two kinds, deliberately separate:

* **Room presence (implemented in the scaffold).** An opaque payload per gossip
  room, re-announced to new neighbours (`Frame::Presence`, `Frame::Hello`).
* **Friend presence (implemented).** A heartbeat over each friend link. Each node
  sends `Heartbeat { status, note }` every interval (15 s by default) to every
  connected friend device and dials friends it has no link to, with backoff. A
  friend is online from their first `Hello`/`Heartbeat` until their link closes
  or three intervals pass without a heartbeat. Status is `Online`, `Away`,
  `Busy` or `Invisible`; invisible stops heartbeats but still delivers messages.
  The note is free text the *user* typed. The library never fills it from game
  state, and the plugin must not either without an explicit opt-in per field.

## Text

**1:1 (implemented).** Over the friend link: one QUIC uni stream per frame, postcard
encoded, capped at `MAX_MESSAGE`. A text is `Text { id, sent_at, body }` where
`id` is a random u64 chosen by the sender. The receiver stores it keyed by
`(peer, id)`, raises `FriendText` only the first time, and always answers
`Ack { id }`; the sender marks it delivered and raises `FriendDelivered`.
Unacknowledged texts stay in SQLite and are re-sent whenever a link to that
device comes up, so delivery is at-least-once on the wire and exactly-once to
the UI. Messages are addressed to a device; fanning out to all of a friend's
devices is the nostr-era change.

**Group channels (implemented).** A channel is a gossip room with
a random 32-byte key (`Scope::Custom`), so its topic cannot be guessed. Every
frame is signed by its author and verified on arrival (`proto.rs`), so a relay
node in the swarm cannot forge or re-attribute. A channel is shared by a room
ticket (`ltroom…`), and between friends by `ChannelInvite { ticket }` over the
friend link, surfaced as an event the UI turns into a "join?" prompt.

What channels do **not** have yet, and should: history for late joiners (a
blob snapshot offered by any member is the likely shape), ordering (gossip does
not order), membership control (anyone holding the topic id can join and read —
the topic is the only secret), and end-to-end encryption beyond each QUIC hop.
Encrypting frames with a key carried in the ticket closes the last one.

The catch-up format foundation now lives in `channel_history.rs`; live room
capture, blob offers and plugin merging are not connected yet. Each
record signs a separate protocol domain, version, author, room, random 128-bit
message ID, claimed sender timestamp and UTF-8 text. A copied record still
verifies under its original author's key; changing its room or any signed
field invalidates it. Repeated identical text gets distinct IDs/signatures.
The snapshot parser bounds bytes, counts and lengths before allocation,
re-verifies every record, drops invalid entries and deduplicates signatures.
Limits are 256 records and 2,048 text bytes per record; malformed framing is
rejected. Ordering uses claimed sender time with deterministic tie breaks;
signature verification cannot prove clock accuracy. Other members retain
copies, so deleting one's local history cannot delete their copies.

Schema 4 adds `channel_history` storage. Insertions verify signatures and room
binding before writing, and reads reverify bounded stored bytes. Original signed
bytes persist with local receipt times: 256 records per room, 8,192 globally and
a 24-hour window. A claimed future sender time cannot pin a record indefinitely;
duplicate signatures do not refresh receipt times. Clearing affects only one
room. The worker prunes at startup and each minute, including idle rooms. Reads
exclude expired rows immediately; physical deletion may lag one interval.
Existing schema-3 identity/profile rows survive migration.

The opt-in Rust `room_send_with_history` queues signing/storage on the worker.
Its wire body is an ordinary postcard `App(text)` followed by `LTHLIVE1` and the
original signed record, all covered by the outer signature. Pre-history decoders
read App and ignore its tail, preserving plain text. New decoders verify the
record author/signature and require the legacy text to equal the signed text.
Receivers verify the actual room, store exact bytes and emit one live message
per new signature. Random IDs make repeated text distinct gossip packets.
Existing generic `room_send` is unchanged. Plain legacy App signatures cover
content alone; snapshot publishers cannot relabel them as bound records.
Actual old/new library interoperability still needs verification for this build.

Bounded snapshot fetches use the Bao header before payload allocation, require
the actual size to match the offer (42..566,314 bytes), authenticate the full
blob and reject extra children/trailing data. Original room/author signatures
are reverified; a valid provider blob hash does not certify the contained
authors. Fetches use memory without disk cache/export, a 15-second deadline and
at most four simultaneous downloads per node. The queued `room_history_fetch`
stores verified original bytes on the worker, skips blocked authors/providers
and does not emit live chat events that could replay timers/notifications. That
bounded-fetch source passed all 93 default and 104 optional workspace tests.

`room_history_enable` adds automatic catch-up in source, disabled by default.
Enable and NeighborUp start a fresh signed room-bound request; attempts change
the signed body on two-second retries so gossip does not collapse retries.
Offers bind the room, recipient and request nonce, validate provider identity
and accept at most four providers per request within 15 seconds. Serving uses
an app-specific blob ALPN and separate read-only memory stores, never the
ordinary file-transfer disk store. Up to 32 snapshots are leased for 60 seconds
after their last offer, with four provider connections and 15-second deadlines.
Cache revisions invalidate stale data after new records. Renewing a snapshot
lease does not alter signed records or database receipt times. Leaving/disabling
removes that handle's ownership; another local handle of the same channel can
keep serving the same hash. Idle cache entries expire on the worker timer.

The automatic source passed 96 default and 107 optional workspace tests. The
latest cancellation/metadata source passes all 68 Core tests. Each room tracks
a cancellation generation: queued fetch insertion checks it under the store
lock, and clear increments it before deleting and acknowledging. Leaving or
changing catch-up cancels pending fetches too.

Optional C ABI controls now expose metadata opt-in, automatic catch-up, signed
sending and local clear. Events 34/35 carry exact original verified records for
live/history lines; event 36 acknowledges successful local deletion. C# opts
into metadata before joining. Older libraries retain ordinary text behavior.
The plugin enables configurable catch-up/sharing on joined channels, uses live
signed events for notifications/countdowns, and loads historical records without
unread counts or replayed actions. Clear updates the view only after native
acknowledgement. Other members may retain copies and later catch-up can restore
them. The offline-author native test uses a connected member's ticket; discovery
from an offline creator's original ticket is not established.

The managed conversation model carries the full signature separately from
friend message IDs, merges signature identities, retains repeated text with
distinct signatures, orders the original claimed integer timestamps and keeps
a 500-line bound. Invalid display-date claims remain preserved. Node identity
and the warning that sender clocks are claims appear in line tooltips. All 85
managed Core tests and three parser tests pass; plugin Release builds without
warnings or errors. Eight FFI unit tests and the actual C ABI history test also pass for the latest
source. All nine C# binding tests pass against the matching debug Linux library,
including late join after author shutdown, original signatures, clear and
rejoin with a third line sent during absence. Actual archived legacy/new debug
libraries also pass signed repeated-text/ordinary-text channel interoperability.
All 98 default workspace tests pass. All 109 corrected calls/nostr workspace
tests pass too. Optimized Windows/Linux builds and actual optimized C# / legacy-library
interoperability checks pass. Windows ABI smoke passes under isolated Fedora 43
and Fedora 44 Wine, even though prefix service refresh times out. A local history
preview package is assembled and its six required files match current native/
managed bytes and the source API-15 manifest. Multi-channel event capacity,
stale creator-ticket discovery and in-game verification remain open. These are source/host checks, not a current history
package, Windows/game verification or release claim.

Channel metadata now uses a separate bounded queue per joined handle instead
of the lossy general event queue. It retains up to 256 distinct pending originals
and one clear acknowledgement. Duplicate originals coalesce; excess arrivals
evict the oldest pending record and increment the drop counter. Clear removes
older queued lines before its acknowledgement, and leaving forgets the handle.
Channels drain round-robin; polling alternates metadata/general events even
with a one-event limit. Polling moves already verified bytes and performs no
SQL or cryptography. This fixes startup backlog loss under general-queue
pressure without claiming unlimited delivery while a client stops polling.

All 71 Core tests pass, including the real two-channel/512-record case with a
16-slot general queue and one-event polls, plus queue fairness/deduplication and
clear-under-pressure unit checks. All 101 default and 112 optional workspace tests pass for that delivery source
in `native/channel-history-delivery-workspace-tests.log`. The earlier optimized
libraries and preview above predate this queue change; the refreshed
`vote-preview-history-delivery` package described below includes it. Stale creator-ticket discovery and the remaining
settings/mute/privacy/game audit stay open.

The subsequent privacy audit now purges pending originals by blocked author.
Enqueue holds the blocked-state read guard; block holds its write guard through
purging, preventing an in-flight publisher from restoring a queued blocked line.
Block/unblock invalidates cached snapshot views by revision (already offered
copies cannot be recalled). Delivery tracks active handles, removes them on
leave/close and refuses late metadata for forgotten handles, without adding
store or room locks to the frame-thread poller. A new actual Node regression
checks valid signed live/history records, cached-view invalidation, unblock,
unrelated clear acknowledgement and a late event after processed RoomLeft.
All 72 Core tests and the actual C ABI history test pass in
`native/channel-history-delivery-privacy-tests.log`. All 102 default and 113 optional workspace tests pass in
`native/channel-history-delivery-privacy-workspace-tests.log`. Optimized builds
completed in `native/channel-history-delivery-optimized-build.log`, with
separate output under `native/channel-history-delivery/`. Earlier preview/runtime
proofs predate these privacy additions; the refreshed package below includes
them and has been rechecked.

**Offline delivery (design, nostr).** When a friend has a nostr pubkey and none
of their devices has been reachable for a while, the text is also sent as a
NIP-17 private direct message: a kind 14 rumor, sealed (kind 13) and gift-wrapped
(kind 1059) to each of the friend's DM relays (their kind 10050 list). The
rumor carries the Lantern message id in a tag, so a receiver that later gets
the same text over iroh drops the duplicate. Relays see a gift-wrapped blob, its
size, timing and the recipient's pubkey — not the sender or the content.

## nostr's role

nostr is optional plumbing, never a requirement. Two players on the same LAN, or
two players with an invite ticket and working hole punching, never touch it.
Behind the cargo feature `nostr` (`src/nostr.rs`, `src/node/nostr.rs`); the
device list and invites are **implemented**, the rest is design:

* **Bootstrap and discovery (implemented).** `nostr_publish_devices` publishes
  the list, merging the devices the current list already proves (a second
  device imports the same nsec with `nostr_import`); `nostr_fetch_devices`
  reads and verifies one. Replacement lists are always strictly newer, since
  relays refuse a same-second replacement. A friend is bound to a nostr key
  only when a verified list names their device — a key inside an invite is a
  hint, never trusted — and binding merges one person's devices into one
  friend.
* **Invites over relays (implemented).** `nostr_send_invite` sends an
  `ltfriend` ticket to an npub as a NIP-17 DM tagged `["lantern","invite/1"]`;
  `nostr_check_inbox` raises `NostrInvite` for each one that is not expired,
  not ours, and not from a device that is already a friend. Nothing is
  redeemed automatically. Relays keep gift wraps, so a pending invite is
  raised again on every check until it is accepted or expires.
* **Offline mailbox.** NIP-17 as above.
* **Names.** NIP-05 (`name@domain`) resolves to a pubkey, which resolves to
  devices. Display names in Lantern are otherwise self-asserted and only as
  trustworthy as the invite that carried them.

Relays learn who talks to whom at the pubkey level and when. Players who do not
want that simply leave the feature off; the relay list is the player's to choose.

## Calls and video

**All design.** The plan follows the Media-over-QUIC fork at
`moq-fork-sync-2026-09-19/moq` (branch `spaceghost`), which already runs moq
over iroh 1.2 — the same iroh version as this tree:

* `moq-native` has an opt-in `iroh` feature: `Client::with_iroh(Endpoint)`,
  `Server::with_iroh(Endpoint)`, and `iroh://<EndpointId>/<path>` URLs
  (`rs/moq-native/src/iroh.rs`). It can take an endpoint we already own, so one
  endpoint can carry gossip, blobs, the friend protocol and moq, provided the
  ALPN sets are merged.
* `moq-room` derives a roster from announces under a path prefix
  (`<room>/<identity>/camera.hang`). `Scope::moq_path` already produces those
  prefixes (`party/<key>`, `fc/<key>`), so a call room and a gossip room share
  one identifier.
* `moq-audio` is pure Rust: Opus via `unsafe-libopus`, capture and playback via
  cpal (WASAPI on Windows), AEC via `sonora`. It is the most plausible media
  crate to run inside a Wine PE. Unverified for `x86_64-pc-windows-gnu`.
* `moq-video` encodes and decodes H.264 with openh264 and hardware backends
  (Media Foundation, NVENC/NVDEC, VAAPI). The fork's CI builds Windows only for
  MSVC; openh264's vendored C++ is the likely problem for the GNU target.
* `moq-rtc` is a WHIP/WHEP gateway, useful only if browsers join; `moq-ffi` is
  UniFFI, and neither it nor `libmoq` enables the `iroh` feature today.

**Where things run.** The game is a Windows PE under Wine. There is no video
decoder a PE can rely on there: Wine's Media Foundation H.264 decoder is backed
by winegstreamer and so by whatever GStreamer plugins the host happens to have,
`ghostty-multiagent/docs/MOQ.md` rates `ID3D11VideoDevice` decode
"least likely to work under Wine", and a software decoder in the game process
spends the frame budget and turns a codec crash into a game crash. So:

* **In the game (plugin + lantern.dll):** call signalling over the friend
  link (`CallRing`, `CallAnswer`, `CallHangup`, carrying a moq path and the
  host's NodeId), the roster and mute state in the UI, and at most a tiny video
  preview supplied as already-decoded frames.
* **In a native sidecar (`lantern-media`, a Linux process on Linux, a Windows
  exe on Windows):** moq-native over its own iroh endpoint, `moq-audio`
  capture/playback against PipeWire directly (no Wine audio path), `moq-video`
  with VAAPI/NVDEC, and its own window for video. The sidecar is simply another
  device of the same user: it gets its own NodeId, the plugin pairs with it by a
  one-time ticket exactly like a friend invite, and friends learn it from the
  device list. The plugin drives it over a loopback friend link, not a new IPC.
* **1:1 calls** need no server: the callee's sidecar runs `Server::with_iroh`
  and the caller connects to `iroh://<callee>/call/<id>`. **Group calls** use a
  moq-relay somebody runs (an FC relay), or a small mesh for three or four.

**Signalling spike (feature `calls`, `src/node/calls.rs`).** `call_ring`,
`call_answer`, `call_hangup` send `CallRing { call, media }`,
`CallAnswer`, `CallHangup` over the friend link, where `media` is
`iroh://<caller node>/call/<id>` — the URL form `moq-native` accepts. Every
call with a friend ends when their link drops. That is all: no media moves,
moq is not linked, and none of it is in the C ABI.

**Next steps, in order,** each one an observable result:

1. Build `moq-native` with `iroh` against this workspace's iroh and publish one
   Opus track from one host process to another over a relay-less endpoint.
2. Same, but with the endpoint shared with `lantern-core` (merged ALPNs), so
   a friend link and a moq session live on one NodeId.
3. ~~Call signalling on the friend wire behind `calls`, with a two-node test.~~
   Done as the spike above; it still needs the C ABI and a UI decision.
4. A `lantern-media` sidecar with PipeWire capture and playback, paired to a
   host node by ticket; measure latency and CPU on the host.
5. Only then: try `moq-audio` inside `lantern.dll` under Wine, in a test
   prefix, never the game's prefix first.
6. Video in the sidecar window; decide about in-game preview after measuring.

## Threat model and privacy

**What must never leave the machine:** game account data. No content id,
character name, Lodestone id, world, retainer, or chat log is read or sent by
Lantern. The library has no way to read them; the plugin is the only thing
that could, and its rule is that anything game-derived is sent only when the
player typed it or ticked a per-field opt-in. Display names are whatever the
player types.

**FFXIV terms of service.** Third-party tools are against the ToS; players who
install Dalamud plugins accept that. Lantern keeps the exposure minimal: it
never automates a game action, never sends game packets, never bridges in-game
chat, and never reads another player's data. It is a separate chat and voice
app that happens to draw inside the game.

**Who sees what:**

* A friend sees your NodeId, your IP addresses (in tickets and on direct
  paths), your display name and presence note, and what you send them.
* Anyone with a room/channel ticket or the topic id can join it and read every
  frame. Frames are authenticated, not confidential against members.
* The iroh relay you use (n0's by default, or your own, or none) sees NodeIds
  and traffic timing, never plaintext: QUIC is end to end.
* With `LT_RELAY_DISABLED` nothing contacts a third party at all, including
  n0's DNS/pkarr address lookup; only direct paths work.
* nostr relays (feature `nostr`, opt-in) see your pubkey, your device list,
  when gift-wrapped messages arrive for you, and their sizes.

**Adversaries and answers:**

* *Impersonation in gossip*: every frame is ed25519-signed by its author and
  verified; forged or re-attributed frames are dropped (`proto` tests).
* *Impersonation on friend links*: QUIC authenticates the NodeId; friendship
  is keyed on it.
* *Stolen or leaked invite*: single use, expiring, and the inviter sees who
  redeemed it. Keep tickets out of public channels.
* *Strangers*: cannot open a friend link. `accept_inbound = 0` refuses every
  inbound connection.
* *A flooding peer*: per-peer token buckets on friend-link frames, room
  messages and strangers drop the excess before it is stored or shown
  (`ratelimit.rs`, `tests/safety.rs`), texts and room messages have size
  limits, event queues are bounded, and any node can be blocked.
* *Someone posing as the author*: announcements verify against the author
  public key embedded in the plugin; the private key never ships
  (`author.rs`, [AUTHOR.md](AUTHOR.md)).
* *Local theft of the SQLite file*: it holds the device key (and the nostr key
  when used) unencrypted, like an SSH key without a passphrase. OS-level
  protection is the answer for now.

No secrets belong in this repository; keys are generated at runtime into the
player's plugin config directory.

## Under Wine

See [WINE.md](WINE.md). On 2026-09-23, `lantern.dll` (x86_64-pc-windows-gnu)
bound, passed its direct and n0-relay selftest, and interoperated with a native
node under stock Wine 11.0 in a container. `noq-udp` 1.3.0 already carries both
of the Wine fixes ghostty-multiagent had to patch into `iroh-quinn-udp`, so
nothing is vendored. None of this is the game's Wine or the game's prefix;
`/lantern selftest` in game is the proof that is still missing.

The latest delivery/privacy libraries pass actual legacy-library interoperability
and Windows ABI smoke. All ten C# binding tests pass against the optimized
Linux library, including 256 live originals and their byte-exact backlog through
a 16-slot general queue, merged without unread counts. The refreshed local
`vote-preview-history-delivery/latest.zip` includes these fixes and contains
exactly the six required current files (API 15/version 0.1.0.3), verified in
`native/channel-history-delivery-package-verification.txt`. It is not installed
or published; offline creator-ticket discovery and game verification stay open.

Remembered channel providers now use schema-5 opaque storage of original signed
HistoryOffer packets: 32 providers per channel, 256 globally, 4,096 bytes each,
and 30-day local receipt-time validity. Admission follows live nonce/provider
validation and successful authenticated snapshot fetch/uncancelled ingestion.
Before reconnecting, workers reverify the offer's signature, channel, recipient,
provider identity and route/size bounds, then apply current blocks. Ticket and
remembered addresses deduplicate by provider ID. These are routing claims;
old request nonces never authorize new snapshot downloads. Fresh catch-up still
uses a new request. Startup and one-minute maintenance prune expired/future
hints. Polling and frame-thread room calls do not perform SQL/cryptography.

All 77 Core tests pass, including actual three-peer identity-preserving restart
using the original creator-only ticket after that creator closes, with the
remaining member found through persisted hints and all three originals delivered
historically without live replay. Revalidation rejects forged or mismatched
cache rows and currently blocked providers. This recovers previously met
providers; a first-time joiner without any reachable contact still needs a
reachable member ticket. All 107 default and 118 optional workspace tests pass in
`native/channel-history-peer-hint-workspace-tests.log`. Optimized Windows/Linux
builds completed under `native/channel-history-peers/`. All ten optimized C#
binding tests pass, including the identity-preserving restart through the
original creator-only ticket and exact three-line historical recovery with
no live replay. Legacy-library interoperability passes. The settings disclose
remembered contacts; the managed build has zero warnings/errors. The checked
local `vote-preview-history-peers/latest.zip` contains exactly six current
build files, API 15/version 0.1.0.3, SHA256
`c740633baa5cc3d697d0c337077036fe4f062b99eb8fdb10572f6e41131c07be`.
The Windows DLL passes the isolated Wine ABI smoke, including exact signed
history and acknowledged local clear. Game verification, first-contact discovery and release
remain open; the preview is uninstalled and unpublished.

Name-card invite transport groundwork exposes existing Rust nostr job results
as C ABI events 37..40 and matching managed kinds. Successful and failed jobs
preserve their handles/details; invitations preserve the verified sender nostr
key and exact friend ticket; device lists preserve the user key and concatenated
32-byte device identities. Tickets no longer become generic log messages. The
ABI-2 event structure and existing event numbers remain unchanged. No invite is
accepted automatically. This does not yet make the feature available in the
plugin: optional exports, key/relay configuration, name resolution and explicit
Add-tab accept/ignore still need implementation and verification.

Name-card identity/send/inbox managed wrappers dynamically resolve all three
optional exports. Missing support fails cleanly (eleven actual old-library
binding tests pass). Optional native tests complete successfully after refreshing
source/header timestamps and including optional-module declarations in the
header contract. Plugin preparation now uses tracked workers and a worker-only
lock shared with Stop/unload disposal; generation checks suppress stale results.
Frame callbacks never wait for that lock. Opt-in defaults off and configured
relay list defaults empty. Plugin build passes without warnings/errors.
Typed job/inbox handling, accept/ignore UI and persistence, name resolution,
feature-enabled runtime proof and release/game verification remain open.

The managed name-card inbox now bounds pending state to 32 invitations and
remembers 256 local sender/ticket fingerprints for explicitly ignored or
successfully accepted invites. Configuration never stores the bearer ticket.
Admission normalizes sender/ticket, rejects wrong kinds/malformed/bounded inputs,
and coalesces replays. Acceptance remains pending until the matching native
FriendAdded; InviteFailed restores the retry action. Stop clears pending state.
The Add UI offers explicit opt-in, chosen relays, local npub preparation/copy,
one-day invite sending, manual inbox checks and accept/ignore. Eighty-eight Core
tests and the UI Release build pass. Email-style name resolution and actual
feature-enabled relay/Windows/game/package proofs remain open. No automatic
relay polling, invite sending or acceptance is introduced.

Email-style name-card lookup follows the NIP-05 well-known HTTPS mapping
(https://github.com/nostr-protocol/nips/blob/master/05.md, checked 2026-10-01).
Requests are explicit, cookies and redirects disabled, deadline ten seconds,
response at most 64 KiB. Local-part/domain parsing rejects URL injection; exact
name mappings require one lowercase 32-byte hex key and reject duplicate names
or names objects. Domain relay suggestions are ignored. UI displays the claimed
public key for a separate Send action; recipient edits/new lookups invalidate
stale mappings. It identifies a key, not a verified person. Reciprocal kind-0
profile verification remains open. Ninety-one Core tests pass using HTTP
fixtures without external requests; the final plugin build passes with zero
warnings/errors. The optional optimized Linux library passes eleven actual
binding tests, including local public identity persistence across restart.
Private local-relay/Windows/game/package proof remains required.

The new C ABI local-relay integration passes
(`native/name-card-local-relay-abi-tests.log`). The synthetic recipient is offline
when an invite is published to an in-process loopback MockRelay. Reopening the
same identity retrieves the verified sender/ticket; neither friend list changes
until a separate explicit acceptance call, whose matching acknowledgement arrives.
An inbox check after friendship does not offer the invite again. No public relay
or existing account is involved. `tools/build-native.sh` now builds the optional
name-card exports by default for both Windows and Linux; runtime use stays opt-in
with an empty relay list. Set `LANTERN_NATIVE_FEATURES` empty for a minimal build.
Windows/Wine/package/game and managed worker lifecycle proofs remain open.

Manual name-card operation state now remains busy until matching relay completion,
not merely until a job is queued. A bounded early-result map covers native
completion observed before a worker posts its returned handle. Unrelated, zero
and stale handles cannot replace status or release controls; Stop resets state.
Completion is processed even after disabling opt-in mid-job, preventing controls
from staying stuck when re-enabled. Local identity/lookup preparation clears
busy on completion/failure without a native job. All 94 Core tests pass and the
final sequential plugin build has zero warnings/errors. These are managed state
checks; actual framework/unload/game runtime proof remains open.

The final native-inclusive name-card plugin builds with zero warnings/errors.
Expanded isolated Wine checks pass optional exports/local identity/invalid-job
rejection without relay contact and the existing history ABI. The checked
`vote-preview-name-cards/latest.zip` contains exactly six current build files
and source API15/version0.1.0.3 manifest; SHA256
`12e6bb43d31097105c401b1681af36b19ddbd790d2f67698118feb7ede655df5`.
It is neither installed nor published. Final default/optional workspace tests
are running in `native/name-card-final-workspace-tests.log`; actual framework,
unload, rendered UI/game/privacy, reciprocal domain-profile and release checks
remain required.

Catalogue risk audit adds explicit reviewed privacy text for optional nostr
relay/domain services and four pending invitations per verified sender within
the 32-total managed cap. Ninety-seven Core tests and the plugin build pass.
Host native source now requests/decrypts at most 128 inbox envelopes and checks
current device blocks before emitting invitations. Those native changes await
tests/rebuild; the prior-source workspace run is still active, so its build copy
has not been changed. The checked name-card preview predates this revision.
Pending/decryption caps do not yet prove temporal rate limiting or a hard bound
on the SDK's allocations when a malicious relay ignores its filter limit.

Nostr SDK 0.45.4 source audit confirms its default normalized relay-message
limit is 5 MB and event-size default is unset. Lantern now explicitly configures
128 KiB normalized messages, 64 KiB events, 64 tags, eight relays and a 64-item
notification channel, in addition to the 128-unique-event fetch/decrypt cap.
These are accepted-message/SDK limits, not proof of every raw websocket
allocation. Native validation is running after correcting a fixture that hit
MockRelay's publish rate limit before testing the fetch cap: signed synthetic
events are now preloaded directly into the in-process relay. Existing checked
preview predates this revision.

Current native spam-limit revision passes three Core relay integrations
(`native/name-card-spam-native-tests.log`), including refusal when the fetched
unique-event collection exceeds 128, plus the strengthened C ABI offline/block
case: blocked device invitations are suppressed, and unblocking restores
retrieval before explicit acceptance. A separate direct relay test confirms
valid signed events above 64 KiB or with 65 tags do not reach the fetch result
(`native/name-card-relay-size-tests.log`). Only synthetic loopback fixtures are
used. Optimized rebuild runs in `native/name-card-limits-optimized-build.log`
with separate `native/name-card-limits/` output; the checked previous preview
remains intact. Final optimized binding/Wine/package/game proof is pending.

The final optimized limit revision now passes all 11 managed binding tests,
actual legacy interoperability and expanded isolated Wine ABI smoke. The plugin
build against the new Windows DLL has zero warnings/errors. The separate
`vote-preview-name-card-limits/latest.zip` is verified against exact final
managed/native bytes and source manifest (API 15, version 0.1.0.3). Earlier
preview remains preserved. Actual framework cancellation/unload/UI and game
verification remain open, as do reciprocal NIP-05 profile verification and
proof of every raw websocket allocation bound. No game-chat commands, real
recipient/domain/relay contact, installation or publication occurred.

File sharing now has a bounded managed offer model and plugin intake, with
strict metadata parsing and current block/mute/room admission. Intake never
fetches; explicit-click state reserves random local destination names and two
concurrent transfers. All 103 Core tests pass; plugin build has zero warnings
and errors. Native downloader audit shows the existing full-blob download does
not enforce authenticated real size. A 16 MiB advertised-size admission alone
is insufficient, so no fetch button is exposed yet. Bounded native streams,
friend transfers, file picker, chat lines, thumbnail decoding and storage
cleanup remain required. No actual file transfer or game verification claimed.

File transfer gains a separate bounded native API, `lt_blob_fetch_bounded`,
with optional managed binding and no unrestricted fallback on older libraries.
It checks the offered/Bao tree size before payload allocation, verifies leaves
against the content hash, caps two queued/running transfers at 16 MiB each and
uses a 30-second network timeout. It bypasses generic disk cache and refuses
to overwrite a destination. All 78 native Core unit tests, five actual two-node
tests and eight FFI/header unit tests pass; the managed plugin builds cleanly.
Tests include exact bounded download bytes, existing-file preservation, sender
block rejection and claimed-size mismatch. Explicit cancellation, cleanup on
shutdown/leave/mute/block during writes, UI, thumbnails and release/game proof
remain required; no optimized native artifact or installation yet.

Bounded file transfers now have cancellation requests retained even before
the worker starts, duplicate-handle exclusion, and signals on block/close.
The optional `lt_blob_cancel_bounded` export and managed binding preserve older
library loading without unrestricted fallback. Managed cancellation keeps its
slot until terminal outcome; completion may race cancel. Partial-file ownership
closes before cleanup on task exit. Six focused native tests, five actual
two-node tests, eight FFI/header tests and all 104 managed Core tests pass;
plugin build has zero warnings/errors. These tests do not certify every async
open/write shutdown race or Windows cleanup. Native release rebuild, download
UI, leave/mute cancellation integration, thumbnails and cleanup remain open.

Channel UI now shows received file offers with explicit Download/Retry,
Cancel and Copy saved path controls. Nothing opens automatically. Filesystem
preparation runs on a worker and rejects observed reparse ancestors; native
queueing rechecks room/block/mute policy. Stale preparation callbacks cannot
start a previous destination after retry. Native queued-handle tracking keeps
cancel/completion races distinct from local preparation. Leave/mute/block
request cancellation; frame reconciliation also covers folder mute changes.
All 106 Core tests pass, including Linux symlink-folder refusal, and plugin
build is clean. Current optimized native preview lacks bounded exports, so
its download buttons remain disabled pending rebuild and call-through tests.
Send picker, friend offers, thumbnails, quota/cleanup and actual framework/
Windows/game proof remain open. Directory checks assume local control; they
do not eliminate hostile parent replacement races.

Channel sending now has a full-path field and explicit screenshot privacy
confirmation. Worker-only capped file snapshots and background native imports
keep disk reads and large memory copies off the frame thread. One preparation
runs at once; native hashing completion may arrive early and is retained in
a bounded map. Current joined/unmuted channel is checked before offer. Shared
cache admission conservatively limits the session to 64 MiB/16 files. All 107
Core tests pass. Optimized Windows/Linux artifacts are in native/file-downloads;
all 12 actual C# binding tests and legacy interoperability pass, and plugin
build against the new Windows DLL is clean. Tests prove byte-exact bounded
remote download, overwrite refusal and block rejection. Actual framework/send
races, Windows bounded transfer, friend offers, thumbnails, cleanup settings
and game/release gates remain unverified. Nothing installed or published.

Remote file metadata now has bounded native admission and scoped withdrawal.
Withdraw/leave free metadata and cancel bounded jobs. Appended ABI-2 event 41
removes unused UI offers while preserving already saved files. Active withdrawn
transfers keep their slot through terminal cleanup. Room admission is serialized
with leave to prevent stale post-leave retention. All 108 Core tests, eight FFI
header/unit tests and five actual two-node tests pass; plugin builds cleanly.
Further reoffer/leave regression is running separately. Optimized artifacts
predate this source revision. Expiry, thumbnails, friend transfers, configurable
storage cleanup and actual Windows/framework/game verification remain open.

Downloaded-file retention is configurable and opt-in (zero keeps files).
Worker cleanup before new downloads deletes only expired GUID-N .download
files, preserves active paths/unrelated names, and reconciles saved UI state.
Cleanup and quota scans share a worker-only lock; the entry cap is checked
before deletion. All 110 Core tests and a clean plugin build validate current
source. Actual framework/Windows/local replacement races remain unverified;
the checked file-sharing preview predates budget and retention changes.

Friend files now use appended friend-link Wire variants and optional ABI-2
exports. Authenticated room-zero offers require a current known friend; sharing
rechecks block/mute/link state before publication. Download remains explicit,
with exact native size/hash validation, cancellation, and scoped withdrawal.
All 13 C# binding tests pass against the optimized friend-file Linux library;
Windows validates exports/invalid inputs only. The actual archived legacy
library continues chat/presence after an unsupported friend-file offer is
queued; this fixture has no acknowledgment of the unknown frame itself.

Saved-file previews require a separate explicit click. Only static,
non-interlaced 8-bit PNG and single-scan baseline JPEG are admitted; each side
is capped at 4096 and area at 8 Mi pixels. PNG scanline decompression is checked
against the claimed pixel budget before platform decoding. Ancillary PNG and
JPEG APP/COM metadata is removed from the immutable decoder snapshot. One
owned texture is retained with explicit larger/close controls and cancellation
plus late-result disposal on unload. All 116 Core tests and the plugin build
pass. Actual Dalamud decoding/rendering/unload remains unverified; header
admission is not a proof of every allocation within the platform decoder.
The checked local friend-files preview remains uninstalled/unpublished.

A subsequent native safety revision keeps offer and blocked-state admission
stable through cancellation-job registration. Blocking synchronously forgets
all remote offers; local unfriend synchronously forgets room-zero offers.
Incoming insertion holds blocked admission through emission, preventing a
post-block retained offer. The checked friend-files preview predates this
revision. Deterministic cleanup and active-removal integration regressions
are tracked separately in the accepted-vote ledger; a new optimized rebuild
and Windows transfer/cleanup proof remain required.

The persistent serving cache has a separate configurable admission budget
(default 256 MiB, range 64–1024) and an explicit off-only clear control. The
native FsStore's default garbage collector is disabled. Worker preflight counts
all known cache files and reserves 32 MiB for an upload plus metadata/outboard
growth; this is not a hard native allocation quota. Unknown entries, observed
links and more than 4096 entries refuse cleanup before any initial deletion.
Only the fixed lantern.blobs cache is touched; identity/friends SQLite,
originals and saved downloads remain outside it. Deletes are non-recursive.
Clear waits for completed background work and drained framework callbacks;
start refuses while clear is busy or the node is Stopping. Any native close
exception disables cache clearing for the current plugin instance. No cache
files are automatically deleted. All 120 Core and 14 Linux binding tests pass,
including clearing actual native cache after close and reopening the same
identity. The plugin build is clean. Actual Windows filesystem/framework
lifecycle, hard quota and adversarial ancestor replacement remain unverified.

### Sealed-channel codec foundation (2026-10-01)

`ltsealed` tickets use a separate, versioned format with a fresh OS-random
32-byte topic and 32-byte key. Their Debug output redacts the key. Plain
room, node and friend ticket parsers refuse this prefix. Parsing bounds
the input and metadata and rejects unknown versions or trailing bytes.

`proto::encode_sealed` encrypts the complete serialized Frame with
XChaCha20-Poly1305 and a fresh 24-byte nonce, then signs the ciphertext
using the existing Ed25519 envelope. Associated data binds LTSEAL01, the
room topic and the signing sender. The sealed decoder bounds the packet,
verifies the signature before decryption and requires exact framing. It
refuses plain packets; the ordinary decoder refuses sealed packets.
The implementation uses RustCrypto chacha20poly1305 0.10.1:
https://docs.rs/chacha20poly1305/0.10.1/chacha20poly1305/

This is a codec foundation, not an enabled sealed-room feature. Node
creation/join, key persistence, replay suppression, reseal, encrypted
history snapshots and file bytes, optional ABI/bindings and plugin lock
UI still need implementation and verification. In particular, encrypting
a HistoryOffer alone does not encrypt its separately served snapshot.
No sealed ticket should be advertised as a working channel yet. All
ticket holders can read a channel, and a new key cannot revoke access to
old ciphertext already obtained with the old key.

### Encrypted history snapshot foundation (2026-10-01)

`channel_history::sealed_snapshot` encrypts the entire bounded snapshot,
including the original signed records, under the channel ticket key and
a separate LTHSEAL1 associated-data domain. The provider signs ciphertext.
`read_sealed_snapshot` requires that exact provider's signature, channel
and key, then rechecks the original room-bound author signatures. Plain
snapshots and sealed room frames cannot be substituted for this format.
Resealing leaves the old ticket able to decrypt old retained ciphertext.

The bounded Bao fetch helper now accepts an optional sealed ticket. It
checks the offered size against a separate encrypted cap (the original
snapshot cap plus 256 bytes), checks the authenticated Bao header before
reading payload, and verifies/decrypts before returning records. Existing
ordinary-room callers explicitly use None; automatic sealed-channel
history exchange has not yet been connected. This helper does not store
or expose plaintext before authentication succeeds.

### Native sealed channel integration (2026-10-01)

Native `channel_create_sealed` creates a random Custom channel. `room_join`
accepts ltsealed only with Custom scope, `room_ticket` preserves its key,
and `room_is_sealed` reports the mode. `channel_reseal` creates a distinct
channel/topic/key; it does not automatically invite anyone or leave the
old room. Existing `channel_invite` sends the new ticket only to the
explicitly selected linked friend. The receiver must explicitly join.

Normal broadcasts, presence and Hello greetings use a central room-mode
encoder. Sealed history uses an encrypted HistoryRecord rather than the
legacy plaintext App tail. History request retries and offers are encrypted;
automatic snapshot serving/fetch and cached original provider offers use
the sealed ticket for verification. Plain/author channels retain their
existing formats. A recent 2048-packet authenticated replay window is bounded;
it is not persistent replay protection after restart or after eviction.

Before subscription, a worker stores a domain-separated key fingerprint
in the existing SQLite profile table under sealed-room:<topic>. A later
plain ticket or different key for that topic is refused, including after
restart. Live mixed-mode joins are refused too. This is an admission marker,
not a stored key, and does not protect against deliberate database rollback
or a different/older client reading the local SQLite file. Local retained
history remains plaintext in SQLite, as ordinary retained history does.

File publication and incoming file offers are temporarily refused in sealed
rooms until file-byte encryption and download integration are implemented.
This is an unfinished part of the vote, not a final sealed-channel limitation.
Optional ABI/bindings, plugin lock/reseal controls, lifecycle/restart and
Windows/game verification also remain pending. No installed plugin or
release is changed by this native source implementation.

### Sealed file bytes and optional exports (2026-10-01)

Native `blob_add_sealed_bytes` queues immutable caller bytes (up to 16 MiB)
for worker encryption before blob-store import. File ciphertext uses LTFSEAL1,
a fresh 24-byte nonce and a provider signature, with topic/provider binding.
Only the resulting ciphertext hash is offered. Append-only SealedBlobOffer
(index 8) carries plaintext size plus the bounded encrypted size inside
the encrypted room frame. Ordinary imports cannot be offered into sealed
channels; sealed files cannot be offered under a different key or to a
friend/plain room. Resealing requires preparing the file again.

Remote admission preserves the existing per-sender/global offer limits.
Bounded fetch reads the exact encrypted Bao header before payload, authenticates
the provider signature and AEAD, checks plaintext length, then creates a
new destination. The generic blob downloader refuses sealed files. Cancellation,
withdrawal and room/block cleanup retain the existing job registration and
partial-file rules; saved files remain intact. Crypto has separate chat,
history and file domains. Local upload names/history metadata remain in
SQLite; downloaded files are intentionally saved as plaintext.

Optional ABI-2 exports lt_channel_create_sealed, lt_channel_reseal,
lt_room_is_sealed and lt_blob_add_sealed_bytes preserve all prior symbols
and layouts. Managed bindings detect all four plus bounded fetch before
enabling sealed APIs. Plugin creation/lock/reseal UI and encrypted-upload
routing are still pending; these exports alone do not update the installed
plugin. Optimized Linux/Windows/Wine/package/game proof also remains pending.

### Plugin sealed-channel controls (2026-10-01)

The Add view recognises ltsealed separately from ordinary room tickets. A
creation checkbox selects sealed native creation; unavailable libraries show
an update message. Explicit joins and saved-channel rejoin refuse sealed
tickets when all required optional native exports are absent, with no plain
fallback. Channel rows display [locked] for sealed tickets.

A sealed channel's context menu opens a reseal selection popup. Selection
starts empty, only linked friends are selectable, and the service rechecks
all selected identities/link/block state before creating the new room.
Only the selected recipients are passed to native private channel_invite.
Invitations are queued, not acknowledged; a partial failure keeps the new
channel and reports how to retry. The old room remains and the popup explains
that old tickets can still read old history. The new room inherits local mute
state. No game-chat commands, public directory publication or automatic
invitation acceptance are added.

Sealed channel file preparation selects BlobAddSealedBytes on the existing
immutable snapshot/background path, retaining cache/session budgets and
generation/room/mute admission checks. Plain-room and friend uploads continue
using their existing preparation paths. Core ticket tests pass (120 total)
and the plugin builds with zero warnings/errors. Actual window rendering,
framework lifecycle/reseal race behavior and game interaction remain unproven.
The build used the previous optimized native directory for compatibility;
its DLL has no sealed exports, so these controls remain unavailable with
that DLL until a current optimized build is supplied. Nothing was installed.

### Sealed history provider restart proof (2026-10-01)

A new real managed/native test creates a sealed channel and retains two
original signed lines at another member. After the creator closes, a late
member obtains both via the member's sealed ticket and encrypted snapshot,
without live-message replay or unread counts. Its authenticated encrypted
provider offer is cached. The late member clears local history, leaves and
restarts using the original creator-only sealed ticket while that creator
is still offline; it reconnects through the cached provider and receives
three original signatures, including a message sent while it was away.
The focused test passed with no skips against the current debug Linux DLL.
This proves returning-member discovery from the same SQLite identity; a
first-time cold member with only an unreachable creator still needs a
reachable provider and is not proven by this test.

Offline Windows-target dependency resolution identified one missing
crypto-common 0.1.7 -> rand_core 0.6.4 lockfile edge; no package versions
changed. The edge is now recorded and locked metadata resolution succeeds.
Optimized native builds and the expanded private Wine fixture are in progress.

Optimized build follow-up: Windows and Linux builds completed with locked
dependencies and nostr enabled. All 35 hashed native source/build inputs
match the current worktree. The current optimized Linux library passes all
16 managed binding tests with no skips, including sealed provider restart.
The plugin rebuilt against the current Windows DLL with zero warnings/errors.
Local preview ../xiv-lantern-build/vote-preview-sealed-channels/latest.zip
has the exact six expected root entries, matching current assemblies/source
manifest and Windows DLL, API 15, and an identical version-named ZIP.
Preview SHA256: d8f9c5f610c382880a241200473d07484ef700c0c2cf280246527e32e34bfe3c
It remains uninstalled/unpublished and is not certification of all votes.
Actual plugin rendering/lifecycle/game/loading gates remain unproven.

Sealed conversation metadata now uses c:sealed:<SHA-256 of canonical ticket>,
keeping bearer keys out of conversation IDs. Legacy sealed mute IDs normalize
at config load, preserving mute/unread behavior; private saved tickets remain
unencrypted and bootstrap variants still produce distinct conversation IDs.
Core passes 122 tests, optimized bindings pass 16 without skips, and the plugin
build has zero warnings/errors. Two Windows DLL instances under Wine pass
sealed original-signature history, exact decrypted file transfer, withdrawal,
new-key isolation and 16 MiB cancellation with explicit localhost-only routes.
The fresh uninstalled/unpublished vote-preview-sealed-conversations preview
passes exact six-entry/current-byte/API15 checks. SHA256: cdf444787223d8172572e7a41cde42e84a612c76ff8abd434ab987abc4598d09
Actual Windows, automatic discovery, ImGui/game/loading gates remain open.
No game-chat commands were entered.

Sealed channel list and conversation header now draw a Dalamud icon-font lock
with a ticket-access/local-storage tooltip. Release build passes with zero
warnings/errors; actual game rendering remains unverified. Unpublished local
preview vote-preview-sealed-lock-icon passes exact six-entry/current-byte/API15
checks and identical version-ZIP bytes. SHA256: 6cfed351ef94c22ffa9e7472cb3f0782ccde26ead9464b3ab4e9050dfe2f1c5d

Reseal selection follow-up: the dialog names its source channel and excludes
blocked friends. If a selected friend becomes unavailable, its pending selection
stays visible with an explicit Remove action; Cancel clears pending state.
Service validation still refuses unavailable recipients before creating a key.
Release build passes with zero warnings/errors. Actual ImGui transition/game
verification remains open; prior preview ZIPs predate this follow-up.

Invitation retry follow-up: the service validates the currently joined channel
and current linked/unblocked friend before queueing. The menu marks sealed
channels and describes ticket access; its local line says queued, not delivered.
Release build passes with zero warnings/errors. No real invitation or game-chat
command was sent; actual menu/delivery checks remain open. Prior previews
predate this source change.

Sender file offers now appear as bounded session rows after successful native
queueing, with admitted filename/plaintext size and a Withdraw button. Withdrawal
uses existing friend/channel native APIs and remains allowed while muted. Rows
are removed only after queue acceptance; downloaded copies/local cache and
cumulative session quotas remain unchanged. Stop/leave/remove/block discard
applicable outgoing metadata. Release build passes; actual framework/UI behavior
remains unverified. No real recipients or game-chat commands used.
