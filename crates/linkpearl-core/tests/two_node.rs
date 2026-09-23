//! The test that decides whether any of this is real: two nodes, in one
//! process, meeting over the loopback interface with the relays switched off,
//! then exchanging a room message, a presence update, a direct message and a
//! blob.
//!
//! Relays are disabled deliberately. Nothing here touches the public internet,
//! so it can run in CI, in a container, and on a machine with no DNS.

use std::time::{Duration, Instant};

use linkpearl_core::{Config, Event, Node, RelayMode, Scope};

fn node(name: &str, dir: &std::path::Path) -> Node {
    let config = Config {
        db_path: Some(dir.join(format!("{name}.sqlite"))),
        blob_dir: Some(dir.join(format!("{name}.blobs"))),
        app_id: "linkpearl-test/1".to_string(),
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
    assert!(ticket.starts_with("lproom"), "ticket shape: {ticket}");

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

    a.blob_unoffer(room_a, blob_a).expect("unoffer");
    assert!(
        !a.is_publishing(room_a),
        "withdrawing the offer must clear the indicator"
    );

    // --- direct connection ------------------------------------------------
    let ticket = a.node_ticket();
    assert!(ticket.starts_with("lpnode"), "node ticket shape: {ticket}");
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
    let too_big = vec![0u8; linkpearl_core::MAX_MESSAGE + 1];
    assert!(n.room_send(room, &too_big).is_err());
    assert!(n.room_send(room, &too_big[..linkpearl_core::MAX_MESSAGE]).is_ok());
    n.close();
}
