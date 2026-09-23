/* linkpearl.h -- stable C ABI for the Linkpearl peer-to-peer layer.
 *
 * Linkpearl wraps iroh (QUIC peer-to-peer with NAT hole punching and relay
 * fallback), iroh-gossip (rooms and group channels) and iroh-blobs
 * (content-addressed file transfer), plus a friend list with presence and 1:1
 * text, behind a small C ABI that a game plugin can drive from its frame
 * thread. docs/DESIGN.md says what is built and what is only designed.
 *
 * THREADING CONTRACT
 *   - Every function except lp_node_open() and lp_node_close() is non-blocking:
 *     it either touches an in-memory snapshot or pushes a command onto a queue
 *     and returns. None of them perform DNS or network work inline. The
 *     exceptions for disk are lp_bookmark_* and lp_friend_history(), which do
 *     one short indexed SQLite statement on a WAL database.
 *   - lp_node_open() and lp_node_close() DO block (they start/stop a tokio
 *     runtime and open SQLite). Call them from a worker thread, never from the
 *     render thread.
 *   - All functions are safe to call from any thread, but a single lp_node
 *     should be polled from one thread at a time. lp_poll() is the only
 *     function that mutates the event arena.
 *
 * STRING AND BUFFER CONTRACT
 *   - All `const char*` inputs are NUL-terminated UTF-8.
 *   - Nothing allocated by Linkpearl is ever freed by the caller. Functions
 *     that return text copy into a caller-owned buffer and write the number of
 *     bytes required (excluding the NUL) to *out_len. If the buffer is too
 *     small they write nothing, set *out_len to the required size and return
 *     LP_E_BUFFER. LP_MAX_TEXT fits every ticket, id and error string; the
 *     JSON lists (bookmarks, friends, history) can be longer, so retry with
 *     *out_len + 1 bytes on LP_E_BUFFER.
 *   - lp_event.data points into an arena owned by the node. It is valid until
 *     the next lp_poll() call on that node. Copy what you need.
 *
 * ABI STABILITY
 *   - lp_abi_version() returns LP_ABI_VERSION of the loaded library. The major
 *     number changes only on a breaking change; bindings must refuse to load a
 *     library whose major differs from the one they were generated against.
 *   - Struct layouts are frozen per major version. New fields are added only by
 *     bumping the major.
 */

#ifndef LINKPEARL_H
#define LINKPEARL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(_WIN32)
#  define LP_API __declspec(dllimport)
#else
#  define LP_API
#endif

#define LP_ABI_VERSION 2   /* 2: lp_stats grew rate_limited; friends, author, selftest */

/* Sizes the caller can rely on. */
#define LP_NODE_ID_LEN 32   /* raw ed25519 public key bytes */
#define LP_MAX_TEXT 4096    /* tickets, node ids, error strings all fit; JSON lists may not */

/* ---------------------------------------------------------------- status --- */

typedef int32_t lp_status;

#define LP_OK              0
#define LP_E_ARG          -1  /* NULL pointer, bad UTF-8, bad length */
#define LP_E_STATE        -2  /* node closed, or handle already released */
#define LP_E_BUFFER       -3  /* output buffer too small; see *out_len */
#define LP_E_NOT_FOUND    -4  /* unknown room/conn/blob handle */
#define LP_E_FULL         -5  /* command or event queue is full, retry later */
#define LP_E_TICKET       -6  /* ticket did not parse */
#define LP_E_STORE        -7  /* SQLite or blob store failure */
#define LP_E_NET          -8  /* endpoint bind / transport failure */
#define LP_E_TOO_LARGE    -9  /* payload exceeds LP_MAX_MESSAGE */
#define LP_E_ABI         -10  /* caller/library ABI mismatch */
#define LP_E_INTERNAL    -99

/* Largest single direct-connection message. Bigger payloads belong in a blob. */
#define LP_MAX_MESSAGE 61440 /* 60 KiB */
/* Largest room/channel message or presence payload. */
#define LP_MAX_ROOM_MESSAGE 6144
/* Largest 1:1 text or support message, in UTF-8 bytes. */
#define LP_MAX_CHAT 2048

/* ---------------------------------------------------------------- config --- */

#define LP_RELAY_DEFAULT  0  /* n0 public relays + hole punching (recommended) */
#define LP_RELAY_DISABLED 1  /* direct/LAN only; no third party ever sees you */
#define LP_RELAY_CUSTOM   2  /* use lp_config.relay_url */

typedef struct lp_config {
    uint32_t    abi_version;     /* must be LP_ABI_VERSION */
    const char *db_path;         /* SQLite file for identity + bookmarks. NULL => ":memory:" */
    const char *blob_dir;        /* iroh-blobs store directory. NULL => <db_path>.blobs */
    const char *app_id;          /* namespace for room derivation + ALPN, e.g. "ffxiv-linkpearl/1" */
    uint32_t    relay_mode;      /* LP_RELAY_* */
    const char *relay_url;       /* used when relay_mode == LP_RELAY_CUSTOM */
    uint32_t    event_queue_cap; /* 0 => 4096 */
    uint32_t    accept_inbound;  /* 0 => do not accept inbound connections at all */
} lp_config;

/* ----------------------------------------------------------------- types --- */

typedef struct lp_node lp_node;

/* Handles are opaque non-zero integers. 0 is never a valid handle. */
typedef uint64_t lp_room;
typedef uint64_t lp_conn;
typedef uint64_t lp_blob;

/* Room scope. Purely advisory metadata: it is mixed into the topic derivation
 * and reported to the UI so a player can see what they are connected to. */
#define LP_SCOPE_PARTY  0
#define LP_SCOPE_FC     1
#define LP_SCOPE_ZONE   2
#define LP_SCOPE_WORLD  3
#define LP_SCOPE_PUBLIC 4
#define LP_SCOPE_CUSTOM 5

/* ---------------------------------------------------------------- events --- */

#define LP_EV_READY           1  /* endpoint bound. data = node id (lowercase hex) */
#define LP_EV_ROOM_JOINED     2  /* room */
#define LP_EV_ROOM_LEFT       3  /* room */
#define LP_EV_PEER_JOINED     4  /* room, peer */
#define LP_EV_PEER_LEFT       5  /* room, peer */
#define LP_EV_PRESENCE        6  /* room, peer, data = opaque presence blob */
#define LP_EV_MESSAGE         7  /* room, peer, data = application bytes */
#define LP_EV_DIRECT          8  /* conn, peer, data = application bytes */
#define LP_EV_CONNECTED       9  /* conn, peer */
#define LP_EV_DISCONNECTED   10  /* conn, peer */
#define LP_EV_BLOB_OFFER     11  /* room, peer, blob, data = offer JSON */
#define LP_EV_BLOB_PROGRESS  12  /* blob, data = two little-endian u64: done, total */
#define LP_EV_BLOB_DONE      13  /* blob, data = local path (fetch) or hash (add) */
#define LP_EV_BLOB_FAILED    14  /* blob, data = UTF-8 reason */
#define LP_EV_TICKET         15  /* room or blob, data = ticket text */
#define LP_EV_PUBLISHING     16  /* room, data = 1 byte: 1 publishing, 0 stopped */
#define LP_EV_ERROR          17  /* data = UTF-8 message */
#define LP_EV_LOG            18  /* data = UTF-8 message */
/* Friends. `peer` is always the friend device's node id. */
#define LP_EV_FRIEND_ADDED     19 /* handle = lp_invite_accept handle, or 0 when they
                                     redeemed our invite; data = display name */
#define LP_EV_FRIEND_REMOVED   20 /* unfriended, by us or by them */
#define LP_EV_INVITE_FAILED    21 /* handle = lp_invite_accept handle; data = reason */
#define LP_EV_FRIEND_ONLINE    22 /* came online or changed status: handle = LP_PRESENCE_*,
                                     data = note (UTF-8, typed by the friend) */
#define LP_EV_FRIEND_OFFLINE   23 /* link closed, went invisible, or 3 heartbeats missed */
#define LP_EV_FRIEND_TEXT      24 /* handle = message id; data = UTF-8 text. Raised once
                                     per message even when the sender retried */
#define LP_EV_FRIEND_DELIVERED 25 /* handle = id from lp_friend_send: they have it */
#define LP_EV_CHANNEL_INVITE   26 /* data = room ticket; join with lp_room_join */
#define LP_EV_RATE_LIMITED     27 /* peer's frames are being dropped; data = "friend",
                                     "room" or "stranger". At most every 30 s per peer */
#define LP_EV_ANNOUNCEMENT     28 /* verified author announcement: handle = room, peer =
                                     author key, data = {"seq","issued_at","title","body"} */
#define LP_EV_SUPPORT_MESSAGE  29 /* author mode only: handle = id, data = text */
#define LP_EV_SUPPORT_REPLY    30 /* a support contact answered: handle = id, data = text */
#define LP_EV_SUPPORT_DELIVERED 31 /* handle = id from lp_support_send */
#define LP_EV_SUPPORT_FAILED   32 /* handle = id; data = reason. Not retried */
#define LP_EV_SELFTEST         33 /* handle = lp_selftest handle; data = JSON report */

typedef struct lp_event {
    uint32_t       kind;                    /* LP_EV_* */
    uint32_t       data_len;
    uint64_t       seq;                     /* monotonic per node */
    uint64_t       handle;                  /* room, conn or blob depending on kind */
    uint8_t        peer[LP_NODE_ID_LEN];    /* zeroed when not applicable */
    const uint8_t *data;                    /* valid until the next lp_poll() */
} lp_event;

/* ----------------------------------------------------------------- stats --- */

typedef struct lp_stats {
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
} lp_stats;

/* ------------------------------------------------------------- lifecycle --- */

LP_API uint32_t    lp_abi_version(void);
LP_API const char *lp_build_info(void); /* static, NUL-terminated: crate + iroh versions */

/* Blocks. Call off the render thread. */
LP_API lp_status lp_node_open(const lp_config *cfg, lp_node **out_node);

/* Blocks (bounded shutdown). Call off the render thread. Idempotent. */
LP_API void lp_node_close(lp_node *node);

/* Node identity, persisted in SQLite across runs. */
LP_API lp_status lp_node_id(lp_node *node, uint8_t out_id[LP_NODE_ID_LEN]);
LP_API lp_status lp_node_id_text(lp_node *node, char *buf, size_t buf_len, size_t *out_len);

/* A ticket another player can paste to reach this node directly. */
LP_API lp_status lp_node_ticket(lp_node *node, char *buf, size_t buf_len, size_t *out_len);

/* -------------------------------------------------------------- polling --- */

/* Drains up to max_events into out_events. Returns the number written, or a
 * negative lp_status. Never blocks; returns 0 when idle. Invalidates the data
 * pointers handed out by the previous call. */
LP_API int32_t lp_poll(lp_node *node, lp_event *out_events, uint32_t max_events);

/* Cheap synchronous snapshot for per-frame UI. */
LP_API lp_status lp_stats_get(lp_node *node, lp_stats *out_stats);

/* 1 when this node is currently publishing (gossip or blob) into `room`. */
LP_API int32_t lp_is_publishing(lp_node *node, lp_room room);

/* ---------------------------------------------------------------- rooms --- */

/* Join a room.
 *   scope  - LP_SCOPE_*
 *   key    - scope-local identifier, e.g. a party hash or an FC id. The topic is
 *            blake3(app_id || 0x1f || scope || 0x1f || key), so two clients that
 *            know the same key meet without any server.
 *   ticket - optional gossip ticket (from lp_room_ticket or the public
 *            directory). When non-NULL its topic wins and `key` is only used as
 *            a display label; the ticket's bootstrap peers are dialed.
 * Returns immediately with a handle; LP_EV_ROOM_JOINED arrives later. */
LP_API lp_status lp_room_join(lp_node *node, uint32_t scope, const char *key,
                              const char *ticket, lp_room *out_room);

LP_API lp_status lp_room_leave(lp_node *node, lp_room room);

/* Ticket for this room, for sharing out of band or publishing to a directory.
 * LP_E_NOT_FOUND for a room this node is not in. */
LP_API lp_status lp_room_ticket(lp_node *node, lp_room room, char *buf,
                                size_t buf_len, size_t *out_len);

/* Broadcast to the room. Fire and forget; ordering is not guaranteed. */
LP_API lp_status lp_room_send(lp_node *node, lp_room room,
                              const uint8_t *data, uint32_t len);

/* Replace this node's presence payload for the room. Re-sent to new peers. */
LP_API lp_status lp_presence_set(lp_node *node, lp_room room,
                                 const uint8_t *data, uint32_t len);

/* Snapshot of known peers. Writes min(max_peers, count) ids; always writes the
 * true count to *out_count. */
LP_API lp_status lp_room_peers(lp_node *node, lp_room room,
                               uint8_t *out_peers, uint32_t max_peers,
                               uint32_t *out_count);

/* --------------------------------------------------------------- direct --- */

/* Dial a node ticket or a bare node id. LP_EV_CONNECTED/LP_EV_DISCONNECTED. */
LP_API lp_status lp_connect(lp_node *node, const char *ticket_or_node_id,
                            lp_conn *out_conn);
LP_API lp_status lp_send(lp_node *node, lp_conn conn,
                         const uint8_t *data, uint32_t len);
LP_API lp_status lp_disconnect(lp_node *node, lp_conn conn);

/* ---------------------------------------------------------------- blobs --- */

/* Hash a local file into the blob store. Non-blocking; LP_EV_BLOB_DONE carries
 * the hash when hashing finishes. */
LP_API lp_status lp_blob_add_file(lp_node *node, const char *path,
                                  const char *name, lp_blob *out_blob);

/* Same for an in-memory buffer (a screenshot already in RAM). The bytes are
 * copied before returning. */
LP_API lp_status lp_blob_add_bytes(lp_node *node, const uint8_t *data,
                                   uint32_t len, const char *name,
                                   lp_blob *out_blob);

/* Announce a blob to a room. THIS is the act of sharing: nothing leaves the
 * machine until the player calls it, and it raises LP_EV_PUBLISHING. */
LP_API lp_status lp_blob_offer(lp_node *node, lp_room room, lp_blob blob);

/* Stop serving a previously offered blob. */
LP_API lp_status lp_blob_unoffer(lp_node *node, lp_room room, lp_blob blob);

/* Accept an offer: download to out_path. Progress arrives as
 * LP_EV_BLOB_PROGRESS, completion as LP_EV_BLOB_DONE. */
LP_API lp_status lp_blob_fetch(lp_node *node, lp_blob blob, const char *out_path);

/* Hash of an added or offered blob, as lowercase hex. */
LP_API lp_status lp_blob_hash_text(lp_node *node, lp_blob blob, char *buf,
                                   size_t buf_len, size_t *out_len);

/* Not implemented yet, and therefore not declared: lp_room_send_to,
 * lp_accept_inbound (runtime toggle), lp_blob_fetch_ticket, lp_blob_cancel,
 * lp_blob_ticket. The ffi crate's header test fails if a declaration here has
 * no export, or an export has no declaration. */

/* -------------------------------------------------------------- friends --- */

/* A friend is a device (node id) that redeemed our invite or whose invite we
 * redeemed. Links, presence and texts run over the friend ALPN. */

#define LP_PRESENCE_INVISIBLE 0  /* no heartbeat; friends see you offline */
#define LP_PRESENCE_ONLINE    1
#define LP_PRESENCE_AWAY      2
#define LP_PRESENCE_BUSY      3

/* The name friends see. Persisted. */
LP_API lp_status lp_profile_set_name(lp_node *node, const char *name);

/* What friends see. `note` is free text the player typed (NULL for none);
 * never fill it from game state without the player asking. Persisted. */
LP_API lp_status lp_presence_status_set(lp_node *node, uint32_t status,
                                        const char *note);

/* Mint a single-use friend invite ("lpfriend..."). ttl_secs 0 never expires.
 * Buffer contract as above; nothing is minted when the buffer is too small. */
LP_API lp_status lp_invite_create(lp_node *node, uint32_t ttl_secs, char *buf,
                                  size_t buf_len, size_t *out_len);

/* Redeem somebody's invite. Returns at once with a handle; LP_EV_FRIEND_ADDED
 * or LP_EV_INVITE_FAILED carries it later. */
LP_API lp_status lp_invite_accept(lp_node *node, const char *ticket,
                                  uint64_t *out_handle);

/* JSON array: [{"id":"<hex>","friend":1,"name":"..","online":true,
 * "status":"online|away|busy|invisible","note":"..","last_seen":<ms>|null,
 * "linked":true}] */
LP_API lp_status lp_friend_list(lp_node *node, char *buf, size_t buf_len,
                                size_t *out_len);

/* 1:1 text (UTF-8, at most LP_MAX_MESSAGE bytes) to a friend device. Queued in
 * SQLite and sent now or when a link next comes up. *out_msg (may be NULL)
 * receives the id that LP_EV_FRIEND_DELIVERED will carry. */
LP_API lp_status lp_friend_send(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN],
                                const uint8_t *text, uint32_t len,
                                uint64_t *out_msg);

/* Unfriend. The friend is told and forgets this node too. */
LP_API lp_status lp_friend_remove(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN]);

/* Last `limit` texts with a device, oldest first, as JSON:
 * [{"id":..,"outgoing":true,"text":"..","sent_at":<ms>,"delivered_at":<ms>|null}]
 * Reads SQLite. */
LP_API lp_status lp_friend_history(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN],
                                   uint32_t limit, char *buf, size_t buf_len,
                                   size_t *out_len);

/* ------------------------------------------------------------- channels --- */

/* A group channel is a gossip room with a random key, so it cannot be found
 * without its ticket. Use the room calls on the returned handle. */
LP_API lp_status lp_channel_create(lp_node *node, const char *label, lp_room *out_room);

/* Send a linked friend this room's ticket; they get LP_EV_CHANNEL_INVITE. */
LP_API lp_status lp_channel_invite(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN],
                                   lp_room room);

/* --------------------------------------------------------------- safety --- */

/* Block a node: dropped from friends without telling them, link closed, and
 * from then on their connections, room messages and invites are refused.
 * Persisted. */
LP_API lp_status lp_block(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN]);
LP_API lp_status lp_unblock(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN]);
/* JSON array of hex node ids. */
LP_API lp_status lp_blocked_list(lp_node *node, char *buf, size_t buf_len, size_t *out_len);

/* Remember where a node can be reached (an lpnode... ticket), so dialing it by
 * id works with relays off. */
LP_API lp_status lp_address_hint(lp_node *node, const char *node_ticket);

/* ------------------------------------------------------- author channel --- */

/* Join the author channel of `author` (an ed25519 public key; the private key
 * never ships). `bootstrap` holds `count` node ids of the author's always-on
 * nodes. Only announcements the author key signed come out of it, as
 * LP_EV_ANNOUNCEMENT; lp_room_send refuses it. Joining twice returns the same
 * room. */
LP_API lp_status lp_author_join(lp_node *node, const uint8_t author[LP_NODE_ID_LEN],
                                const uint8_t *bootstrap, uint32_t count,
                                lp_room *out_room);

/* Kept announcements, newest first, as JSON
 * [{"seq":..,"issued_at":<unix s>,"title":"..","body":".."}]. Reads SQLite. */
LP_API lp_status lp_announcements(lp_node *node, const uint8_t author[LP_NODE_ID_LEN],
                                  uint32_t limit, char *buf, size_t buf_len,
                                  size_t *out_len);

/* The nodes allowed to answer support messages (the author's). Replaces the
 * previous set; at most 64. */
LP_API lp_status lp_support_contacts_set(lp_node *node, const uint8_t *peers, uint32_t count);

/* Write to a support contact (UTF-8, at most LP_MAX_CHAT). Not queued:
 * LP_EV_SUPPORT_DELIVERED or LP_EV_SUPPORT_FAILED follows. */
LP_API lp_status lp_support_send(lp_node *node, const uint8_t peer[LP_NODE_ID_LEN],
                                 const uint8_t *text, uint32_t len, uint64_t *out_msg);

/* ------------------------------------------------------------- selftest --- */

/* Is this node reachable, in this process, now? Dials itself directly with a
 * throwaway endpoint, and through its home relay unless relays are off.
 * LP_EV_SELFTEST carries a JSON report:
 * {"node_id","platform","bound":[..],"relay_url",
 *  "direct":{"ok","ms","detail"},"relay":{"ok","ms","detail"}} */
LP_API lp_status lp_selftest(lp_node *node, uint64_t *out_handle);

/* ----------------------------------------------------------- bookmarks ---- */

/* Rooms the player chose to remember, stored in SQLite. Bookmarks are inert
 * data: they are never auto-joined by the library. */
LP_API lp_status lp_bookmark_put(lp_node *node, const char *id, uint32_t scope,
                                 const char *title, const char *ticket);
LP_API lp_status lp_bookmark_forget(lp_node *node, const char *id);
/* Writes a JSON array of bookmarks. */
LP_API lp_status lp_bookmark_list(lp_node *node, char *buf, size_t buf_len,
                                  size_t *out_len);

/* ------------------------------------------------------------ utilities --- */

/* Lowercase hex helpers so bindings never hand-roll node id formatting. */
LP_API lp_status lp_id_to_text(const uint8_t id[LP_NODE_ID_LEN], char *buf,
                               size_t buf_len, size_t *out_len);
LP_API lp_status lp_id_from_text(const char *text, uint8_t out_id[LP_NODE_ID_LEN]);

/* Human-readable name for a status code. Static string, never NULL. */
LP_API const char *lp_status_text(lp_status status);

#ifdef __cplusplus
}
#endif

#endif /* LINKPEARL_H */
