/* lantern.h -- stable C ABI for the Lantern peer-to-peer layer.
 *
 * Lantern wraps iroh (QUIC peer-to-peer with NAT hole punching and relay
 * fallback), iroh-gossip (rooms and group channels) and iroh-blobs
 * (content-addressed file transfer), plus a friend list with presence and 1:1
 * text, behind a small C ABI that a game plugin can drive from its frame
 * thread. docs/DESIGN.md says what is built and what is only designed.
 *
 * THREADING CONTRACT
 *   - Every function except lt_node_open() and lt_node_close() is non-blocking:
 *     it either touches an in-memory snapshot or pushes a command onto a queue
 *     and returns. None of them perform DNS or network work inline. The
 *     exceptions for disk are lt_bookmark_* and lt_friend_history(), which do
 *     one short indexed SQLite statement on a WAL database.
 *   - lt_node_open() and lt_node_close() DO block (they start/stop a tokio
 *     runtime and open SQLite). Call them from a worker thread, never from the
 *     render thread.
 *   - All functions are safe to call from any thread, but a single lt_node
 *     should be polled from one thread at a time. lt_poll() is the only
 *     function that mutates the event arena.
 *
 * STRING AND BUFFER CONTRACT
 *   - All `const char*` inputs are NUL-terminated UTF-8.
 *   - Nothing allocated by Lantern is ever freed by the caller. Functions
 *     that return text copy into a caller-owned buffer and write the number of
 *     bytes required (excluding the NUL) to *out_len. If the buffer is too
 *     small they write nothing, set *out_len to the required size and return
 *     LT_E_BUFFER. LT_MAX_TEXT fits every ticket, id and error string; the
 *     JSON lists (bookmarks, friends, history) can be longer, so retry with
 *     *out_len + 1 bytes on LT_E_BUFFER.
 *   - lt_event.data points into an arena owned by the node. It is valid until
 *     the next lt_poll() call on that node. Copy what you need.
 *
 * ABI STABILITY
 *   - lt_abi_version() returns LT_ABI_VERSION of the loaded library. The major
 *     number changes only on a breaking change; bindings must refuse to load a
 *     library whose major differs from the one they were generated against.
 *   - Struct layouts are frozen per major version. New fields are added only by
 *     bumping the major.
 */

#ifndef LANTERN_H
#define LANTERN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32)
#  define LT_API __declspec(dllimport)
#else
#  define LT_API
#endif

#define LT_ABI_VERSION 2   /* 2: lt_stats grew rate_limited; friends, author, selftest */

/* Sizes the caller can rely on. */
#define LT_NODE_ID_LEN 32   /* raw ed25519 public key bytes */
#define LT_MAX_TEXT 4096    /* tickets, node ids, error strings all fit; JSON lists may not */

/* ---------------------------------------------------------------- status --- */

typedef int32_t lt_status;

#define LT_OK              0
#define LT_E_ARG          -1  /* NULL pointer, bad UTF-8, bad length */
#define LT_E_STATE        -2  /* node closed, or handle already released */
#define LT_E_BUFFER       -3  /* output buffer too small; see *out_len */
#define LT_E_NOT_FOUND    -4  /* unknown room/conn/blob handle */
#define LT_E_FULL         -5  /* command or event queue is full, retry later */
#define LT_E_TICKET       -6  /* ticket did not parse */
#define LT_E_STORE        -7  /* SQLite or blob store failure */
#define LT_E_NET          -8  /* endpoint bind / transport failure */
#define LT_E_TOO_LARGE    -9  /* payload exceeds LT_MAX_MESSAGE */
#define LT_E_ABI         -10  /* caller/library ABI mismatch */
#define LT_E_INTERNAL    -99

/* Largest single direct-connection message. Bigger payloads belong in a blob. */
#define LT_MAX_MESSAGE 61440 /* 60 KiB */
/* Largest room/channel message or presence payload. */
#define LT_MAX_ROOM_MESSAGE 6144
/* Largest 1:1 text or support message, in UTF-8 bytes. */
#define LT_MAX_CHAT 2048

/* ---------------------------------------------------------------- config --- */

#define LT_RELAY_DEFAULT  0  /* n0 public relays + hole punching (recommended) */
#define LT_RELAY_DISABLED 1  /* direct/LAN only; no third party ever sees you */
#define LT_RELAY_CUSTOM   2  /* use lt_config.relay_url */

typedef struct lt_config {
    uint32_t    abi_version;     /* must be LT_ABI_VERSION */
    const char *db_path;         /* SQLite file for identity + bookmarks. NULL => ":memory:" */
    const char *blob_dir;        /* iroh-blobs store directory. NULL => <db_path>.blobs */
    const char *app_id;          /* namespace for room derivation + ALPN, e.g. "xivlantern/1" */
    uint32_t    relay_mode;      /* LT_RELAY_* */
    const char *relay_url;       /* used when relay_mode == LT_RELAY_CUSTOM */
    uint32_t    event_queue_cap; /* 0 => 4096 */
    uint32_t    accept_inbound;  /* 0 => do not accept inbound connections at all */
} lt_config;

/* ----------------------------------------------------------------- types --- */

typedef struct lt_node lt_node;

/* Handles are opaque non-zero integers. 0 is never a valid handle. */
typedef uint64_t lt_room;
typedef uint64_t lt_conn;
typedef uint64_t lt_blob;

/* Room scope. Purely advisory metadata: it is mixed into the topic derivation
 * and reported to the UI so a player can see what they are connected to. */
#define LT_SCOPE_PARTY  0
#define LT_SCOPE_FC     1
#define LT_SCOPE_ZONE   2
#define LT_SCOPE_WORLD  3
#define LT_SCOPE_PUBLIC 4
#define LT_SCOPE_CUSTOM 5

/* ---------------------------------------------------------------- events --- */

#define LT_EV_READY           1  /* endpoint bound. data = node id (lowercase hex) */
#define LT_EV_ROOM_JOINED     2  /* room */
#define LT_EV_ROOM_LEFT       3  /* room */
#define LT_EV_PEER_JOINED     4  /* room, peer */
#define LT_EV_PEER_LEFT       5  /* room, peer */
#define LT_EV_PRESENCE        6  /* room, peer, data = opaque presence blob */
#define LT_EV_MESSAGE         7  /* room, peer, data = application bytes */
#define LT_EV_DIRECT          8  /* conn, peer, data = application bytes */
#define LT_EV_CONNECTED       9  /* conn, peer */
#define LT_EV_DISCONNECTED   10  /* conn, peer */
#define LT_EV_BLOB_OFFER     11  /* room, peer, blob, data = offer JSON */
#define LT_EV_BLOB_PROGRESS  12  /* blob, data = two little-endian u64: done, total */
#define LT_EV_BLOB_DONE      13  /* blob, data = local path (fetch) or hash (add) */
#define LT_EV_BLOB_FAILED    14  /* blob, data = UTF-8 reason */
#define LT_EV_TICKET         15  /* room or blob, data = ticket text */
#define LT_EV_PUBLISHING     16  /* room, data = 1 byte: 1 publishing, 0 stopped */
#define LT_EV_ERROR          17  /* data = UTF-8 message */
#define LT_EV_LOG            18  /* data = UTF-8 message */
/* Friends. `peer` is always the friend device's node id. */
#define LT_EV_FRIEND_ADDED     19 /* handle = lt_invite_accept handle, or 0 when they
                                     redeemed our invite; data = display name */
#define LT_EV_FRIEND_REMOVED   20 /* unfriended, by us or by them */
#define LT_EV_INVITE_FAILED    21 /* handle = lt_invite_accept handle; data = reason */
#define LT_EV_FRIEND_ONLINE    22 /* came online or changed status: handle = LT_PRESENCE_*,
                                     data = note (UTF-8, typed by the friend) */
#define LT_EV_FRIEND_OFFLINE   23 /* link closed, went invisible, or 3 heartbeats missed */
#define LT_EV_FRIEND_TEXT      24 /* handle = message id; data = UTF-8 text. Raised once
                                     per message even when the sender retried */
#define LT_EV_FRIEND_DELIVERED 25 /* handle = id from lt_friend_send: they have it */
#define LT_EV_CHANNEL_INVITE   26 /* data = room ticket; join with lt_room_join */
#define LT_EV_RATE_LIMITED     27 /* peer's frames are being dropped; data = "friend",
                                     "room" or "stranger". At most every 30 s per peer */
#define LT_EV_ANNOUNCEMENT     28 /* verified author announcement: handle = room, peer =
                                     author key, data = {"seq","issued_at","title","body"} */
#define LT_EV_SUPPORT_MESSAGE  29 /* author mode only: handle = id, data = text */
#define LT_EV_SUPPORT_REPLY    30 /* a support contact answered: handle = id, data = text */
#define LT_EV_SUPPORT_DELIVERED 31 /* handle = id from lt_support_send */
#define LT_EV_SUPPORT_FAILED   32 /* handle = id; data = reason. Not retried */
#define LT_EV_SELFTEST         33 /* handle = lt_selftest handle; data = JSON report */

typedef struct lt_event {
    uint32_t       kind;                    /* LT_EV_* */
    uint32_t       data_len;
    uint64_t       seq;                     /* monotonic per node */
    uint64_t       handle;                  /* room, conn or blob depending on kind */
    uint8_t        peer[LT_NODE_ID_LEN];    /* zeroed when not applicable */
    const uint8_t *data;                    /* valid until the next lt_poll() */
} lt_event;

/* ----------------------------------------------------------------- stats --- */

typedef struct lt_stats {
    uint64_t bytes_sent;
    uint64_t bytes_recv;
    uint32_t rooms;
    uint32_t peers;            /* distinct peers across all rooms */
    uint32_t direct_conns;
    uint32_t relayed_conns;    /* connections currently going through a relay */
    uint32_t publishing_rooms; /* rooms this node is actively publishing into */
    uint32_t events_dropped;   /* event queue overflow count since open */
    uint32_t rate_limited;     /* frames dropped by per-peer rate limits since open */
    uint32_t reserved;
} lt_stats;

/* ------------------------------------------------------------- lifecycle --- */

LT_API uint32_t    lt_abi_version(void);
LT_API const char *lt_build_info(void); /* static, NUL-terminated: crate + iroh versions */

/* Blocks. Call off the render thread. */
LT_API lt_status lt_node_open(const lt_config *cfg, lt_node **out_node);

/* Blocks (bounded shutdown). Call off the render thread. Idempotent. */
LT_API void lt_node_close(lt_node *node);

/* Node identity, persisted in SQLite across runs. */
LT_API lt_status lt_node_id(lt_node *node, uint8_t out_id[LT_NODE_ID_LEN]);
LT_API lt_status lt_node_id_text(lt_node *node, char *buf, size_t buf_len, size_t *out_len);

/* A ticket another player can paste to reach this node directly. */
LT_API lt_status lt_node_ticket(lt_node *node, char *buf, size_t buf_len, size_t *out_len);

/* -------------------------------------------------------------- polling --- */

/* Drains up to max_events into out_events. Returns the number written, or a
 * negative lt_status. Never blocks; returns 0 when idle. Invalidates the data
 * pointers handed out by the previous call. */
LT_API int32_t lt_poll(lt_node *node, lt_event *out_events, uint32_t max_events);

/* Cheap synchronous snapshot for per-frame UI. */
LT_API lt_status lt_stats_get(lt_node *node, lt_stats *out_stats);

/* 1 when this node is currently publishing (gossip or blob) into `room`. */
LT_API int32_t lt_is_publishing(lt_node *node, lt_room room);

/* ---------------------------------------------------------------- rooms --- */

/* Join a room.
 *   scope  - LT_SCOPE_*
 *   key    - scope-local identifier, e.g. a party hash or an FC id. The topic is
 *            blake3(app_id || 0x1f || scope || 0x1f || key), so two clients that
 *            know the same key meet without any server.
 *   ticket - optional gossip ticket (from lt_room_ticket or the public
 *            directory). When non-NULL its topic wins and `key` is only used as
 *            a display label; the ticket's bootstrap peers are dialed.
 * Returns immediately with a handle; LT_EV_ROOM_JOINED arrives later. */
LT_API lt_status lt_room_join(lt_node *node, uint32_t scope, const char *key,
                              const char *ticket, lt_room *out_room);

LT_API lt_status lt_room_leave(lt_node *node, lt_room room);

/* Ticket for this room, for sharing out of band or publishing to a directory.
 * LT_E_NOT_FOUND for a room this node is not in. */
LT_API lt_status lt_room_ticket(lt_node *node, lt_room room, char *buf,
                                size_t buf_len, size_t *out_len);

/* Broadcast to the room. Fire and forget; ordering is not guaranteed. */
LT_API lt_status lt_room_send(lt_node *node, lt_room room,
                              const uint8_t *data, uint32_t len);

/* Replace this node's presence payload for the room. Re-sent to new peers. */
LT_API lt_status lt_presence_set(lt_node *node, lt_room room,
                                 const uint8_t *data, uint32_t len);

/* Snapshot of known peers. Writes min(max_peers, count) ids; always writes the
 * true count to *out_count. */
LT_API lt_status lt_room_peers(lt_node *node, lt_room room,
                               uint8_t *out_peers, uint32_t max_peers,
                               uint32_t *out_count);

/* --------------------------------------------------------------- direct --- */

/* Dial a node ticket or a bare node id. LT_EV_CONNECTED/LT_EV_DISCONNECTED. */
LT_API lt_status lt_connect(lt_node *node, const char *ticket_or_node_id,
                            lt_conn *out_conn);
LT_API lt_status lt_send(lt_node *node, lt_conn conn,
                         const uint8_t *data, uint32_t len);
LT_API lt_status lt_disconnect(lt_node *node, lt_conn conn);

/* ---------------------------------------------------------------- blobs --- */

/* Hash a local file into the blob store. Non-blocking; LT_EV_BLOB_DONE carries
 * the hash when hashing finishes. */
LT_API lt_status lt_blob_add_file(lt_node *node, const char *path,
                                  const char *name, lt_blob *out_blob);

/* Same for an in-memory buffer (a screenshot already in RAM). The bytes are
 * copied before returning. */
LT_API lt_status lt_blob_add_bytes(lt_node *node, const uint8_t *data,
                                   uint32_t len, const char *name,
                                   lt_blob *out_blob);

/* Announce a blob to a room. THIS is the act of sharing: nothing leaves the
 * machine until the player calls it, and it raises LT_EV_PUBLISHING. */
LT_API lt_status lt_blob_offer(lt_node *node, lt_room room, lt_blob blob);

/* Stop serving a previously offered blob. */
LT_API lt_status lt_blob_unoffer(lt_node *node, lt_room room, lt_blob blob);

/* Accept an offer: download to out_path. Progress arrives as
 * LT_EV_BLOB_PROGRESS, completion as LT_EV_BLOB_DONE. */
LT_API lt_status lt_blob_fetch(lt_node *node, lt_blob blob, const char *out_path);

/* Hash of an added or offered blob, as lowercase hex. */
LT_API lt_status lt_blob_hash_text(lt_node *node, lt_blob blob, char *buf,
                                   size_t buf_len, size_t *out_len);

/* Not implemented yet, and therefore not declared: lt_room_send_to,
 * lt_accept_inbound (runtime toggle), lt_blob_fetch_ticket, lt_blob_cancel,
 * lt_blob_ticket. The ffi crate's header test fails if a declaration here has
 * no export, or an export has no declaration. */

/* -------------------------------------------------------------- friends --- */

/* A friend is a device (node id) that redeemed our invite or whose invite we
 * redeemed. Links, presence and texts run over the friend ALPN. */

#define LT_PRESENCE_INVISIBLE 0  /* no heartbeat; friends see you offline */
#define LT_PRESENCE_ONLINE    1
#define LT_PRESENCE_AWAY      2
#define LT_PRESENCE_BUSY      3

/* The name friends see. Persisted. */
LT_API lt_status lt_profile_set_name(lt_node *node, const char *name);

/* What friends see. `note` is free text the player typed (NULL for none);
 * never fill it from game state without the player asking. Persisted. */
LT_API lt_status lt_presence_status_set(lt_node *node, uint32_t status,
                                        const char *note);

/* Mint a single-use friend invite ("ltfriend..."). ttl_secs 0 never expires.
 * Buffer contract as above; nothing is minted when the buffer is too small. */
LT_API lt_status lt_invite_create(lt_node *node, uint32_t ttl_secs, char *buf,
                                  size_t buf_len, size_t *out_len);

/* Redeem somebody's invite. Returns at once with a handle; LT_EV_FRIEND_ADDED
 * or LT_EV_INVITE_FAILED carries it later. */
LT_API lt_status lt_invite_accept(lt_node *node, const char *ticket,
                                  uint64_t *out_handle);

/* JSON array: [{"id":"<hex>","friend":1,"name":"..","online":true,
 * "status":"online|away|busy|invisible","note":"..","last_seen":<ms>|null,
 * "linked":true}] */
LT_API lt_status lt_friend_list(lt_node *node, char *buf, size_t buf_len,
                                size_t *out_len);

/* 1:1 text (UTF-8, at most LT_MAX_MESSAGE bytes) to a friend device. Queued in
 * SQLite and sent now or when a link next comes up. *out_msg (may be NULL)
 * receives the id that LT_EV_FRIEND_DELIVERED will carry. */
LT_API lt_status lt_friend_send(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN],
                                const uint8_t *text, uint32_t len,
                                uint64_t *out_msg);

/* Unfriend. The friend is told and forgets this node too. */
LT_API lt_status lt_friend_remove(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN]);

/* Last `limit` texts with a device, oldest first, as JSON:
 * [{"id":..,"outgoing":true,"text":"..","sent_at":<ms>,"delivered_at":<ms>|null}]
 * Reads SQLite. */
LT_API lt_status lt_friend_history(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN],
                                   uint32_t limit, char *buf, size_t buf_len,
                                   size_t *out_len);

/* ------------------------------------------------------------- channels --- */

/* A group channel is a gossip room with a random key, so it cannot be found
 * without its ticket. Use the room calls on the returned handle. */
LT_API lt_status lt_channel_create(lt_node *node, const char *label, lt_room *out_room);

/* Send a linked friend this room's ticket; they get LT_EV_CHANNEL_INVITE. */
LT_API lt_status lt_channel_invite(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN],
                                   lt_room room);

/* --------------------------------------------------------------- safety --- */

/* Block a node: dropped from friends without telling them, link closed, and
 * from then on their connections, room messages and invites are refused.
 * Persisted. */
LT_API lt_status lt_block(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN]);
LT_API lt_status lt_unblock(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN]);
/* JSON array of hex node ids. */
LT_API lt_status lt_blocked_list(lt_node *node, char *buf, size_t buf_len, size_t *out_len);

/* Remember where a node can be reached (an ltnode... ticket), so dialing it by
 * id works with relays off. */
LT_API lt_status lt_address_hint(lt_node *node, const char *node_ticket);

/* ------------------------------------------------------- author channel --- */

/* Join the author channel of `author` (an ed25519 public key; the private key
 * never ships). `bootstrap` holds `count` node ids of the author's always-on
 * nodes. Only announcements the author key signed come out of it, as
 * LT_EV_ANNOUNCEMENT; lt_room_send refuses it. Joining twice returns the same
 * room. */
LT_API lt_status lt_author_join(lt_node *node, const uint8_t author[LT_NODE_ID_LEN],
                                const uint8_t *bootstrap, uint32_t count,
                                lt_room *out_room);

/* Kept announcements, newest first, as JSON
 * [{"seq":..,"issued_at":<unix s>,"title":"..","body":".."}]. Reads SQLite. */
LT_API lt_status lt_announcements(lt_node *node, const uint8_t author[LT_NODE_ID_LEN],
                                  uint32_t limit, char *buf, size_t buf_len,
                                  size_t *out_len);

/* The nodes allowed to answer support messages (the author's). Replaces the
 * previous set; at most 64. */
LT_API lt_status lt_support_contacts_set(lt_node *node, const uint8_t *peers, uint32_t count);

/* Write to a support contact (UTF-8, at most LT_MAX_CHAT). Not queued:
 * LT_EV_SUPPORT_DELIVERED or LT_EV_SUPPORT_FAILED follows. */
LT_API lt_status lt_support_send(lt_node *node, const uint8_t peer[LT_NODE_ID_LEN],
                                 const uint8_t *text, uint32_t len, uint64_t *out_msg);

/* ------------------------------------------------------------- selftest --- */

/* Is this node reachable, in this process, now? Dials itself directly with a
 * throwaway endpoint, and through its home relay unless relays are off.
 * LT_EV_SELFTEST carries a JSON report:
 * {"node_id","platform","bound":[..],"relay_url",
 *  "direct":{"ok","ms","detail"},"relay":{"ok","ms","detail"}} */
LT_API lt_status lt_selftest(lt_node *node, uint64_t *out_handle);

/* ----------------------------------------------------------- bookmarks ---- */

/* Rooms the player chose to remember, stored in SQLite. Bookmarks are inert
 * data: they are never auto-joined by the library. */
LT_API lt_status lt_bookmark_put(lt_node *node, const char *id, uint32_t scope,
                                 const char *title, const char *ticket);
LT_API lt_status lt_bookmark_forget(lt_node *node, const char *id);
/* Writes a JSON array of bookmarks. */
LT_API lt_status lt_bookmark_list(lt_node *node, char *buf, size_t buf_len,
                                  size_t *out_len);

/* ------------------------------------------------------------ utilities --- */

/* Lowercase hex helpers so bindings never hand-roll node id formatting. */
LT_API lt_status lt_id_to_text(const uint8_t id[LT_NODE_ID_LEN], char *buf,
                               size_t buf_len, size_t *out_len);
LT_API lt_status lt_id_from_text(const char *text, uint8_t out_id[LT_NODE_ID_LEN]);

/* Human-readable name for a status code. Static string, never NULL. */
LT_API const char *lt_status_text(lt_status status);

#ifdef __cplusplus
}
#endif

#endif /* LANTERN_H */
