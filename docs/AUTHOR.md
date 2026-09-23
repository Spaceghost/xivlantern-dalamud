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
