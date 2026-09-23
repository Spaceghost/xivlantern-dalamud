//! The node: a synchronous handle in front of an async world.

#[cfg(feature = "calls")]
mod calls;
mod friends;
#[cfg(feature = "nostr")]
mod nostr;
mod selftest;

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
    ticket::{NodeTicket, RoomTicket, Scope},
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
    scope: Scope,
    label: String,
    peers: BTreeSet<EndpointId>,
    joined: bool,
    /// Set for an author channel: only announcements signed by this key are
    /// surfaced, and nothing else from the room is.
    author: Option<[u8; 32]>,
    /// Blobs we are actively serving into this room.
    offering: BTreeSet<[u8; 32]>,
}

#[derive(Debug, Clone)]
struct BlobState {
    hash: Option<[u8; 32]>,
    name: String,
    size: u64,
    /// Where to fetch it from, for a blob we learned about from an offer.
    from: Option<EndpointId>,
}

struct Shared {
    app_id: String,
    endpoint: Endpoint,
    secret: SecretKey,
    rooms: RwLock<HashMap<RoomHandle, RoomState>>,
    conns: RwLock<HashMap<ConnHandle, EndpointId>>,
    blobs: RwLock<HashMap<BlobHandle, BlobState>>,
    store: Mutex<Store>,
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
    fn handle(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Never blocks and never fails loudly: a caller that stops polling must not
    /// be able to wedge the network tasks.
    fn emit(&self, event: Event) {
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
        f.write_str("linkpearl")
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
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Shutdown => break,
                other => self.handle(other).await,
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
            Cmd::RoomBroadcast { room, frame } => {
                let bytes = proto::encode(&self.shared.secret, &frame);
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
            Cmd::BlobAddFile { blob, path } => self.blob_add_file(blob, path).await,
            Cmd::BlobFetch {
                blob,
                hash,
                from,
                out,
            } => self.blob_fetch(blob, hash, from, out).await,
        }
    }

    async fn room_join(&mut self, room: RoomHandle, topic: TopicId, bootstrap: Vec<EndpointAddr>) {
        let mut ids = Vec::new();
        for addr in bootstrap {
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
                        let bytes = proto::encode(&secret, &Frame::Hello { presence: mine });
                        let _ = tx.broadcast_neighbors(bytes.into()).await;
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
                        let Some((from, frame)) = proto::decode(&msg.content) else {
                            // Unsigned, forged or from a future protocol.
                            continue;
                        };
                        if shared.is_blocked(&from) || !shared.allow(&from, "room") {
                            continue;
                        }
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
                                let blob = shared.handle();
                                shared.blobs.write().unwrap().insert(
                                    blob,
                                    BlobState {
                                        hash: Some(hash),
                                        name: name.clone(),
                                        size,
                                        from: Some(from),
                                    },
                                );
                                shared.emit(Event::BlobOffer {
                                    room,
                                    peer,
                                    blob,
                                    meta: BlobMeta { hash, size, name },
                                });
                            }
                            Frame::BlobUnoffer { .. } => {}
                        }
                    }
                }
            }
        });
        self.tasks.insert(room, task);
    }

    fn room_leave(&mut self, room: RoomHandle) {
        self.senders.remove(&room);
        if let Some(task) = self.tasks.remove(&room) {
            task.abort();
        }
        self.shared.rooms.write().unwrap().remove(&room);
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
            .thread_name("linkpearl")
            .enable_all()
            .build()
            .map_err(|e| Error::Internal(format!("runtime: {e}")))?;

        let mut store = Store::open(config.db_path.as_deref())?;
        let secret = store.identity()?;

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
            store: Mutex::new(store),
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
        while out.len() < max {
            match rx.try_recv() {
                Ok(e) => out.push(e),
                Err(_) => break,
            }
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
        self.join(scope, topic, bootstrap, label, None)
    }

    fn join(
        &self,
        scope: Scope,
        topic: TopicId,
        bootstrap: Vec<EndpointAddr>,
        label: String,
        author: Option<[u8; 32]>,
    ) -> Result<RoomHandle> {
        let room = self.shared.handle();
        self.shared.rooms.write().unwrap().insert(
            room,
            RoomState {
                topic,
                scope,
                label,
                peers: BTreeSet::new(),
                joined: false,
                offering: BTreeSet::new(),
                author,
            },
        );
        self.push(Cmd::RoomJoin {
            room,
            topic,
            bootstrap,
        })?;
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
        self.join(Scope::Custom, topic, Vec::new(), label.to_string(), None)
    }

    /// Remember where a node can be reached (an `lpnode…` ticket), so dialing
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
        self.join(Scope::Public, topic, peers, "author".to_string(), Some(*author))
    }

    /// Publish an announcement signed offline (`lpannounce…`) into an author
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
                hash: None,
                name: name.to_string(),
                size: data.len() as u64,
                from: None,
            },
        );
        self.push(Cmd::BlobAddBytes {
            blob,
            data: data.to_vec(),
        })?;
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
                hash: None,
                name: name.to_string(),
                size: 0,
                from: None,
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
        let (hash, size, name) = {
            let blobs = self.shared.blobs.read().unwrap();
            let state = blobs.get(&blob).ok_or(Error::NotFound)?;
            let hash = state.hash.ok_or(Error::State("blob is still hashing"))?;
            (hash, state.size, state.name.clone())
        };
        {
            let mut rooms = self.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).ok_or(Error::NotFound)?;
            state.offering.insert(hash);
        }
        self.push(Cmd::RoomBroadcast {
            room,
            frame: Frame::BlobOffer { hash, size, name },
        })?;
        self.shared.emit(Event::Publishing {
            room,
            publishing: true,
        });
        Ok(())
    }

    pub fn blob_unoffer(&self, room: RoomHandle, blob: BlobHandle) -> Result<()> {
        let hash = {
            let blobs = self.shared.blobs.read().unwrap();
            blobs
                .get(&blob)
                .and_then(|s| s.hash)
                .ok_or(Error::NotFound)?
        };
        let still_publishing = {
            let mut rooms = self.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).ok_or(Error::NotFound)?;
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
    fn json_strings_are_escaped() {
        assert_eq!(json_string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn garbage_dial_targets_are_refused() {
        for s in ["", "nonsense", "lproom", "lpnodezzz"] {
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
