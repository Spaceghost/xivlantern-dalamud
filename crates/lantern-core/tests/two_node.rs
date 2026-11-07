//! The test that decides whether any of this is real: two nodes, in one
//! process, meeting over the loopback interface with the relays switched off,
//! then exchanging a room message, a presence update, a direct message and a
//! blob.
//!
//! Relays are disabled deliberately. Nothing here touches the public internet,
//! so it can run in CI, in a container, and on a machine with no DNS.

use std::time::{Duration, Instant};

use lantern_core::{Config, Event, Node, RelayMode, Scope};

fn node(name: &str, dir: &std::path::Path) -> Node {
    let config = Config {
        db_path: Some(dir.join(format!("{name}.sqlite"))),
        blob_dir: Some(dir.join(format!("{name}.blobs"))),
        app_id: "lantern-test/1".to_string(),
        // No relay: the two nodes must find each other on the local interface.
        relay: RelayMode::Disabled,
        event_queue_cap: 1024,
        accept_inbound: true,
        ..Config::default()
    };
    Node::open(config).expect("node must open")
}

/// Poll both nodes until `pred` sees the event it wants, or give up.
///
/// Every event is handed to the predicate and also accumulated, so a test can
/// assert on what happened *and* fail with the whole transcript.
///
/// Every polled event goes into a per-node backlog and `wait` removes only the
/// event it matched, so an event polled before anyone asked for it (or in the
/// same batch as another match) is not lost.
struct Pump {
    log: Vec<(usize, Event)>,
    pending: [std::collections::VecDeque<Event>; 2],
}

impl Pump {
    fn new() -> Self {
        Pump {
            log: Vec::new(),
            pending: Default::default(),
        }
    }

    fn wait<F>(&mut self, nodes: [&Node; 2], what: &str, secs: u64, mut pred: F) -> Event
    where
        F: FnMut(usize, &Event) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            for (i, n) in nodes.iter().enumerate() {
                let fresh = n.poll(64);
                self.log.extend(fresh.iter().cloned().map(|e| (i, e)));
                self.pending[i].extend(fresh);
                if let Some(pos) = self.pending[i].iter().position(|e| pred(i, e)) {
                    return self.pending[i].remove(pos).unwrap();
                }
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what} after {secs}s\ntranscript:\n{}",
                    self.transcript()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn transcript(&self) -> String {
        self.log
            .iter()
            .map(|(i, e)| format!("  node{i}: {e:?}\n"))
            .collect()
    }
}

#[test]
fn two_nodes_meet_talk_and_share_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = node("alice", dir.path());
    let b = node("bob", dir.path());
    let mut pump = Pump::new();

    assert_ne!(a.node_id(), b.node_id(), "two nodes, two identities");

    // --- ready ------------------------------------------------------------
    pump.wait([&a, &b], "both nodes ready", 10, {
        let mut seen = [false, false];
        move |i, e| {
            if matches!(e, Event::Ready { .. }) {
                seen[i] = true;
            }
            seen[0] && seen[1]
        }
    });

    // --- rooms ------------------------------------------------------------
    let room_a = a
        .room_join(Scope::Party, "a-party-of-two", None)
        .expect("alice joins her own room");
    pump.wait([&a, &b], "alice joined", 10, |i, e| {
        i == 0 && matches!(e, Event::RoomJoined { .. })
    });

    let ticket = a.room_ticket(room_a).expect("ticket");
    assert!(ticket.starts_with("ltroom"), "ticket shape: {ticket}");

    let room_b = b
        .room_join(Scope::Party, "", Some(&ticket))
        .expect("bob joins from the ticket");

    pump.wait([&a, &b], "alice sees bob", 30, |i, e| {
        i == 0 && matches!(e, Event::PeerJoined { .. })
    });
    // Each side learns of the neighbour from its own gossip event; Alice's
    // arriving first says nothing about Bob's.
    pump.wait([&a, &b], "bob sees alice", 30, |i, e| {
        i == 1 && matches!(e, Event::PeerJoined { .. })
    });

    // Bob's view of the room must name Alice.
    let peers = b.room_peers(room_b).expect("peers");
    assert!(
        peers.contains(&a.node_id()),
        "bob should know alice: {peers:?}"
    );

    // --- presence ---------------------------------------------------------
    a.presence_set(room_a, b"Alice@Ultros").expect("presence");
    let seen = pump.wait([&a, &b], "bob sees alice's presence", 30, |i, e| {
        i == 1 && matches!(e, Event::Presence { .. })
    });
    match seen {
        Event::Presence { peer, data, .. } => {
            assert_eq!(peer, a.node_id(), "presence is attributed to its author");
            assert_eq!(&data, b"Alice@Ultros");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // --- room message -----------------------------------------------------
    a.room_send(room_a, b"shall we queue?").expect("send");
    let seen = pump.wait([&a, &b], "bob receives the chat line", 30, |i, e| {
        i == 1 && matches!(e, Event::Message { .. })
    });
    match seen {
        Event::Message { room, peer, data } => {
            assert_eq!(room, room_b);
            assert_eq!(peer, a.node_id());
            assert_eq!(&data, b"shall we queue?");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // --- blob: add, offer, fetch -----------------------------------------
    let payload: Vec<u8> = (0..64_000u32).map(|i| (i % 251) as u8).collect();
    let blob_a = a
        .blob_add_bytes(&payload, "screenshot.png")
        .expect("add bytes");
    pump.wait([&a, &b], "alice finished hashing", 30, |i, e| {
        i == 0 && matches!(e, Event::BlobDone { .. })
    });

    assert!(
        !a.is_publishing(room_a),
        "hashing something must not publish it"
    );
    a.blob_offer(room_a, blob_a).expect("offer");
    assert!(
        a.is_publishing(room_a),
        "offering is the moment the UI must show 'publishing'"
    );

    let offer = pump.wait([&a, &b], "bob sees the offer", 30, |i, e| {
        i == 1 && matches!(e, Event::BlobOffer { .. })
    });
    let blob_b = match offer {
        Event::BlobOffer { blob, peer, meta, .. } => {
            assert_eq!(peer, a.node_id());
            assert_eq!(meta.name, "screenshot.png");
            assert_eq!(meta.size, payload.len() as u64);
            blob
        }
        other => panic!("unexpected: {other:?}"),
    };

    let out = dir.path().join("bob-download.bin");
    b.blob_fetch(blob_b, out.to_str().unwrap()).expect("fetch");
    pump.wait([&a, &b], "bob finished downloading", 60, |i, e| {
        i == 1 && matches!(e, Event::BlobDone { .. })
    });

    let got = std::fs::read(&out).expect("downloaded file");
    assert_eq!(got.len(), payload.len(), "downloaded the whole file");
    assert_eq!(got, payload, "bytes match, so the hash matched");

    let bounded_out = dir.path().join("bob-bounded.download");
    b.blob_fetch_bounded(blob_b, bounded_out.to_str().unwrap()).expect("bounded fetch");
    pump.wait([&a, &b], "bounded download complete", 40, |i, e| {
        i == 1 && matches!(e, Event::BlobDone { blob, .. } if *blob == blob_b)
    });
    assert_eq!(std::fs::read(&bounded_out).unwrap(), payload);
    std::fs::write(&bounded_out, b"existing local file").unwrap();
    b.blob_fetch_bounded(blob_b, bounded_out.to_str().unwrap()).expect("queue overwrite rejection");
    pump.wait([&a, &b], "bounded destination already exists", 40, |i, e| {
        i == 1 && matches!(e, Event::BlobFailed { blob, .. } if *blob == blob_b)
    });
    assert_eq!(std::fs::read(&bounded_out).unwrap(), b"existing local file");
    b.block(&a.node_id()).unwrap();
    assert!(b.blob_fetch_bounded(blob_b, dir.path().join("blocked.download").to_str().unwrap()).is_err());
    b.unblock(&a.node_id()).unwrap();

    a.blob_unoffer(room_a, blob_a).expect("unoffer");
    pump.wait([&a, &b], "remote offer withdrawn", 30, |i, e| {
        i == 1 && matches!(e, Event::BlobWithdrawn { blob } if *blob == blob_b)
    });
    assert!(b.blob_fetch_bounded(blob_b, dir.path().join("withdrawn.download").to_str().unwrap()).is_err());
    assert!(
        !a.is_publishing(room_a),
        "withdrawing the offer must clear the indicator"
    );
    a.blob_offer(room_a, blob_a).expect("offer again after withdrawal");
    let offered_again = pump.wait([&a, &b], "bob sees fresh offer", 30, |i, e| {
        i == 1 && matches!(e, Event::BlobOffer { .. })
    });
    let again = match offered_again { Event::BlobOffer { blob, .. } => blob, _ => unreachable!() };
    assert_ne!(again, blob_b, "removed offers never reuse a stale handle");
    b.room_leave(room_b).expect("bob leaves file channel");
    pump.wait([&a, &b], "bob left file channel", 30, |i, e| {
        i == 1 && matches!(e, Event::RoomLeft { room } if *room == room_b)
    });
    assert!(b.blob_fetch_bounded(again, dir.path().join("left-channel.download").to_str().unwrap()).is_err());

    // --- direct connection ------------------------------------------------
    let ticket = a.node_ticket();
    assert!(ticket.starts_with("ltnode"), "node ticket shape: {ticket}");
    let conn_b = b.connect(&ticket).expect("dial alice");
    pump.wait([&a, &b], "alice accepts the direct connection", 30, |i, e| {
        i == 0 && matches!(e, Event::Connected { .. })
    });
    b.send(conn_b, b"just between us").expect("direct send");
    let seen = pump.wait([&a, &b], "alice receives the direct message", 30, |i, e| {
        i == 0 && matches!(e, Event::Direct { .. })
    });
    match seen {
        Event::Direct { peer, data, .. } => {
            assert_eq!(peer, b.node_id());
            assert_eq!(&data, b"just between us");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // --- stats and teardown ------------------------------------------------
    let stats = a.stats();
    assert!(stats.bytes_sent > 0, "we sent something");
    assert_eq!(stats.rooms, 1);
    assert_eq!(stats.events_dropped, 0, "nobody starved the event queue");

    a.room_leave(room_a).expect("leave");
    pump.wait([&a, &b], "alice left", 10, |i, e| {
        i == 0 && matches!(e, Event::RoomLeft { .. })
    });
    assert!(
        a.room_ticket(room_a).is_err(),
        "a room you left has no ticket"
    );

    a.close();
    b.close();
    assert!(
        a.room_join(Scope::Public, "after-close", None).is_err(),
        "a closed node refuses work instead of pretending"
    );
}

#[test]
fn identity_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = {
        let n = node("persistent", dir.path());
        let id = n.node_id();
        n.close();
        id
    };
    let second = {
        let n = node("persistent", dir.path());
        let id = n.node_id();
        n.close();
        id
    };
    assert_eq!(
        first, second,
        "the node id is what other players bookmark; it must not rotate"
    );
}

#[test]
fn a_room_with_no_ticket_and_no_key_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let n = node("solo", dir.path());
    assert!(n.room_join(Scope::Zone, "", None).is_err());
    // A derived room needs no network, so this one succeeds immediately.
    assert!(n.room_join(Scope::Zone, "153", None).is_ok());
    n.close();
}

#[test]
fn oversized_messages_are_refused_before_they_hit_the_wire() {
    let dir = tempfile::tempdir().expect("tempdir");
    let n = node("big", dir.path());
    let room = n.room_join(Scope::Custom, "big", None).unwrap();
    let too_big = vec![0u8; lantern_core::MAX_ROOM_MESSAGE + 1];
    assert!(n.room_send(room, &too_big).is_err());
    assert!(n.room_send(room, &too_big[..lantern_core::MAX_ROOM_MESSAGE]).is_ok());
    n.close();
}

#[test]
fn a_node_with_no_database_opens_in_memory() {
    // db_path None is what a throwaway selftest node uses; it once panicked
    // because the in-memory blob store was created outside the runtime.
    let config = Config {
        relay: RelayMode::Disabled,
        ..Config::in_memory("lantern-test/1")
    };
    let a = Node::open(config.clone()).expect("in-memory node opens");
    let b = Node::open(config).expect("and another");
    assert_ne!(a.node_id(), b.node_id(), "no database, so a fresh identity each time");
    a.close();
    b.close();
}

#[test]
fn sealed_channels_deliver_presence_signed_history_and_reseal() {
    let dir = tempfile::tempdir().unwrap();
    let a = node("sealed-alice", dir.path());
    let b = node("sealed-bob", dir.path());
    let mut pump = Pump::new();
    a.channel_record_events_enable(true).unwrap();
    b.channel_record_events_enable(true).unwrap();
    let room_a = a.channel_create_sealed("Private friends").unwrap();
    pump.wait([&a, &b], "sealed creator joined", 20, |i, e|
        i == 0 && matches!(e, Event::RoomJoined { room } if *room == room_a));
    assert!(a.room_is_sealed(room_a).unwrap());
    assert_eq!(a.room_scope(room_a).unwrap(), Scope::Custom);
    let text = a.room_ticket(room_a).unwrap();
    let ticket: lantern_core::ticket::SealedRoomTicket = text.parse().unwrap();
    assert!(text.starts_with("ltsealed"));
    assert!(text.parse::<lantern_core::RoomTicket>().is_err());
    assert!(b.room_join(Scope::Party, "", Some(&text)).is_err());
    let forged_plain = lantern_core::RoomTicket::new(ticket.topic, Scope::Custom,
        "downgrade", ticket.peers.clone()).to_string();
    assert!(a.room_join(Scope::Custom, "", Some(&forged_plain)).is_err());
    a.room_history_enable(room_a, true).unwrap();
    a.room_send_with_history(room_a, "before Bob joined").unwrap();
    let original = pump.wait([&a, &b], "creator's original signed line", 20, |i, e|
        i == 0 && matches!(e, Event::ChannelRecord { room, historical: false, .. } if *room == room_a));
    let Event::ChannelRecord { encoded: original, .. } = original else { unreachable!() };
    a.presence_set(room_a, b"private greeting").unwrap();
    let room_b = b.room_join(Scope::Custom, "", Some(&text)).unwrap();
    b.room_history_enable(room_b, true).unwrap();
    pump.wait([&a, &b], "Bob joined sealed channel", 30, |i, e|
        i == 1 && matches!(e, Event::RoomJoined { room } if *room == room_b));
    pump.wait([&a, &b], "sealed hello presence", 30, |i, e|
        i == 1 && matches!(e, Event::Presence { room, data, .. } if *room == room_b && data == b"private greeting"));
    let caught = pump.wait([&a, &b], "encrypted automatic history catch-up", 30, |i, e|
        i == 1 && matches!(e, Event::ChannelRecord { room, historical: true, .. } if *room == room_b));
    let Event::ChannelRecord { encoded: caught, peer, .. } = caught else { unreachable!() };
    assert_eq!(caught, original);
    assert_eq!(peer, a.node_id());
    b.presence_set(room_b, b"private update").unwrap();
    pump.wait([&a, &b], "sealed presence update", 30, |i, e|
        i == 0 && matches!(e, Event::Presence { room, data, .. } if *room == room_a && data == b"private update"));
    b.room_send_with_history(room_b, "same private line").unwrap();
    b.room_send_with_history(room_b, "same private line").unwrap();
    let mut ids = std::collections::HashSet::new();
    for _ in 0..2 {
        let event = pump.wait([&a, &b], "distinct repeated sealed chat", 30, |i, e|
            i == 0 && matches!(e, Event::ChannelRecord { room, historical: false, .. } if *room == room_a));
        let Event::ChannelRecord { encoded, .. } = event else { unreachable!() };
        let record = lantern_core::channel_history::verify(ticket.topic.as_bytes(), &encoded).unwrap();
        assert_eq!(record.text(), "same private line");
        assert!(ids.insert(*record.id()));
    }
    // Ordinary plaintext imports cannot be published into a sealed channel.
    let plain = a.blob_add_bytes(b"private file", "private.png").unwrap();
    pump.wait([&a, &b], "local plain file prepared", 20, |i, e|
        i == 0 && matches!(e, Event::BlobDone { blob: done, .. } if *done == plain));
    assert!(a.blob_offer(room_a, plain).is_err());
    assert!(!a.is_publishing(room_a));
    let bytes: Vec<u8> = (0..65537).map(|i| (i % 251) as u8).collect();
    let blob = a.blob_add_sealed_bytes(room_a, &bytes, "private.png").unwrap();
    pump.wait([&a, &b], "encrypted file prepared", 20, |i, e|
        i == 0 && matches!(e, Event::BlobDone { blob: done, .. } if *done == blob));
    assert_ne!(a.blob_hash(blob).unwrap(), *blake3::hash(&bytes).as_bytes(), "served blob is ciphertext");
    a.blob_offer(room_a, blob).unwrap();
    let offer = pump.wait([&a, &b], "sealed file offer", 30, |i, e|
        i == 1 && matches!(e, Event::BlobOffer { room, .. } if *room == room_b));
    let Event::BlobOffer { blob: received, meta, .. } = offer else { unreachable!() };
    assert_eq!(meta.size, bytes.len() as u64);
    assert_eq!(meta.name, "private.png");
    let destination = dir.path().join("sealed.download");
    assert!(!destination.exists(), "offers do not trigger downloads");
    assert!(b.blob_fetch(received, destination.to_str().unwrap()).is_err(), "generic fetch cannot export ciphertext");
    b.blob_fetch_bounded(received, destination.to_str().unwrap()).unwrap();
    pump.wait([&a, &b], "authenticated decrypted file saved", 30, |i, e|
        i == 1 && matches!(e, Event::BlobDone { blob, .. } if *blob == received));
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    b.blob_fetch_bounded(received, destination.to_str().unwrap()).unwrap();
    pump.wait([&a, &b], "sealed file refuses overwrite", 30, |i, e|
        i == 1 && matches!(e, Event::BlobFailed { blob, .. } if *blob == received));
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    a.blob_unoffer(room_a, blob).unwrap();
    pump.wait([&a, &b], "sealed file withdrawn", 30, |i, e|
        i == 1 && matches!(e, Event::BlobWithdrawn { blob, .. } if *blob == received));
    assert!(b.blob_fetch_bounded(received, dir.path().join("stale.download").to_str().unwrap()).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    // Maximum-size encrypted file cancellation follows the same terminal
    // rules as ordinary bounded downloads: intact completion or no partial.
    let large = vec![91; 16 * 1024 * 1024];
    let large_blob = a.blob_add_sealed_bytes(room_a, &large, "large.bin").unwrap();
    pump.wait([&a, &b], "maximum encrypted file prepared", 30, |i, e|
        i == 0 && matches!(e, Event::BlobDone { blob, .. } if *blob == large_blob));
    a.blob_offer(room_a, large_blob).unwrap();
    let offer = pump.wait([&a, &b], "maximum sealed file offer", 30, |i, e|
        i == 1 && matches!(e, Event::BlobOffer { room, meta, .. } if *room == room_b && meta.size == large.len() as u64));
    let Event::BlobOffer { blob: large_received, .. } = offer else { unreachable!() };
    let canceled_path = dir.path().join("sealed-canceled.download");
    b.blob_fetch_bounded(large_received, canceled_path.to_str().unwrap()).unwrap();
    let _ = b.blob_cancel_bounded(large_received); // A complete save may win a race.
    let terminal = pump.wait([&a, &b], "encrypted file cancellation terminal", 30, |i, e|
        i == 1 && matches!(e, Event::BlobDone { blob, .. } | Event::BlobFailed { blob, .. } if *blob == large_received));
    match terminal {
        Event::BlobDone { .. } => assert_eq!(std::fs::read(&canceled_path).unwrap(), large),
        Event::BlobFailed { .. } => assert!(!canceled_path.exists()),
        _ => unreachable!(),
    }
    a.blob_unoffer(room_a, large_blob).unwrap();
    pump.wait([&a, &b], "canceled encrypted offer withdrawn", 30, |i, e|
        i == 1 && matches!(e, Event::BlobWithdrawn { blob, .. } if *blob == large_received));
    assert!(b.blob_fetch_bounded(large_received, canceled_path.to_str().unwrap()).is_err());
    let next_a = a.channel_reseal(room_a).unwrap();
    pump.wait([&a, &b], "resealed creator joined", 20, |i, e|
        i == 0 && matches!(e, Event::RoomJoined { room } if *room == next_a));
    assert!(a.blob_offer(next_a, blob).is_err(), "old encrypted file must be prepared again under the new key");
    let next_text = a.room_ticket(next_a).unwrap();
    let next_ticket: lantern_core::ticket::SealedRoomTicket = next_text.parse().unwrap();
    assert_ne!(ticket.topic, next_ticket.topic);
    let next_b = b.room_join(Scope::Custom, "", Some(&next_text)).unwrap();
    pump.wait([&a, &b], "resealed peers connected", 30, |i, e|
        i == 1 && matches!(e, Event::PeerJoined { room, .. } if *room == next_b));
    a.room_send(next_a, b"new key conversation").unwrap();
    pump.wait([&a, &b], "new sealed channel chat", 30, |i, e|
        i == 1 && matches!(e, Event::Message { room, data, .. } if *room == next_b && data == b"new key conversation"));
    assert!(!pump.log.iter().any(|(i, e)| *i == 1 && matches!(e,
        Event::Message { room, data, .. } if *room == room_b && data == b"new key conversation")));
    a.close();
    b.close();
}
