# XivLantern IPC

> Status: implemented in the plugin and compiled against Dalamud 15.0.3.5;
> the payloads are unit tested (`tests/XivLantern.Core.Tests`). No other
> plugin has called these gates yet, and none of it has run in game.

XivLantern publishes Dalamud IPC gates so sibling plugins (Ghostty, XivMcp,
a launcher) can use the player's Lantern without referencing its assembly.
The names are in `src/Shared/IpcContract.cs`; this file is their contract.

**Consent comes first.** Reading the friend list or incoming messages, and
sending messages as the player, are each refused until the player ticks the
matching box in Lantern's settings ("Let other plugins see my friends and
incoming messages", "Let other plugins send messages as me"). Both are off by
default. A caller gets an `error: …` string, never an exception.

| Gate | Shape | Needs |
| --- | --- | --- |
| `XivLantern.v1.GetStatus` | `Func<string>` | nothing |
| `XivLantern.v1.GetFriends` | `Func<string>` | read allowed |
| `XivLantern.v1.SendToFriend` | `Func<string who, string text, string>` | send allowed |
| `XivLantern.v1.OpenChat` | `Action<string who>` | nothing |
| `XivLantern.v1.MessageReceived` | event, `string` | read allowed |

## GetStatus

```json
{"enabled":true,"online":true,"node_id":"<64 hex>","name":"<typed name>","friends_online":2,"unread":3}
```

`enabled` is the player's switch; `online` is whether the node is running.
Always answers, because it says nothing about anybody else.

## GetFriends

```json
[{"id":"<64 hex node id>","name":"Bob","online":true,"status":"away","note":"crafting","tags":["maps","chat"]}]
```

`status` is `online`, `away`, `busy`, or `offline` (an invisible friend looks
offline). `note` is empty and `tags` is an empty array for an offline friend.
`tags` contains only activities the friend chose or typed, with at most eight
strings of 32 UTF-8 bytes. Older native libraries/friends supply no tags.
Or `error: …`.

## SendToFriend

`who` is a friend's exact name (any case) or a unique prefix of their id.
`text` is UTF-8, cut to 2048 bytes. Returns `ok` or `error: …`
(`error: the player has not allowed other plugins to send on Lantern`,
`error: Lantern is off`, `error: no friend called "x"`, …). The message is
queued like one the player typed: it goes out now, or when the friend is next
online, and shows in the player's window as theirs.

## OpenChat

Opens the Lantern window, on that friend's chat when `who` names one, else
as it was. Harmless, so always allowed.

## MessageReceived

Sent for each new friend text, author reply and verified author announcement,
while reading is allowed:

```json
{"kind":"friend","from":"<64 hex, or empty>","name":"Bob","text":"shall we queue?"}
```

`kind` is `friend`, `author_reply` or `announcement`. Channel lines are not
sent: a channel is shared with people who are not the player's friends.

## Calling them

```csharp
var friends = pi.GetIpcSubscriber<string>("XivLantern.v1.GetFriends").InvokeFunc();
var result  = pi.GetIpcSubscriber<string, string, string>("XivLantern.v1.SendToFriend").InvokeFunc("Bob", "hi");
pi.GetIpcSubscriber<string, object>("XivLantern.v1.MessageReceived").Subscribe(json => { /* ... */ });
```

A caller must expect `IpcNotReadyError` when XivLantern is not installed or
not loaded, and must treat every string as untrusted text from another player.

### Sealed-channel conversation identities (2026-10-01)

Current IPC status/friend/message payloads do not export channel tickets or
channel-line events. Internal sealed conversation keys now use
`c:sealed:<SHA256>` of the canonical ticket rather than `c:<sealed-ticket>`.
The digest is a routing/mute identifier, not a ticket and cannot be used to
join or decrypt. Ticket-bearing fields remain in the private saved-channel
configuration and explicit copy/invite operations. Plain channel and friend
IDs retain their existing format. Config loading migrates earlier sealed mute
IDs and preserves their notification/unread policy. This does not encrypt
the local saved configuration or chat database.
