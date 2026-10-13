//! Friends end to end: in-process nodes over the loopback interface with
//! relays and n0 address lookup switched off, so nothing here needs a network
//! beyond `lo`.
//!
//! One long test walks the whole life of a friendship, because each step needs
//! the state the previous one built: invite, presence, text both ways, a group
//! channel, an offline text that waits for a restart, and unfriending.

mod common;

use std::{str::FromStr, time::Duration};

use common::{node, Pump, HEARTBEAT_MS};
use lantern_core::{Config, Event, FriendInvite, Node, PeerId, RelayMode, Status};

fn befriend(pump: &mut Pump, a: &Node, b: &Node) {
    let invite = a.invite_create(None).expect("invite");
    let handle = b.invite_accept(&invite).expect("accept");
    let (a_id, b_id) = (a.node_id(), b.node_id());
    let mut seen = [false, false];
    pump.wait(&[a, b], "both sides add the friend", 20, |i, e| {
        match e {
            Event::FriendAdded { handle: 0, peer, .. } if i == 0 && *peer == b_id => seen[0] = true,
            Event::FriendAdded { handle: h, peer, .. } if i == 1 && *h == handle && *peer == a_id => {
                seen[1] = true
            }
            Event::InviteFailed { reason, .. } => panic!("invite failed: {reason}"),
            _ => {}
        }
        seen[0] && seen[1]
    });
}

fn is_online(e: &Event, who: PeerId) -> bool {
    matches!(e, Event::FriendOnline { peer, .. } if *peer == who)
}

#[test]
fn friend_files_require_explicit_fetch_and_withdraw_without_deleting_saved_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let alice = node("FileAlice", dir.path());
    let bob = node("FileBob", dir.path());
    let mut pump = Pump::new();
    befriend(&mut pump, &alice, &bob);
    let bytes = vec![42u8; 65537];
    let blob = alice.blob_add_bytes(&bytes, "clear.png").unwrap();
    pump.wait(&[&alice, &bob], "friend file prepared", 20, |i, e|
        i == 0 && matches!(e, Event::BlobDone { blob: h, .. } if *h == blob));
    assert!(alice.friend_file_offer(&[7; 32], blob).is_err());
    alice.friend_file_offer(&bob.node_id(), blob).unwrap();
    let offer = pump.wait(&[&alice, &bob], "friend file offer", 20, |i, e|
        i == 1 && matches!(e, Event::BlobOffer { room: 0, .. }));
    let remote = match offer {
        Event::BlobOffer { peer, blob, meta, .. } => {
            assert_eq!(peer, alice.node_id());
            assert_eq!(meta.size, bytes.len() as u64);
            assert_eq!(meta.name, "clear.png");
            blob
        },
        _ => unreachable!(),
    };
    let path = dir.path().join("friend.download");
    assert!(!path.exists(), "an offer does not transfer a file");
    bob.blob_fetch_bounded(remote, path.to_str().unwrap()).unwrap();
    pump.wait(&[&alice, &bob], "friend file saved", 30, |i, e|
        i == 1 && matches!(e, Event::BlobDone { blob, .. } if *blob == remote));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    alice.friend_file_withdraw(&bob.node_id(), blob).unwrap();
    pump.wait(&[&alice, &bob], "friend file withdrawn", 20, |i, e|
        i == 1 && matches!(e, Event::BlobWithdrawn { blob } if *blob == remote));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(bob.blob_fetch_bounded(remote, dir.path().join("stale.download").to_str().unwrap()).is_err());
}

#[test]
fn removing_or_blocking_a_friend_forgets_file_offers_before_returning() {
    for block in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let alice = node("RemoveFileAlice", dir.path());
        let bob = node("RemoveFileBob", dir.path());
        let mut pump = Pump::new();
        befriend(&mut pump, &alice, &bob);
        let bytes = vec![19u8; 16 * 1024 * 1024];
        let blob = alice.blob_add_bytes(&bytes, "removed.png").unwrap();
        pump.wait(&[&alice, &bob], "removal file prepared", 20, |i, e|
            i == 0 && matches!(e, Event::BlobDone { blob: h, .. } if *h == blob));
        alice.friend_file_offer(&bob.node_id(), blob).unwrap();
        let offer = pump.wait(&[&alice, &bob], "removal file offer", 20, |i, e|
            i == 1 && matches!(e, Event::BlobOffer { room: 0, .. }));
        let remote = match offer { Event::BlobOffer { blob, .. } => blob, _ => unreachable!() };
        let active_path = dir.path().join("active-removal.download");
        bob.blob_fetch_bounded(remote, active_path.to_str().unwrap()).unwrap();
        if block { bob.block(&alice.node_id()).unwrap(); }
        else { bob.friend_remove(&alice.node_id()).unwrap(); }
        let path = dir.path().join("forgotten.download");
        assert!(bob.blob_fetch_bounded(remote, path.to_str().unwrap()).is_err(), "removal must invalidate the handle synchronously");
        pump.wait(&[&alice, &bob], "removed file withdrawal", 20, |i, e|
            i == 1 && matches!(e, Event::BlobWithdrawn { blob } if *blob == remote));
        if block {
            bob.unblock(&alice.node_id()).unwrap();
            assert!(bob.blob_fetch_bounded(remote, path.to_str().unwrap()).is_err(), "unblock must not resurrect retained metadata");
        }
        let terminal = pump.wait(&[&alice, &bob], "removed transfer terminal", 30, |i, e|
            i == 1 && matches!(e, Event::BlobDone { blob, .. } | Event::BlobFailed { blob, .. } if *blob == remote));
        match terminal {
            Event::BlobDone { .. } => assert_eq!(std::fs::read(&active_path).unwrap(), bytes, "a completion that won removal remains intact"),
            Event::BlobFailed { .. } => assert!(!active_path.exists(), "cancellation leaves no partial destination"),
            _ => unreachable!(),
        }
        assert!(!path.exists());
    }
}

#[test]
fn a_friendship_from_invite_to_unfriend() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    let (a_id, b_id) = (alice.node_id(), bob.node_id());

    // --- invite ------------------------------------------------------------
    befriend(&mut pump, &alice, &bob);
    let names: Vec<String> = alice.friends().into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["Bob"], "the redeemer's name comes with the redeem");
    let names: Vec<String> = bob.friends().into_iter().map(|f| f.name).collect();
    assert_eq!(names, ["Alice"], "the inviter's name comes with the welcome");

    // --- presence ------------------------------------------------------------
    pump.until(&[&alice, &bob], "both see the other online", 20, || {
        alice.friends().first().is_some_and(|f| f.online && f.status == Status::Online)
            && bob.friends().first().is_some_and(|f| f.online)
    });
    assert!(alice.friends()[0].online);
    assert!(alice.friends()[0].linked);

    // Activities travel independently of legacy presence. Clearing and
    // invisible presence must not expose stale activities in friend JSON.
    alice.set_activity_tags(&["maps".into(), " Maps ".into(), "crafting".into()]).unwrap();
    pump.until(&[&alice, &bob], "bob sees Alice's activities", 20, || {
        bob.friends()[0].tags == ["maps", "crafting"]
    });
    assert!(bob.friends_json().contains("\"tags\":[\"maps\",\"crafting\"]"));
    alice.set_activity_tags(&[]).unwrap();
    pump.until(&[&alice, &bob], "activities clear", 20, || bob.friends()[0].tags.is_empty());
    alice.set_activity_tags(&["maps".into()]).unwrap();
    pump.until(&[&alice, &bob], "activities restore", 20, || bob.friends()[0].tags == ["maps"]);

    alice.set_presence(Status::Away, "  brb, crafting\n").unwrap();
    let seen = pump.wait(&[&alice, &bob], "bob sees alice away", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendOnline { status: Status::Away, .. })
    });
    match seen {
        Event::FriendOnline { note, .. } => assert_eq!(note, "brb, crafting", "notes are cleaned"),
        other => panic!("unexpected {other:?}"),
    }

    // Heartbeats keep a quiet friend online well past the 3-beat timeout.
    assert!(
        pump.quiet(&[&alice, &bob], HEARTBEAT_MS * 6, |_, e| matches!(
            e,
            Event::FriendOffline { .. }
        )),
        "a heartbeating friend must not time out"
    );

    // Invisible looks exactly like offline to the friend...
    alice.set_presence(Status::Invisible, "secret").unwrap();
    pump.wait(&[&alice, &bob], "bob sees alice go dark", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendOffline { peer } if *peer == a_id)
    });
    assert!(!bob.friends()[0].online);
    assert_eq!(bob.friends()[0].note, "", "an invisible friend's note is not shown");
    assert!(bob.friends()[0].tags.is_empty(), "invisible activities are not shown");

    // ...but texts still flow.
    let id = bob.friend_send(&a_id, "you there?").unwrap();
    pump.wait(&[&alice, &bob], "alice gets bob's text while invisible", 20, |i, e| {
        i == 0 && matches!(e, Event::FriendText { text, .. } if text == "you there?")
    });
    pump.wait(&[&alice, &bob], "bob sees it delivered", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendDelivered { id: got, peer } if *got == id && *peer == a_id)
    });
    alice.set_presence(Status::Online, "").unwrap();
    pump.wait(&[&alice, &bob], "alice visible again", 20, |i, e| i == 1 && is_online(e, a_id));

    // --- text ------------------------------------------------------------------
    let id = alice.friend_send(&b_id, "shall we queue?").unwrap();
    let seen = pump.wait(&[&alice, &bob], "bob receives the text", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendText { .. })
    });
    match seen {
        Event::FriendText { peer, id: got, text, .. } => {
            assert_eq!(peer, a_id);
            assert_eq!(got, id, "the receiver sees the sender's id");
            assert_eq!(text, "shall we queue?");
        }
        other => panic!("unexpected {other:?}"),
    }
    pump.wait(&[&alice, &bob], "alice sees it delivered", 20, |i, e| {
        i == 0 && matches!(e, Event::FriendDelivered { id: got, .. } if *got == id)
    });
    let history = alice.friend_history(&b_id, 10).unwrap();
    assert_eq!(history.last().unwrap().text, "shall we queue?");
    assert!(history.last().unwrap().outgoing);
    assert!(history.last().unwrap().delivered_at.is_some());

    assert!(alice.friend_send(&[7u8; 32], "stranger").is_err(), "only friends");
    assert!(alice.friend_send(&b_id, "").is_err(), "empty texts are refused");

    // --- group channel -------------------------------------------------------
    let room = alice.channel_create("Static").unwrap();
    pump.wait(&[&alice, &bob], "channel up", 10, |i, e| {
        i == 0 && matches!(e, Event::RoomJoined { room: r } if *r == room)
    });
    alice.channel_invite(&b_id, room).unwrap();
    let invite = pump.wait(&[&alice, &bob], "bob gets the channel invite", 20, |i, e| {
        i == 1 && matches!(e, Event::ChannelInvite { .. })
    });
    let Event::ChannelInvite { ticket, peer } = invite else { unreachable!() };
    assert_eq!(peer, a_id);
    let bob_room = bob
        .room_join(lantern_core::Scope::Custom, "", Some(&ticket))
        .expect("join from the channel invite");
    assert_eq!(bob.room_label(bob_room).unwrap(), "Static");
    pump.wait(&[&alice, &bob], "alice sees bob in the channel", 20, |i, e| {
        i == 0 && matches!(e, Event::PeerJoined { room: r, peer } if *r == room && *peer == b_id)
    });
    alice.room_send(room, b"pull in 5").unwrap();
    pump.wait(&[&alice, &bob], "bob hears the channel", 20, |i, e| {
        i == 1 && matches!(e, Event::Message { room: r, data, peer } if *r == bob_room && data == b"pull in 5" && *peer == a_id)
    });

    // The same explicitly selected friend receives a sealed ticket privately.
    // Receiving it does not join automatically; the application opts in below.
    let sealed_room = alice.channel_create_sealed("Private static").unwrap();
    pump.wait(&[&alice, &bob], "sealed channel up", 20, |i, e|
        i == 0 && matches!(e, Event::RoomJoined { room: r } if *r == sealed_room));
    alice.channel_invite(&b_id, sealed_room).unwrap();
    let invite = pump.wait(&[&alice, &bob], "private sealed invitation", 20, |i, e|
        i == 1 && matches!(e, Event::ChannelInvite { ticket, .. } if ticket.starts_with("ltsealed")));
    let Event::ChannelInvite { ticket, peer } = invite else { unreachable!() };
    assert_eq!(peer, a_id);
    assert!(ticket.parse::<lantern_core::RoomTicket>().is_err());
    let sealed_bob = bob.room_join(lantern_core::Scope::Custom, "", Some(&ticket)).unwrap();
    assert!(bob.room_is_sealed(sealed_bob).unwrap());
    pump.wait(&[&alice, &bob], "sealed invited peers connected", 30, |i, e|
        i == 1 && matches!(e, Event::PeerJoined { room: r, peer } if *r == sealed_bob && *peer == a_id));
    alice.room_send(sealed_room, b"private invited conversation").unwrap();
    pump.wait(&[&alice, &bob], "sealed invited chat", 30, |i, e|
        i == 1 && matches!(e, Event::Message { room: r, data, .. }
            if *r == sealed_bob && data == b"private invited conversation"));

    // --- offline text waits for the friend to come back ----------------------
    bob.set_activity_tags(&["chat".into()]).unwrap();
    pump.until(&[&alice, &bob], "Alice sees Bob's activities before restart", 20, || alice.friends()[0].tags == ["chat"]);
    bob.close();
    drop(bob);
    pump.wait(&[&alice], "alice sees bob leave", 20, |_, e| {
        matches!(e, Event::FriendOffline { peer } if *peer == b_id)
    });
    let queued = alice.friend_send(&b_id, "read this when you're back").unwrap();
    assert!(
        pump.quiet(&[&alice], 300, |_, e| matches!(e, Event::FriendDelivered { .. })),
        "nobody can ack a text to a closed node"
    );

    let bob = node("Bob", dir.path());
    pump.reset(1);
    assert_eq!(bob.node_id(), b_id, "same database, same identity");
    assert_eq!(bob.friends().len(), 1, "the friend list survives a restart");
    assert_eq!(bob.activity_tags(), ["chat"], "chosen activities survive a restart");
    let seen = pump.wait(&[&alice, &bob], "bob gets the queued text after restart", 30, |i, e| {
        i == 1 && matches!(e, Event::FriendText { .. })
    });
    match seen {
        Event::FriendText { id, text, .. } => {
            assert_eq!(id, queued);
            assert_eq!(text, "read this when you're back");
        }
        other => panic!("unexpected {other:?}"),
    }
    pump.wait(&[&alice, &bob], "alice's queued text is acked", 20, |i, e| {
        i == 0 && matches!(e, Event::FriendDelivered { id, .. } if *id == queued)
    });
    pump.until(&[&alice, &bob], "activities return after reconnect", 20, || alice.friends()[0].tags == ["chat"]);
    assert_eq!(
        bob.friend_history(&a_id, 100)
            .unwrap()
            .iter()
            .filter(|m| m.text == "read this when you're back")
            .count(),
        1,
        "delivered exactly once"
    );

    // --- unfriend ------------------------------------------------------------
    bob.friend_remove(&a_id).unwrap();
    pump.wait(&[&alice, &bob], "alice is told", 20, |i, e| {
        i == 0 && matches!(e, Event::FriendRemoved { peer } if *peer == b_id)
    });
    assert!(alice.friends().is_empty());
    assert!(bob.friends().is_empty());
    assert!(bob.friend_send(&a_id, "hello?").is_err());

    alice.close();
    bob.close();
}

#[test]
fn invites_are_single_use_and_checked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let carol = node("Carol", dir.path());
    let mut pump = Pump::new();

    let invite = alice.invite_create(None).unwrap();
    assert!(invite.starts_with("ltfriend"));
    assert!(alice.invite_accept(&invite).is_err(), "your own invite");

    // A forged secret is refused by the inviter.
    let mut forged = FriendInvite::from_str(&invite).unwrap();
    forged.secret = [0u8; 16];
    let h = carol.invite_accept(&forged.to_string()).unwrap();
    let seen = pump.wait(&[&alice, &carol], "forged invite refused", 20, |i, e| {
        i == 1 && matches!(e, Event::InviteFailed { handle, .. } if *handle == h)
    });
    let Event::InviteFailed { reason, .. } = seen else { unreachable!() };
    assert!(reason.starts_with("refused"), "{reason}");

    // First redemption wins; the second is refused.
    let hb = bob.invite_accept(&invite).unwrap();
    pump.wait(&[&alice, &bob], "bob redeems", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendAdded { handle, .. } if *handle == hb)
    });
    let hc = carol.invite_accept(&invite).unwrap();
    pump.wait(&[&alice, &carol], "carol's replay refused", 20, |i, e| {
        i == 1 && matches!(e, Event::InviteFailed { handle, .. } if *handle == hc)
    });
    assert_eq!(alice.friends().len(), 1, "only bob");
    assert!(carol.friends().is_empty());

    // An expired invite is refused before it is even sent.
    let short = alice.invite_create(Some(Duration::from_secs(1))).unwrap();
    std::thread::sleep(Duration::from_millis(2100));
    assert!(carol.invite_accept(&short).is_err());

    // Garbage never reaches the network.
    assert!(carol.invite_accept("ltfriendnope").is_err());
    assert!(carol.invite_accept("").is_err());
}

#[test]
fn open_invites_survive_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let invite = {
        let alice = node("Alice", dir.path());
        let invite = alice.invite_create(None).unwrap();
        // Let the async side persist it before closing.
        std::thread::sleep(Duration::from_millis(200));
        alice.close();
        invite
    };
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    // The address in the ticket is stale (new port), so point bob at the
    // reopened alice: same NodeId, same secret, fresh address.
    let mut t = FriendInvite::from_str(&invite).unwrap();
    t.addr = FriendInvite::from_str(&alice.invite_create(None).unwrap()).unwrap().addr;
    let h = bob.invite_accept(&t.to_string()).unwrap();
    pump.wait(&[&alice, &bob], "the old invite still works", 20, |i, e| {
        if let Event::InviteFailed { reason, .. } = e {
            panic!("{reason}");
        }
        i == 1 && matches!(e, Event::FriendAdded { handle, .. } if *handle == h)
    });
}

#[test]
fn display_name_and_presence_persist() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let n = node("Alice", dir.path());
        n.set_presence(Status::Invisible, "hidden").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        n.close();
    }
    let config = Config {
        db_path: Some(dir.path().join("Alice.sqlite")),
        relay: RelayMode::Disabled,
        ..Config::default()
    };
    let n = Node::open(config).unwrap();
    assert_eq!(n.display_name(), "Alice");
    assert_eq!(n.presence(), (Status::Invisible, "hidden".to_string()),
        "going invisible must survive a restart, or a restart would out you");
}
