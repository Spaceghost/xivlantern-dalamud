# Linkpearl design

> Status: this is the required direction, not a claim that it works. Each
> section says whether it is **implemented** (code in this tree, with the test
> that exercises it named), a **spike**, or **design** (nothing built yet).
> Nothing in this document has been observed inside the game or under Wine;
> every test named here runs on a Linux host (the Incus builder) with no
> display, and passes inside a network namespace that has only `lo`.

The owner's brief: "a sort of friend list and our own comms net with calls and
video and text — maybe even nostr", for FFXIV players, driven from a Dalamud
plugin. Linkpearl is the transport and state underneath that plugin. It does not
talk to the game, read game memory, or send game commands, and it never will
(see [Threat model](#threat-model-and-privacy)).

## Status at a glance

| Piece | State | Evidence |
| --- | --- | --- |
| Device identity (iroh key in SQLite) | implemented | `identity_survives_a_restart`, `store::tests` |
| Gossip rooms, room presence, blobs, direct connections | implemented (scaffold) | `two_nodes_meet_talk_and_share_a_file` |
| Friend invites, friend list, presence heartbeat, 1:1 text | implemented | `tests/friends.rs` |
| Group channels over iroh-gossip, channel invites | implemented | `tests/friends.rs`, `tests/demo.rs` |
| C ABI for the above, `include/linkpearl.h` in sync | implemented | `tests/abi_friends.rs`, `the_header_matches_the_exports_and_constants` |
| C# binding for the above | implemented | `Linkpearl.Tests/FriendTests.cs` against `liblinkpearl.so` |
| CLI demo (`linkpearl`) | implemented | `crates/linkpearl-cli/tests/demo.rs`, `scripts/demo.sh` |
| nostr: device list, invites over relays (feature `nostr`) | implemented | `tests/nostr.rs` against an in-process relay |
| nostr: NIP-17 offline mailbox for texts, NIP-05 names | design | — |
| nostr in the C ABI | design | — |
| Call signalling (feature `calls`) | spike | `tests/calls.rs` |
| Call media (moq over iroh, sidecar) | design | — |
| `linkpearl.dll` for `x86_64-pc-windows-gnu` | builds; selftest passes under stock Wine 11 | [WINE.md](WINE.md) |
| Blocking, per-peer rate limits, size limits | implemented | `tests/safety.rs` |
| Author channel (signed announcements), support messages | implemented | `tests/safety.rs`, `crates/linkpearl-cli/tests/demo.rs` |
| Dalamud plugin (XivLinkpearl) | builds; host tests only | `tests/XivLinkpearl.Core.Tests` |
| Anything in the game, or in the game's Wine prefix | not observed | — |

This table is kept current by the commits that change it.

### Try it

Builds and tests run in the Incus builder, never on the machine the game runs
on:

```sh
scripts/sync-to-builder.sh
incus exec fedora:iroh-build -- bash -lc 'cd /root/linkpearl && cargo test --workspace'
incus exec fedora:iroh-build -- bash -lc 'cd /root/linkpearl && scripts/demo.sh'
# everything, including nostr and the calls spike:
incus exec fedora:iroh-build -- bash -lc 'cd /root/linkpearl && cargo test --workspace \
    --features linkpearl-core/nostr,linkpearl-core/calls,linkpearl-cli/nostr'
```

By hand, two terminals on one machine (relays off, so only direct paths):

```text
A$ linkpearl --db alice.sqlite --name Alice --relay off
B$ linkpearl --db bob.sqlite --name Bob --relay off
A> /invite                      -> invite lpfriend...
B> /accept lpfriend...          -> friend added: Alice
B> /msg alice hello             A sees: <Bob> hello; B sees: delivered [...]
A> /status away crafting        B sees: * Alice is away "crafting"
A> /channel new static
A> /channel invite Bob          B sees: /channel join lproom...
B> /channel join lproom...
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
* No per-peer rate limits on friend links or rooms; event queues are bounded
  and count drops, which is the only protection.
* Channels have no history for late joiners, no ordering and no encryption
  beyond each QUIC hop; anyone holding the ticket can read.
* `lp_stats.relayed_conns` is still always 0 (scaffold); friend links are not
  counted in `lp_stats` at all.
* nostr and call signalling are not in the C ABI; if a build enables them,
  their events reach `lp_poll` as `LP_EV_LOG` lines.
* The C# binding is tested against `liblinkpearl.so` on Linux only; the DLL
  has never been loaded by Dalamud.

## Processes and layers

```
 FFXIV (Wine) ─ Dalamud ─ plugin (C#, ImGui)
                              │ P/Invoke, bindings/csharp
                              ▼
                        linkpearl.dll  (include/linkpearl.h, linkpearl-ffi)
                              │
                        linkpearl-core (tokio, iroh, gossip, blobs, SQLite)
                              │ QUIC (iroh), hole punching, optional relay
                              ▼
                friends' linkpearl nodes ── optional nostr relays
                              │
          (design) linkpearl-media sidecar: moq over iroh, audio/video
```

The C ABI is poll-driven: the plugin calls `lp_poll` once per frame and gets
`lp_event`s. No callback ever crosses into the plugin, for the same reason as
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
application data, kind 30078, `d` = `ffxiv-linkpearl/devices`) whose content
lists the user's NodeIds. Each entry carries the device's own ed25519 signature
over `"linkpearl-device-claim/1" || nostr_pubkey || node_id`, so the list proves
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

**Implemented** (`crates/linkpearl-core/src/node/friends.rs`, store schema 2).

**Adding a friend is mutual and needs an invite ticket.** There is no directory
and no "add by name" without nostr.

1. Alice calls `invite_create(ttl)`. The ticket text is `lpfriend` + lowercase
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
ticket (`lproom…`), and between friends by `ChannelInvite { ticket }` over the
friend link, surfaced as an event the UI turns into a "join?" prompt.

What channels do **not** have yet, and should: history for late joiners (a
blob snapshot offered by any member is the likely shape), ordering (gossip does
not order), membership control (anyone holding the topic id can join and read —
the topic is the only secret), and end-to-end encryption beyond each QUIC hop.
Encrypting frames with a key carried in the ticket closes the last one.

**Offline delivery (design, nostr).** When a friend has a nostr pubkey and none
of their devices has been reachable for a while, the text is also sent as a
NIP-17 private direct message: a kind 14 rumor, sealed (kind 13) and gift-wrapped
(kind 1059) to each of the friend's DM relays (their kind 10050 list). The
rumor carries the Linkpearl message id in a tag, so a receiver that later gets
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
  `lpfriend` ticket to an npub as a NIP-17 DM tagged `["linkpearl","invite/1"]`;
  `nostr_check_inbox` raises `NostrInvite` for each one that is not expired,
  not ours, and not from a device that is already a friend. Nothing is
  redeemed automatically. Relays keep gift wraps, so a pending invite is
  raised again on every check until it is accepted or expires.
* **Offline mailbox.** NIP-17 as above.
* **Names.** NIP-05 (`name@domain`) resolves to a pubkey, which resolves to
  devices. Display names in Linkpearl are otherwise self-asserted and only as
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

* **In the game (plugin + linkpearl.dll):** call signalling over the friend
  link (`CallRing`, `CallAnswer`, `CallHangup`, carrying a moq path and the
  host's NodeId), the roster and mute state in the UI, and at most a tiny video
  preview supplied as already-decoded frames.
* **In a native sidecar (`linkpearl-media`, a Linux process on Linux, a Windows
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
2. Same, but with the endpoint shared with `linkpearl-core` (merged ALPNs), so
   a friend link and a moq session live on one NodeId.
3. ~~Call signalling on the friend wire behind `calls`, with a two-node test.~~
   Done as the spike above; it still needs the C ABI and a UI decision.
4. A `linkpearl-media` sidecar with PipeWire capture and playback, paired to a
   host node by ticket; measure latency and CPU on the host.
5. Only then: try `moq-audio` inside `linkpearl.dll` under Wine, in a test
   prefix, never the game's prefix first.
6. Video in the sidecar window; decide about in-game preview after measuring.

## Threat model and privacy

**What must never leave the machine:** game account data. No content id,
character name, Lodestone id, world, retainer, or chat log is read or sent by
Linkpearl. The library has no way to read them; the plugin is the only thing
that could, and its rule is that anything game-derived is sent only when the
player typed it or ticked a per-field opt-in. Display names are whatever the
player types.

**FFXIV terms of service.** Third-party tools are against the ToS; players who
install Dalamud plugins accept that. Linkpearl keeps the exposure minimal: it
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
* With `LP_RELAY_DISABLED` nothing contacts a third party at all, including
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
* *A flooding peer*: event queues are bounded and count drops instead of
  blocking; per-peer rate limits are still missing (design).
* *Local theft of the SQLite file*: it holds the device key (and the nostr key
  when used) unencrypted, like an SSH key without a passphrase. OS-level
  protection is the answer for now.

No secrets belong in this repository; keys are generated at runtime into the
player's plugin config directory.

## Under Wine

See [WINE.md](WINE.md). On 2026-09-23, `linkpearl.dll` (x86_64-pc-windows-gnu)
bound, passed its direct and n0-relay selftest, and interoperated with a native
node under stock Wine 11.0 in a container. `noq-udp` 1.3.0 already carries both
of the Wine fixes ghostty-multiagent had to patch into `iroh-quinn-udp`, so
nothing is vendored. None of this is the game's Wine or the game's prefix;
`/linkpearl selftest` in game is the proof that is still missing.
