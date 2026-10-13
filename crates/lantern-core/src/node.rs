//! The node: a synchronous handle in front of an async world.

#[cfg(feature = "calls")]
mod calls;
mod friends;
#[cfg(feature = "nostr")]
mod nostr;
mod selftest;
mod history;
mod sealed_room;

pub use friends::{FriendInfo, HistoryEntry, PreparedInvite};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant},
};

use iroh::{
    address_lookup::memory::MemoryLookup,
    endpoint::{presets, Connection},
    protocol::{AcceptError, ProtocolHandler, Router},
    Endpoint, EndpointAddr, EndpointId, RelayUrl, SecretKey,
};
use iroh_blobs::{api::Store as BlobStore, store::fs::FsStore, store::mem::MemStore, BlobsProtocol};
use iroh_gossip::{
    api::{Event as GossipEvent, GossipSender},
    net::Gossip,
    proto::TopicId,
};
use n0_future::StreamExt;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::{
    config::{Config, RelayMode},
    event::{BlobMeta, Event, PeerId},
    proto::{self, Frame},
    store::Store,
    ticket::{NodeTicket, RoomTicket, SealedRoomTicket, Scope},
    ratelimit::Limits,
    BlobHandle, ConnHandle, Error, Result, RoomHandle, GOSSIP_MAX_FRAME, MAX_MESSAGE,
    MAX_ROOM_MESSAGE,
};

// ---------------------------------------------------------------- statistics

#[derive(Debug, Default)]
pub struct Stats {
    pub bytes_sent: AtomicU64,
    pub bytes_recv: AtomicU64,
    pub events_dropped: AtomicU64,
    pub rate_limited: AtomicU64,
}

/// Snapshot handed to the UI once a frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub rooms: u32,
    pub peers: u32,
    pub direct_conns: u32,
    pub relayed_conns: u32,
    pub publishing_rooms: u32,
    pub events_dropped: u32,
    /// Frames dropped by the per-peer rate limits since open.
    pub rate_limited: u32,
}

// --------------------------------------------------------------- shared state

#[derive(Debug, Clone)]
struct RoomState {
    topic: TopicId,
    sealed: Option<SealedRoomTicket>,
    scope: Scope,
    label: String,
    peers: BTreeSet<EndpointId>,
    joined: bool,
    history_enabled: bool,
    history_request: Option<history::Request>,
    history_generation: Arc<AtomicU64>,
    /// Set for an author channel: only announcements signed by this key are
    /// surfaced, and nothing else from the room is.
    author: Option<[u8; 32]>,
    /// Blobs we are actively serving into this room.
    offering: BTreeSet<[u8; 32]>,
}

#[derive(Debug, Clone)]
struct SealedFile {
    ticket: SealedRoomTicket,
    encrypted_size: u64,
}

#[derive(Debug, Clone)]
struct BlobState {
    sealed: Option<SealedFile>,
    hash: Option<[u8; 32]>,
    name: String,
    size: u64,
    /// Where to fetch it from, for a blob we learned about from an offer.
    from: Option<EndpointId>,
    room: Option<RoomHandle>,
}

fn remote_offer_allowed(blobs: &HashMap<BlobHandle, BlobState>, room: RoomHandle,
    from: EndpointId, hash: [u8; 32], name: &str) -> bool {
    !name.is_empty() && name.len() <= 1024 && !name.chars().any(char::is_control)
        && blobs.values().filter(|s| s.from.is_some()).count() < 128
        && blobs.values().filter(|s| s.from == Some(from)).count() < 8
        && !blobs.values().any(|s| s.from == Some(from) && s.room == Some(room) && s.hash == Some(hash))
}

struct FileJob {
    shared: Arc<Shared>,
    blob: BlobHandle,
    cancel: tokio::sync::watch::Receiver<bool>,
}
impl Drop for FileJob {
    fn drop(&mut self) { self.shared.file_jobs.lock().unwrap().remove(&self.blob); }
}

// A failed/canceled save never leaves a partial file after the owning task exits.
struct PartialFile { path: PathBuf, keep: bool, file: Option<tokio::fs::File> }
impl Drop for PartialFile {
    fn drop(&mut self) {
        self.file.take();
        if !self.keep { let _ = std::fs::remove_file(&self.path); }
    }
}

struct Shared {
    app_id: String,
    endpoint: Endpoint,
    secret: SecretKey,
    rooms: RwLock<HashMap<RoomHandle, RoomState>>,
    conns: RwLock<HashMap<ConnHandle, EndpointId>>,
    blobs: RwLock<HashMap<BlobHandle, BlobState>>,
    file_downloads: Arc<tokio::sync::Semaphore>,
    file_jobs: Mutex<HashMap<BlobHandle, (EndpointId, tokio::sync::watch::Sender<bool>)>>,
    store: Mutex<Store>,
    /// At most four bounded history fetches per node, including all rooms.
    history_downloads: tokio::sync::Semaphore,
    history_serving: tokio::sync::Semaphore,
    history_cache: Mutex<history::Cache>,
    history_alpn: Vec<u8>,
    history_revision: AtomicU64,
    history_metadata: AtomicBool,
    history_events: Mutex<history::delivery::Delivery>,
    poll_history_turn: AtomicBool,
    next: AtomicU64,
    stats: Stats,
    ev: Mutex<std::sync::mpsc::SyncSender<Event>>,
    /// Set by `close()`; background loops check it and stop.
    closed: AtomicBool,
    lookup: MemoryLookup,
    friend_alpn: Vec<u8>,
    heartbeat: Duration,
    /// Friend devices, loaded from SQLite at open and kept in step with it.
    friends: RwLock<HashMap<EndpointId, friends::FriendDevice>>,
    /// The live friend link per device, whichever side dialed.
    links: Mutex<HashMap<EndpointId, Connection>>,
    /// Open invites: secret -> expiry (unix seconds, 0 = never).
    invites: Mutex<HashMap<[u8; 16], i64>>,
    /// Calls being set up or in progress (spike, feature `calls`): id -> peer.
    #[cfg_attr(not(feature = "calls"), allow(dead_code))]
    calls: Mutex<HashMap<u64, EndpointId>>,
    me: RwLock<friends::Me>,
    /// Refused at every door: friend links, direct connections, room frames.
    blocked: RwLock<HashSet<EndpointId>>,
    limits: Mutex<Limits>,
    /// Nodes whose `SupportReply` frames are accepted from outside the friend
    /// list: the author's nodes, as configured by the plugin.
    support_contacts: RwLock<HashSet<EndpointId>>,
    /// Author mode: accept support messages from anyone (rate limited).
    accept_support: bool,
    selftest_alpn: Vec<u8>,
}

impl Shared {
    /// All normal room frame paths go through the room's immutable sealing mode.
    fn encode_room(&self, room: RoomHandle, frame: &Frame) -> Result<Vec<u8>> {
        let sealed = self.rooms.read().unwrap().get(&room).ok_or(Error::NotFound)?.sealed.clone();
        match sealed {
            Some(ticket) => proto::encode_sealed(&self.secret, &ticket, frame),
            None if matches!(frame, Frame::BlobOffer { .. } | Frame::BlobUnoffer { .. }) =>
                proto::encode_file_offer(&self.secret, frame).ok_or(Error::State("file offer randomness unavailable")),
            None => Ok(proto::encode(&self.secret, frame)),
        }
    }

    fn forget_remote_blobs(&self, remove: impl Fn(&BlobState) -> bool) {
        let removed = {
            let mut blobs = self.blobs.write().unwrap();
            let handles: Vec<_> = blobs.iter().filter(|(_, s)| s.from.is_some() && remove(s)).map(|(h, _)| *h).collect();
            for handle in &handles { blobs.remove(handle); }
            handles
        };
        let jobs = self.file_jobs.lock().unwrap();
        for blob in removed {
            if let Some((_, cancel)) = jobs.get(&blob) { let _ = cancel.send(true); }
            self.emit(Event::BlobWithdrawn { blob });
        }
    }
    fn handle(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Never blocks and never fails loudly: a caller that stops polling must not
    /// be able to wedge the network tasks.
    fn emit(&self, event: Event) {
        if matches!(&event, Event::ChannelRecord { .. } | Event::ChannelHistoryCleared { .. }) {
            if self.history_events.lock().unwrap().push(event) {
                self.stats.events_dropped.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        let tx = self.ev.lock().expect("event sender poisoned");
        if tx.try_send(event).is_err() {
            self.stats.events_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn log(&self, message: impl Into<String>) {
        self.emit(Event::Log {
            message: message.into(),
        });
    }

    fn error(&self, message: impl Into<String>) {
        self.emit(Event::Error {
            message: message.into(),
        });
    }

    fn is_blocked(&self, peer: &EndpointId) -> bool {
        self.blocked.read().unwrap().contains(peer)
    }

    /// Spend a token from `peer`'s bucket for `what`; on refusal, maybe tell
    /// the UI (once per peer per `NOTICE_EVERY`).
    fn allow(&self, peer: &EndpointId, what: &'static str) -> bool {
        let now = Instant::now();
        let mut limits = self.limits.lock().unwrap();
        let ok = match what {
            "room" => limits.room.allow(peer.as_bytes(), now),
            "stranger" => limits.stranger.allow(peer.as_bytes(), now),
            _ => limits.friend.allow(peer.as_bytes(), now),
        };
        if !ok {
            self.stats.rate_limited.fetch_add(1, Ordering::Relaxed);
            if limits.should_warn(peer.as_bytes(), now) {
                drop(limits);
                self.emit(Event::RateLimited {
                    peer: *peer.as_bytes(),
                    what: what.to_string(),
                });
            }
        }
        ok
    }
}

// -------------------------------------------------------------------- commands

enum Cmd {
    RoomJoin {
        room: RoomHandle,
        topic: TopicId,
        bootstrap: Vec<EndpointAddr>,
    },
    RoomLeave(RoomHandle),
    HistoryEnable { room: RoomHandle, enabled: bool },
    HistoryClear(RoomHandle),
    HistoryPublish { room: RoomHandle, text: String },
    HistoryFetch { room: RoomHandle, provider: EndpointId, hash: [u8; 32], size: u64 },
    RoomBroadcast {
        room: RoomHandle,
        frame: Frame,
    },
    Connect {
        conn: ConnHandle,
        addr: EndpointAddr,
    },
    Send {
        conn: ConnHandle,
        data: Vec<u8>,
    },
    Disconnect(ConnHandle),
    Friend(friends::FriendCmd),
    BlobAddBytes {
        blob: BlobHandle,
        data: Vec<u8>,
    },
    BlobAddSealedBytes { blob: BlobHandle, data: Vec<u8>, ticket: SealedRoomTicket },
    BlobAddFile {
        blob: BlobHandle,
        path: PathBuf,
    },
    BlobFetch {
        blob: BlobHandle,
        hash: [u8; 32],
        from: Option<EndpointId>,
        out: PathBuf,
    },
    BlobFetchBounded {
        blob: BlobHandle,
        hash: [u8; 32],
        from: EndpointId,
        size: u64,
        sealed: Option<SealedFile>,
        out: PathBuf,
        permit: tokio::sync::OwnedSemaphorePermit,
        job: FileJob,
    },
    Shutdown,
}

// ------------------------------------------------------------- direct protocol

#[derive(Debug, Clone)]
struct DirectProtocol {
    shared: Arc<SharedRef>,
}

/// `Shared` is not `Debug`, and `ProtocolHandler` wants `Debug`. Wrapping it
/// keeps the derive honest without spilling keys into a log line.
struct SharedRef(Arc<Shared>, Arc<Mutex<HashMap<ConnHandle, Connection>>>);

impl std::fmt::Debug for SharedRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("lantern")
    }
}

async fn pump_connection(
    shared: Arc<Shared>,
    conns: Arc<Mutex<HashMap<ConnHandle, Connection>>>,
    connection: Connection,
    handle: ConnHandle,
) {
    let peer = connection.remote_id();
    let peer_bytes: PeerId = *peer.as_bytes();
    shared.conns.write().unwrap().insert(handle, peer);
    conns.lock().unwrap().insert(handle, connection.clone());
    shared.emit(Event::Connected {
        conn: handle,
        peer: peer_bytes,
    });

    loop {
        match connection.accept_uni().await {
            Ok(mut recv) => match recv.read_to_end(MAX_MESSAGE).await {
                Ok(data) => {
                    shared
                        .stats
                        .bytes_recv
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    shared.emit(Event::Direct {
                        conn: handle,
                        peer: peer_bytes,
                        data,
                    });
                }
                Err(e) => {
                    shared.error(format!("direct read failed: {e}"));
                    break;
                }
            },
            Err(_) => break,
        }
    }

    conns.lock().unwrap().remove(&handle);
    shared.conns.write().unwrap().remove(&handle);
    shared.emit(Event::Disconnected {
        conn: handle,
        peer: peer_bytes,
    });
}

impl ProtocolHandler for DirectProtocol {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        let shared = self.shared.0.clone();
        if shared.is_blocked(&connection.remote_id()) {
            connection.close(1u32.into(), b"blocked");
            return Ok(());
        }
        let conns = self.shared.1.clone();
        let handle = shared.handle();
        pump_connection(shared, conns, connection, handle).await;
        Ok(())
    }
}

// ------------------------------------------------------------------ the runner

struct Runner {
    shared: Arc<Shared>,
    gossip: Gossip,
    blobs: BlobStore,
    lookup: MemoryLookup,
    alpn: Vec<u8>,
    senders: HashMap<RoomHandle, GossipSender>,
    tasks: HashMap<RoomHandle, tokio::task::JoinHandle<()>>,
    conns: Arc<Mutex<HashMap<ConnHandle, Connection>>>,
    presence: Arc<RwLock<HashMap<RoomHandle, Vec<u8>>>>,
}

impl Runner {
    async fn run(mut self, mut rx: UnboundedReceiver<Cmd>) {
        let mut history_maintenance = tokio::time::interval(Duration::from_secs(60));
        history_maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut history_retry = tokio::time::interval(Duration::from_secs(2));
        history_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = history_retry.tick() => history::tick(&self.shared, &self.senders).await,
                cmd = rx.recv() => match cmd {
                    None | Some(Cmd::Shutdown) => break,
                    Some(other) => self.handle(other).await,
                },
                _ = history_maintenance.tick() => {
                    if let Err(e) = self.shared.store.lock().unwrap()
                        .channel_history_prune(crate::store::now_ms()) {
                        self.shared.error(format!("history maintenance failed: {e}"));
                    }
                    if let Err(e) = self.shared.store.lock().unwrap()
                        .channel_peer_hint_prune(crate::store::now_ms()) {
                        self.shared.error(format!("provider hint maintenance failed: {e}"));
                    }
                }
            }
        }
        for (_, task) in self.tasks.drain() {
            task.abort();
        }
    }

    async fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Shutdown => {}
            Cmd::RoomJoin {
                room,
                topic,
                bootstrap,
            } => self.room_join(room, topic, bootstrap).await,
            Cmd::RoomLeave(room) => self.room_leave(room),
            Cmd::HistoryClear(room) => {
                let topic = {
                    let mut rooms = self.shared.rooms.write().unwrap();
                    rooms.get_mut(&room).map(|s| { s.history_request = None; (s.topic, s.history_generation.clone()) })
                };
                if let Some((topic, generation)) = topic {
                    let mut store = self.shared.store.lock().unwrap();
                    generation.fetch_add(1, Ordering::Relaxed);
                    if let Err(e) = store.channel_history_clear(topic.as_bytes()) {
                        self.shared.error(format!("history clear failed: {e}"));
                        return;
                    }
                    self.shared.history_cache.lock().unwrap().leave(room);
                    self.shared.history_revision.fetch_add(1, Ordering::Relaxed);
                    self.shared.emit(Event::ChannelHistoryCleared { room });
                }
            }
            Cmd::HistoryEnable { room, enabled } => {
                if let Some(state) = self.shared.rooms.write().unwrap().get_mut(&room) {
                    state.history_enabled = enabled;
                    state.history_request = None;
                    state.history_generation.fetch_add(1, Ordering::Relaxed);
                }
                if !enabled { self.shared.history_cache.lock().unwrap().leave(room); }
                if enabled {
                    let topic = self.shared.rooms.read().unwrap().get(&room).map(|s| s.topic);
                    if let Some(topic) = topic {
                        let records = self.shared.store.lock().unwrap().channel_history_read(topic.as_bytes(), crate::store::now_ms());
                        match records {
                            Ok(records) => for record in records { history::emit(&self.shared, room, &record, true); },
                            Err(e) => self.shared.error(format!("history load failed: {e}")),
                        }
                    }
                }
                if let Some(tx) = self.senders.get(&room) {
                    history::request(&self.shared, room, tx, true).await;
                }
            }
            Cmd::HistoryFetch { room, provider, hash, size } => {
                let state = self.shared.rooms.read().unwrap().get(&room).map(|s| (s.topic, s.history_generation.clone(), s.history_generation.load(Ordering::Relaxed), s.sealed.clone()));
                let Some((topic, generation, expected_generation, sealed)) = state else { return; };
                let shared = self.shared.clone();
                tokio::spawn(async move {
                    let records = match crate::channel_history::network::fetch_snapshot_over(
                        &shared.endpoint, &shared.history_downloads, provider, hash, size, topic.as_bytes(),
                        iroh_blobs::ALPN, sealed.as_ref(),
                    ).await {
                        Ok(records) => records,
                        Err(e) => { shared.error(format!("history fetch failed: {e}")); return; }
                    };
                    if shared.closed.load(Ordering::Relaxed) || shared.is_blocked(&provider) || !shared.rooms.read().unwrap().contains_key(&room) {
                        return;
                    }
                    let inserted = match history::ingest_snapshot(&shared, room, &topic, &generation, expected_generation, records) {
                        Ok(Some(inserted)) => inserted,
                        Ok(None) => return,
                        Err(e) => { shared.error(format!("history storage failed: {e}")); return; }
                    };
                    // Historical data must never be emitted as live Message events:
                    // that would replay pull timers and channel notifications.
                    shared.emit(Event::Log { message: format!("channel history fetched: {inserted} new verified records") });
                });
            }
            Cmd::HistoryPublish { room, text } => {
                let topic = self.shared.rooms.read().unwrap().get(&room).map(|s| s.topic);
                let Some(topic) = topic else { return; };
                let received_at = crate::store::now_ms();
                let record = match crate::channel_history::sign(
                    &self.shared.secret, *topic.as_bytes(), received_at, &text,
                ) {
                    Ok(record) => record,
                    Err(e) => { self.shared.error(format!("history signing failed: {e}")); return; }
                };
                if let Err(e) = self.shared.store.lock().unwrap().channel_history_insert(
                    topic.as_bytes(), &record, received_at,
                ) {
                    self.shared.error(format!("history storage failed: {e}"));
                    return;
                }
                self.shared.history_revision.fetch_add(1, Ordering::Relaxed);
                if let Some(verified) = crate::channel_history::verify_sender(topic.as_bytes(), self.shared.secret.public(), &record) {
                    history::emit(&self.shared, room, &verified, false);
                }
                if let Some(tx) = self.senders.get(&room) {
                    let sealed = self.shared.rooms.read().unwrap().get(&room).and_then(|s| s.sealed.clone());
                    let encoded = match sealed {
                        Some(ticket) => proto::encode_sealed(&self.shared.secret, &ticket, &Frame::HistoryRecord(record)).ok(),
                        None => proto::encode_history(&self.shared.secret, topic.as_bytes(), &record),
                    };
                    let Some(bytes) = encoded else {
                        self.shared.error("history encoding failed");
                        return;
                    };
                    self.shared.stats.bytes_sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    if let Err(e) = tx.broadcast(bytes.into()).await {
                        self.shared.error(format!("broadcast failed: {e}"));
                    }
                }
            }
            Cmd::RoomBroadcast { room, frame } => {
                let bytes = match self.shared.encode_room(room, &frame) {
                    Ok(bytes) => bytes,
                    Err(error) => { self.shared.error(format!("room encoding failed: {error}")); return; }
                };
                self.shared
                    .stats
                    .bytes_sent
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                if let Some(tx) = self.senders.get(&room) {
                    if let Err(e) = tx.broadcast(bytes.into()).await {
                        self.shared.error(format!("broadcast failed: {e}"));
                    }
                }
            }
            Cmd::Connect { conn, addr } => self.connect(conn, addr).await,
            Cmd::Send { conn, data } => self.send(conn, data).await,
            Cmd::Disconnect(conn) => {
                if let Some(c) = self.conns.lock().unwrap().remove(&conn) {
                    c.close(0u32.into(), b"bye");
                }
            }
            Cmd::Friend(cmd) => friends::handle(&self.shared, cmd),
            Cmd::BlobAddBytes { blob, data } => self.blob_add_bytes(blob, data).await,
            Cmd::BlobAddSealedBytes { blob, data, ticket } => {
                let size = data.len() as u64;
                let result = match crate::sealed_file::encode(&self.shared.secret, &ticket, &data) {
                    Ok(encrypted) => {
                        if let Some(state) = self.shared.blobs.write().unwrap().get_mut(&blob) {
                            state.sealed = Some(SealedFile { ticket, encrypted_size: encrypted.len() as u64 });
                        }
                        self.blobs.add_bytes(encrypted).await.map(|tag| *tag.hash.as_bytes()).map_err(|e| e.to_string())
                    }
                    Err(error) => Err(error.to_string()),
                };
                self.finish_add(blob, result, size).await;
            },
            Cmd::BlobAddFile { blob, path } => self.blob_add_file(blob, path).await,
            Cmd::BlobFetch {
                blob,
                hash,
                from,
                out,
            } => self.blob_fetch(blob, hash, from, out).await,
            Cmd::BlobFetchBounded { blob, hash, from, size, sealed, out, permit, mut job } => {
                let shared = self.shared.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result: anyhow::Result<()> = async {
                        if shared.is_blocked(&from) { anyhow::bail!("file sender is blocked"); }
                        if *job.cancel.borrow() { anyhow::bail!("file download canceled"); }
                        let (wire_size, maximum) = match &sealed {
                            Some(file) => (file.encrypted_size, crate::sealed_file::MAX_CIPHERTEXT_BYTES),
                            None => (size, crate::bounded_blob::MAX_FILE_BYTES),
                        };
                        let bytes = tokio::select! {
                            biased;
                            _ = job.cancel.changed() => { anyhow::bail!("file download canceled"); }
                            result = crate::bounded_blob::fetch(&shared.endpoint, from.into(), hash,
                                wire_size, maximum, iroh_blobs::ALPN) => result?,
                        };
                        let bytes = match &sealed {
                            Some(file) => crate::sealed_file::decode(&file.ticket, from, &bytes, size)
                                .map_err(|error| anyhow::anyhow!(error.to_string()))?,
                            None => bytes,
                        };
                        if *job.cancel.borrow() { anyhow::bail!("file download canceled"); }
                        if shared.is_blocked(&from) { anyhow::bail!("file sender is blocked"); }
                        // Never overwrite an existing file or follow its final symlink.
                        let file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&out).await?;
                        let mut partial = PartialFile { path: out.clone(), keep: false, file: Some(file) };
                        use tokio::io::AsyncWriteExt;
                        let saved: anyhow::Result<()> = async {
                            if *job.cancel.borrow() { anyhow::bail!("file download canceled"); }
                            tokio::select! {
                                biased;
                                _ = job.cancel.changed() => { anyhow::bail!("file download canceled"); }
                                result = async {
                                    let file = partial.file.as_mut().unwrap();
                                    file.write_all(&bytes).await?; file.sync_all().await
                                } => { result?; }
                            }
                            if *job.cancel.borrow() || shared.is_blocked(&from) { anyhow::bail!("file download canceled"); }
                            Ok(())
                        }.await;
                        // On Windows the file must close before the partial guard removes it.
                        partial.file.take();
                        saved?;
                        partial.keep = true;
                        shared.emit(Event::BlobProgress { blob, done: size, total: size });
                        Ok(())
                    }.await;
                    match result {
                        Ok(()) => shared.emit(Event::BlobDone { blob, detail: out.display().to_string() }),
                        Err(error) => shared.emit(Event::BlobFailed { blob, reason: error.to_string() }),
                    }
                });
            }
        }
    }

    async fn room_join(&mut self, room: RoomHandle, topic: TopicId, mut bootstrap: Vec<EndpointAddr>) {
        let sealed = self.shared.rooms.read().unwrap().get(&room).and_then(|s| s.sealed.clone());
        // Persist a key fingerprint before subscription: an ordinary join must
        // never expose this topic's saved sealed history, including after restart.
        let registered = sealed_room::register(&mut self.shared.store.lock().unwrap(), &topic, sealed.as_ref());
        if let Err(error) = registered {
            self.shared.error(format!("room sealing admission failed: {error}"));
            self.shared.rooms.write().unwrap().remove(&room);
            self.shared.history_events.lock().unwrap().forget(room);
            return;
        }
        bootstrap.extend(match &sealed {
            Some(ticket) => history::known_providers_sealed(&self.shared, &topic, Some(ticket)),
            None => history::known_providers(&self.shared, &topic),
        });
        let mut ids = Vec::new();
        for addr in bootstrap {
            if self.shared.is_blocked(&addr.id) || ids.contains(&addr.id) { continue; }
            ids.push(addr.id);
            self.lookup.add_endpoint_info(addr);
        }
        let handle = match self.gossip.subscribe(topic, ids).await {
            Ok(h) => h,
            Err(e) => {
                self.shared.error(format!("room join failed: {e}"));
                self.shared.rooms.write().unwrap().remove(&room);
                return;
            }
        };
        let (tx, mut rx) = handle.split();
        self.senders.insert(room, tx.clone());
        if let Some(state) = self.shared.rooms.write().unwrap().get_mut(&room) {
            state.joined = true;
        }
        self.shared.emit(Event::RoomJoined { room });

        let shared = self.shared.clone();
        let presence = self.presence.clone();
        let secret = self.shared.secret.clone();
        let author = self.shared.rooms.read().unwrap().get(&room).and_then(|s| s.author);
        let task = tokio::spawn(async move {
            let mut replay = sealed_room::Replay::default();
            while let Some(next) = rx.next().await {
                let event = match next {
                    Ok(e) => e,
                    Err(e) => {
                        shared.error(format!("room stream ended: {e}"));
                        break;
                    }
                };
                match event {
                    GossipEvent::NeighborUp(peer) => {
                        shared
                            .rooms
                            .write()
                            .unwrap()
                            .get_mut(&room)
                            .map(|s| s.peers.insert(peer));
                        if let Some(author) = author {
                            // An author channel shows nobody to anybody; it
                            // only passes the latest announcements along.
                            for signed in author_reoffer(&shared, &author) {
                                let bytes = proto::encode(&secret, &Frame::App(signed));
                                let _ = tx.broadcast_neighbors(bytes.into()).await;
                            }
                            continue;
                        }
                        if shared.is_blocked(&peer) {
                            continue;
                        }
                        shared.emit(Event::PeerJoined {
                            room,
                            peer: *peer.as_bytes(),
                        });
                        // Tell the newcomer who we are. Presence is not stored
                        // by the swarm, so every node re-announces itself.
                        let mine = presence
                            .read()
                            .unwrap()
                            .get(&room)
                            .cloned()
                            .unwrap_or_default();
                        if let Ok(bytes) = shared.encode_room(room, &Frame::Hello { presence: mine }) {
                            let _ = tx.broadcast_neighbors(bytes.into()).await;
                        }
                        history::request(&shared, room, &tx, false).await;
                    }
                    GossipEvent::NeighborDown(peer) => {
                        shared
                            .rooms
                            .write()
                            .unwrap()
                            .get_mut(&room)
                            .map(|s| s.peers.remove(&peer));
                        if author.is_some() || shared.is_blocked(&peer) {
                            continue;
                        }
                        shared.emit(Event::PeerLeft {
                            room,
                            peer: *peer.as_bytes(),
                        });
                    }
                    GossipEvent::Lagged => {
                        shared.log("room fell behind; some messages were skipped");
                    }
                    GossipEvent::Received(msg) => {
                        shared
                            .stats
                            .bytes_recv
                            .fetch_add(msg.content.len() as u64, Ordering::Relaxed);
                        let decoded = match &sealed {
                            Some(ticket) => proto::decode_sealed(ticket, &msg.content),
                            None => proto::decode(&msg.content),
                        };
                        let Some((from, frame)) = decoded else {
                            // Unsigned, forged or from a future protocol.
                            continue;
                        };
                        if shared.is_blocked(&from) || !shared.allow(&from, "room") {
                            continue;
                        }
                        if sealed.is_some() && !replay.admit(&msg.content) { continue; }
                        if let Some(author) = author {
                            if let Frame::App(data) = frame {
                                author_frame(&shared, room, &author, &data);
                            }
                            continue;
                        }
                        let peer: PeerId = *from.as_bytes();
                        match frame {
                            Frame::App(data) => {
                                shared.emit(Event::Message { room, peer, data })
                            }
                            Frame::Presence(data) => {
                                shared.emit(Event::Presence { room, peer, data })
                            }
                            // A greeting from a peer that has set no presence
                            // carries nothing; saying so would only make the
                            // caller render an empty row.
                            Frame::Hello { presence } if presence.is_empty() => {}
                            Frame::Hello { presence } => shared.emit(Event::Presence {
                                room,
                                peer,
                                data: presence,
                            }),
                            Frame::BlobOffer { hash, size, name } => {
                                // Sealed file-byte encryption is still being wired.
                                if sealed.is_some() { continue; }
                                // Keep room admission through insertion/emission so a
                                // concurrent leave cannot be followed by a stale offer.
                                let blocked = shared.blocked.read().unwrap();
                                if blocked.contains(&from) { continue; }
                                let rooms = shared.rooms.read().unwrap();
                                if !rooms.contains_key(&room) { continue; }
                                let mut blobs = shared.blobs.write().unwrap();
                                if !remote_offer_allowed(&blobs, room, from, hash, &name) { continue; }
                                let blob = shared.handle();
                                blobs.insert(
                                    blob,
                                    BlobState {
                                        sealed: None,
                                        hash: Some(hash),
                                        name: name.clone(),
                                        size,
                                        from: Some(from),
                                        room: Some(room),
                                    },
                                );
                                drop(blobs);
                                shared.emit(Event::BlobOffer {
                                    room,
                                    peer,
                                    blob,
                                    meta: BlobMeta { hash, size, name },
                                });
                                drop(rooms);
                                drop(blocked);
                            }
                            Frame::SealedBlobOffer { hash, size, encrypted_size, name } => {
                                let Some(ticket) = &sealed else { continue; };
                                if size > crate::bounded_blob::MAX_FILE_BYTES
                                    || encrypted_size < size || encrypted_size > size.saturating_add(256)
                                    || encrypted_size > crate::sealed_file::MAX_CIPHERTEXT_BYTES { continue; }
                                let blocked = shared.blocked.read().unwrap();
                                if blocked.contains(&from) { continue; }
                                let rooms = shared.rooms.read().unwrap();
                                if !rooms.contains_key(&room) { continue; }
                                let mut blobs = shared.blobs.write().unwrap();
                                if !remote_offer_allowed(&blobs, room, from, hash, &name) { continue; }
                                let blob = shared.handle();
                                blobs.insert(blob, BlobState {
                                    sealed: Some(SealedFile { ticket: ticket.clone(), encrypted_size }),
                                    hash: Some(hash), name: name.clone(), size, from: Some(from), room: Some(room),
                                });
                                drop(blobs);
                                shared.emit(Event::BlobOffer { room, peer, blob, meta: BlobMeta { hash, size, name } });
                            }
                            Frame::BlobUnoffer { hash } => shared.forget_remote_blobs(|s|
                                s.room == Some(room) && s.from == Some(from) && s.hash == Some(hash)),
                            Frame::HistoryRequest { topic: frame_topic, nonce, attempt } => history::offer(&shared, room, &tx, from, frame_topic, nonce, attempt).await,
                            Frame::HistoryOffer { topic: frame_topic, to, nonce, attempt, hash, size, provider } => history::receive_offer(shared.clone(), room, from, frame_topic, to, nonce, attempt, hash, size, provider, &msg.content),
                            Frame::HistoryRecord(encoded) => {
                                let Some(record) = crate::channel_history::verify_sender(topic.as_bytes(), from, &encoded) else { continue; };
                                let stored = shared.store.lock().unwrap().channel_history_insert(
                                    topic.as_bytes(), &encoded, crate::store::now_ms(),
                                );
                                match stored {
                                    Ok(true) => {
                                        shared.history_revision.fetch_add(1, Ordering::Relaxed);
                                        if !history::emit(&shared, room, &record, false) {
                                            shared.emit(Event::Message { room, peer, data: record.text().as_bytes().to_vec() });
                                        }
                                    },
                                    Ok(false) => {},
                                    Err(e) => shared.error(format!("history storage failed: {e}")),
                                }
                            }
                        }
                    }
                }
            }
        });
        self.tasks.insert(room, task);
    }

    fn room_leave(&mut self, room: RoomHandle) {
        self.shared.history_cache.lock().unwrap().leave(room);
        self.shared.history_events.lock().unwrap().forget(room);
        self.senders.remove(&room);
        if let Some(task) = self.tasks.remove(&room) {
            task.abort();
        }
        if let Some(state) = self.shared.rooms.write().unwrap().remove(&room) {
            state.history_generation.fetch_add(1, Ordering::Relaxed);
        }
        self.shared.forget_remote_blobs(|s| s.room == Some(room));
        self.presence.write().unwrap().remove(&room);
        self.shared.emit(Event::RoomLeft { room });
    }

    async fn connect(&mut self, conn: ConnHandle, addr: EndpointAddr) {
        let endpoint = self.shared.endpoint.clone();
        let alpn = self.alpn.clone();
        let shared = self.shared.clone();
        let conns = self.conns.clone();
        tokio::spawn(async move {
            match endpoint.connect(addr, &alpn).await {
                Ok(connection) => pump_connection(shared, conns, connection, conn).await,
                Err(e) => {
                    shared.error(format!("connect failed: {e}"));
                    shared.emit(Event::Disconnected {
                        conn,
                        peer: [0u8; 32],
                    });
                }
            }
        });
    }

    async fn send(&mut self, conn: ConnHandle, data: Vec<u8>) {
        let connection = self.conns.lock().unwrap().get(&conn).cloned();
        let Some(connection) = connection else {
            self.shared.error("send to a connection that is not open");
            return;
        };
        let shared = self.shared.clone();
        tokio::spawn(async move {
            match connection.open_uni().await {
                Ok(mut send) => {
                    if let Err(e) = send.write_all(&data).await {
                        shared.error(format!("direct send failed: {e}"));
                        return;
                    }
                    let _ = send.finish();
                    shared
                        .stats
                        .bytes_sent
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                Err(e) => shared.error(format!("could not open a stream: {e}")),
            }
        });
    }

    async fn blob_add_bytes(&mut self, blob: BlobHandle, data: Vec<u8>) {
        let size = data.len() as u64;
        let added = self
            .blobs
            .add_bytes(data)
            .await
            .map(|tag| *tag.hash.as_bytes())
            .map_err(|e| e.to_string());
        self.finish_add(blob, added, size).await;
    }

    async fn blob_add_file(&mut self, blob: BlobHandle, path: PathBuf) {
        let size = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
        let added = self
            .blobs
            .add_path(path)
            .await
            .map(|tag| *tag.hash.as_bytes())
            .map_err(|e| e.to_string());
        self.finish_add(blob, added, size).await;
    }

    async fn finish_add(
        &mut self,
        blob: BlobHandle,
        added: std::result::Result<[u8; 32], String>,
        size: u64,
    ) {
        match added {
            Ok(hash) => {
                let name = {
                    let mut blobs = self.shared.blobs.write().unwrap();
                    if let Some(state) = blobs.get_mut(&blob) {
                        state.hash = Some(hash);
                        state.size = size;
                        state.name.clone()
                    } else {
                        String::new()
                    }
                };
                if let Ok(mut store) = self.shared.store.lock() {
                    let _ = store.blob_note(&hash, &name, size);
                }
                self.shared.emit(Event::BlobDone {
                    blob,
                    detail: data_encoding::HEXLOWER.encode(&hash),
                });
            }
            Err(reason) => self.shared.emit(Event::BlobFailed { blob, reason }),
        }
    }

    async fn blob_fetch(
        &mut self,
        blob: BlobHandle,
        hash: [u8; 32],
        from: Option<EndpointId>,
        out: PathBuf,
    ) {
        let store = self.blobs.clone();
        let endpoint = self.shared.endpoint.clone();
        let shared = self.shared.clone();
        tokio::spawn(async move {
            let hash = iroh_blobs::Hash::from(hash);
            let downloader = store.downloader(&endpoint);
            let progress = downloader.download(hash, from);
            match progress.stream().await {
                Ok(mut stream) => {
                    while let Some(item) = stream.next().await {
                        match item {
                            iroh_blobs::api::downloader::DownloadProgressItem::Progress(done) => {
                                shared.emit(Event::BlobProgress {
                                    blob,
                                    done,
                                    total: 0,
                                });
                            }
                            iroh_blobs::api::downloader::DownloadProgressItem::Error(e) => {
                                shared.emit(Event::BlobFailed {
                                    blob,
                                    reason: format!("{e}"),
                                });
                                return;
                            }
                            iroh_blobs::api::downloader::DownloadProgressItem::DownloadError => {
                                shared.emit(Event::BlobFailed {
                                    blob,
                                    reason: "download failed".into(),
                                });
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    shared.emit(Event::BlobFailed {
                        blob,
                        reason: format!("{e}"),
                    });
                    return;
                }
            }
            match store.export(hash, &out).await {
                Ok(_) => {
                    shared
                        .stats
                        .bytes_recv
                        .fetch_add(0, Ordering::Relaxed);
                    shared.emit(Event::BlobDone {
                        blob,
                        detail: out.display().to_string(),
                    });
                }
                Err(e) => shared.emit(Event::BlobFailed {
                    blob,
                    reason: format!("export: {e}"),
                }),
            }
        });
    }
}

// --------------------------------------------------------------------- Node

/// A running node. Every method below is non-blocking unless it says otherwise.
pub struct Node {
    rt: Option<tokio::runtime::Runtime>,
    cmd: UnboundedSender<Cmd>,
    events: Mutex<std::sync::mpsc::Receiver<Event>>,
    shared: Arc<Shared>,
    presence: Arc<RwLock<HashMap<RoomHandle, Vec<u8>>>>,
    blob_owner: Mutex<Option<BlobOwner>>,
    closed: AtomicBool,
    _router: Mutex<Option<Router>>,
    /// The relay mode the endpoint was built with (`None`: relays off), for
    /// the selftest's probes.
    relay_mode: Option<iroh::RelayMode>,
    accept_inbound: bool,
}

/// Held only so the store's actor lives as long as the node does. Dropping it
/// is how the blob store shuts down, which is why `close()` takes it.
enum BlobOwner {
    Mem(#[allow(dead_code)] MemStore),
    Fs(#[allow(dead_code)] FsStore),
}

impl Node {
    /// **Blocks.** Starts the runtime, opens SQLite and binds the endpoint.
    /// Call it off the render thread.
    pub fn open(config: Config) -> Result<Node> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("lantern")
            .enable_all()
            .build()
            .map_err(|e| Error::Internal(format!("runtime: {e}")))?;

        let mut store = Store::open(config.db_path.as_deref())?;
        let secret = store.identity()?;
        store.channel_peer_hint_prune(crate::store::now_ms())?;

        let (ev_tx, ev_rx) = std::sync::mpsc::sync_channel(config.event_queue_cap.max(16));
        let lookup = MemoryLookup::new();

        // With relays disabled the promise is "nothing touches a third party",
        // so the n0 preset (which publishes to and resolves from n0's DNS) is
        // not used at all. A custom relay URL that does not parse is an error,
        // never a silent fallback to somebody else's relay.
        let relay_mode = match &config.relay {
            RelayMode::Disabled => None,
            RelayMode::Default => Some(iroh::RelayMode::Default),
            RelayMode::Custom(url) => {
                let url: RelayUrl = url.parse().map_err(|_| Error::Arg("relay url did not parse"))?;
                Some(iroh::RelayMode::Custom(url.into()))
            }
        };
        let mut builder = match &relay_mode {
            None => Endpoint::builder(presets::Minimal).relay_mode(iroh::RelayMode::Disabled),
            Some(mode) => Endpoint::builder(presets::N0).relay_mode(mode.clone()),
        };
        builder = builder
            .secret_key(secret.clone())
            .address_lookup(lookup.clone());
        if let Some(port) = config.bind_port {
            builder = builder
                .clear_ip_transports()
                .bind_addr(SocketAddr::from(([0, 0, 0, 0], port)))
                .map_err(|e| Error::Net(format!("bind address: {e}")))?;
        }
        let endpoint = rt
            .block_on(builder.bind())
            .map_err(|e| Error::Net(format!("bind: {e}")))?;

        let node_id: PeerId = *endpoint.id().as_bytes();

        let shared = Arc::new(Shared {
            app_id: config.app_id.clone(),
            endpoint: endpoint.clone(),
            secret: secret.clone(),
            rooms: RwLock::new(HashMap::new()),
            conns: RwLock::new(HashMap::new()),
            blobs: RwLock::new(HashMap::new()),
            file_downloads: Arc::new(tokio::sync::Semaphore::new(2)),
            file_jobs: Mutex::new(HashMap::new()),
            store: Mutex::new(store),
            history_downloads: tokio::sync::Semaphore::new(4),
            history_serving: tokio::sync::Semaphore::new(4),
            history_cache: Mutex::new(history::Cache::default()),
            history_alpn: format!("{}/history-blob/1", config.app_id).into_bytes(),
            history_revision: AtomicU64::new(0),
            history_metadata: AtomicBool::new(false),
            history_events: Mutex::new(history::delivery::Delivery::default()),
            poll_history_turn: AtomicBool::new(false),
            // Handle 0 is reserved for "none".
            next: AtomicU64::new(1),
            stats: Stats::default(),
            ev: Mutex::new(ev_tx),
            closed: AtomicBool::new(false),
            lookup: lookup.clone(),
            friend_alpn: config.friend_alpn(),
            heartbeat: Duration::from_millis(config.heartbeat_ms.max(50)),
            friends: RwLock::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            invites: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
            blocked: RwLock::new(HashSet::new()),
            limits: Mutex::new(Limits::default()),
            support_contacts: RwLock::new(HashSet::new()),
            accept_support: config.accept_support,
            selftest_alpn: config.selftest_alpn(),
            me: RwLock::new(friends::Me::default()),
        });
        friends::load(&shared)?;

        // Blob store: on disk beside the database, in memory when there is no
        // database (tests, and a player who asked for nothing to be kept).
        let blob_dir = config.blob_dir.clone().or_else(|| {
            config
                .db_path
                .as_ref()
                .map(|p| p.with_extension("blobs"))
        });
        let (blob_owner, blobs): (BlobOwner, BlobStore) = match blob_dir {
            Some(dir) => {
                let fs = rt
                    .block_on(FsStore::load(&dir))
                    .map_err(|e| Error::Store(format!("blob store {}: {e}", dir.display())))?;
                let api = (*fs).clone();
                (BlobOwner::Fs(fs), api)
            }
            None => {
                // MemStore spawns its actor on the current runtime.
                let _in_runtime = rt.enter();
                let mem = MemStore::new();
                let api = (*mem).clone();
                (BlobOwner::Mem(mem), api)
            }
        };

        let conns: Arc<Mutex<HashMap<ConnHandle, Connection>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let presence: Arc<RwLock<HashMap<RoomHandle, Vec<u8>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let alpn = config.direct_alpn();

        let router = rt.block_on(async {
            let gossip = Gossip::builder()
                .max_message_size(GOSSIP_MAX_FRAME)
                .spawn(endpoint.clone());
            let blobs_proto = BlobsProtocol::new(&blobs, None);
            let router = if config.accept_inbound {
                Router::builder(endpoint.clone())
                    .accept(iroh_gossip::ALPN, gossip.clone())
                    .accept(iroh_blobs::ALPN, blobs_proto)
                    .accept(shared.history_alpn.clone(), history::Protocol { shared: shared.clone() })
                    .accept(
                        alpn.clone(),
                        DirectProtocol {
                            shared: Arc::new(SharedRef(shared.clone(), conns.clone())),
                        },
                    )
                    .accept(
                        shared.friend_alpn.clone(),
                        friends::FriendProtocol::new(shared.clone()),
                    )
                    .accept(shared.selftest_alpn.clone(), selftest::Probe)
                    .spawn()
            } else {
                Router::builder(endpoint.clone()).spawn()
            };
            (router, gossip)
        });
        let (router, gossip) = router;

        let (cmd_tx, cmd_rx) = unbounded_channel();
        let runner = Runner {
            shared: shared.clone(),
            gossip,
            blobs,
            lookup,
            alpn,
            senders: HashMap::new(),
            tasks: HashMap::new(),
            conns,
            presence: presence.clone(),
        };
        rt.spawn(runner.run(cmd_rx));
        rt.spawn(friends::heartbeat_loop(shared.clone()));

        shared.emit(Event::Ready { node_id });

        Ok(Node {
            rt: Some(rt),
            cmd: cmd_tx,
            events: Mutex::new(ev_rx),
            shared,
            presence,
            blob_owner: Mutex::new(Some(blob_owner)),
            closed: AtomicBool::new(false),
            _router: Mutex::new(Some(router)),
            relay_mode,
            accept_inbound: config.accept_inbound,
        })
    }

    /// **Blocks** (bounded). Idempotent.
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.closed.store(true, Ordering::SeqCst);
        for (_, cancel) in self.shared.file_jobs.lock().unwrap().values() { let _ = cancel.send(true); }
        *self.shared.history_events.lock().unwrap() = history::delivery::Delivery::default();
        let _ = self.cmd.send(Cmd::Shutdown);
        if let Ok(mut r) = self._router.lock() {
            if let (Some(router), Some(rt)) = (r.take(), self.rt.as_ref()) {
                let _ = rt.block_on(router.shutdown());
            }
        }
        if let Ok(mut owner) = self.blob_owner.lock() {
            owner.take();
        }
    }

    fn check_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(Error::State("node is closed"))
        } else {
            Ok(())
        }
    }

    fn push(&self, cmd: Cmd) -> Result<()> {
        self.check_open()?;
        self.cmd.send(cmd).map_err(|_| Error::State("node is closed"))
    }

    // ------------------------------------------------------------- identity

    pub fn node_id(&self) -> PeerId {
        *self.shared.endpoint.id().as_bytes()
    }

    pub fn node_ticket(&self) -> String {
        NodeTicket::new(self.shared.endpoint.addr()).to_string()
    }

    pub fn build_info(&self) -> &'static str {
        crate::build_info()
    }

    // ---------------------------------------------------------------- polling

    /// Drain up to `max` events. Never blocks.
    pub fn poll(&self, max: usize) -> Vec<Event> {
        let rx = self.events.lock().expect("event receiver poisoned");
        let mut out = Vec::new();
        let mut history_first = self.shared.poll_history_turn.fetch_xor(true, Ordering::Relaxed);
        while out.len() < max {
            let event = if history_first {
                self.shared.history_events.lock().unwrap().pop().or_else(|| rx.try_recv().ok())
            } else {
                rx.try_recv().ok().or_else(|| self.shared.history_events.lock().unwrap().pop())
            };
            match event { Some(e) => out.push(e), None => break }
            history_first = !history_first;
        }
        out
    }

    pub fn stats(&self) -> StatsSnapshot {
        let rooms = self.shared.rooms.read().unwrap();
        let peers: BTreeSet<EndpointId> = rooms.values().flat_map(|r| r.peers.iter().copied()).collect();
        let publishing = rooms.values().filter(|r| !r.offering.is_empty()).count() as u32;
        StatsSnapshot {
            bytes_sent: self.shared.stats.bytes_sent.load(Ordering::Relaxed),
            bytes_recv: self.shared.stats.bytes_recv.load(Ordering::Relaxed),
            rooms: rooms.len() as u32,
            peers: peers.len() as u32,
            direct_conns: self.shared.conns.read().unwrap().len() as u32,
            // Filled in once iroh exposes a cheap per-connection path summary;
            // reported as 0 rather than guessed.
            relayed_conns: 0,
            publishing_rooms: publishing,
            events_dropped: self.shared.stats.events_dropped.load(Ordering::Relaxed) as u32,
            rate_limited: self.shared.stats.rate_limited.load(Ordering::Relaxed) as u32,
        }
    }

    /// Is this node actively serving anything into `room`?
    pub fn is_publishing(&self, room: RoomHandle) -> bool {
        self.shared
            .rooms
            .read()
            .unwrap()
            .get(&room)
            .map(|r| !r.offering.is_empty())
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------ rooms

    /// Join a derived room, or one named by a ticket.
    pub fn room_join(
        &self,
        scope: Scope,
        key: &str,
        ticket: Option<&str>,
    ) -> Result<RoomHandle> {
        self.check_open()?;
        if let Some(text) = ticket {
            if text.trim().starts_with(crate::ticket::SEALED_TICKET_PREFIX) {
                if scope != Scope::Custom { return Err(Error::Arg("sealed tickets require custom channel scope")); }
                let t: SealedRoomTicket = text.parse()?;
                return self.join(Scope::Custom, t.topic, t.peers.clone(), t.label.clone(), None, Some(t));
            }
        }
        let (topic, bootstrap, label) = match ticket {
            Some(text) => {
                let t: RoomTicket = text.parse()?;
                (t.topic, t.peers, t.label)
            }
            None => {
                if key.is_empty() {
                    return Err(Error::Arg("a derived room needs a key"));
                }
                (
                    crate::topic::derive(&self.shared.app_id, scope, key),
                    Vec::new(),
                    key.to_string(),
                )
            }
        };
        self.join(scope, topic, bootstrap, label, None, None)
    }

    fn join(
        &self,
        scope: Scope,
        topic: TopicId,
        bootstrap: Vec<EndpointAddr>,
        label: String,
        author: Option<[u8; 32]>,
        sealed: Option<SealedRoomTicket>,
    ) -> Result<RoomHandle> {
        let room = self.shared.handle();
        self.shared.history_events.lock().unwrap().register(room);
        let mut rooms = self.shared.rooms.write().unwrap();
        if rooms.values().any(|state| state.topic == topic &&
            match (&state.sealed, &sealed) {
                (None, None) => false,
                (Some(a), Some(b)) => a.key() != b.key(),
                _ => true,
            }) {
            self.shared.history_events.lock().unwrap().forget(room);
            return Err(Error::State("channel sealing mode or key does not match"));
        }
        rooms.insert(
            room,
            RoomState {
                topic,
                sealed,
                scope,
                label,
                peers: BTreeSet::new(),
                joined: false,
                history_enabled: false,
                history_request: None,
                history_generation: Arc::new(AtomicU64::new(0)),
                offering: BTreeSet::new(),
                author,
            },
        );
        drop(rooms);
        if let Err(error) = self.push(Cmd::RoomJoin { room, topic, bootstrap }) {
            self.shared.rooms.write().unwrap().remove(&room);
            self.shared.history_events.lock().unwrap().forget(room);
            return Err(error);
        }
        Ok(room)
    }

    /// Create a group channel: a gossip room whose topic comes from a fresh
    /// random key, so nobody can find it without a ticket. `label` is only for
    /// display. Share it with `room_ticket` or `channel_invite`.
    pub fn channel_create(&self, label: &str) -> Result<RoomHandle> {
        self.check_open()?;
        let key = data_encoding::HEXLOWER.encode(&friends::random::<32>());
        let topic = crate::topic::derive(&self.shared.app_id, Scope::Custom, &key);
        let label = if label.is_empty() { "channel" } else { label };
        self.join(Scope::Custom, topic, Vec::new(), label.to_string(), None, None)
    }

    /// Create a sealed channel with a fresh random topic and symmetric key.
    pub fn channel_create_sealed(&self, label: &str) -> Result<RoomHandle> {
        self.check_open()?;
        let ticket = SealedRoomTicket::new(label, Vec::new())?;
        self.join(Scope::Custom, ticket.topic, Vec::new(), ticket.label.clone(), None, Some(ticket))
    }

    pub fn room_is_sealed(&self, room: RoomHandle) -> Result<bool> {
        self.shared.rooms.read().unwrap().get(&room).map(|s| s.sealed.is_some()).ok_or(Error::NotFound)
    }

    /// Start a fresh channel. The old channel and its tickets remain valid;
    /// explicit ticket distribution and leaving the old room are caller actions.
    pub fn channel_reseal(&self, room: RoomHandle) -> Result<RoomHandle> {
        let label = {
            let rooms = self.shared.rooms.read().unwrap();
            let state = rooms.get(&room).ok_or(Error::NotFound)?;
            if state.sealed.is_none() { return Err(Error::State("not a sealed channel")); }
            state.label.clone()
        };
        self.channel_create_sealed(&label)
    }

    /// Remember where a node can be reached (an `ltnode…` ticket), so dialing
    /// it by NodeId alone works without relays or DNS: on a LAN, or in tests.
    pub fn add_address_hint(&self, node_ticket: &str) -> Result<()> {
        self.check_open()?;
        let addr = parse_dial_target(node_ticket)?;
        self.shared.lookup.add_endpoint_info(addr);
        Ok(())
    }

    /// Join the author channel for `author` (an ed25519 public key; see
    /// `author.rs`). `bootstrap` are the author's always-on nodes. Joining
    /// twice returns the same room. Only verified announcements come out of
    /// it, as `Event::Announcement`; `room_send` refuses it.
    pub fn author_join(&self, author: &[u8; 32], bootstrap: &[PeerId]) -> Result<RoomHandle> {
        self.check_open()?;
        iroh::PublicKey::from_bytes(author).map_err(|_| Error::Arg("not an author key"))?;
        if let Some((room, _)) = self
            .shared
            .rooms
            .read()
            .unwrap()
            .iter()
            .find(|(_, s)| s.author.as_ref() == Some(author))
        {
            return Ok(*room);
        }
        let me = self.shared.endpoint.id();
        let peers = bootstrap
            .iter()
            .filter_map(|p| EndpointId::from_bytes(p).ok())
            .filter(|p| *p != me)
            .map(EndpointAddr::from)
            .collect();
        let topic = crate::author::topic(&self.shared.app_id, author);
        self.join(Scope::Public, topic, peers, "author".to_string(), Some(*author), None)
    }

    /// Publish an announcement signed offline (`ltannounce…`) into an author
    /// channel. Refused unless the room's author key signed it. Returns its
    /// sequence number. **Touches SQLite**; for the author's own tools.
    pub fn author_announce(&self, room: RoomHandle, signed: &str) -> Result<u64> {
        self.check_open()?;
        let author = self
            .shared
            .rooms
            .read()
            .unwrap()
            .get(&room)
            .ok_or(Error::NotFound)?
            .author
            .ok_or(Error::State("not an author channel"))?;
        let signed: crate::author::SignedAnnouncement = signed.parse()?;
        let a = signed
            .verify(&author)
            .ok_or(Error::Arg("not signed by this channel's author key"))?;
        let bytes = signed.to_bytes();
        author_frame(&self.shared, room, &author, &bytes);
        self.push(Cmd::RoomBroadcast {
            room,
            frame: Frame::App(bytes),
        })?;
        Ok(a.seq)
    }

    /// Announcements kept for `author`, newest first. **Reads SQLite.**
    pub fn announcements(&self, author: &[u8; 32], limit: u32) -> Result<Vec<crate::author::Announcement>> {
        let store = self
            .shared
            .store
            .lock()
            .map_err(|_| Error::Store("store poisoned".into()))?;
        Ok(store.announcements(author, limit)?.into_iter().map(|(a, _)| a).collect())
    }

    pub fn announcements_json(&self, author: &[u8; 32], limit: u32) -> Result<String> {
        let mut out = String::from("[");
        for (i, a) in self.announcements(author, limit)?.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"seq\":{},\"issued_at\":{},\"title\":{},\"body\":{}}}",
                a.seq,
                a.issued_at,
                json_string(&a.title),
                json_string(&a.body)
            ));
        }
        out.push(']');
        Ok(out)
    }

    /// The display label a room was joined or created with.
    pub fn room_label(&self, room: RoomHandle) -> Result<String> {
        let rooms = self.shared.rooms.read().unwrap();
        rooms.get(&room).map(|s| s.label.clone()).ok_or(Error::NotFound)
    }

    pub fn room_leave(&self, room: RoomHandle) -> Result<()> {
        if !self.shared.rooms.read().unwrap().contains_key(&room) {
            return Err(Error::NotFound);
        }
        self.push(Cmd::RoomLeave(room))
    }

    pub fn room_ticket(&self, room: RoomHandle) -> Result<String> {
        let rooms = self.shared.rooms.read().unwrap();
        let state = rooms.get(&room).ok_or(Error::NotFound)?;
        if let Some(ticket) = &state.sealed {
            let mut ticket = ticket.clone();
            ticket.peers = vec![self.shared.endpoint.addr()];
            return Ok(ticket.to_string());
        }
        Ok(RoomTicket::new(
            state.topic,
            state.scope,
            state.label.clone(),
            vec![self.shared.endpoint.addr()],
        )
        .to_string())
    }

    pub fn room_scope(&self, room: RoomHandle) -> Result<Scope> {
        let rooms = self.shared.rooms.read().unwrap();
        rooms.get(&room).map(|s| s.scope).ok_or(Error::NotFound)
    }

    pub fn room_send(&self, room: RoomHandle, data: &[u8]) -> Result<()> {
        if data.len() > MAX_ROOM_MESSAGE {
            return Err(Error::TooLarge);
        }
        match self.shared.rooms.read().unwrap().get(&room) {
            None => return Err(Error::NotFound),
            Some(s) if s.author.is_some() => {
                return Err(Error::State("the author channel only carries signed announcements"))
            }
            Some(_) => {}
        }
        self.push(Cmd::RoomBroadcast {
            room,
            frame: Frame::App(data.to_vec()),
        })
    }

    /// Queue text for signed history capture and normal live delivery.
    /// Signing and database work run on the worker, never the caller's frame thread.
    /// Metadata opt-in selects original signed events; catch-up is separately configured.
    pub fn room_send_with_history(&self, room: RoomHandle, text: &str) -> Result<()> {
        if !crate::channel_history::valid_text(text) {
            return Err(Error::Arg("invalid history text"));
        }
        match self.shared.rooms.read().unwrap().get(&room) {
            None => return Err(Error::NotFound),
            Some(s) if s.author.is_some() => return Err(Error::State("author channels do not carry chat history")),
            Some(_) => {}
        }
        self.push(Cmd::HistoryPublish { room, text: text.to_owned() })
    }

    /// Opt into signed metadata events, independently of automatic catch-up.
    /// New bindings call this before joining rooms; old clients keep Message.
    pub fn channel_record_events_enable(&self, enabled: bool) -> Result<()> {
        self.check_open()?;
        self.shared.history_metadata.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    /// Enable automatic catch-up requests and bounded snapshot serving for a room.
    /// No historical live-message events are generated. Disabled by default.
    pub fn room_history_enable(&self, room: RoomHandle, enabled: bool) -> Result<()> {
        match self.shared.rooms.read().unwrap().get(&room) {
            None => return Err(Error::NotFound),
            Some(s) if s.author.is_some() => return Err(Error::State("author channels do not carry chat history")),
            Some(_) => {}
        }
        self.shared.history_metadata.store(true, Ordering::Relaxed);
        self.push(Cmd::HistoryEnable { room, enabled })
    }

    /// Clear only this channel's local history and cancel its pending catch-up.
    /// Other members retain their copies; a later catch-up can restore them.
    pub fn room_history_clear(&self, room: RoomHandle) -> Result<()> {
        if !self.shared.rooms.read().unwrap().contains_key(&room) { return Err(Error::NotFound); }
        self.push(Cmd::HistoryClear(room))
    }

    /// Queue a bounded, reverified snapshot fetch and store its signed records.
    /// Historical event/UI delivery and automatic requests are separate work.
    pub fn room_history_fetch(&self, room: RoomHandle, provider: &PeerId, hash: [u8; 32], size: u64) -> Result<()> {
        let maximum = if self.room_is_sealed(room)? { crate::channel_history::MAX_SEALED_SNAPSHOT_BYTES } else { crate::channel_history::MAX_SNAPSHOT_BYTES };
        if size < crate::channel_history::SNAPSHOT_HEADER as u64 || size > maximum as u64 {
            return Err(Error::Arg("invalid history snapshot size"));
        }
        let provider = EndpointId::from_bytes(provider).map_err(|_| Error::Arg("invalid history provider"))?;
        if self.shared.is_blocked(&provider) { return Err(Error::State("history provider is blocked")); }
        match self.shared.rooms.read().unwrap().get(&room) {
            None => return Err(Error::NotFound),
            Some(s) if s.author.is_some() => return Err(Error::State("author channels do not carry chat history")),
            Some(_) => {}
        }
        self.push(Cmd::HistoryFetch { room, provider, hash, size })
    }

    pub fn presence_set(&self, room: RoomHandle, data: &[u8]) -> Result<()> {
        if data.len() > MAX_ROOM_MESSAGE {
            return Err(Error::TooLarge);
        }
        if !self.shared.rooms.read().unwrap().contains_key(&room) {
            return Err(Error::NotFound);
        }
        self.presence.write().unwrap().insert(room, data.to_vec());
        self.push(Cmd::RoomBroadcast {
            room,
            frame: Frame::Presence(data.to_vec()),
        })
    }

    pub fn room_peers(&self, room: RoomHandle) -> Result<Vec<PeerId>> {
        let rooms = self.shared.rooms.read().unwrap();
        let state = rooms.get(&room).ok_or(Error::NotFound)?;
        Ok(state.peers.iter().map(|p| *p.as_bytes()).collect())
    }

    // ----------------------------------------------------------------- direct

    pub fn connect(&self, ticket_or_id: &str) -> Result<ConnHandle> {
        self.check_open()?;
        let addr = parse_dial_target(ticket_or_id)?;
        let conn = self.shared.handle();
        self.push(Cmd::Connect { conn, addr })?;
        Ok(conn)
    }

    pub fn send(&self, conn: ConnHandle, data: &[u8]) -> Result<()> {
        if data.len() > MAX_MESSAGE {
            return Err(Error::TooLarge);
        }
        self.push(Cmd::Send {
            conn,
            data: data.to_vec(),
        })
    }

    pub fn disconnect(&self, conn: ConnHandle) -> Result<()> {
        self.push(Cmd::Disconnect(conn))
    }

    // ------------------------------------------------------------------ blobs

    pub fn blob_add_bytes(&self, data: &[u8], name: &str) -> Result<BlobHandle> {
        self.check_open()?;
        let blob = self.shared.handle();
        self.shared.blobs.write().unwrap().insert(
            blob,
            BlobState {
                sealed: None,
                hash: None,
                name: name.to_string(),
                size: data.len() as u64,
                from: None,
                room: None,
            },
        );
        self.push(Cmd::BlobAddBytes {
            blob,
            data: data.to_vec(),
        })?;
        Ok(blob)
    }

    /// Prepare encrypted bytes for this sealed channel only. Signing/encryption
    /// and blob-store work run on the worker; no plaintext blob is imported.
    pub fn blob_add_sealed_bytes(&self, room: RoomHandle, data: &[u8], name: &str) -> Result<BlobHandle> {
        self.check_open()?;
        if data.len() as u64 > crate::bounded_blob::MAX_FILE_BYTES { return Err(Error::TooLarge); }
        if name.is_empty() || name.len() > 1024 || name.chars().any(char::is_control) {
            return Err(Error::Arg("invalid shared file name"));
        }
        let ticket = self.shared.rooms.read().unwrap().get(&room).ok_or(Error::NotFound)?
            .sealed.clone().ok_or(Error::State("not a sealed channel"))?;
        let blob = self.shared.handle();
        self.shared.blobs.write().unwrap().insert(blob, BlobState {
            sealed: Some(SealedFile { ticket: ticket.clone(), encrypted_size: 0 }),
            hash: None, name: name.into(), size: data.len() as u64, from: None, room: Some(room),
        });
        if let Err(error) = self.push(Cmd::BlobAddSealedBytes { blob, data: data.to_vec(), ticket }) {
            self.shared.blobs.write().unwrap().remove(&blob);
            return Err(error);
        }
        Ok(blob)
    }

    pub fn blob_add_file(&self, path: &str, name: &str) -> Result<BlobHandle> {
        self.check_open()?;
        if path.is_empty() {
            return Err(Error::Arg("blob path must not be empty"));
        }
        let blob = self.shared.handle();
        self.shared.blobs.write().unwrap().insert(
            blob,
            BlobState {
                sealed: None,
                hash: None,
                name: name.to_string(),
                size: 0,
                from: None,
                room: None,
            },
        );
        self.push(Cmd::BlobAddFile {
            blob,
            path: PathBuf::from(path),
        })?;
        Ok(blob)
    }

    /// Announce a blob to a room. This is the moment something becomes visible
    /// to other people, and the only one.
    pub fn blob_offer(&self, room: RoomHandle, blob: BlobHandle) -> Result<()> {
        let (hash, size, name, sealed_file, from) = {
            let blobs = self.shared.blobs.read().unwrap();
            let state = blobs.get(&blob).ok_or(Error::NotFound)?;
            let hash = state.hash.ok_or(Error::State("blob is still hashing"))?;
            (hash, state.size, state.name.clone(), state.sealed.clone(), state.from)
        };
        let frame = {
            let mut rooms = self.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).ok_or(Error::NotFound)?;
            let frame = match (&state.sealed, sealed_file) {
                (Some(ticket), Some(file)) if from.is_none() && file.ticket.topic == ticket.topic
                    && file.ticket.key() == ticket.key() => Frame::SealedBlobOffer {
                        hash, size, encrypted_size: file.encrypted_size, name,
                    },
                (None, None) => Frame::BlobOffer { hash, size, name },
                _ => return Err(Error::State("shared file encryption does not match channel")),
            };
            state.offering.insert(hash);
            frame
        };
        self.push(Cmd::RoomBroadcast { room, frame })?;
        self.shared.emit(Event::Publishing {
            room,
            publishing: true,
        });
        Ok(())
    }

    pub fn blob_unoffer(&self, room: RoomHandle, blob: BlobHandle) -> Result<()> {
        let (hash, sealed_file) = {
            let blobs = self.shared.blobs.read().unwrap();
            let state = blobs.get(&blob).ok_or(Error::NotFound)?;
            (state.hash.ok_or(Error::NotFound)?, state.sealed.clone())
        };
        let still_publishing = {
            let mut rooms = self.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).ok_or(Error::NotFound)?;
            if let Some(file) = &sealed_file {
                if !state.sealed.as_ref().is_some_and(|ticket|
                    file.ticket.topic == ticket.topic && file.ticket.key() == ticket.key()) {
                    return Err(Error::State("shared file encryption does not match channel"));
                }
            }
            state.offering.remove(&hash);
            !state.offering.is_empty()
        };
        self.push(Cmd::RoomBroadcast {
            room,
            frame: Frame::BlobUnoffer { hash },
        })?;
        self.shared.emit(Event::Publishing {
            room,
            publishing: still_publishing,
        });
        Ok(())
    }

    pub fn blob_fetch(&self, blob: BlobHandle, out_path: &str) -> Result<()> {
        if out_path.is_empty() {
            return Err(Error::Arg("fetch needs a destination path"));
        }
        let (hash, from) = {
            let blobs = self.shared.blobs.read().unwrap();
            let state = blobs.get(&blob).ok_or(Error::NotFound)?;
            if state.sealed.is_some() { return Err(Error::State("sealed files require bounded authenticated fetch")); }
            (
                state.hash.ok_or(Error::State("blob has no hash yet"))?,
                state.from,
            )
        };
        self.push(Cmd::BlobFetch {
            blob,
            hash,
            from,
            out: PathBuf::from(out_path),
        })
    }

    pub fn blob_hash(&self, blob: BlobHandle) -> Result<[u8; 32]> {
        let blobs = self.shared.blobs.read().unwrap();
        blobs
            .get(&blob)
            .and_then(|s| s.hash)
            .ok_or(Error::State("blob has no hash yet"))
    }

    /// Explicit opt-in file fetch: at most two queued/running jobs, 16 MiB each,
    /// exact offered/Bao tree size, verified content and a create-new destination.
    /// No ordinary blob-cache download or export. Caller must protect the parent
    /// directory and recheck room/mute admission before calling.
    pub fn blob_fetch_bounded(&self, blob: BlobHandle, out_path: &str) -> Result<()> {
        self.check_open()?;
        if out_path.is_empty() { return Err(Error::Arg("fetch needs a destination path")); }
        // Keep offer admission and block state stable through job registration.
        // Withdrawal/unfriend can then remove the offer and signal this job;
        // neither can miss a fetch that read metadata just before removal.
        let blocked = self.shared.blocked.read().unwrap();
        let blobs = self.shared.blobs.read().unwrap();
        let state = blobs.get(&blob).ok_or(Error::NotFound)?;
        let hash = state.hash.ok_or(Error::State("blob has no hash yet"))?;
        let from = state.from.ok_or(Error::State("bounded fetch needs a remote offer"))?;
        let size = state.size;
        let sealed = state.sealed.clone();
        if size > crate::bounded_blob::MAX_FILE_BYTES { return Err(Error::Arg("file offer exceeds download bounds")); }
        if blocked.contains(&from) { return Err(Error::State("file sender is blocked")); }
        let permit = self.shared.file_downloads.clone().try_acquire_owned().map_err(|_| Error::Full)?;
        let (cancel, receiver) = tokio::sync::watch::channel(false);
        {
            let mut jobs = self.shared.file_jobs.lock().unwrap();
            if jobs.contains_key(&blob) { return Err(Error::State("file download already pending")); }
            jobs.insert(blob, (from, cancel));
        }
        drop(blobs);
        drop(blocked);
        let job = FileJob { shared: self.shared.clone(), blob, cancel: receiver };
        self.push(Cmd::BlobFetchBounded { blob, hash, from, size, sealed, out: PathBuf::from(out_path), permit, job })
    }

    /// Request cancellation; the terminal event determines the outcome if completion races it.
    pub fn blob_cancel_bounded(&self, blob: BlobHandle) -> Result<()> {
        self.check_open()?;
        let jobs = self.shared.file_jobs.lock().unwrap();
        let (_, cancel) = jobs.get(&blob).ok_or(Error::NotFound)?;
        cancel.send(true).map_err(|_| Error::State("file download already finished"))
    }

    // -------------------------------------------------------------- bookmarks

    pub fn bookmark_put(&self, id: &str, scope: Scope, title: &str, ticket: &str) -> Result<()> {
        let mut store = self
            .shared
            .store
            .lock()
            .map_err(|_| Error::Store("store poisoned".into()))?;
        store.bookmark_put(id, scope, title, ticket)
    }

    pub fn bookmark_forget(&self, id: &str) -> Result<bool> {
        let mut store = self
            .shared
            .store
            .lock()
            .map_err(|_| Error::Store("store poisoned".into()))?;
        store.bookmark_forget(id)
    }

    pub fn bookmarks_json(&self) -> Result<String> {
        let store = self
            .shared
            .store
            .lock()
            .map_err(|_| Error::Store("store poisoned".into()))?;
        let list = store.bookmarks()?;
        let mut out = String::from("[");
        for (i, b) in list.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"id\":{},\"scope\":\"{}\",\"title\":{},\"ticket\":{},\"updated_at\":{}}}",
                json_string(&b.id),
                b.scope.as_str(),
                json_string(&b.title),
                json_string(&b.ticket),
                b.updated_at
            ));
        }
        out.push(']');
        Ok(out)
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.close();
        // Drop the runtime last: tasks still reference the shared state.
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(std::time::Duration::from_millis(500));
        }
    }
}

/// The newest announcements this node keeps for `author`, signed, oldest
/// first, for a new neighbour in the author channel.
fn author_reoffer(shared: &Shared, author: &[u8; 32]) -> Vec<Vec<u8>> {
    let Ok(store) = shared.store.lock() else {
        return Vec::new();
    };
    let mut out: Vec<Vec<u8>> = store
        .announcements(author, 3)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, signed)| signed)
        .collect();
    out.reverse();
    out
}

/// A message in an author channel: shown only if it is an announcement the
/// author key signed, and only the first time.
fn author_frame(shared: &Shared, room: RoomHandle, author: &[u8; 32], data: &[u8]) {
    let Some(signed) = crate::author::SignedAnnouncement::from_bytes(data) else {
        return;
    };
    let Some(a) = signed.verify(author) else {
        return;
    };
    let fresh = shared
        .store
        .lock()
        .ok()
        .and_then(|mut s| s.announcement_put(author, &a, data).ok())
        .unwrap_or(false);
    if fresh {
        shared.emit(Event::Announcement {
            room,
            author: *author,
            seq: a.seq,
            issued_at: a.issued_at,
            title: a.title,
            body: a.body,
        });
    }
}

/// Accept a node ticket, a room ticket's peer, or a bare node id.
fn parse_dial_target(text: &str) -> Result<EndpointAddr> {
    let text = text.trim();
    if let Ok(t) = text.parse::<NodeTicket>() {
        return Ok(t.addr);
    }
    if let Ok(t) = text.parse::<RoomTicket>() {
        return t.peers.into_iter().next().ok_or(Error::Ticket);
    }
    match text.parse::<EndpointId>() {
        Ok(id) => Ok(EndpointAddr::from(id)),
        Err(_) => Err(Error::Ticket),
    }
}

pub(crate) fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
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
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_file_offers_are_bounded_before_handle_allocation() {
        let peer = SecretKey::generate().public();
        let other = SecretKey::generate().public();
        let mut blobs = HashMap::new();
        assert!(!remote_offer_allowed(&blobs, 1, peer, [1; 32], "bad\nname"));
        for i in 0..8u8 {
            let hash = [i; 32];
            assert!(remote_offer_allowed(&blobs, 1, peer, hash, "clear.png"));
            blobs.insert(i as u64, BlobState { sealed: None, hash: Some(hash), name: "clear.png".into(),
                size: 10, from: Some(peer), room: Some(1) });
            assert!(!remote_offer_allowed(&blobs, 1, peer, hash, "clear.png"));
        }
        assert!(!remote_offer_allowed(&blobs, 2, peer, [99; 32], "clear.png"));
        assert!(remote_offer_allowed(&blobs, 1, other, [0; 32], "clear.png"));
        for i in 8..128u64 {
            let sender = SecretKey::generate().public();
            blobs.insert(i, BlobState { sealed: None, hash: Some([0; 32]), name: "clear.png".into(),
                size: 10, from: Some(sender), room: Some(1) });
        }
        assert!(!remote_offer_allowed(&blobs, 1, other, [99; 32], "clear.png"));
    }

    #[test]
    fn bounded_file_job_cancellation_survives_queueing_and_drop_unregisters() {
        let node = Node::open(Config { relay: RelayMode::Disabled,
            app_id: "file-cancel-test/1".into(), ..Config::default() }).unwrap();
        let (sender, receiver) = tokio::sync::watch::channel(false);
        node.shared.file_jobs.lock().unwrap().insert(123, (node.shared.endpoint.id(), sender));
        let job = FileJob { shared: node.shared.clone(), blob: 123, cancel: receiver };
        node.blob_cancel_bounded(123).unwrap();
        assert!(*job.cancel.borrow(), "queued worker observes cancellation immediately");
        drop(job);
        assert!(node.shared.file_jobs.lock().unwrap().is_empty());
        assert_eq!(node.blob_cancel_bounded(123), Err(Error::NotFound));
        node.close();
    }

    #[test]
    fn forgetting_remote_offers_cancels_registered_jobs_and_preserves_local_blobs() {
        let node = Node::open(Config { relay: RelayMode::Disabled,
            app_id: "file-forget-test/1".into(), ..Config::default() }).unwrap();
        let peer = SecretKey::generate().public();
        let other = SecretKey::generate().public();
        let state = |from, room| BlobState { sealed: None, hash: Some([1; 32]), name: "clear.png".into(), size: 10, from, room };
        {
            let mut blobs = node.shared.blobs.write().unwrap();
            blobs.insert(123, state(Some(peer), Some(0)));
            blobs.insert(124, state(None, None));
            blobs.insert(125, state(Some(other), Some(0)));
        }
        let (sender, receiver) = tokio::sync::watch::channel(false);
        node.shared.file_jobs.lock().unwrap().insert(123, (peer, sender));
        let job = FileJob { shared: node.shared.clone(), blob: 123, cancel: receiver };
        node.shared.forget_remote_blobs(|s| s.from == Some(peer));
        assert!(*job.cancel.borrow());
        let blobs = node.shared.blobs.read().unwrap();
        assert!(!blobs.contains_key(&123));
        assert!(blobs.contains_key(&124));
        assert!(blobs.contains_key(&125));
        drop(blobs);
        assert!(node.poll(64).iter().any(|e| matches!(e, Event::BlobWithdrawn { blob: 123 })));
        drop(job);
        node.close();
    }

    #[tokio::test]
    async fn bounded_partial_file_closes_and_is_removed_when_task_is_aborted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.download");
        let file = tokio::fs::OpenOptions::new().write(true).create_new(true).open(&path).await.unwrap();
        let partial = PartialFile { path: path.clone(), keep: false, file: Some(file) };
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _partial = partial;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        assert!(path.exists());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!path.exists());
    }

    #[test]
    fn repeated_text_has_two_signed_records_and_two_live_messages() {
        use std::time::{Duration, Instant};
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let nodes: Vec<_> = dirs.iter().map(|dir| Node::open(Config {
            db_path: Some(dir.path().join("history.sqlite")),
            blob_dir: Some(dir.path().join("blobs")),
            relay: RelayMode::Disabled,
            app_id: "history-capture-test".into(),
            ..Config::default()
        }).unwrap()).collect();
        let ready_deadline = Instant::now() + Duration::from_secs(15);
        let mut ready = [false; 2];
        while !ready.iter().all(|v| *v) {
            assert!(Instant::now() < ready_deadline, "nodes did not become ready");
            for (i, node) in nodes.iter().enumerate() {
                if node.poll(64).iter().any(|e| matches!(e, Event::Ready { .. })) { ready[i] = true; }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let room_a = nodes[0].channel_create("test").unwrap();
        let join_deadline = Instant::now() + Duration::from_secs(15);
        while !nodes[0].poll(64).iter().any(|e| matches!(e, Event::RoomJoined { .. })) {
            assert!(Instant::now() < join_deadline, "room did not join");
            std::thread::sleep(Duration::from_millis(10));
        }
        let ticket = nodes[0].room_ticket(room_a).unwrap();
        let room_b = nodes[1].room_join(Scope::Custom, "", Some(&ticket)).unwrap();
        let topic = nodes[0].shared.rooms.read().unwrap()[&room_a].topic;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut joined = [false; 2];
        while !joined.iter().all(|v| *v) {
            assert!(Instant::now() < deadline, "peers did not connect");
            for (i, node) in nodes.iter().enumerate() {
                for event in node.poll(64) {
                    if matches!(event, Event::PeerJoined { .. }) { joined[i] = true; }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(nodes[0].room_send_with_history(room_a, "bad\0text").is_err());
        for _ in 0..2 { nodes[0].room_send_with_history(room_a, "same text").unwrap(); }
        let mut live = 0;
        loop {
            assert!(Instant::now() < deadline, "history did not arrive");
            for event in nodes[1].poll(64) {
                if let Event::Message { room, peer, data } = event {
                    assert_eq!(room, room_b);
                    assert_eq!(peer, *nodes[0].shared.secret.public().as_bytes());
                    assert_eq!(data, b"same text");
                    live += 1;
                }
            }
            let records = nodes[1].shared.store.lock().unwrap()
                .channel_history_read(topic.as_bytes(), crate::store::now_ms()).unwrap();
            if records.len() == 2 && live == 2 {
                assert_ne!(records[0].signature(), records[1].signature());
                let originals = nodes[0].shared.store.lock().unwrap()
                    .channel_history_read(topic.as_bytes(), crate::store::now_ms()).unwrap();
                assert_eq!(originals.len(), 2);
                for record in &records {
                    assert_eq!(record.author(), nodes[0].shared.secret.public());
                    assert!(originals.iter().any(|r| r.encoded() == record.encoded()));
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // History envelopes do not generate a second live event.
        std::thread::sleep(Duration::from_millis(100));
        assert!(!nodes[1].poll(64).iter().any(|e| matches!(e, Event::Message { .. })));
        for node in &nodes { node.close(); }
    }

    #[test]
    fn snapshot_fetch_is_bounded_reverified_and_keeps_original_author() {
        use crate::channel_history::{self, network::fetch_snapshot};
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let nodes: Vec<_> = dirs.iter().map(|dir| Node::open(Config {
            db_path: Some(dir.path().join("fetch.sqlite")),
            blob_dir: Some(dir.path().join("blobs")),
            relay: RelayMode::Disabled,
            app_id: "history-fetch-test".into(),
            ..Config::default()
        }).unwrap()).collect();
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut ready = [false; 2];
        while !ready.iter().all(|v| *v) {
            assert!(Instant::now() < deadline, "nodes did not become ready");
            for (i, node) in nodes.iter().enumerate() {
                if node.poll(64).iter().any(|e| matches!(e, Event::Ready { .. })) { ready[i] = true; }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        nodes[1].add_address_hint(&nodes[0].node_ticket()).unwrap();
        let original_author = SecretKey::generate(); // Publisher never has this author's key.
        let room_handle = nodes[1].channel_create("snapshot-fetch").unwrap();
        let room = *nodes[1].shared.rooms.read().unwrap()[&room_handle].topic.as_bytes();
        let original = channel_history::sign(&original_author, room, 42, "signed by someone else").unwrap();
        let snapshot = channel_history::snapshot(room, &[original.clone()]).unwrap();
        let added = nodes[0].blob_add_bytes(&snapshot, "history-test").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let hash = loop {
            assert!(Instant::now() < deadline, "snapshot was not added");
            if nodes[0].poll(64).iter().any(|e| matches!(e, Event::BlobDone { blob, .. } if *blob == added)) {
                break nodes[0].shared.blobs.read().unwrap()[&added].hash.unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let endpoint = &nodes[1].shared.endpoint;
        let provider = nodes[0].shared.secret.public();
        let run = |size, room: &[u8; 32]| nodes[1].rt.as_ref().unwrap().block_on(fetch_snapshot(endpoint, &nodes[1].shared.history_downloads, provider, hash, size, room));
        let occupied: Vec<_> = (0..4).map(|_| nodes[1].shared.history_downloads.try_acquire().unwrap()).collect();
        assert!(run(snapshot.len() as u64, &room).is_err(), "busy node must refuse extra fetches");
        drop(occupied);
        let records = run(snapshot.len() as u64, &room).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].encoded(), original);
        assert_eq!(records[0].author(), original_author.public());
        assert_ne!(records[0].author(), provider);
        assert!(run(snapshot.len() as u64 + 1, &room).is_err());
        assert!(run(snapshot.len() as u64, &[18; 32]).is_err());
        assert!(run(channel_history::MAX_SNAPSHOT_BYTES as u64 + 1, &room).is_err());
        assert!(run(0, &room).is_err());
        assert!(nodes[1].room_history_fetch(0, provider.as_bytes(), hash, snapshot.len() as u64).is_err());
        assert!(nodes[1].room_history_fetch(room_handle, provider.as_bytes(), hash, 0).is_err());
        nodes[1].room_history_fetch(room_handle, provider.as_bytes(), hash, snapshot.len() as u64).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "queued snapshot was not stored");
            assert!(nodes[1].poll(64).iter().all(|e| !matches!(e, Event::Message { .. })));
            let stored = nodes[1].shared.store.lock().unwrap().channel_history_read(&room, crate::store::now_ms()).unwrap();
            if !stored.is_empty() {
                assert_eq!(stored.len(), 1);
                assert_eq!(stored[0].encoded(), original);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // An oversized real blob cannot bypass the limit with a small offer.
        let oversized = vec![0; channel_history::MAX_SNAPSHOT_BYTES + 1];
        let added = nodes[0].blob_add_bytes(&oversized, "oversized-history-test").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let big_hash = loop {
            assert!(Instant::now() < deadline, "oversized fixture was not added");
            if nodes[0].poll(64).iter().any(|e| matches!(e, Event::BlobDone { blob, .. } if *blob == added)) {
                break nodes[0].shared.blobs.read().unwrap()[&added].hash.unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(nodes[1].rt.as_ref().unwrap().block_on(fetch_snapshot(endpoint, &nodes[1].shared.history_downloads, provider, big_hash, snapshot.len() as u64, &room)).is_err());
        // A genuine content hash does not make a forged author signature valid.
        let mut forged = snapshot.clone();
        *forged.last_mut().unwrap() ^= 1;
        let added = nodes[0].blob_add_bytes(&forged, "forged-history-test").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let forged_hash = loop {
            assert!(Instant::now() < deadline, "forged fixture was not added");
            if nodes[0].poll(64).iter().any(|e| matches!(e, Event::BlobDone { blob, .. } if *blob == added)) {
                break nodes[0].shared.blobs.read().unwrap()[&added].hash.unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let records = nodes[1].rt.as_ref().unwrap().block_on(fetch_snapshot(endpoint, &nodes[1].shared.history_downloads, provider, forged_hash, forged.len() as u64, &room)).unwrap();
        assert!(records.is_empty());
        // Real loopback transport of a ciphertext snapshot. The normal Bao
        // blob hash authenticates transferred bytes; the sealed codec then
        // authenticates provider, channel and each original author's record.
        let sealed_ticket = crate::ticket::SealedRoomTicket::new("private history", vec![]).unwrap();
        let sealed_room = *sealed_ticket.topic.as_bytes();
        let sealed_original = channel_history::sign(&original_author, sealed_room, 42, "encrypted catch-up").unwrap();
        let sealed_bytes = channel_history::sealed_snapshot(&nodes[0].shared.secret,
            &sealed_ticket, &[sealed_original.clone()]).unwrap();
        let added = nodes[0].blob_add_bytes(&sealed_bytes, "sealed-history-test").unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let sealed_hash = loop {
            assert!(Instant::now() < deadline, "sealed fixture was not added");
            if nodes[0].poll(64).iter().any(|e| matches!(e, Event::BlobDone { blob, .. } if *blob == added)) {
                break nodes[0].shared.blobs.read().unwrap()[&added].hash.unwrap();
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let sealed_run = |hash, size, ticket: &crate::ticket::SealedRoomTicket| {
            nodes[1].rt.as_ref().unwrap().block_on(channel_history::network::fetch_snapshot_over(
                endpoint, &nodes[1].shared.history_downloads, provider, hash, size,
                &sealed_room, iroh_blobs::ALPN, Some(ticket)))
        };
        let fetched = sealed_run(sealed_hash, sealed_bytes.len() as u64, &sealed_ticket).unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].encoded(), sealed_original);
        assert_eq!(fetched[0].author(), original_author.public());
        assert!(sealed_run(sealed_hash, sealed_bytes.len() as u64 + 1, &sealed_ticket).is_err());
        assert!(sealed_run(sealed_hash, channel_history::MAX_SEALED_SNAPSHOT_BYTES as u64 + 1, &sealed_ticket).is_err());
        let mut wrong_key = crate::ticket::SealedRoomTicket::new("wrong key", vec![]).unwrap();
        wrong_key.topic = sealed_ticket.topic;
        assert!(sealed_run(sealed_hash, sealed_bytes.len() as u64, &wrong_key).is_err());
        assert!(sealed_run(hash, snapshot.len() as u64, &sealed_ticket).is_err(), "plaintext is not a sealed snapshot");
        assert!(nodes[1].rt.as_ref().unwrap().block_on(fetch_snapshot(endpoint,
            &nodes[1].shared.history_downloads, provider, sealed_hash,
            sealed_bytes.len() as u64, &sealed_room)).is_err(), "plain path refuses ciphertext");
        nodes[1].shared.store.lock().unwrap().channel_history_clear(&room).unwrap();
        nodes[1].block(original_author.public().as_bytes()).unwrap();
        nodes[1].room_history_fetch(room_handle, provider.as_bytes(), hash, snapshot.len() as u64).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "blocked-author fetch did not finish");
            let events = nodes[1].poll(64);
            assert!(events.iter().all(|e| !matches!(e, Event::Message { .. })));
            if events.iter().any(|e| matches!(e, Event::Log { message } if message == "channel history fetched: 0 new verified records")) { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(nodes[1].shared.store.lock().unwrap().channel_history_read(&room, crate::store::now_ms()).unwrap().is_empty());
        nodes[1].block(provider.as_bytes()).unwrap();
        assert!(nodes[1].room_history_fetch(room_handle, provider.as_bytes(), hash, snapshot.len() as u64).is_err());
        assert!(nodes[1].poll(64).iter().all(|e| !matches!(e, Event::Message { .. })), "fetching history must not replay live chat");
        for node in &nodes { node.close(); }
    }

    #[test]
    fn automatic_catch_up_uses_another_member_after_author_leaves_and_on_rejoin() {
        let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let nodes: Vec<_> = dirs.iter().map(|dir| Node::open(Config {
            db_path: Some(dir.path().join("automatic-history.sqlite")),
            blob_dir: Some(dir.path().join("blobs")),
            relay: RelayMode::Disabled,
            app_id: "automatic-history-test".into(),
            ..Config::default()
        }).unwrap()).collect();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut ready = [false; 3];
        while !ready.iter().all(|v| *v) {
            assert!(Instant::now() < deadline, "nodes not ready");
            for (i, node) in nodes.iter().enumerate() {
                if node.poll(64).iter().any(|e| matches!(e, Event::Ready { .. })) { ready[i] = true; }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let room_a = nodes[0].channel_create("automatic").unwrap();
        while !nodes[0].poll(64).iter().any(|e| matches!(e, Event::RoomJoined { .. })) {
            assert!(Instant::now() < deadline, "first room did not join");
            std::thread::sleep(Duration::from_millis(10));
        }
        let original_ticket = nodes[0].room_ticket(room_a).unwrap();
        let topic = nodes[0].shared.rooms.read().unwrap()[&room_a].topic;
        let room_b = nodes[1].room_join(Scope::Custom, "", Some(&nodes[0].room_ticket(room_a).unwrap())).unwrap();
        nodes[1].room_history_enable(room_b, true).unwrap();
        let mut connected = [false; 2];
        while !connected.iter().all(|v| *v) {
            assert!(Instant::now() < deadline, "first peers did not connect");
            for i in 0..2 {
                if nodes[i].poll(64).iter().any(|e| matches!(e, Event::PeerJoined { .. })) { connected[i] = true; }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..2 { nodes[0].room_send_with_history(room_a, "repeated original").unwrap(); }
        let read = |i: usize| nodes[i].shared.store.lock().unwrap().channel_history_read(topic.as_bytes(), crate::store::now_ms()).unwrap();
        while read(1).len() != 2 {
            assert!(Instant::now() < deadline, "member did not retain originals");
            nodes[1].poll(64);
            std::thread::sleep(Duration::from_millis(10));
        }
        let originals = read(1);
        let author = nodes[0].shared.secret.public();
        nodes[0].close();
        let ticket_b = nodes[1].room_ticket(room_b).unwrap();
        let room_c = nodes[2].room_join(Scope::Custom, "", Some(&ticket_b)).unwrap();
        nodes[2].room_history_enable(room_c, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut catchup_events = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "late join did not catch up");
            for event in nodes[2].poll(64) {
                match event {
                    Event::ChannelRecord { room, peer, encoded, historical } => {
                        assert_eq!(room, room_c);
                        assert_eq!(peer, *author.as_bytes());
                        assert!(historical);
                        catchup_events.push(encoded);
                    }
                    Event::Message { .. } => panic!("backlog replayed as live text"),
                    _ => {},
                }
            }
            if read(2).len() == 2 && catchup_events.len() == 2 { break; }
            nodes[1].poll(64);
            std::thread::sleep(Duration::from_millis(10));
        }
        for original in &originals { assert!(catchup_events.iter().any(|bytes| bytes == original.encoded())); }
        for record in read(2) {
            assert_eq!(record.author(), author);
            assert!(originals.iter().any(|r| r.encoded() == record.encoded()));
        }
        while history::known_providers(&nodes[2].shared, &topic).is_empty() {
            assert!(Instant::now() < deadline, "authenticated provider was not remembered");
            nodes[2].poll(64);
            std::thread::sleep(Duration::from_millis(10));
        }
        nodes[2].room_leave(room_c).unwrap();
        while !nodes[2].poll(64).iter().any(|e| matches!(e, Event::RoomLeft { room } if *room == room_c)) {
            assert!(Instant::now() < deadline, "room did not leave");
            std::thread::sleep(Duration::from_millis(10));
        }
        nodes[1].room_send_with_history(room_b, "while you were gone").unwrap();
        while read(1).len() != 3 {
            assert!(Instant::now() < deadline, "offline message was not stored");
            std::thread::sleep(Duration::from_millis(10));
        }
        nodes[2].close();
        let reopened = Node::open(Config {
            db_path: Some(dirs[2].path().join("automatic-history.sqlite")),
            blob_dir: Some(dirs[2].path().join("reopened-blobs")),
            relay: RelayMode::Disabled,
            app_id: "automatic-history-test".into(),
            ..Config::default()
        }).unwrap();
        assert_eq!(reopened.shared.secret.public(), nodes[2].shared.secret.public());
        let room_c = reopened.room_join(Scope::Custom, "", Some(&original_ticket)).unwrap();
        reopened.room_history_enable(room_c, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut restored_events = HashSet::new();
        while read(2).len() != 3 || restored_events.len() != 3 {
            assert!(Instant::now() < deadline, "rejoin did not catch up");
            for event in reopened.poll(64) {
                match event {
                    Event::Message { .. } | Event::ChannelRecord { historical: false, .. } => panic!("rejoin replayed live text"),
                    Event::ChannelRecord { encoded, room, .. } => { assert_eq!(room, room_c); restored_events.insert(encoded); },
                    _ => {},
                }
            }
            nodes[1].poll(64);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(read(2).iter().any(|r| r.text() == "while you were gone" && r.author() == nodes[1].shared.secret.public()));
        assert!(reopened.poll(64).iter().all(|e| !matches!(e, Event::Message { .. } | Event::ChannelRecord { historical: false, .. })));
        for record in read(2) { assert!(restored_events.contains(record.encoded())); }
        reopened.close();
        for node in &nodes { node.close(); }
    }

    #[test]
    fn blocking_an_author_purges_pending_live_and_historical_metadata() {
        let node = Node::open(Config { relay: RelayMode::Disabled, ..Config::default() }).unwrap();
        let room = node.channel_create("pending-block").unwrap();
        node.channel_record_events_enable(true).unwrap();
        let topic = node.shared.rooms.read().unwrap()[&room].topic;
        let author = SecretKey::generate();
        let original = crate::channel_history::sign(&author, *topic.as_bytes(), 1, "blocked fixture").unwrap();
        let record = crate::channel_history::verify(topic.as_bytes(), &original).unwrap();
        assert!(history::emit(&node.shared, room, &record, false));
        let other_room = node.channel_create("unrelated-clear").unwrap();
        node.shared.emit(Event::ChannelHistoryCleared { room: other_room });
        let second = crate::channel_history::sign(&author, *topic.as_bytes(), 2, "historical fixture").unwrap();
        let record2 = crate::channel_history::verify(topic.as_bytes(), &second).unwrap();
        assert!(history::emit(&node.shared, room, &record2, true));
        let revision = node.shared.history_revision.load(Ordering::Relaxed);
        let snapshot = crate::channel_history::snapshot(*topic.as_bytes(), &[original.clone(), second]).unwrap();
        let hash = *blake3::hash(&snapshot).as_bytes();
        assert!(node.shared.history_cache.lock().unwrap().put(room, revision, hash, snapshot));
        assert!(node.shared.history_cache.lock().unwrap().get(room, revision).is_some());
        node.block(author.public().as_bytes()).unwrap();
        let blocked_revision = node.shared.history_revision.load(Ordering::Relaxed);
        assert_eq!(blocked_revision, revision + 1);
        assert!(node.shared.history_cache.lock().unwrap().get(room, blocked_revision).is_none());
        assert!(history::emit(&node.shared, room, &record, true));
        let events = node.poll(64);
        assert!(events.iter().all(|event| !matches!(event, Event::ChannelRecord { .. })));
        assert!(events.iter().any(|event| matches!(event, Event::ChannelHistoryCleared { room: got } if *got == other_room)));
        node.unblock(author.public().as_bytes()).unwrap();
        assert_eq!(node.shared.history_revision.load(Ordering::Relaxed), blocked_revision + 1);
        assert!(history::emit(&node.shared, room, &record, true));
        assert!(node.poll(64).iter().any(|event| matches!(event, Event::ChannelRecord { peer, .. } if peer == author.public().as_bytes())));
        node.room_leave(room).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "leave did not finish");
            if node.poll(64).iter().any(|event| matches!(event, Event::RoomLeft { room: got } if *got == room)) { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(history::emit(&node.shared, room, &record, true));
        assert!(node.poll(64).iter().all(|event| !matches!(event, Event::ChannelRecord { .. })));
        node.close();
    }

    #[test]
    fn full_saved_backlogs_survive_a_small_general_queue_and_single_event_polls() {
        let node = Node::open(Config { relay: RelayMode::Disabled, event_queue_cap: 16, ..Config::default() }).unwrap();
        let rooms = [node.channel_create("pressure-a").unwrap(), node.channel_create("pressure-b").unwrap()];
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut joined = HashSet::new();
        while joined.len() < 2 {
            assert!(Instant::now() < deadline);
            for e in node.poll(64) { if let Event::RoomJoined { room } = e { joined.insert(room); } }
            std::thread::sleep(Duration::from_millis(10));
        }
        let topics: HashMap<_, _> = rooms.iter().map(|r| (*r, node.shared.rooms.read().unwrap()[r].topic)).collect();
        for room in rooms {
            for i in 0..crate::channel_history::MAX_RECORDS {
                let raw = crate::channel_history::sign(&node.shared.secret, *topics[&room].as_bytes(), i as i64, &format!("saved {i}")).unwrap();
                assert!(node.shared.store.lock().unwrap().channel_history_insert(topics[&room].as_bytes(), &raw, crate::store::now_ms()).unwrap());
            }
            node.room_history_enable(room, true).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut received: HashMap<u64, HashSet<Vec<u8>>> = HashMap::new();
        while received.values().map(HashSet::len).sum::<usize>() < 512 {
            assert!(Instant::now() < deadline, "saved records were lost under queue pressure");
            for e in node.poll(1) {
                if let Event::ChannelRecord { room, encoded, historical, .. } = e {
                    assert!(historical);
                    assert!(crate::channel_history::verify(topics[&room].as_bytes(), &encoded).is_some());
                    assert!(received.entry(room).or_default().insert(encoded));
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        for room in rooms { assert_eq!(received[&room].len(), 256); }
        node.close();
    }

    #[test]
    fn signed_events_load_saved_history_and_clear_without_replaying_live_text() {
        let dir = tempfile::tempdir().unwrap();
        let node = Node::open(Config { db_path: Some(dir.path().join("saved.sqlite")), relay: RelayMode::Disabled, ..Config::default() }).unwrap();
        let room = node.channel_create("saved").unwrap();
        node.room_history_enable(room, true).unwrap();
        node.room_send_with_history(room, "own signed line").unwrap();
        let wait = |historical: bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                assert!(Instant::now() < deadline, "signed event did not arrive");
                for event in node.poll(64) {
                    assert!(!matches!(event, Event::Message { .. }), "signed mode emitted legacy live text");
                    if let Event::ChannelRecord { room: got, peer, encoded, historical: kind } = event {
                        assert_eq!(got, room);
                        assert_eq!(peer, *node.shared.secret.public().as_bytes());
                        assert_eq!(kind, historical);
                        return encoded;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let original = wait(false);
        node.room_history_enable(room, true).unwrap();
        assert_eq!(wait(true), original);
        let generation = node.shared.rooms.read().unwrap()[&room].history_generation.clone();
        let before_clear = generation.load(Ordering::Relaxed);
        node.room_history_clear(room).unwrap();
        let topic = node.shared.rooms.read().unwrap()[&room].topic;
        let deadline = Instant::now() + Duration::from_secs(20);
        while !node.shared.store.lock().unwrap().channel_history_read(topic.as_bytes(), crate::store::now_ms()).unwrap().is_empty() {
            assert!(Instant::now() < deadline, "local history did not clear");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(node.shared.rooms.read().unwrap()[&room].history_request.is_none());
        let delayed = crate::channel_history::verify(topic.as_bytes(), &original).unwrap();
        assert!(history::ingest_snapshot(&node.shared, room, &topic, &generation, before_clear, vec![delayed]).unwrap().is_none());
        assert!(node.shared.store.lock().unwrap().channel_history_read(topic.as_bytes(), crate::store::now_ms()).unwrap().is_empty());
        assert!(node.poll(64).iter().all(|e| !matches!(e, Event::Message { .. } | Event::ChannelRecord { .. })));
        node.close();
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn garbage_dial_targets_are_refused() {
        for s in ["", "nonsense", "ltroom", "ltnodezzz"] {
            assert!(parse_dial_target(s).is_err(), "{s} should not parse");
        }
    }

    #[test]
    fn a_node_ticket_dials_its_own_address() {
        let sk = SecretKey::generate();
        let addr = EndpointAddr::from(sk.public());
        let ticket = NodeTicket::new(addr.clone()).to_string();
        assert_eq!(parse_dial_target(&ticket).unwrap().id, addr.id);
    }
}
