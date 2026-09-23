/* linkpearl.h -- stable C ABI for the Linkpearl peer-to-peer layer.
 *
 * Linkpearl wraps iroh (QUIC peer-to-peer with NAT hole punching and relay
 * fallback), iroh-gossip (rooms) and iroh-blobs (content-addressed file
 * transfer) behind a small C ABI that a game plugin can drive from its frame
 * thread.
 *
 * THREADING CONTRACT
 *   - Every function except lp_node_open() and lp_node_close() is non-blocking:
 *     it either touches an atomic snapshot or pushes a command onto a queue and
 *     returns. None of them perform I/O, DNS, disk or network work inline.
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
 *     LP_E_BUFFER. LP_MAX_TEXT is large enough for every string this ABI emits.
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

#define LP_ABI_VERSION 1

/* Sizes the caller can rely on. */
#define LP_NODE_ID_LEN 32   /* raw ed25519 public key bytes */
#define LP_MAX_TEXT 4096    /* tickets, node ids, error strings all fit */

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

/* Largest single room/direct message. Bigger payloads belong in a blob. */
#define LP_MAX_MESSAGE 61440 /* 60 KiB */

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

#define LP_EV_READY           1  /* endpoint bound. data = node id (z-base32 text) */
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
 * Requires the room to be joined (LP_E_STATE otherwise). */
LP_API lp_status lp_room_ticket(lp_node *node, lp_room room, char *buf,
                                size_t buf_len, size_t *out_len);

/* Broadcast to the room. Fire and forget; ordering is not guaranteed. */
LP_API lp_status lp_room_send(lp_node *node, lp_room room,
                              const uint8_t *data, uint32_t len);

/* Direct message to one room member (gossip neighbour or dialed peer). */
LP_API lp_status lp_room_send_to(lp_node *node, lp_room room,
                                 const uint8_t peer[LP_NODE_ID_LEN],
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

/* Turn inbound acceptance on or off at runtime. Off means no stranger can open
 * a connection to this node; joined rooms still work outbound. */
LP_API lp_status lp_accept_inbound(lp_node *node, int32_t enabled);

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

/* Fetch straight from a blob ticket (no room involved). */
LP_API lp_status lp_blob_fetch_ticket(lp_node *node, const char *ticket,
                                      const char *out_path, lp_blob *out_blob);

LP_API lp_status lp_blob_cancel(lp_node *node, lp_blob blob);

LP_API lp_status lp_blob_ticket(lp_node *node, lp_blob blob, char *buf,
                                size_t buf_len, size_t *out_len);

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

/* Hex/z-base32 helpers so bindings never hand-roll node id formatting. */
LP_API lp_status lp_id_to_text(const uint8_t id[LP_NODE_ID_LEN], char *buf,
                               size_t buf_len, size_t *out_len);
LP_API lp_status lp_id_from_text(const char *text, uint8_t out_id[LP_NODE_ID_LEN]);

/* Human-readable name for a status code. Static string, never NULL. */
LP_API const char *lp_status_text(lp_status status);

#ifdef __cplusplus
}
#endif

#endif /* LINKPEARL_H */
