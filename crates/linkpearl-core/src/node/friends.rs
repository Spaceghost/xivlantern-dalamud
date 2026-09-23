//! Friends: invites, friend links, the presence heartbeat and 1:1 text.
//!
//! One QUIC connection per friend device on the friend ALPN, whichever side
//! dialed it. Every frame is one uni stream holding `(PROTOCOL_VERSION, Wire)`
//! in postcard. QUIC has already authenticated the remote NodeId, so frames on
//! a friend link are not signed a second time; friendship is keyed on that id.
//!
//! A connection from a NodeId that is not a friend gets exactly one frame to
//! present an invite (`Redeem`) and is closed otherwise.

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
    EndpointAddr, EndpointId,
};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use super::{json_string, Cmd, Node, Shared};
use crate::{
    event::{Event, PeerId, Status},
    store::{now_ms, Store},
    ticket::{FriendInvite, RoomTicket},
    Error, Result, RoomHandle, MAX_MESSAGE, PROTOCOL_VERSION,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_WIRE: usize = MAX_MESSAGE + 4096;
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const MAX_NAME: usize = 64;
const MAX_NOTE: usize = 256;

/// Bytes from the OS CSPRNG.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("the OS has no random source");
    out
}

fn now_secs() -> i64 {
    now_ms() / 1000
}

/// Trim, drop control characters, cap the length. Names and notes arrive from
/// other people and end up in an ImGui window and a log line.
fn clean(text: &str, max_chars: usize) -> String {
    text.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone)]
pub(crate) struct Me {
    pub name: String,
    pub status: Status,
    pub note: String,
    /// This user's nostr public key, once `nostr_keys` has made one.
    pub nostr: Option<[u8; 32]>,
}

impl Default for Me {
    fn default() -> Self {
        Me {
            name: String::new(),
            status: Status::Online,
            note: String::new(),
            nostr: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FriendDevice {
    pub(super) friend_id: i64,
    pub(super) nostr: Option<[u8; 32]>,
    name: String,
    online: bool,
    status: Status,
    note: String,
    last_heard: Option<Instant>,
    last_seen_ms: Option<i64>,
    dialing: bool,
    next_dial: Instant,
    backoff: Duration,
}

impl FriendDevice {
    fn new(friend_id: i64, name: String, last_seen_ms: Option<i64>, heartbeat: Duration) -> Self {
        FriendDevice {
            friend_id,
            nostr: None,
            name,
            online: false,
            status: Status::Invisible,
            note: String::new(),
            last_heard: None,
            last_seen_ms,
            dialing: false,
            next_dial: Instant::now(),
            backoff: heartbeat,
        }
    }
}

/// One friend device as the UI sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendInfo {
    pub peer: PeerId,
    /// Devices of the same person share this id.
    pub friend_id: i64,
    pub name: String,
    pub online: bool,
    pub status: Status,
    pub note: String,
    /// Unix milliseconds we last heard from this device, if ever.
    pub last_seen: Option<i64>,
    /// A friend link is currently open (it may still be invisible).
    pub linked: bool,
    /// The friend's nostr key, set only once a device list signed by that
    /// key has listed this device.
    pub nostr: Option<[u8; 32]>,
}

/// One line of 1:1 history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: u64,
    pub outgoing: bool,
    pub text: String,
    pub sent_at: i64,
    /// For outgoing: when the friend acknowledged it. For incoming: when it
    /// arrived.
    pub delivered_at: Option<i64>,
}

/// An invite that exists only in memory until [`Node::invite_commit`].
#[derive(Debug, Clone)]
pub struct PreparedInvite {
    text: String,
    secret: [u8; 16],
    expires_at: i64,
}

impl PreparedInvite {
    pub fn text(&self) -> &str {
        &self.text
    }
}

// ------------------------------------------------------------------- wire

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Wire {
    /// Accepter -> inviter, first frame on a fresh connection.
    Redeem {
        secret: [u8; 16],
        name: String,
        addr: EndpointAddr,
    },
    /// Inviter -> accepter: the invite was good, we are friends.
    Welcome { name: String, addr: EndpointAddr },
    Refused { reason: String },
    /// First frame on every friend link, and after a name or status change.
    Hello {
        name: String,
        addr: EndpointAddr,
        status: u32,
        note: String,
    },
    Heartbeat { status: u32, note: String },
    Text { id: u64, sent_at: i64, body: String },
    Ack { id: u64 },
    ChannelInvite { ticket: String },
    Unfriend,
}

pub(crate) fn encode(wire: &Wire) -> Vec<u8> {
    postcard::to_stdvec(&(PROTOCOL_VERSION, wire)).expect("postcard::to_stdvec is infallible for Wire")
}

pub(crate) fn decode(bytes: &[u8]) -> Option<Wire> {
    let (version, wire): (u16, Wire) = postcard::from_bytes(bytes).ok()?;
    (version == PROTOCOL_VERSION).then_some(wire)
}

async fn send(shared: &Shared, conn: &Connection, wire: &Wire) -> std::result::Result<(), String> {
    let bytes = encode(wire);
    let mut stream = conn.open_uni().await.map_err(|e| e.to_string())?;
    stream.write_all(&bytes).await.map_err(|e| e.to_string())?;
    stream.finish().map_err(|e| e.to_string())?;
    shared
        .stats
        .bytes_sent
        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
    Ok(())
}

/// `Err` when the connection is gone; `Ok(None)` for a frame we could not
/// read, which is dropped.
async fn recv(shared: &Shared, conn: &Connection) -> std::result::Result<Option<Wire>, ()> {
    let mut stream = conn.accept_uni().await.map_err(|_| ())?;
    match stream.read_to_end(MAX_WIRE).await {
        Ok(bytes) => {
            shared
                .stats
                .bytes_recv
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
            Ok(decode(&bytes))
        }
        Err(_) => Ok(None),
    }
}

// --------------------------------------------------------------- commands

pub(crate) enum FriendCmd {
    InviteStore { secret: [u8; 16], expires_at: i64 },
    InviteAccept { handle: u64, invite: FriendInvite },
    Text {
        peer: EndpointId,
        id: u64,
        sent_at: i64,
        body: String,
    },
    /// Send a fresh `Hello` on every link and persist the profile.
    Announce,
    Remove { peer: EndpointId },
    Send { peer: EndpointId, wire: Wire },
}

fn with_store<T>(shared: &Shared, f: impl FnOnce(&mut Store) -> Result<T>) -> Option<T> {
    let mut store = shared.store.lock().ok()?;
    match f(&mut store) {
        Ok(v) => Some(v),
        Err(e) => {
            drop(store);
            shared.error(format!("store: {e}"));
            None
        }
    }
}

fn link(shared: &Shared, peer: &EndpointId) -> Option<Connection> {
    shared.links.lock().unwrap().get(peer).cloned()
}

/// Runs on the command task; everything slow is spawned.
pub(crate) fn handle(shared: &Arc<Shared>, cmd: FriendCmd) {
    match cmd {
        FriendCmd::InviteStore { secret, expires_at } => {
            with_store(shared, |s| s.invite_put(&secret, expires_at));
        }
        FriendCmd::InviteAccept { handle, invite } => {
            tokio::spawn(accept_invite(shared.clone(), handle, invite));
        }
        FriendCmd::Text {
            peer,
            id,
            sent_at,
            body,
        } => {
            with_store(shared, |s| s.message_out(peer.as_bytes(), id, sent_at, &body));
            // No link: it waits in the outbox until one comes up.
            if let Some(conn) = link(shared, &peer) {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let wire = Wire::Text { id, sent_at, body };
                    if let Err(e) = send(&shared, &conn, &wire).await {
                        shared.log(format!("text queued, send failed: {e}"));
                    }
                });
            }
        }
        FriendCmd::Announce => {
            let me = shared.me.read().unwrap().clone();
            with_store(shared, |s| {
                s.profile_set("name", &me.name)?;
                s.profile_set("status", &(me.status as u32).to_string())?;
                s.profile_set("note", &me.note)
            });
            let hello = hello_frame(shared);
            let links: Vec<Connection> = shared.links.lock().unwrap().values().cloned().collect();
            for conn in links {
                let shared = shared.clone();
                let hello = hello.clone();
                tokio::spawn(async move {
                    let _ = send(&shared, &conn, &hello).await;
                });
            }
        }
        FriendCmd::Remove { peer } => {
            with_store(shared, |s| s.friend_remove_device(peer.as_bytes()));
            let conn = shared.links.lock().unwrap().remove(&peer);
            if let Some(conn) = conn {
                let shared = shared.clone();
                tokio::spawn(async move {
                    let _ = send(&shared, &conn, &Wire::Unfriend).await;
                    // Give the frame a moment to land before tearing down.
                    let _ = timeout(Duration::from_secs(3), conn.closed()).await;
                    conn.close(0u32.into(), b"unfriended");
                });
            }
            shared.emit(Event::FriendRemoved {
                peer: *peer.as_bytes(),
            });
        }
        FriendCmd::Send { peer, wire } => match link(shared, &peer) {
            Some(conn) => {
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = send(&shared, &conn, &wire).await {
                        shared.error(format!("send to friend failed: {e}"));
                    }
                });
            }
            None => shared.error("that friend is not connected right now"),
        },
    }
}

// ------------------------------------------------------------- lifecycle

/// Load the profile, the friend list and open invites. Called from `open`.
pub(crate) fn load(shared: &Arc<Shared>) -> Result<()> {
    let store = shared
        .store
        .lock()
        .map_err(|_| Error::Store("store poisoned".into()))?;
    {
        let mut me = shared.me.write().unwrap();
        me.name = store.profile_get("name")?.unwrap_or_default();
        me.status = store
            .profile_get("status")?
            .and_then(|v| v.parse::<u32>().ok())
            .and_then(Status::from_u32)
            .unwrap_or(Status::Online);
        me.note = store.profile_get("note")?.unwrap_or_default();
        me.nostr = store
            .profile_get("nostr_pubkey")?
            .and_then(|h| data_encoding::HEXLOWER.decode(h.as_bytes()).ok())
            .and_then(|b| <[u8; 32]>::try_from(b).ok());
    }
    let mut friends = shared.friends.write().unwrap();
    for row in store.friend_devices()? {
        let Ok(id) = EndpointId::from_bytes(&row.node_id) else {
            continue;
        };
        if let Some(addr) = row
            .addr
            .as_deref()
            .and_then(|b| postcard::from_bytes::<EndpointAddr>(b).ok())
        {
            if addr.id == id {
                shared.lookup.add_endpoint_info(addr);
            }
        }
        let mut device = FriendDevice::new(row.friend_id, row.name, row.last_seen, shared.heartbeat);
        device.nostr = row.nostr;
        friends.insert(id, device);
    }
    let mut invites = shared.invites.lock().unwrap();
    for (secret, expires_at) in store.invites_open(now_secs())? {
        invites.insert(secret, expires_at);
    }
    Ok(())
}

fn hello_frame(shared: &Shared) -> Wire {
    let me = shared.me.read().unwrap().clone();
    let hidden = me.status == Status::Invisible;
    Wire::Hello {
        name: me.name,
        addr: shared.endpoint.addr(),
        status: me.status as u32,
        note: if hidden { String::new() } else { me.note },
    }
}

fn add_friend(shared: &Shared, peer: EndpointId, name: &str, addr: Option<&EndpointAddr>) {
    let addr = addr.filter(|a| a.id == peer);
    if let Some(addr) = addr {
        shared.lookup.add_endpoint_info(addr.clone());
    }
    let addr_bytes = addr.map(|a| postcard::to_stdvec(a).expect("infallible"));
    let friend_id = with_store(shared, |s| {
        s.friend_add(peer.as_bytes(), name, addr_bytes.as_deref())
    })
    .unwrap_or(0);
    let mut friends = shared.friends.write().unwrap();
    friends
        .entry(peer)
        .and_modify(|d| d.name = name.to_string())
        .or_insert_with(|| FriendDevice::new(friend_id, name.to_string(), None, shared.heartbeat));
}

fn is_friend(shared: &Shared, peer: &EndpointId) -> bool {
    shared.friends.read().unwrap().contains_key(peer)
}

/// Apply a status we heard from a friend; raise an event if it changed.
fn heard(shared: &Shared, peer: EndpointId, status: u32, note: String) {
    let status = Status::from_u32(status).unwrap_or(Status::Online);
    let note = clean(&note, MAX_NOTE);
    let event = {
        let mut friends = shared.friends.write().unwrap();
        let Some(d) = friends.get_mut(&peer) else {
            return;
        };
        d.last_seen_ms = Some(now_ms());
        if status == Status::Invisible {
            d.last_heard = None;
            d.status = status;
            d.note.clear();
            if d.online {
                d.online = false;
                Some(Event::FriendOffline {
                    peer: *peer.as_bytes(),
                })
            } else {
                None
            }
        } else {
            d.last_heard = Some(Instant::now());
            if !d.online || d.status != status || d.note != note {
                d.online = true;
                d.status = status;
                d.note = note.clone();
                Some(Event::FriendOnline {
                    peer: *peer.as_bytes(),
                    status,
                    note,
                })
            } else {
                None
            }
        }
    };
    if let Some(e) = event {
        shared.emit(e);
    }
}

fn set_offline(shared: &Shared, peer: EndpointId) {
    let (was_online, last_seen) = {
        let mut friends = shared.friends.write().unwrap();
        match friends.get_mut(&peer) {
            Some(d) => {
                let was = d.online;
                d.online = false;
                d.last_heard = None;
                (was, d.last_seen_ms)
            }
            None => return,
        }
    };
    with_store(shared, |s| s.friend_touch(peer.as_bytes(), None, None, last_seen));
    if was_online {
        shared.emit(Event::FriendOffline {
            peer: *peer.as_bytes(),
        });
    }
}

async fn flush_outbox(shared: &Shared, conn: &Connection, peer: EndpointId) {
    let Some(pending) = with_store(shared, |s| s.outbox(peer.as_bytes())) else {
        return;
    };
    for m in pending {
        let wire = Wire::Text {
            id: m.id,
            sent_at: m.sent_at,
            body: m.body,
        };
        if send(shared, conn, &wire).await.is_err() {
            return;
        }
    }
}

/// Own a friend link until it closes.
async fn run_link(shared: Arc<Shared>, conn: Connection) {
    let peer = conn.remote_id();
    let stable = conn.stable_id();
    shared.links.lock().unwrap().insert(peer, conn.clone());
    if let Some(d) = shared.friends.write().unwrap().get_mut(&peer) {
        d.backoff = shared.heartbeat;
    }

    let _ = send(&shared, &conn, &hello_frame(&shared)).await;
    flush_outbox(&shared, &conn, peer).await;

    while let Ok(frame) = recv(&shared, &conn).await {
        let Some(wire) = frame else { continue };
        if !on_frame(&shared, &conn, peer, wire).await {
            break;
        }
    }

    let still_linked = {
        let mut links = shared.links.lock().unwrap();
        if links.get(&peer).map(|c| c.stable_id()) == Some(stable) {
            links.remove(&peer);
            false
        } else {
            links.contains_key(&peer)
        }
    };
    if !still_linked {
        set_offline(&shared, peer);
    }
}

/// `false` ends the link.
async fn on_frame(shared: &Arc<Shared>, conn: &Connection, peer: EndpointId, wire: Wire) -> bool {
    if !is_friend(shared, &peer) {
        conn.close(1u32.into(), b"not a friend");
        return false;
    }
    let peer_bytes: PeerId = *peer.as_bytes();
    match wire {
        Wire::Hello {
            name,
            addr,
            status,
            note,
        } => {
            let name = clean(&name, MAX_NAME);
            let addr = (addr.id == peer).then_some(addr);
            if let Some(a) = &addr {
                shared.lookup.add_endpoint_info(a.clone());
            }
            let addr_bytes = addr.as_ref().map(|a| postcard::to_stdvec(a).expect("infallible"));
            if let Some(d) = shared.friends.write().unwrap().get_mut(&peer) {
                if !name.is_empty() {
                    d.name = name.clone();
                }
            }
            with_store(shared, |s| {
                s.friend_touch(
                    &peer_bytes,
                    (!name.is_empty()).then_some(name.as_str()),
                    addr_bytes.as_deref(),
                    Some(now_ms()),
                )
            });
            heard(shared, peer, status, note);
        }
        Wire::Heartbeat { status, note } => heard(shared, peer, status, note),
        Wire::Text { id, sent_at, body } => {
            let fresh = with_store(shared, |s| s.message_in(&peer_bytes, id, sent_at, &body));
            if fresh == Some(true) {
                shared.emit(Event::FriendText {
                    peer: peer_bytes,
                    id,
                    sent_at,
                    text: body,
                });
            }
            // Ack a duplicate too: the first ack is what got lost.
            if fresh.is_some() {
                let _ = send(shared, conn, &Wire::Ack { id }).await;
            }
        }
        Wire::Ack { id } => {
            if with_store(shared, |s| s.message_delivered(&peer_bytes, id)) == Some(true) {
                shared.emit(Event::FriendDelivered {
                    peer: peer_bytes,
                    id,
                });
            }
        }
        Wire::ChannelInvite { ticket } => {
            if ticket.parse::<RoomTicket>().is_ok() {
                shared.emit(Event::ChannelInvite {
                    peer: peer_bytes,
                    ticket,
                });
            }
        }
        Wire::Unfriend => {
            shared.friends.write().unwrap().remove(&peer);
            with_store(shared, |s| s.friend_remove_device(&peer_bytes));
            shared.emit(Event::FriendRemoved { peer: peer_bytes });
            conn.close(0u32.into(), b"unfriended");
            return false;
        }
        Wire::Redeem { .. } | Wire::Welcome { .. } | Wire::Refused { .. } => {}
    }
    true
}

// ---------------------------------------------------------------- invites

async fn accept_invite(shared: Arc<Shared>, handle: u64, invite: FriendInvite) {
    let fail = |reason: String| shared.emit(Event::InviteFailed { handle, reason });
    shared.lookup.add_endpoint_info(invite.addr.clone());
    let conn = match timeout(
        HANDSHAKE_TIMEOUT,
        shared.endpoint.connect(invite.addr.clone(), &shared.friend_alpn),
    )
    .await
    {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => return fail(format!("could not reach the inviter: {e}")),
        Err(_) => return fail("timed out reaching the inviter".into()),
    };
    let redeem = Wire::Redeem {
        secret: invite.secret,
        name: shared.me.read().unwrap().name.clone(),
        addr: shared.endpoint.addr(),
    };
    if let Err(e) = send(&shared, &conn, &redeem).await {
        return fail(format!("could not present the invite: {e}"));
    }
    match timeout(HANDSHAKE_TIMEOUT, recv(&shared, &conn)).await {
        Ok(Ok(Some(Wire::Welcome { name, addr }))) => {
            let peer = conn.remote_id();
            let name = clean(&name, MAX_NAME);
            add_friend(&shared, peer, &name, Some(&addr));
            shared.emit(Event::FriendAdded {
                handle,
                peer: *peer.as_bytes(),
                name,
            });
            run_link(shared, conn).await;
        }
        Ok(Ok(Some(Wire::Refused { reason }))) => {
            conn.close(0u32.into(), b"ok");
            fail(format!("refused: {}", clean(&reason, MAX_NOTE)));
        }
        Ok(Ok(_)) => {
            conn.close(1u32.into(), b"unexpected");
            fail("the inviter answered with something else".into());
        }
        Ok(Err(())) => fail("the inviter closed the connection".into()),
        Err(_) => {
            conn.close(1u32.into(), b"timeout");
            fail("timed out waiting for the inviter".into());
        }
    }
}

/// A stranger presenting an invite. `true` when it was good and the link
/// should run.
async fn redeem(shared: &Arc<Shared>, conn: &Connection) -> bool {
    let peer = conn.remote_id();
    let Ok(Ok(Some(Wire::Redeem { secret, name, addr }))) =
        timeout(HANDSHAKE_TIMEOUT, recv(shared, conn)).await
    else {
        conn.close(1u32.into(), b"not a friend");
        return false;
    };
    let valid = {
        let mut invites = shared.invites.lock().unwrap();
        match invites.remove(&secret) {
            Some(expires_at) => expires_at == 0 || now_secs() < expires_at,
            None => false,
        }
    };
    if !valid {
        let refused = Wire::Refused {
            reason: "invite unknown, already used, or expired".into(),
        };
        let _ = send(shared, conn, &refused).await;
        // The accepter closes once it has read the refusal.
        let _ = timeout(Duration::from_secs(5), conn.closed()).await;
        return false;
    }
    with_store(shared, |s| s.invite_redeemed(&secret, peer.as_bytes()));
    let name = clean(&name, MAX_NAME);
    add_friend(shared, peer, &name, Some(&addr));
    shared.emit(Event::FriendAdded {
        handle: 0,
        peer: *peer.as_bytes(),
        name,
    });
    let welcome = Wire::Welcome {
        name: shared.me.read().unwrap().name.clone(),
        addr: shared.endpoint.addr(),
    };
    send(shared, conn, &welcome).await.is_ok()
}

// ------------------------------------------------------------- protocol

#[derive(Clone)]
pub(crate) struct FriendProtocol {
    shared: Arc<Shared>,
}

impl FriendProtocol {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        FriendProtocol { shared }
    }
}

impl std::fmt::Debug for FriendProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("linkpearl-friends")
    }
}

impl ProtocolHandler for FriendProtocol {
    async fn accept(&self, conn: Connection) -> std::result::Result<(), AcceptError> {
        let shared = self.shared.clone();
        let peer = conn.remote_id();
        if is_friend(&shared, &peer) || redeem(&shared, &conn).await {
            run_link(shared, conn).await;
        }
        Ok(())
    }
}

// -------------------------------------------------------------- heartbeat

/// Send presence to every linked friend, time out silent ones, and redial
/// friends with no link, backing off per device.
pub(crate) async fn heartbeat_loop(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(shared.heartbeat);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if shared.closed.load(Ordering::SeqCst) {
            break;
        }
        let me = shared.me.read().unwrap().clone();
        let links: Vec<(EndpointId, Connection)> = shared
            .links
            .lock()
            .unwrap()
            .iter()
            .map(|(p, c)| (*p, c.clone()))
            .collect();
        if me.status != Status::Invisible {
            let beat = Wire::Heartbeat {
                status: me.status as u32,
                note: me.note.clone(),
            };
            for (_, conn) in &links {
                let shared = shared.clone();
                let conn = conn.clone();
                let beat = beat.clone();
                tokio::spawn(async move {
                    let _ = send(&shared, &conn, &beat).await;
                });
            }
        }

        let now = Instant::now();
        let silent_after = shared.heartbeat * 3;
        let mut offline = Vec::new();
        let mut dial = Vec::new();
        {
            let mut friends = shared.friends.write().unwrap();
            for (peer, d) in friends.iter_mut() {
                if d.online && d.last_heard.is_none_or(|t| now.duration_since(t) > silent_after) {
                    d.online = false;
                    offline.push(*peer);
                }
                let linked = links.iter().any(|(p, _)| p == peer);
                if !linked && !d.dialing && now >= d.next_dial {
                    d.dialing = true;
                    dial.push(*peer);
                }
            }
        }
        for peer in offline {
            shared.emit(Event::FriendOffline {
                peer: *peer.as_bytes(),
            });
        }
        for peer in dial {
            tokio::spawn(dial_friend(shared.clone(), peer));
        }
    }
}

async fn dial_friend(shared: Arc<Shared>, peer: EndpointId) {
    let connected = timeout(
        HANDSHAKE_TIMEOUT,
        shared
            .endpoint
            .connect(EndpointAddr::from(peer), &shared.friend_alpn),
    )
    .await;
    let ok = matches!(connected, Ok(Ok(_)));
    if let Ok(Ok(conn)) = connected {
        run_link(shared.clone(), conn).await;
    }
    let mut friends = shared.friends.write().unwrap();
    if let Some(d) = friends.get_mut(&peer) {
        d.dialing = false;
        if ok {
            d.backoff = shared.heartbeat;
        } else {
            d.backoff = (d.backoff * 2).min(MAX_BACKOFF);
        }
        d.next_dial = Instant::now() + d.backoff;
    }
}

// -------------------------------------------------------------- Node API

fn endpoint_id(peer: &PeerId) -> Result<EndpointId> {
    EndpointId::from_bytes(peer).map_err(|_| Error::Arg("not a node id"))
}

impl Node {
    fn friend_cmd(&self, cmd: FriendCmd) -> Result<()> {
        self.push(Cmd::Friend(cmd))
    }

    fn require_friend(&self, peer: &PeerId) -> Result<EndpointId> {
        let id = endpoint_id(peer)?;
        if is_friend(&self.shared, &id) {
            Ok(id)
        } else {
            Err(Error::NotFound)
        }
    }

    // ------------------------------------------------------------ profile

    pub fn display_name(&self) -> String {
        self.shared.me.read().unwrap().name.clone()
    }

    /// What friends see as this node's name. Persisted; pushed to linked
    /// friends right away.
    pub fn set_display_name(&self, name: &str) -> Result<()> {
        self.check_open()?;
        let name = clean(name, MAX_NAME);
        self.shared.me.write().unwrap().name = name;
        self.friend_cmd(FriendCmd::Announce)
    }

    pub fn presence(&self) -> (Status, String) {
        let me = self.shared.me.read().unwrap();
        (me.status, me.note.clone())
    }

    /// Set what friends see. The note is free text the *player* typed; this
    /// library never fills it from anything else. `Invisible` stops the
    /// heartbeat and friends see this node go offline; texts still flow.
    pub fn set_presence(&self, status: Status, note: &str) -> Result<()> {
        self.check_open()?;
        {
            let mut me = self.shared.me.write().unwrap();
            me.status = status;
            me.note = clean(note, MAX_NOTE);
        }
        self.friend_cmd(FriendCmd::Announce)
    }

    // ------------------------------------------------------------ invites

    /// Mint an invite without making it redeemable yet. See
    /// [`Node::invite_commit`]; [`Node::invite_create`] does both.
    pub fn invite_prepare(&self, ttl: Option<Duration>) -> Result<PreparedInvite> {
        self.check_open()?;
        let secret = random::<16>();
        let expires_at = match ttl {
            Some(d) => now_secs() + (d.as_secs() as i64).max(1),
            None => 0,
        };
        let invite = FriendInvite {
            version: PROTOCOL_VERSION,
            addr: self.shared.endpoint.addr(),
            secret,
            name: self.display_name(),
            expires_at,
            nostr: self.shared.me.read().unwrap().nostr,
        };
        Ok(PreparedInvite {
            text: invite.to_string(),
            secret,
            expires_at,
        })
    }

    pub fn invite_commit(&self, invite: PreparedInvite) -> Result<String> {
        self.check_open()?;
        self.shared
            .invites
            .lock()
            .unwrap()
            .insert(invite.secret, invite.expires_at);
        self.friend_cmd(FriendCmd::InviteStore {
            secret: invite.secret,
            expires_at: invite.expires_at,
        })?;
        Ok(invite.text)
    }

    /// A single-use friend invite (`lpfriend…`) for somebody else to accept.
    /// `None` never expires.
    pub fn invite_create(&self, ttl: Option<Duration>) -> Result<String> {
        let prepared = self.invite_prepare(ttl)?;
        self.invite_commit(prepared)
    }

    /// Redeem somebody's invite. Returns a handle; `FriendAdded` or
    /// `InviteFailed` with that handle arrives later.
    pub fn invite_accept(&self, ticket: &str) -> Result<u64> {
        self.check_open()?;
        let invite: FriendInvite = ticket.parse()?;
        if invite.addr.id == self.shared.endpoint.id() {
            return Err(Error::Arg("that invite is your own"));
        }
        if invite.is_expired(now_secs()) {
            return Err(Error::Arg("that invite has expired"));
        }
        let handle = self.shared.handle();
        self.friend_cmd(FriendCmd::InviteAccept { handle, invite })?;
        Ok(handle)
    }

    // ------------------------------------------------------------ friends

    pub fn friends(&self) -> Vec<FriendInfo> {
        let links = self.shared.links.lock().unwrap();
        let friends = self.shared.friends.read().unwrap();
        let mut out: Vec<FriendInfo> = friends
            .iter()
            .map(|(peer, d)| FriendInfo {
                peer: *peer.as_bytes(),
                friend_id: d.friend_id,
                name: d.name.clone(),
                online: d.online,
                status: if d.online { d.status } else { Status::Invisible },
                note: if d.online { d.note.clone() } else { String::new() },
                last_seen: d.last_seen_ms,
                linked: links.contains_key(peer),
                nostr: d.nostr,
            })
            .collect();
        out.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.peer.cmp(&b.peer))
        });
        out
    }

    pub fn friends_json(&self) -> String {
        let mut out = String::from("[");
        for (i, f) in self.friends().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"id\":\"{}\",\"friend\":{},\"name\":{},\"online\":{},\"status\":\"{}\",\"note\":{},\"last_seen\":{},\"linked\":{},\"nostr\":{}}}",
                data_encoding::HEXLOWER.encode(&f.peer),
                f.friend_id,
                json_string(&f.name),
                f.online,
                f.status.as_str(),
                json_string(&f.note),
                f.last_seen.map_or("null".to_string(), |t| t.to_string()),
                f.linked,
                f.nostr.map_or("null".to_string(), |k| format!("\"{}\"", data_encoding::HEXLOWER.encode(&k))),
            ));
        }
        out.push(']');
        out
    }

    /// Send a 1:1 text to a friend device. Returns the message id;
    /// `FriendDelivered` with it arrives when they have it. With no link the
    /// text waits in SQLite and goes out when one comes up.
    pub fn friend_send(&self, peer: &PeerId, text: &str) -> Result<u64> {
        self.check_open()?;
        if text.is_empty() {
            return Err(Error::Arg("empty message"));
        }
        if text.len() > MAX_MESSAGE {
            return Err(Error::TooLarge);
        }
        let peer = self.require_friend(peer)?;
        let id = loop {
            let id = u64::from_le_bytes(random::<8>());
            if id != 0 {
                break id;
            }
        };
        self.friend_cmd(FriendCmd::Text {
            peer,
            id,
            sent_at: now_ms(),
            body: text.to_string(),
        })?;
        Ok(id)
    }

    /// Unfriend. The other side is told and forgets us too.
    pub fn friend_remove(&self, peer: &PeerId) -> Result<()> {
        self.check_open()?;
        let peer = self.require_friend(peer)?;
        self.shared.friends.write().unwrap().remove(&peer);
        self.friend_cmd(FriendCmd::Remove { peer })
    }

    /// The last `limit` texts with a device, oldest first. **Reads SQLite**
    /// (a short indexed read on a WAL database).
    pub fn friend_history(&self, peer: &PeerId, limit: u32) -> Result<Vec<HistoryEntry>> {
        endpoint_id(peer)?;
        let store = self
            .shared
            .store
            .lock()
            .map_err(|_| Error::Store("store poisoned".into()))?;
        Ok(store
            .history(peer, limit)?
            .into_iter()
            .map(|m| HistoryEntry {
                id: m.id,
                outgoing: m.outgoing,
                text: m.body,
                sent_at: m.sent_at,
                delivered_at: m.delivered_at,
            })
            .collect())
    }

    pub fn friend_history_json(&self, peer: &PeerId, limit: u32) -> Result<String> {
        let mut out = String::from("[");
        for (i, m) in self.friend_history(peer, limit)?.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"id\":{},\"outgoing\":{},\"text\":{},\"sent_at\":{},\"delivered_at\":{}}}",
                m.id,
                m.outgoing,
                json_string(&m.text),
                m.sent_at,
                m.delivered_at.map_or("null".to_string(), |t| t.to_string()),
            ));
        }
        out.push(']');
        Ok(out)
    }

    /// Invite a linked friend into a room/channel. They get `ChannelInvite`
    /// with the ticket and decide whether to join.
    pub fn channel_invite(&self, peer: &PeerId, room: RoomHandle) -> Result<()> {
        let ticket = self.room_ticket(room)?;
        let peer = self.require_friend(peer)?;
        self.friend_cmd(FriendCmd::Send {
            peer,
            wire: Wire::ChannelInvite { ticket },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> EndpointAddr {
        EndpointAddr::from(iroh::SecretKey::generate().public())
    }

    #[test]
    fn every_wire_frame_round_trips() {
        let frames = [
            Wire::Redeem {
                secret: [1; 16],
                name: "Bob".into(),
                addr: addr(),
            },
            Wire::Welcome {
                name: "Alice".into(),
                addr: addr(),
            },
            Wire::Refused { reason: "no".into() },
            Wire::Hello {
                name: "A".into(),
                addr: addr(),
                status: 2,
                note: "brb".into(),
            },
            Wire::Heartbeat {
                status: 1,
                note: String::new(),
            },
            Wire::Text {
                id: u64::MAX,
                sent_at: 1,
                body: "hi".into(),
            },
            Wire::Ack { id: 7 },
            Wire::ChannelInvite {
                ticket: "lproomx".into(),
            },
            Wire::Unfriend,
        ];
        for f in frames {
            assert_eq!(decode(&encode(&f)), Some(f));
        }
    }

    #[test]
    fn other_versions_and_garbage_are_dropped() {
        let bytes = postcard::to_stdvec(&(PROTOCOL_VERSION + 1, Wire::Unfriend)).unwrap();
        assert_eq!(decode(&bytes), None);
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0xff; 40]), None);
    }

    #[test]
    fn names_and_notes_are_cleaned() {
        assert_eq!(clean("  Alice\u{7}\n ", MAX_NAME), "Alice");
        assert_eq!(clean(&"x".repeat(500), MAX_NAME).chars().count(), MAX_NAME);
        assert_eq!(clean("Café ☕", 4), "Café");
    }

    #[test]
    fn random_is_not_constant() {
        assert_ne!(random::<16>(), random::<16>());
    }
}
