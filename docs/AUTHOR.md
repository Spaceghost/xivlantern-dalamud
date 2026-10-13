# The author channel

> Status: implemented and host tested (`crates/lantern-core/tests/safety.rs`,
> `crates/lantern-cli/tests/demo.rs`). **Not configured in this build**:
> `src/XivLantern.Core/AuthorIdentity.cs` is empty until the author does the
> steps below. Nothing here has run in game.

The author channel is how the mod's author talks to everyone who opts in:
announcements only the author can post, and a way for players to write back.

## Keys, and where they live

* **Author key** — an ed25519 key made once with `lantern author keygen`.
  The private half is one file, `~/.config/lantern/author.key`, mode 0600,
  on the author's own machine. It is never committed, never copied into a
  container, never shipped. It only signs announcements, offline.
* **Public key** — printed by `keygen`/`pubkey`, embedded in the plugin as
  `AuthorIdentity.PublicKeyHex`. It names the channel (the gossip topic is
  derived from it) and is what every player's node checks signatures against.
* **Author node(s)** — an ordinary Lantern node the author keeps running
  (`lantern --author-mode <public key>`), with its own node key in its own
  database. Its NodeId is embedded as `AuthorIdentity.NodeIdHex`: players'
  nodes bootstrap the channel from it and send support messages to it. Losing
  or rotating it only needs a plugin update; the author key does not change.

A unit test fails the build if `PublicKeyHex` is anything but empty or 64 hex
characters (a 128-character value would be a secret key).

## Setting it up (once, on the author's machine)

```sh
lantern author keygen                 # prints: public key: <64 hex>
lantern --db ~/.local/share/lantern/author-node.sqlite --name "Author" \
          --author-mode <public key>    # prints the node's ticket; its id is on the "ready" line
```

Put the public key and the node id into `AuthorIdentity.cs`, commit, release.
Keep the author node running wherever it is always on (the relays find it by
NodeId, so no port forward is needed).

## Announcing

```sh
lantern author sign --seq 1 --title "Lantern 0.1" --body "What changed…"
# -> ltannounce…
```

Paste the line into the running author node: `/announce ltannounce…`. `--seq`
must grow with every announcement; a player's node keeps each number once, so
re-offers never notify twice. Any node in the channel can carry a signed
announcement — members hand the latest three to newcomers — but only the
author key can make one.

## Support messages

A player who has joined the channel can write to the author node (Author tab,
or `/support …` in the CLI). The author node shows `[support from <id>] …`;
`/reply <id-prefix> <text>` answers. Replies are accepted only from the
embedded author node ids. Nothing is queued: if the author node is offline the
player is told the message was not delivered. Messages are rate limited per
sender, capped at 2048 bytes, and a sender can be blocked with `/block <id>`.

## Preparing release news offline

`tools/release-announcement.py` prepares an unsigned draft from a release tag,
an explicitly chosen sequence and a fresh output directory. It uses the same
changelog section and player-facing summaries as the release tooling, includes
a link to complete release notes, and keeps the title/body within the native
120/1500-byte UTF-8 limits without splitting a summary line. The directory holds
`announcement-body.txt`, complete `release-notes.md`, and `announcement.json`
with structured arguments for the existing offline author signer. It refuses
to overwrite an existing draft. No key is read, signature made or message sent.

Review the content and choose a sequence greater than every announcement already
signed with this author key; preparing a draft does not reserve that sequence.
Sign separately on the author's machine and verify before handing the resulting
announcement to the author node. These are native Lantern CLI operations, not
commands to enter into FFXIV chat. Never put the signing key into CI or a builder.
The current build still has no author identity/node configured, and preparation
alone does not establish publication or game verification.

## Sequence reservations and offline verification

The offline author signer now keeps a sibling `<key-path>.announcements`
directory. New key generation initializes it at zero. Signing takes an
exclusive create-new lock, checks the public key and greatest reserved number,
and persists a new reservation before returning a signature. Duplicate and
older numbers fail. Interrupted signing can consume a number; gaps are safe
and must not be reclaimed. On Unix, record files are mode 0600, the directory
is mode 0700, and files plus parent directory are synced before success.

An older key without this history requires `--previous-seq` once, with the
greatest number ever signed by that key. This is an operator assertion, not a
network audit; it cannot override existing history. Keep the reservation
history with the key when moving the signing setup. Do not roll back or delete
it. A stale lock fails closed; review whether a signer is still active and the
reserved numbers before manually recovering it. This protects this local
signing workflow, not independent copies of the key signing elsewhere.

The offline `author verify` operation accepts only an expected public key and
a signed-announcement file. It reads at most 8192 bytes, rejects noncanonical
encoding and invalid/tampered signatures, and reports the verified sequence,
title and body with escaped control characters. It neither reads the private
key nor opens a network node. Verify the draft's expected title/body as well as
the signature before any separately authorized handoff to the author node.

## Handing off a verified file

The native author node accepts `--announce-file` together with `--author-mode`
and the expected public key. It reads a bounded immutable snapshot and verifies
canonical encoding, signature and author before opening SQLite or any network
endpoint. A wrong author, invalid signature or unreadable file exits without
opening the node. Successful startup queues the verified packet into the author
channel and retains it for subscribers who join later. Queueing is not a
receipt acknowledgment from every subscriber.

This is an explicitly requested publication operation on the native Lantern
node; it is not an FFXIV chat command and never reads the signing key. Keep the
author-node database stable across restarts so the node identity stays the same.
The draft metadata contains structured handoff arguments with placeholders for
the author database, public key and signed file. Preparing that metadata does
not start a node or publish anything. Current tests use temporary synthetic
keys/nodes with public relays disabled; the real author setup remains empty.

Signing input is bounded before reading a key: body files allow at most 1502
bytes (including a final LF/CRLF), and the trimmed body must fit the native
1500-byte budget. Title, body and positive sequence are validated before key
access or reservation, so rejecting an oversized draft does not consume its
sequence. The file read itself is capped; it never reads the full contents of
an oversized note file merely to reject it afterward.

## Local draft signing orchestration

The release entrypoint's `announcement-sign` operation accepts a reviewed draft,
a native CLI executable, an explicit local key path and the expected public
key. It is disabled when CI is set. Python never reads key contents: it asks the
native CLI for the public half, checks it, signs the reviewed immutable body
snapshot, and uses the public-only native verifier to compare exact sequence,
title and body. Structured subprocess arguments avoid shell interpretation.

A successful operation creates a mode-0600 `signed.ltannounce` and a
`signed-receipt.json` containing public identity, sequence and content hashes;
the receipt explicitly says publication is false. It never opens an author
node or publishes. Existing packet files are never overwritten. Failure leaves
a reserved empty packet marker for operator review, and a sequence might already
have been consumed; do not blindly remove the marker or retry the same number.
Unsigned draft metadata stays unchanged. These steps are separate from the
normal release-cut command and from author-node startup/publication.

Local draft signing also accepts `--previous-seq` for the one-time older-key
migration described above. The value must be nonnegative and below the draft's
sequence. It is forwarded to the native signer as an explicit operator
assertion; it is never inferred or silently set to zero. The native signer
still refuses that option when sequence history already exists. This preserves
the expected public identity while establishing future local reservations;
it cannot establish what independent copies of the key signed elsewhere.
