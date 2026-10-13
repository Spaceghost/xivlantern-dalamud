//! Automatic bounded catch-up. All SQL/crypto/network work stays on the worker.
pub(super) mod delivery;
mod peers;
use super::*;
use bytes::Bytes;
use iroh_blobs::store::readonly_mem::ReadonlyMemStore;

const REQUEST_LIFETIME: Duration = Duration::from_secs(15);
const SNAPSHOT_LEASE: Duration = Duration::from_secs(60);
const MAX_SNAPSHOTS: usize = 32;

#[derive(Debug, Clone)]
pub(super) struct Request {
    nonce: [u8; 16],
    started: Instant,
    sent: Instant,
    attempt: u8,
    providers: HashSet<EndpointId>,
}

struct Snapshot {
    rooms: HashSet<RoomHandle>,
    leased: Instant,
    bytes: Bytes,
    checked: Instant,
    revision: u64,
}

#[derive(Default)]
pub(super) struct Cache {
    entries: HashMap<[u8; 32], Snapshot>,
}

impl Cache {
    pub(super) fn prune(&mut self) {
        self.entries
            .retain(|_, s| s.leased.elapsed() < SNAPSHOT_LEASE);
    }
    pub(super) fn leave(&mut self, room: RoomHandle) {
        self.entries.retain(|_, s| {
            s.rooms.remove(&room);
            !s.rooms.is_empty()
        });
    }
    pub(super) fn get(&mut self, room: RoomHandle, revision: u64) -> Option<([u8; 32], u64)> {
        self.prune();
        let (hash, snapshot) = self.entries.iter_mut().find(|(_, s)| {
            s.rooms.contains(&room)
                && s.revision == revision
                && s.checked.elapsed() < Duration::from_secs(2)
        })?;
        snapshot.leased = Instant::now(); // Offer stays available for the fetch deadline.
        Some((*hash, snapshot.bytes.len() as u64))
    }
    pub(super) fn put(&mut self, room: RoomHandle, revision: u64, hash: [u8; 32], bytes: Vec<u8>) -> bool {
        self.put_bounded(room, revision, hash, bytes, crate::channel_history::MAX_SNAPSHOT_BYTES)
    }
    pub(super) fn put_sealed(&mut self, room: RoomHandle, revision: u64, hash: [u8; 32], bytes: Vec<u8>) -> bool {
        self.put_bounded(room, revision, hash, bytes, crate::channel_history::MAX_SEALED_SNAPSHOT_BYTES)
    }
    fn put_bounded(&mut self, room: RoomHandle, revision: u64, hash: [u8; 32], bytes: Vec<u8>, maximum: usize) -> bool {
        if bytes.len() < crate::channel_history::SNAPSHOT_HEADER
            || bytes.len() > maximum
        {
            return false;
        }
        self.prune();
        if !self.entries.contains_key(&hash) && self.entries.len() >= MAX_SNAPSHOTS {
            return false;
        }
        self.entries
            .entry(hash)
            .and_modify(|s| {
                s.rooms.insert(room);
                s.checked = Instant::now();
                s.leased = Instant::now();
                s.revision = revision;
            })
            .or_insert(Snapshot {
                rooms: HashSet::from([room]),
                leased: Instant::now(),
                checked: Instant::now(),
                revision,
                bytes: bytes.into(),
            });
        true
    }
}

pub(super) struct Protocol {
    pub shared: Arc<Shared>,
}
impl std::fmt::Debug for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryProtocol").finish_non_exhaustive()
    }
}
impl ProtocolHandler for Protocol {
    async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
        if self.shared.is_blocked(&connection.remote_id()) {
            connection.close(1u32.into(), b"blocked");
            return Ok(());
        }
        let Ok(_permit) = self.shared.history_serving.try_acquire() else {
            connection.close(1u32.into(), b"history busy");
            return Ok(());
        };
        let bytes: Vec<_> = {
            let mut cache = self.shared.history_cache.lock().unwrap();
            cache.prune();
            cache.entries.values().map(|s| s.bytes.clone()).collect()
        };
        // Separate read-only memory store: no history blobs in the normal disk
        // store. At most 32 snapshots and four short-lived provider connections.
        let store = ReadonlyMemStore::new(bytes);
        let protocol = BlobsProtocol::new(&store, None);
        let result =
            tokio::time::timeout(REQUEST_LIFETIME, protocol.accept(connection.clone())).await;
        connection.close(0u32.into(), b"history lease ended");
        let _ = tokio::time::timeout(Duration::from_secs(1), protocol.store().shutdown()).await;
        match result {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }
}

pub(super) fn emit(shared: &Shared, room: RoomHandle, record: &crate::channel_history::HistoryRecord, historical: bool) -> bool {
    if !shared.history_metadata.load(Ordering::Relaxed) { return false; }
    // Keep this read guard through enqueue: blocking purges the queue under
    // the corresponding write guard, so an in-flight publisher cannot race it.
    let blocked = shared.blocked.read().unwrap();
    if blocked.contains(&record.author()) { return true; }
    shared.emit(Event::ChannelRecord { room, peer: *record.author().as_bytes(), encoded: record.encoded().to_vec(), historical });
    true
}

/// Generation checks and writes share the SQLite mutex with clear. No room
/// lock is held during SQL/crypto, so game-thread room APIs cannot wait on it.
pub(super) fn ingest_snapshot(
    shared: &Shared, room: RoomHandle, topic: &TopicId,
    generation: &AtomicU64, expected: u64, records: Vec<crate::channel_history::HistoryRecord>,
) -> Result<Option<usize>> {
    let mut added = 0;
    for record in records {
        if shared.is_blocked(&record.author()) { continue; }
        let mut store = shared.store.lock().unwrap();
        if generation.load(Ordering::Relaxed) != expected { return Ok(None); }
        if store.channel_history_insert(topic.as_bytes(), record.encoded(), crate::store::now_ms())? {
            added += 1;
            shared.history_revision.fetch_add(1, Ordering::Relaxed);
            emit(shared, room, &record, true);
        }
    }
    Ok(Some(added))
}

pub(super) async fn request(shared: &Shared, room: RoomHandle, tx: &GossipSender, force: bool) {
    let frame = {
        let mut rooms = shared.rooms.write().unwrap();
        let Some(state) = rooms.get_mut(&room) else {
            return;
        };
        if !state.history_enabled || state.author.is_some() {
            return;
        }
        if !force
            && state
                .history_request
                .as_ref()
                .is_some_and(|r| r.started.elapsed() < REQUEST_LIFETIME)
        {
            return;
        }
        let nonce = friends::random::<16>();
        let now = Instant::now();
        state.history_request = Some(Request {
            nonce,
            started: now,
            sent: now,
            attempt: 0,
            providers: HashSet::new(),
        });
        Frame::HistoryRequest {
            topic: *state.topic.as_bytes(),
            nonce,
            attempt: 0,
        }
    };
    if let Ok(bytes) = shared.encode_room(room, &frame) {
        let _ = tx.broadcast(bytes.into()).await;
    }
}

pub(super) async fn tick(shared: &Shared, senders: &HashMap<RoomHandle, GossipSender>) {
    shared.history_cache.lock().unwrap().prune();
    let frames: Vec<_> = {
        let mut rooms = shared.rooms.write().unwrap();
        rooms
            .iter_mut()
            .filter_map(|(&room, state)| {
                let pending = state.history_request.as_mut()?;
                if pending.started.elapsed() >= REQUEST_LIFETIME {
                    state.history_request = None;
                    return None;
                }
                if !state.history_enabled
                    || pending.sent.elapsed() < Duration::from_secs(2)
                    || pending.attempt >= 7
                {
                    return None;
                }
                pending.attempt += 1;
                pending.sent = Instant::now();
                Some((
                    room,
                    Frame::HistoryRequest {
                        topic: *state.topic.as_bytes(),
                        nonce: pending.nonce,
                        attempt: pending.attempt,
                    },
                ))
            })
            .collect()
    };
    for (room, frame) in frames {
        if let Some(tx) = senders.get(&room) {
            if let Ok(bytes) = shared.encode_room(room, &frame) {
                let _ = tx.broadcast(bytes.into()).await;
            }
        }
    }
}

pub(super) async fn offer(
    shared: &Shared,
    room: RoomHandle,
    tx: &GossipSender,
    to: EndpointId,
    frame_topic: [u8; 32],
    nonce: [u8; 16],
    attempt: u8,
) {
    if attempt > 7 {
        return;
    }
    let (topic, sealed) = {
        let rooms = shared.rooms.read().unwrap();
        let Some(state) = rooms.get(&room) else {
            return;
        };
        if !state.history_enabled
            || state.author.is_some()
            || state.topic.as_bytes() != &frame_topic
        {
            return;
        }
        (state.topic, state.sealed.clone())
    };
    let revision = shared.history_revision.load(Ordering::Relaxed);
    let cached = shared.history_cache.lock().unwrap().get(room, revision);
    let (hash, size) = match cached {
        Some(cached) => cached,
        None => {
            let records = match shared
                .store
                .lock()
                .unwrap()
                .channel_history_read(topic.as_bytes(), crate::store::now_ms())
            {
                Ok(records) => records,
                Err(_) => return,
            };
            let records: Vec<_> = records
                .into_iter()
                .filter(|r| !shared.is_blocked(&r.author()))
                .map(|r| r.encoded().to_vec())
                .collect();
            if records.is_empty() {
                return;
            }
            let encoded = match &sealed {
                Some(ticket) => crate::channel_history::sealed_snapshot(&shared.secret, ticket, &records),
                None => crate::channel_history::snapshot(*topic.as_bytes(), &records),
            };
            let Ok(bytes) = encoded else { return; };
            let hash = *blake3::hash(&bytes).as_bytes();
            let size = bytes.len() as u64;
            // A snapshot lease protects an already-offered payload. This never
            // refreshes any signed record's database receipt or retention age.
            let admitted = {
                let mut cache = shared.history_cache.lock().unwrap();
                if sealed.is_some() { cache.put_sealed(room, revision, hash, bytes) }
                else { cache.put(room, revision, hash, bytes) }
            };
            if !admitted { return; }
            (hash, size)
        }
    };
    let frame = Frame::HistoryOffer {
        topic: *topic.as_bytes(),
        to: *to.as_bytes(),
        nonce,
        attempt,
        hash,
        size,
        provider: shared.endpoint.addr(),
    };
    if let Ok(bytes) = shared.encode_room(room, &frame) {
        let _ = tx.broadcast(bytes.into()).await;
    }
}

/// Worker-only: saved offers are routing claims, not renewed snapshot offers.
pub(super) fn known_providers(shared: &Shared, topic: &TopicId) -> Vec<EndpointAddr> {
    known_providers_sealed(shared, topic, None)
}

pub(super) fn known_providers_sealed(shared: &Shared, topic: &TopicId, sealed: Option<&SealedRoomTicket>) -> Vec<EndpointAddr> {
    let rows = shared.store.lock().unwrap().channel_peer_hints(topic.as_bytes(), crate::store::now_ms());
    let Ok(rows) = rows else { return Vec::new(); };
    rows.into_iter().filter_map(|(peer, original)| {
        let addr = peers::verified_address_sealed(topic.as_bytes(), shared.secret.public().as_bytes(), &original, sealed)?;
        (addr.id.as_bytes() == &peer && addr.id != shared.secret.public() && !shared.is_blocked(&addr.id)).then_some(addr)
    }).collect()
}

pub(super) fn receive_offer(
    shared: Arc<Shared>,
    room: RoomHandle,
    from: EndpointId,
    frame_topic: [u8; 32],
    to: PeerId,
    nonce: [u8; 16],
    attempt: u8,
    hash: [u8; 32],
    size: u64,
    provider: EndpointAddr,
    original: &[u8],
) {
    if to != *shared.secret.public().as_bytes()
        || provider.id != from
        || attempt > 7
        || size < crate::channel_history::SNAPSHOT_HEADER as u64
    {
        return;
    }
    let (topic, generation, expected_generation, sealed) = {
        let mut rooms = shared.rooms.write().unwrap();
        let Some(state) = rooms.get_mut(&room) else {
            return;
        };
        if !state.history_enabled || state.topic.as_bytes() != &frame_topic {
            return;
        }
        let maximum = if state.sealed.is_some() { crate::channel_history::MAX_SEALED_SNAPSHOT_BYTES } else { crate::channel_history::MAX_SNAPSHOT_BYTES };
        if size > maximum as u64 { return; }
        let Some(pending) = state.history_request.as_mut() else {
            return;
        };
        if pending.nonce != nonce
            || pending.started.elapsed() >= REQUEST_LIFETIME
            || pending.providers.len() >= 4
            || !pending.providers.insert(from)
        {
            return;
        }
        (state.topic, state.history_generation.clone(), state.history_generation.load(Ordering::Relaxed), state.sealed.clone())
    };
    let original = (original.len() <= peers::MAX_HINT_BYTES).then(|| original.to_vec());
    shared.lookup.add_endpoint_info(provider);
    tokio::spawn(async move {
        let records = match crate::channel_history::network::fetch_snapshot_over(
            &shared.endpoint,
            &shared.history_downloads,
            from,
            hash,
            size,
            topic.as_bytes(),
            &shared.history_alpn,
            sealed.as_ref(),
        )
        .await
        {
            Ok(records) => records,
            Err(_) => return,
        };
        let enabled = shared.rooms.read().unwrap().get(&room).is_some_and(|s| {
            s.history_enabled && s.history_request.as_ref().is_some_and(|r| r.nonce == nonce)
        });
        if !enabled || shared.closed.load(Ordering::Relaxed) || shared.is_blocked(&from) {
            return;
        }
        let added = match ingest_snapshot(&shared, room, &topic, &generation, expected_generation, records) {
            Ok(Some(added)) => added,
            _ => return,
        };
        if let Some(original) = original {
            if let Some(addr) = peers::verified_address_sealed(topic.as_bytes(), shared.secret.public().as_bytes(), &original, sealed.as_ref()) {
                if addr.id == from {
                    let mut store = shared.store.lock().unwrap();
                    if generation.load(Ordering::Relaxed) == expected_generation {
                        if let Err(e) = store.channel_peer_hint_put(topic.as_bytes(), from.as_bytes(), &original, crate::store::now_ms()) {
                            shared.error(format!("channel provider cache failed: {e}"));
                        }
                    }
                }
            }
        }
        shared.emit(Event::Log {
            message: format!("channel catch-up stored: {added} new verified records"),
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembered_routes_reverify_originals_and_apply_current_blocks() {
        let node = Node::open(Config { relay: RelayMode::Disabled, ..Config::default() }).unwrap();
        let topic = TopicId::from_bytes([3; 32]);
        let sender = SecretKey::generate();
        let to = *node.shared.secret.public().as_bytes();
        let raw = proto::encode(&sender, &Frame::HistoryOffer {
            topic: *topic.as_bytes(), to, nonce: [1; 16], attempt: 0, hash: [2; 32], size: 42,
            provider: EndpointAddr::new(sender.public()),
        });
        let now = crate::store::now_ms();
        node.shared.store.lock().unwrap().channel_peer_hint_put(topic.as_bytes(), sender.public().as_bytes(), &raw, now).unwrap();
        assert_eq!(known_providers(&node.shared, &topic), [EndpointAddr::new(sender.public())]);
        assert!(known_providers(&node.shared, &TopicId::from_bytes([4; 32])).is_empty());
        node.block(sender.public().as_bytes()).unwrap();
        assert!(known_providers(&node.shared, &topic).is_empty());
        node.unblock(sender.public().as_bytes()).unwrap();
        let mut forged = raw.clone(); *forged.last_mut().unwrap() ^= 1;
        node.shared.store.lock().unwrap().channel_peer_hint_put(topic.as_bytes(), sender.public().as_bytes(), &forged, now).unwrap();
        assert!(known_providers(&node.shared, &topic).is_empty());
        node.shared.store.lock().unwrap().channel_peer_hint_put(topic.as_bytes(), &[8; 32], &raw, now).unwrap();
        assert!(known_providers(&node.shared, &topic).is_empty(), "row key must match signed provider");
        node.close();
    }

    #[test]
    fn snapshot_leases_owners_revision_and_capacity_are_bounded() {
        let author = SecretKey::generate();
        let make = |topic| {
            let record = crate::channel_history::sign(&author, topic, 42, "kept original").unwrap();
            let bytes = crate::channel_history::snapshot(topic, &[record]).unwrap();
            (*blake3::hash(&bytes).as_bytes(), bytes)
        };
        let (hash, bytes) = make([7; 32]);
        let mut cache = Cache::default();
        assert!(!cache.put(1, 0, hash, vec![]));
        assert!(!cache.put(
            1,
            0,
            hash,
            vec![0; crate::channel_history::MAX_SNAPSHOT_BYTES + 1]
        ));
        assert!(cache.put(1, 0, hash, bytes.clone()));
        cache.entries.get_mut(&hash).unwrap().leased = Instant::now() - Duration::from_secs(59);
        assert_eq!(cache.get(1, 0), Some((hash, bytes.len() as u64)));
        assert!(
            cache.entries[&hash].leased.elapsed() < Duration::from_secs(1),
            "an offer must leave time for its fetch"
        );
        assert!(
            cache.get(1, 1).is_none(),
            "new stored records invalidate a previous snapshot"
        );
        assert!(cache.put(2, 1, hash, bytes));
        cache.leave(1);
        assert_eq!(
            cache.entries.len(),
            1,
            "another local handle still serves this exact hash"
        );
        cache.leave(2);
        assert!(cache.entries.is_empty());
        for i in 0..MAX_SNAPSHOTS {
            let (hash, bytes) = make([i as u8; 32]);
            assert!(cache.put(i as u64 + 1, 1, hash, bytes));
        }
        let (extra_hash, extra_bytes) = make([99; 32]);
        assert!(!cache.put(99, 1, extra_hash, extra_bytes.clone()));
        assert_eq!(cache.entries.len(), MAX_SNAPSHOTS);
        cache.entries.values_mut().next().unwrap().leased =
            Instant::now() - Duration::from_secs(61);
        assert!(cache.put(99, 1, extra_hash, extra_bytes));
        assert_eq!(cache.entries.len(), MAX_SNAPSHOTS);
    }

    #[test]
    fn invalid_stale_or_unsolicited_offers_never_start_a_download() {
        let node = Node::open(Config {
            relay: RelayMode::Disabled,
            app_id: "history-offer-guards".into(),
            ..Config::default()
        })
        .unwrap();
        let room = node.channel_create("guards").unwrap();
        let topic = *node.shared.rooms.read().unwrap()[&room].topic.as_bytes();
        let nonce = [1; 16];
        let from = SecretKey::generate().public();
        let other = SecretKey::generate().public();
        let to = *node.shared.secret.public().as_bytes();
        {
            let mut rooms = node.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).unwrap();
            state.history_enabled = true;
            state.history_request = Some(Request {
                nonce,
                started: Instant::now(),
                sent: Instant::now(),
                attempt: 0,
                providers: HashSet::new(),
            });
        }
        let call = |topic, to, nonce, attempt, size, provider| {
            receive_offer(
                node.shared.clone(),
                room,
                from,
                topic,
                to,
                nonce,
                attempt,
                [0; 32],
                size,
                provider,
                &[],
            )
        };
        // Calls occur outside a runtime: an accidental accepted offer would also
        // fail immediately at tokio::spawn instead of making a network request.
        call(
            topic,
            *other.as_bytes(),
            nonce,
            0,
            42,
            EndpointAddr::from(from),
        );
        call([0; 32], to, nonce, 0, 42, EndpointAddr::from(from));
        call(topic, to, [2; 16], 0, 42, EndpointAddr::from(from));
        call(topic, to, nonce, 8, 42, EndpointAddr::from(from));
        call(
            topic,
            to,
            nonce,
            0,
            crate::channel_history::MAX_SNAPSHOT_BYTES as u64 + 1,
            EndpointAddr::from(from),
        );
        call(topic, to, nonce, 0, 0, EndpointAddr::from(from));
        call(topic, to, nonce, 0, 42, EndpointAddr::from(other));
        assert!(node.shared.rooms.read().unwrap()[&room]
            .history_request
            .as_ref()
            .unwrap()
            .providers
            .is_empty());
        node.shared
            .rooms
            .write()
            .unwrap()
            .get_mut(&room)
            .unwrap()
            .history_request
            .as_mut()
            .unwrap()
            .started = Instant::now() - Duration::from_secs(16);
        call(topic, to, nonce, 0, 42, EndpointAddr::from(from));
        assert!(node.shared.rooms.read().unwrap()[&room]
            .history_request
            .as_ref()
            .unwrap()
            .providers
            .is_empty());
        {
            let mut rooms = node.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).unwrap();
            state.history_request.as_mut().unwrap().started = Instant::now();
            state.history_enabled = false;
        }
        call(topic, to, nonce, 0, 42, EndpointAddr::from(from));
        {
            let mut rooms = node.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).unwrap();
            state.history_enabled = true;
            state
                .history_request
                .as_mut()
                .unwrap()
                .providers
                .insert(from);
        }
        call(topic, to, nonce, 0, 42, EndpointAddr::from(from)); // Replayed offer.
        assert_eq!(
            node.shared.rooms.read().unwrap()[&room]
                .history_request
                .as_ref()
                .unwrap()
                .providers
                .len(),
            1
        );
        {
            let mut rooms = node.shared.rooms.write().unwrap();
            let state = rooms.get_mut(&room).unwrap();
            state.history_request.as_mut().unwrap().providers =
                (0..4).map(|_| SecretKey::generate().public()).collect();
        }
        call(topic, to, nonce, 0, 42, EndpointAddr::from(from)); // Per-request provider bound.
        assert_eq!(
            node.shared.rooms.read().unwrap()[&room]
                .history_request
                .as_ref()
                .unwrap()
                .providers
                .len(),
            4
        );
        node.shared
            .rooms
            .write()
            .unwrap()
            .get_mut(&room)
            .unwrap()
            .history_request = None;
        call(topic, to, nonce, 0, 42, EndpointAddr::from(from));
        assert_eq!(node.shared.history_downloads.available_permits(), 4);
        node.close();
    }
}
