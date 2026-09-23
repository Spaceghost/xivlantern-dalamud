//! The C ABI declared in `include/linkpearl.h`.
//!
//! Rules this file keeps, because a game plugin cannot recover from us breaking
//! them:
//!
//! * no function panics across the boundary. Every entry point is wrapped in
//!   `catch_unwind` and turns a panic into `LP_E_INTERNAL`.
//! * nothing here allocates memory the caller has to free. Text is copied into
//!   caller buffers; event payloads live in a per-node arena that is cleared at
//!   the top of `lp_poll`.
//! * every function except `lp_node_open` / `lp_node_close` is non-blocking.

#![allow(non_camel_case_types)]

use std::{
    ffi::{c_char, c_void, CStr},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::Mutex,
};

use linkpearl_core::{event::Event, Config, Error, Node, RelayMode, Scope, Status};

pub const LP_ABI_VERSION: u32 = 1;
pub const LP_NODE_ID_LEN: usize = 32;
pub const LP_MAX_TEXT: usize = 4096;

// --------------------------------------------------------------- status codes

pub type lp_status = i32;

pub const LP_OK: lp_status = 0;
pub const LP_E_ARG: lp_status = -1;
pub const LP_E_STATE: lp_status = -2;
pub const LP_E_BUFFER: lp_status = -3;
pub const LP_E_NOT_FOUND: lp_status = -4;
pub const LP_E_FULL: lp_status = -5;
pub const LP_E_TICKET: lp_status = -6;
pub const LP_E_STORE: lp_status = -7;
pub const LP_E_NET: lp_status = -8;
pub const LP_E_TOO_LARGE: lp_status = -9;
pub const LP_E_ABI: lp_status = -10;
pub const LP_E_INTERNAL: lp_status = -99;

fn status_of(e: &Error) -> lp_status {
    match e {
        Error::Arg(_) => LP_E_ARG,
        Error::State(_) => LP_E_STATE,
        Error::NotFound => LP_E_NOT_FOUND,
        Error::Full => LP_E_FULL,
        Error::Ticket => LP_E_TICKET,
        Error::Store(_) => LP_E_STORE,
        Error::Net(_) => LP_E_NET,
        Error::TooLarge => LP_E_TOO_LARGE,
        Error::Internal(_) => LP_E_INTERNAL,
    }
}

// --------------------------------------------------------------- event kinds

pub const LP_EV_READY: u32 = 1;
pub const LP_EV_ROOM_JOINED: u32 = 2;
pub const LP_EV_ROOM_LEFT: u32 = 3;
pub const LP_EV_PEER_JOINED: u32 = 4;
pub const LP_EV_PEER_LEFT: u32 = 5;
pub const LP_EV_PRESENCE: u32 = 6;
pub const LP_EV_MESSAGE: u32 = 7;
pub const LP_EV_DIRECT: u32 = 8;
pub const LP_EV_CONNECTED: u32 = 9;
pub const LP_EV_DISCONNECTED: u32 = 10;
pub const LP_EV_BLOB_OFFER: u32 = 11;
pub const LP_EV_BLOB_PROGRESS: u32 = 12;
pub const LP_EV_BLOB_DONE: u32 = 13;
pub const LP_EV_BLOB_FAILED: u32 = 14;
pub const LP_EV_TICKET: u32 = 15;
pub const LP_EV_PUBLISHING: u32 = 16;
pub const LP_EV_ERROR: u32 = 17;
pub const LP_EV_LOG: u32 = 18;
pub const LP_EV_FRIEND_ADDED: u32 = 19;
pub const LP_EV_FRIEND_REMOVED: u32 = 20;
pub const LP_EV_INVITE_FAILED: u32 = 21;
pub const LP_EV_FRIEND_ONLINE: u32 = 22;
pub const LP_EV_FRIEND_OFFLINE: u32 = 23;
pub const LP_EV_FRIEND_TEXT: u32 = 24;
pub const LP_EV_FRIEND_DELIVERED: u32 = 25;
pub const LP_EV_CHANNEL_INVITE: u32 = 26;

// ------------------------------------------------------------ presence status

pub const LP_PRESENCE_INVISIBLE: u32 = 0;
pub const LP_PRESENCE_ONLINE: u32 = 1;
pub const LP_PRESENCE_AWAY: u32 = 2;
pub const LP_PRESENCE_BUSY: u32 = 3;

// -------------------------------------------------------------------- structs

#[repr(C)]
pub struct lp_config {
    pub abi_version: u32,
    pub db_path: *const c_char,
    pub blob_dir: *const c_char,
    pub app_id: *const c_char,
    pub relay_mode: u32,
    pub relay_url: *const c_char,
    pub event_queue_cap: u32,
    pub accept_inbound: u32,
}

#[repr(C)]
pub struct lp_event {
    pub kind: u32,
    pub data_len: u32,
    pub seq: u64,
    pub handle: u64,
    pub peer: [u8; LP_NODE_ID_LEN],
    pub data: *const u8,
}

#[repr(C)]
#[derive(Default)]
pub struct lp_stats {
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub rooms: u32,
    pub peers: u32,
    pub direct_conns: u32,
    pub relayed_conns: u32,
    pub publishing_rooms: u32,
    pub events_dropped: u32,
}

/// The opaque handle the header calls `lp_node`.
pub struct lp_node {
    node: Node,
    /// Payload storage handed out by the previous `lp_poll`. Cleared at the top
    /// of the next one, which is exactly the lifetime the header promises.
    arena: Mutex<Arena>,
}

#[derive(Default)]
struct Arena {
    buffers: Vec<Vec<u8>>,
    seq: u64,
}

// ------------------------------------------------------------------- helpers

/// Wrap a body so a panic becomes a status code instead of unwinding into C.
fn guard<F: FnOnce() -> lp_status>(f: F) -> lp_status {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(status) => status,
        Err(_) => LP_E_INTERNAL,
    }
}

unsafe fn opt_str<'a>(p: *const c_char) -> Result<Option<&'a str>, lp_status> {
    if p.is_null() {
        return Ok(None);
    }
    match CStr::from_ptr(p).to_str() {
        Ok(s) => Ok(Some(s)),
        Err(_) => Err(LP_E_ARG),
    }
}

unsafe fn req_str<'a>(p: *const c_char) -> Result<&'a str, lp_status> {
    opt_str(p)?.ok_or(LP_E_ARG)
}

unsafe fn node_ref<'a>(p: *mut lp_node) -> Result<&'a lp_node, lp_status> {
    if p.is_null() {
        Err(LP_E_ARG)
    } else {
        Ok(&*p)
    }
}

/// Copy `text` into a caller buffer, NUL terminated, reporting the length it
/// needs when it does not fit.
unsafe fn write_text(
    text: &str,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    let needed = text.len();
    if !out_len.is_null() {
        *out_len = needed;
    }
    if buf.is_null() || buf_len == 0 || buf_len < needed + 1 {
        return LP_E_BUFFER;
    }
    ptr::copy_nonoverlapping(text.as_ptr(), buf as *mut u8, needed);
    *buf.add(needed) = 0;
    LP_OK
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ----------------------------------------------------------------- lifecycle

#[no_mangle]
pub extern "C" fn lp_abi_version() -> u32 {
    LP_ABI_VERSION
}

#[no_mangle]
pub extern "C" fn lp_build_info() -> *const c_char {
    // A static NUL-terminated copy so the caller never frees anything.
    static ONCE: std::sync::OnceLock<std::ffi::CString> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| std::ffi::CString::new(linkpearl_core::build_info()).unwrap())
        .as_ptr()
}

#[no_mangle]
pub extern "C" fn lp_status_text(status: lp_status) -> *const c_char {
    let text: &CStr = match status {
        LP_OK => c"ok",
        LP_E_ARG => c"bad argument",
        LP_E_STATE => c"bad state",
        LP_E_BUFFER => c"buffer too small",
        LP_E_NOT_FOUND => c"unknown handle",
        LP_E_FULL => c"queue full",
        LP_E_TICKET => c"ticket did not parse",
        LP_E_STORE => c"store failure",
        LP_E_NET => c"network failure",
        LP_E_TOO_LARGE => c"payload too large",
        LP_E_ABI => c"abi mismatch",
        _ => c"internal error",
    };
    text.as_ptr()
}

/// # Safety
/// `cfg` and `out_node` must be valid pointers; strings in `cfg` must be
/// NUL-terminated UTF-8. Blocks: call off the render thread.
#[no_mangle]
pub unsafe extern "C" fn lp_node_open(cfg: *const lp_config, out_node: *mut *mut lp_node) -> lp_status {
    guard(|| {
        if cfg.is_null() || out_node.is_null() {
            return LP_E_ARG;
        }
        *out_node = ptr::null_mut();
        let cfg = &*cfg;
        if cfg.abi_version != LP_ABI_VERSION {
            return LP_E_ABI;
        }

        let db_path = match opt_str(cfg.db_path) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let blob_dir = match opt_str(cfg.blob_dir) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let app_id = match opt_str(cfg.app_id) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let relay_url = match opt_str(cfg.relay_url) {
            Ok(v) => v,
            Err(e) => return e,
        };

        let relay = match cfg.relay_mode {
            0 => RelayMode::Default,
            1 => RelayMode::Disabled,
            2 => match relay_url {
                Some(url) => RelayMode::Custom(url.to_string()),
                None => return LP_E_ARG,
            },
            _ => return LP_E_ARG,
        };

        let config = Config {
            db_path: db_path.map(Into::into),
            blob_dir: blob_dir.map(Into::into),
            app_id: app_id.unwrap_or("ffxiv-linkpearl/1").to_string(),
            relay,
            event_queue_cap: if cfg.event_queue_cap == 0 {
                4096
            } else {
                cfg.event_queue_cap as usize
            },
            accept_inbound: cfg.accept_inbound != 0,
            ..Config::default()
        };

        match Node::open(config) {
            Ok(node) => {
                let boxed = Box::new(lp_node {
                    node,
                    arena: Mutex::new(Arena::default()),
                });
                *out_node = Box::into_raw(boxed);
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `node` must come from `lp_node_open` and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn lp_node_close(node: *mut lp_node) {
    if node.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let boxed = Box::from_raw(node);
        boxed.node.close();
        drop(boxed);
    }));
}

/// # Safety
/// `out_id` must point to 32 writable bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_node_id(node: *mut lp_node, out_id: *mut u8) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_id.is_null() {
            return LP_E_ARG;
        }
        let id = node.node.node_id();
        ptr::copy_nonoverlapping(id.as_ptr(), out_id, LP_NODE_ID_LEN);
        LP_OK
    })
}

/// # Safety
/// See the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_node_id_text(
    node: *mut lp_node,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let text = data_encoding::HEXLOWER.encode(&node.node.node_id());
        write_text(&text, buf, buf_len, out_len)
    })
}

/// # Safety
/// See the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_node_ticket(
    node: *mut lp_node,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        write_text(&node.node.node_ticket(), buf, buf_len, out_len)
    })
}

// -------------------------------------------------------------------- polling

/// # Safety
/// `out_events` must point to `max_events` writable `lp_event`s.
#[no_mangle]
pub unsafe extern "C" fn lp_poll(
    node: *mut lp_node,
    out_events: *mut lp_event,
    max_events: u32,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_events.is_null() || max_events == 0 {
            return LP_E_ARG;
        }
        let mut arena = match node.arena.lock() {
            Ok(a) => a,
            Err(_) => return LP_E_INTERNAL,
        };
        // Everything handed out last time dies here, exactly as documented.
        arena.buffers.clear();

        let events = node.node.poll(max_events as usize);
        let mut written = 0usize;
        for event in events {
            let slot = &mut *out_events.add(written);
            arena.seq += 1;
            *slot = lp_event {
                kind: 0,
                data_len: 0,
                seq: arena.seq,
                handle: 0,
                peer: [0u8; LP_NODE_ID_LEN],
                data: ptr::null(),
            };
            let payload: Option<Vec<u8>> = match event {
                Event::Ready { node_id } => {
                    slot.kind = LP_EV_READY;
                    slot.peer = node_id;
                    Some(data_encoding::HEXLOWER.encode(&node_id).into_bytes())
                }
                Event::RoomJoined { room } => {
                    slot.kind = LP_EV_ROOM_JOINED;
                    slot.handle = room;
                    None
                }
                Event::RoomLeft { room } => {
                    slot.kind = LP_EV_ROOM_LEFT;
                    slot.handle = room;
                    None
                }
                Event::PeerJoined { room, peer } => {
                    slot.kind = LP_EV_PEER_JOINED;
                    slot.handle = room;
                    slot.peer = peer;
                    None
                }
                Event::PeerLeft { room, peer } => {
                    slot.kind = LP_EV_PEER_LEFT;
                    slot.handle = room;
                    slot.peer = peer;
                    None
                }
                Event::Presence { room, peer, data } => {
                    slot.kind = LP_EV_PRESENCE;
                    slot.handle = room;
                    slot.peer = peer;
                    Some(data)
                }
                Event::Message { room, peer, data } => {
                    slot.kind = LP_EV_MESSAGE;
                    slot.handle = room;
                    slot.peer = peer;
                    Some(data)
                }
                Event::Direct { conn, peer, data } => {
                    slot.kind = LP_EV_DIRECT;
                    slot.handle = conn;
                    slot.peer = peer;
                    Some(data)
                }
                Event::Connected { conn, peer } => {
                    slot.kind = LP_EV_CONNECTED;
                    slot.handle = conn;
                    slot.peer = peer;
                    None
                }
                Event::Disconnected { conn, peer } => {
                    slot.kind = LP_EV_DISCONNECTED;
                    slot.handle = conn;
                    slot.peer = peer;
                    None
                }
                Event::BlobOffer {
                    room,
                    peer,
                    blob,
                    meta,
                } => {
                    slot.kind = LP_EV_BLOB_OFFER;
                    slot.handle = blob;
                    slot.peer = peer;
                    let json = format!(
                        "{{\"room\":{},\"hash\":\"{}\",\"size\":{},\"name\":\"{}\"}}",
                        room,
                        data_encoding::HEXLOWER.encode(&meta.hash),
                        meta.size,
                        json_escape(&meta.name)
                    );
                    Some(json.into_bytes())
                }
                Event::BlobProgress { blob, done, total } => {
                    slot.kind = LP_EV_BLOB_PROGRESS;
                    slot.handle = blob;
                    let mut bytes = Vec::with_capacity(16);
                    bytes.extend_from_slice(&done.to_le_bytes());
                    bytes.extend_from_slice(&total.to_le_bytes());
                    Some(bytes)
                }
                Event::BlobDone { blob, detail } => {
                    slot.kind = LP_EV_BLOB_DONE;
                    slot.handle = blob;
                    Some(detail.into_bytes())
                }
                Event::BlobFailed { blob, reason } => {
                    slot.kind = LP_EV_BLOB_FAILED;
                    slot.handle = blob;
                    Some(reason.into_bytes())
                }
                Event::Ticket { handle, text } => {
                    slot.kind = LP_EV_TICKET;
                    slot.handle = handle;
                    Some(text.into_bytes())
                }
                Event::Publishing { room, publishing } => {
                    slot.kind = LP_EV_PUBLISHING;
                    slot.handle = room;
                    Some(vec![u8::from(publishing)])
                }
                Event::Error { message } => {
                    slot.kind = LP_EV_ERROR;
                    Some(message.into_bytes())
                }
                Event::Log { message } => {
                    slot.kind = LP_EV_LOG;
                    Some(message.into_bytes())
                }
                Event::FriendAdded { handle, peer, name } => {
                    slot.kind = LP_EV_FRIEND_ADDED;
                    slot.handle = handle;
                    slot.peer = peer;
                    Some(name.into_bytes())
                }
                Event::FriendRemoved { peer } => {
                    slot.kind = LP_EV_FRIEND_REMOVED;
                    slot.peer = peer;
                    None
                }
                Event::InviteFailed { handle, reason } => {
                    slot.kind = LP_EV_INVITE_FAILED;
                    slot.handle = handle;
                    Some(reason.into_bytes())
                }
                Event::FriendOnline { peer, status, note } => {
                    slot.kind = LP_EV_FRIEND_ONLINE;
                    slot.handle = status as u64;
                    slot.peer = peer;
                    Some(note.into_bytes())
                }
                Event::FriendOffline { peer } => {
                    slot.kind = LP_EV_FRIEND_OFFLINE;
                    slot.peer = peer;
                    None
                }
                Event::FriendText { peer, id, text, .. } => {
                    slot.kind = LP_EV_FRIEND_TEXT;
                    slot.handle = id;
                    slot.peer = peer;
                    Some(text.into_bytes())
                }
                Event::FriendDelivered { peer, id } => {
                    slot.kind = LP_EV_FRIEND_DELIVERED;
                    slot.handle = id;
                    slot.peer = peer;
                    None
                }
                Event::ChannelInvite { peer, ticket } => {
                    slot.kind = LP_EV_CHANNEL_INVITE;
                    slot.peer = peer;
                    Some(ticket.into_bytes())
                }
                // nostr is not in the C ABI yet; if a build enables it, its
                // results surface as log lines rather than new event kinds.
                Event::NostrDone { handle, ok, detail } => {
                    slot.kind = LP_EV_LOG;
                    slot.handle = handle;
                    Some(format!("nostr: {} {detail}", if ok { "ok" } else { "failed" }).into_bytes())
                }
                Event::NostrInvite { ticket, .. } => {
                    slot.kind = LP_EV_LOG;
                    Some(format!("nostr invite: {ticket}").into_bytes())
                }
                Event::NostrDevices { handle, devices, .. } => {
                    slot.kind = LP_EV_LOG;
                    slot.handle = handle;
                    Some(format!("nostr: {} devices", devices.len()).into_bytes())
                }
                // Call signalling is a spike behind the `calls` feature and not
                // in the C ABI.
                Event::CallIncoming { call, .. }
                | Event::CallAnswered { call, .. }
                | Event::CallEnded { call, .. } => {
                    slot.kind = LP_EV_LOG;
                    slot.handle = call;
                    Some(b"call signalling event (not in the C ABI yet)".to_vec())
                }
            };
            if let Some(bytes) = payload {
                slot.data_len = bytes.len() as u32;
                arena.buffers.push(bytes);
                slot.data = arena.buffers.last().unwrap().as_ptr();
            }
            written += 1;
        }
        written as lp_status
    }));
    result.unwrap_or(LP_E_INTERNAL)
}

/// # Safety
/// `out_stats` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lp_stats_get(node: *mut lp_node, out_stats: *mut lp_stats) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_stats.is_null() {
            return LP_E_ARG;
        }
        let s = node.node.stats();
        *out_stats = lp_stats {
            bytes_sent: s.bytes_sent,
            bytes_recv: s.bytes_recv,
            rooms: s.rooms,
            peers: s.peers,
            direct_conns: s.direct_conns,
            relayed_conns: s.relayed_conns,
            publishing_rooms: s.publishing_rooms,
            events_dropped: s.events_dropped,
        };
        LP_OK
    })
}

/// # Safety
/// `node` must be valid.
#[no_mangle]
pub unsafe extern "C" fn lp_is_publishing(node: *mut lp_node, room: u64) -> i32 {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        i32::from(node.node.is_publishing(room))
    })
}

// ---------------------------------------------------------------------- rooms

/// # Safety
/// `key` may be NULL only when `ticket` is not.
#[no_mangle]
pub unsafe extern "C" fn lp_room_join(
    node: *mut lp_node,
    scope: u32,
    key: *const c_char,
    ticket: *const c_char,
    out_room: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_room.is_null() {
            return LP_E_ARG;
        }
        *out_room = 0;
        let Some(scope) = Scope::from_u32(scope) else {
            return LP_E_ARG;
        };
        let key = match opt_str(key) {
            Ok(v) => v.unwrap_or(""),
            Err(e) => return e,
        };
        let ticket = match opt_str(ticket) {
            Ok(v) => v,
            Err(e) => return e,
        };
        match node.node.room_join(scope, key, ticket) {
            Ok(room) => {
                *out_room = room;
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `node` must be valid.
#[no_mangle]
pub unsafe extern "C" fn lp_room_leave(node: *mut lp_node, room: u64) -> lp_status {
    guard(|| match node_ref(node) {
        Ok(n) => n.node.room_leave(room).map_or_else(|e| status_of(&e), |_| LP_OK),
        Err(e) => e,
    })
}

/// # Safety
/// See the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_room_ticket(
    node: *mut lp_node,
    room: u64,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        match node.node.room_ticket(room) {
            Ok(text) => write_text(&text, buf, buf_len, out_len),
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_room_send(
    node: *mut lp_node,
    room: u64,
    data: *const u8,
    len: u32,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(slice) = slice_of(data, len) else {
            return LP_E_ARG;
        };
        node.node
            .room_send(room, slice)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_presence_set(
    node: *mut lp_node,
    room: u64,
    data: *const u8,
    len: u32,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(slice) = slice_of(data, len) else {
            return LP_E_ARG;
        };
        node.node
            .presence_set(room, slice)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// `out_peers` must hold `max_peers * 32` bytes; `out_count` must be writable.
#[no_mangle]
pub unsafe extern "C" fn lp_room_peers(
    node: *mut lp_node,
    room: u64,
    out_peers: *mut u8,
    max_peers: u32,
    out_count: *mut u32,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_count.is_null() {
            return LP_E_ARG;
        }
        match node.node.room_peers(room) {
            Ok(peers) => {
                *out_count = peers.len() as u32;
                if !out_peers.is_null() {
                    for (i, peer) in peers.iter().take(max_peers as usize).enumerate() {
                        ptr::copy_nonoverlapping(
                            peer.as_ptr(),
                            out_peers.add(i * LP_NODE_ID_LEN),
                            LP_NODE_ID_LEN,
                        );
                    }
                }
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

// --------------------------------------------------------------------- direct

/// # Safety
/// `ticket_or_node_id` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_connect(
    node: *mut lp_node,
    ticket_or_node_id: *const c_char,
    out_conn: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_conn.is_null() {
            return LP_E_ARG;
        }
        *out_conn = 0;
        let text = match req_str(ticket_or_node_id) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match node.node.connect(text) {
            Ok(conn) => {
                *out_conn = conn;
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_send(
    node: *mut lp_node,
    conn: u64,
    data: *const u8,
    len: u32,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(slice) = slice_of(data, len) else {
            return LP_E_ARG;
        };
        node.node
            .send(conn, slice)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// `node` must be valid.
#[no_mangle]
pub unsafe extern "C" fn lp_disconnect(node: *mut lp_node, conn: u64) -> lp_status {
    guard(|| match node_ref(node) {
        Ok(n) => n
            .node
            .disconnect(conn)
            .map_or_else(|e| status_of(&e), |_| LP_OK),
        Err(e) => e,
    })
}

// ---------------------------------------------------------------------- blobs

/// # Safety
/// `path` and `name` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_blob_add_file(
    node: *mut lp_node,
    path: *const c_char,
    name: *const c_char,
    out_blob: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_blob.is_null() {
            return LP_E_ARG;
        }
        *out_blob = 0;
        let path = match req_str(path) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let name = match opt_str(name) {
            Ok(v) => v.unwrap_or(""),
            Err(e) => return e,
        };
        match node.node.blob_add_file(path, name) {
            Ok(blob) => {
                *out_blob = blob;
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_blob_add_bytes(
    node: *mut lp_node,
    data: *const u8,
    len: u32,
    name: *const c_char,
    out_blob: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_blob.is_null() {
            return LP_E_ARG;
        }
        *out_blob = 0;
        let Some(slice) = slice_of(data, len) else {
            return LP_E_ARG;
        };
        let name = match opt_str(name) {
            Ok(v) => v.unwrap_or(""),
            Err(e) => return e,
        };
        match node.node.blob_add_bytes(slice, name) {
            Ok(blob) => {
                *out_blob = blob;
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `node` must be valid.
#[no_mangle]
pub unsafe extern "C" fn lp_blob_offer(node: *mut lp_node, room: u64, blob: u64) -> lp_status {
    guard(|| match node_ref(node) {
        Ok(n) => n
            .node
            .blob_offer(room, blob)
            .map_or_else(|e| status_of(&e), |_| LP_OK),
        Err(e) => e,
    })
}

/// # Safety
/// `node` must be valid.
#[no_mangle]
pub unsafe extern "C" fn lp_blob_unoffer(node: *mut lp_node, room: u64, blob: u64) -> lp_status {
    guard(|| match node_ref(node) {
        Ok(n) => n
            .node
            .blob_unoffer(room, blob)
            .map_or_else(|e| status_of(&e), |_| LP_OK),
        Err(e) => e,
    })
}

/// # Safety
/// `out_path` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_blob_fetch(
    node: *mut lp_node,
    blob: u64,
    out_path: *const c_char,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let path = match req_str(out_path) {
            Ok(p) => p,
            Err(e) => return e,
        };
        node.node
            .blob_fetch(blob, path)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// See the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_blob_hash_text(
    node: *mut lp_node,
    blob: u64,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        match node.node.blob_hash(blob) {
            Ok(hash) => write_text(&data_encoding::HEXLOWER.encode(&hash), buf, buf_len, out_len),
            Err(e) => status_of(&e),
        }
    })
}

// ------------------------------------------------------------------ bookmarks

/// # Safety
/// All strings must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_bookmark_put(
    node: *mut lp_node,
    id: *const c_char,
    scope: u32,
    title: *const c_char,
    ticket: *const c_char,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(scope) = Scope::from_u32(scope) else {
            return LP_E_ARG;
        };
        let (id, title, ticket) = match (req_str(id), opt_str(title), req_str(ticket)) {
            (Ok(i), Ok(t), Ok(k)) => (i, t.unwrap_or(""), k),
            _ => return LP_E_ARG,
        };
        node.node
            .bookmark_put(id, scope, title, ticket)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// `id` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_bookmark_forget(node: *mut lp_node, id: *const c_char) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let id = match req_str(id) {
            Ok(i) => i,
            Err(e) => return e,
        };
        match node.node.bookmark_forget(id) {
            Ok(true) => LP_OK,
            Ok(false) => LP_E_NOT_FOUND,
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// See the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_bookmark_list(
    node: *mut lp_node,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        match node.node.bookmarks_json() {
            Ok(json) => write_text(&json, buf, buf_len, out_len),
            Err(e) => status_of(&e),
        }
    })
}

// -------------------------------------------------------------------- friends

unsafe fn peer_of(p: *const u8) -> Option<[u8; LP_NODE_ID_LEN]> {
    if p.is_null() {
        return None;
    }
    let mut out = [0u8; LP_NODE_ID_LEN];
    ptr::copy_nonoverlapping(p, out.as_mut_ptr(), LP_NODE_ID_LEN);
    Some(out)
}

/// # Safety
/// `name` must be NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_profile_set_name(node: *mut lp_node, name: *const c_char) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        match req_str(name) {
            Ok(name) => node
                .node
                .set_display_name(name)
                .map_or_else(|e| status_of(&e), |_| LP_OK),
            Err(e) => e,
        }
    })
}

/// # Safety
/// `note` may be NULL (no note) or NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_presence_status_set(
    node: *mut lp_node,
    status: u32,
    note: *const c_char,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(status) = Status::from_u32(status) else {
            return LP_E_ARG;
        };
        let note = match opt_str(note) {
            Ok(v) => v.unwrap_or(""),
            Err(e) => return e,
        };
        node.node
            .set_presence(status, note)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// See the buffer contract in the header. The invite only becomes redeemable
/// once it has been written out, so a too-small buffer mints nothing.
#[no_mangle]
pub unsafe extern "C" fn lp_invite_create(
    node: *mut lp_node,
    ttl_secs: u32,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let ttl = (ttl_secs != 0).then(|| std::time::Duration::from_secs(ttl_secs as u64));
        let prepared = match node.node.invite_prepare(ttl) {
            Ok(p) => p,
            Err(e) => return status_of(&e),
        };
        let needed = prepared.text().len();
        if buf.is_null() || buf_len < needed + 1 {
            if !out_len.is_null() {
                *out_len = needed;
            }
            return LP_E_BUFFER;
        }
        match node.node.invite_commit(prepared) {
            Ok(text) => write_text(&text, buf, buf_len, out_len),
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `ticket` must be NUL-terminated UTF-8; `out_handle` writable.
#[no_mangle]
pub unsafe extern "C" fn lp_invite_accept(
    node: *mut lp_node,
    ticket: *const c_char,
    out_handle: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_handle.is_null() {
            return LP_E_ARG;
        }
        *out_handle = 0;
        let ticket = match req_str(ticket) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match node.node.invite_accept(ticket) {
            Ok(handle) => {
                *out_handle = handle;
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// See the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_friend_list(
    node: *mut lp_node,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| match node_ref(node) {
        Ok(n) => write_text(&n.node.friends_json(), buf, buf_len, out_len),
        Err(e) => e,
    })
}

/// # Safety
/// `peer` must point to 32 bytes; `text` to `len` bytes of UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_friend_send(
    node: *mut lp_node,
    peer: *const u8,
    text: *const u8,
    len: u32,
    out_msg: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if !out_msg.is_null() {
            *out_msg = 0;
        }
        let Some(peer) = peer_of(peer) else {
            return LP_E_ARG;
        };
        let Some(bytes) = slice_of(text, len) else {
            return LP_E_ARG;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return LP_E_ARG;
        };
        match node.node.friend_send(&peer, text) {
            Ok(id) => {
                if !out_msg.is_null() {
                    *out_msg = id;
                }
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `peer` must point to 32 bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_friend_remove(node: *mut lp_node, peer: *const u8) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(peer) = peer_of(peer) else {
            return LP_E_ARG;
        };
        node.node
            .friend_remove(&peer)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

/// # Safety
/// `peer` must point to 32 bytes; see the buffer contract in the header.
#[no_mangle]
pub unsafe extern "C" fn lp_friend_history(
    node: *mut lp_node,
    peer: *const u8,
    limit: u32,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(peer) = peer_of(peer) else {
            return LP_E_ARG;
        };
        match node.node.friend_history_json(&peer, limit) {
            Ok(json) => write_text(&json, buf, buf_len, out_len),
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `label` may be NULL or NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn lp_channel_create(
    node: *mut lp_node,
    label: *const c_char,
    out_room: *mut u64,
) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        if out_room.is_null() {
            return LP_E_ARG;
        }
        *out_room = 0;
        let label = match opt_str(label) {
            Ok(v) => v.unwrap_or(""),
            Err(e) => return e,
        };
        match node.node.channel_create(label) {
            Ok(room) => {
                *out_room = room;
                LP_OK
            }
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// `peer` must point to 32 bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_channel_invite(node: *mut lp_node, peer: *const u8, room: u64) -> lp_status {
    guard(|| {
        let node = match node_ref(node) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let Some(peer) = peer_of(peer) else {
            return LP_E_ARG;
        };
        node.node
            .channel_invite(&peer, room)
            .map_or_else(|e| status_of(&e), |_| LP_OK)
    })
}

// ------------------------------------------------------------------ utilities

/// # Safety
/// `id` must point to 32 readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_id_to_text(
    id: *const u8,
    buf: *mut c_char,
    buf_len: usize,
    out_len: *mut usize,
) -> lp_status {
    guard(|| {
        if id.is_null() {
            return LP_E_ARG;
        }
        let bytes = std::slice::from_raw_parts(id, LP_NODE_ID_LEN);
        write_text(&data_encoding::HEXLOWER.encode(bytes), buf, buf_len, out_len)
    })
}

/// # Safety
/// `text` must be NUL-terminated; `out_id` must hold 32 bytes.
#[no_mangle]
pub unsafe extern "C" fn lp_id_from_text(text: *const c_char, out_id: *mut u8) -> lp_status {
    guard(|| {
        let text = match req_str(text) {
            Ok(t) => t,
            Err(e) => return e,
        };
        if out_id.is_null() {
            return LP_E_ARG;
        }
        match data_encoding::HEXLOWER.decode(text.as_bytes()) {
            Ok(bytes) if bytes.len() == LP_NODE_ID_LEN => {
                ptr::copy_nonoverlapping(bytes.as_ptr(), out_id, LP_NODE_ID_LEN);
                LP_OK
            }
            _ => LP_E_ARG,
        }
    })
}

unsafe fn slice_of<'a>(data: *const u8, len: u32) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if data.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(data, len as usize))
}

/// Keeps the unused-import lint honest about `c_void`, which the header uses
/// only in comments.
#[allow(dead_code)]
fn _unused(_: *const c_void) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_text_reports_the_size_it_needs() {
        let mut need: usize = 0;
        let status = unsafe { write_text("hello", ptr::null_mut(), 0, &mut need) };
        assert_eq!(status, LP_E_BUFFER);
        assert_eq!(need, 5, "the caller is told the length without the NUL");

        let mut buf = [0i8; 6];
        let status =
            unsafe { write_text("hello", buf.as_mut_ptr() as *mut c_char, buf.len(), &mut need) };
        assert_eq!(status, LP_OK);
        assert_eq!(&buf[..5], b"hello".map(|b| b as i8));
        assert_eq!(buf[5], 0, "output is NUL terminated");
    }

    #[test]
    fn write_text_refuses_a_buffer_that_is_one_byte_short() {
        let mut need = 0usize;
        let mut buf = [0i8; 5];
        let status =
            unsafe { write_text("hello", buf.as_mut_ptr() as *mut c_char, buf.len(), &mut need) };
        assert_eq!(status, LP_E_BUFFER, "no room for the NUL");
        assert_eq!(buf, [0i8; 5], "nothing is written on failure");
    }

    #[test]
    fn null_nodes_are_refused_not_dereferenced() {
        unsafe {
            assert_eq!(lp_node_id(ptr::null_mut(), ptr::null_mut()), LP_E_ARG);
            assert_eq!(lp_room_leave(ptr::null_mut(), 1), LP_E_ARG);
            assert_eq!(lp_poll(ptr::null_mut(), ptr::null_mut(), 8), LP_E_ARG);
            lp_node_close(ptr::null_mut()); // must not crash
        }
    }

    #[test]
    fn opening_with_the_wrong_abi_version_is_refused() {
        let cfg = lp_config {
            abi_version: LP_ABI_VERSION + 1,
            db_path: ptr::null(),
            blob_dir: ptr::null(),
            app_id: ptr::null(),
            relay_mode: 1,
            relay_url: ptr::null(),
            event_queue_cap: 0,
            accept_inbound: 0,
        };
        let mut node: *mut lp_node = ptr::null_mut();
        assert_eq!(unsafe { lp_node_open(&cfg, &mut node) }, LP_E_ABI);
        assert!(node.is_null());
    }

    #[test]
    fn status_text_is_never_null() {
        for s in [LP_OK, LP_E_ARG, LP_E_BUFFER, LP_E_INTERNAL, -12345] {
            let p = lp_status_text(s);
            assert!(!p.is_null());
            assert!(!unsafe { CStr::from_ptr(p) }.to_bytes().is_empty());
        }
    }

    #[test]
    fn id_text_round_trips() {
        let id = [0xabu8; LP_NODE_ID_LEN];
        let mut buf = [0i8; 128];
        let mut need = 0usize;
        assert_eq!(
            unsafe {
                lp_id_to_text(
                    id.as_ptr(),
                    buf.as_mut_ptr() as *mut c_char,
                    buf.len(),
                    &mut need,
                )
            },
            LP_OK
        );
        let mut back = [0u8; LP_NODE_ID_LEN];
        assert_eq!(
            unsafe { lp_id_from_text(buf.as_ptr() as *const c_char, back.as_mut_ptr()) },
            LP_OK
        );
        assert_eq!(back, id);
    }

    /// `include/linkpearl.h` is written by hand. This keeps it honest: every
    /// declared function is exported, every export is declared, and every
    /// `LP_` constant has the same value on both sides.
    #[test]
    fn the_header_matches_the_exports_and_constants() {
        use std::collections::BTreeMap;
        let header = include_str!("../../../include/linkpearl.h");
        let source = include_str!("lib.rs");

        let declared: std::collections::BTreeSet<&str> = header
            .lines()
            .filter_map(|l| {
                let decl = l.strip_prefix("LP_API ")?;
                let head = &decl[..decl.find('(')?];
                head.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .rev()
                    .find(|w| w.starts_with("lp_"))
            })
            .collect();
        let exported: std::collections::BTreeSet<&str> = source
            .split("extern \"C\" fn ")
            .skip(1)
            .filter_map(|rest| rest.split('(').next())
            .filter(|name| name.starts_with("lp_"))
            .collect();
        assert_eq!(declared, exported, "header declarations vs #[no_mangle] exports");

        let header_defines: BTreeMap<&str, i64> = header
            .lines()
            .filter_map(|l| {
                let mut w = l.strip_prefix("#define ")?.split_whitespace();
                let name = w.next()?;
                let value = w.next()?.parse().ok()?;
                name.starts_with("LP_").then_some((name, value))
            })
            .collect();
        let rust_consts: BTreeMap<&str, i64> = source
            .lines()
            .filter_map(|l| {
                let rest = l.trim().strip_prefix("pub const ")?;
                let (name, rest) = rest.split_once(':')?;
                let value = rest.split_once('=')?.1.trim().trim_end_matches(';').parse().ok()?;
                Some((name, value))
            })
            .collect();
        for (name, value) in &rust_consts {
            assert_eq!(header_defines.get(name), Some(value), "{name} differs or is missing in the header");
        }
        for name in header_defines.keys() {
            let covered = ["LP_EV_", "LP_E_", "LP_OK", "LP_PRESENCE_"]
                .iter()
                .any(|p| name.starts_with(p));
            if covered {
                assert!(rust_consts.contains_key(name), "{name} is in the header but not in lib.rs");
            }
        }
        assert_eq!(
            header_defines["LP_MAX_MESSAGE"],
            linkpearl_core::MAX_MESSAGE as i64,
            "LP_MAX_MESSAGE"
        );
        assert_eq!(header_defines["LP_SCOPE_CUSTOM"], Scope::Custom as i64);
        assert_eq!(header_defines["LP_RELAY_DISABLED"], 1);
    }

    #[test]
    fn bad_id_text_is_refused() {
        let mut back = [0u8; LP_NODE_ID_LEN];
        let bad = c"not hex";
        assert_eq!(
            unsafe { lp_id_from_text(bad.as_ptr(), back.as_mut_ptr()) },
            LP_E_ARG
        );
    }
}
