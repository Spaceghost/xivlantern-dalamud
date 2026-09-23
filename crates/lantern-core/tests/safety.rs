//! Blocking, rate limits, size limits, the author channel, support messages
//! and the selftest, between in-process nodes with relays off.

mod common;

use std::time::Duration;

use common::{node, Pump};
use lantern_core::{
    author::{Announcement, SignedAnnouncement},
    Config, Event, Node, RelayMode, Scope, MAX_ROOM_MESSAGE, MAX_TEXT,
};

fn befriend(pump: &mut Pump, a: &Node, b: &Node) {
    let invite = a.invite_create(None).unwrap();
    let h = b.invite_accept(&invite).unwrap();
    pump.wait(&[a, b], "friends", 20, |i, e| {
        if let Event::InviteFailed { reason, .. } = e {
            panic!("{reason}");
        }
        i == 1 && matches!(e, Event::FriendAdded { handle, .. } if *handle == h)
    });
    pump.until(&[a, b], "linked", 20, || {
        a.friends().iter().any(|f| f.linked) && b.friends().iter().any(|f| f.linked)
    });
}

fn author_node(name: &str, dir: &std::path::Path) -> Node {
    let n = Node::open(Config {
        db_path: Some(dir.join(format!("{name}.sqlite"))),
        app_id: "lantern-test/1".into(),
        relay: RelayMode::Disabled,
        heartbeat_ms: common::HEARTBEAT_MS,
        accept_support: true,
        ..Config::default()
    })
    .unwrap();
    n.set_display_name(name).unwrap();
    n
}

#[test]
fn blocking_drops_a_friend_silently_and_refuses_them_after() {
    let dir = tempfile::tempdir().unwrap();
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    let (a_id, b_id) = (alice.node_id(), bob.node_id());
    befriend(&mut pump, &alice, &bob);

    let room = alice.channel_create("hall").unwrap();
    let ticket = alice.room_ticket(room).unwrap();
    let bob_room = bob.room_join(Scope::Custom, "", Some(&ticket)).unwrap();
    pump.wait(&[&alice, &bob], "bob in hall", 20, |i, e| {
        i == 0 && matches!(e, Event::PeerJoined { peer, .. } if *peer == b_id)
    });

    alice.block(&b_id).unwrap();
    assert!(alice.block(&a_id).is_err(), "not yourself");
    pump.wait(&[&alice, &bob], "alice drops bob", 20, |i, e| {
        i == 0 && matches!(e, Event::FriendRemoved { peer } if *peer == b_id)
    });
    assert!(alice.friends().is_empty());
    assert_eq!(alice.blocked(), vec![b_id]);
    // Bob is not told he was blocked; he just sees Alice go offline.
    pump.wait(&[&alice, &bob], "bob sees alice offline", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendOffline { peer } if *peer == a_id)
    });
    assert!(
        pump.quiet(&[&alice, &bob], 300, |i, e| i == 1 && matches!(e, Event::FriendRemoved { .. })),
        "a block is silent"
    );

    // Bob's room messages no longer reach Alice.
    bob.room_send(bob_room, b"can you hear me").unwrap();
    assert!(pump.quiet(&[&alice, &bob], 1500, |i, e| i == 0 && matches!(e, Event::Message { .. })));

    // Bob's new invites are refused; Alice refuses to redeem Bob's.
    let bob_invite = bob.invite_create(None).unwrap();
    assert!(alice.invite_accept(&bob_invite).is_err());
    let alice_invite = alice.invite_create(None).unwrap();
    let h = bob.invite_accept(&alice_invite).unwrap();
    pump.wait(&[&alice, &bob], "bob's redeem refused", 20, |i, e| {
        i == 1 && matches!(e, Event::InviteFailed { handle, .. } if *handle == h)
    });

    // It survives a restart, and unblock undoes it.
    alice.close();
    drop(alice);
    let alice = node("Alice", dir.path());
    pump.reset(0);
    assert_eq!(alice.blocked(), vec![b_id]);
    alice.unblock(&b_id).unwrap();
    assert!(alice.unblock(&b_id).is_err());
    assert!(alice.blocked().is_empty());
}

#[test]
fn a_flood_is_rate_limited_and_reported_once() {
    let dir = tempfile::tempdir().unwrap();
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    let b_id = bob.node_id();
    befriend(&mut pump, &alice, &bob);

    for i in 0..300 {
        bob.friend_send(&alice.node_id(), &format!("spam {i}")).unwrap();
    }
    let seen = pump.wait(&[&alice, &bob], "alice limits bob", 20, |i, e| {
        i == 0 && matches!(e, Event::RateLimited { .. })
    });
    let Event::RateLimited { peer, what } = seen else { unreachable!() };
    assert_eq!((peer, what.as_str()), (b_id, "friend"));
    std::thread::sleep(Duration::from_millis(1500));
    pump.fill(&[&alice, &bob]);
    let texts = pump
        .log
        .iter()
        .filter(|(i, e)| *i == 0 && matches!(e, Event::FriendText { .. }))
        .count();
    assert!(texts < 300, "some of the flood was dropped ({texts} got through)");
    let notices = pump
        .log
        .iter()
        .filter(|(i, e)| *i == 0 && matches!(e, Event::RateLimited { .. }))
        .count();
    assert_eq!(notices, 1, "one notice per peer per 30 s, not one per frame");
    assert!(alice.stats().rate_limited > 0);
}

#[test]
fn sizes_are_limited_and_a_full_size_room_message_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    befriend(&mut pump, &alice, &bob);

    let long = "x".repeat(MAX_TEXT + 1);
    assert!(alice.friend_send(&bob.node_id(), &long).is_err());
    assert!(alice.friend_send(&bob.node_id(), &long[..MAX_TEXT]).is_ok());
    pump.wait(&[&alice, &bob], "a max-size text arrives", 20, |i, e| {
        i == 1 && matches!(e, Event::FriendText { text, .. } if text.len() == MAX_TEXT)
    });

    let room = alice.channel_create("big").unwrap();
    let bob_room = bob
        .room_join(Scope::Custom, "", Some(&alice.room_ticket(room).unwrap()))
        .unwrap();
    pump.wait(&[&alice, &bob], "joined", 20, |i, e| i == 0 && matches!(e, Event::PeerJoined { .. }));
    assert!(alice.room_send(room, &vec![1u8; MAX_ROOM_MESSAGE + 1]).is_err());
    alice.room_send(room, &vec![7u8; MAX_ROOM_MESSAGE]).unwrap();
    pump.wait(&[&alice, &bob], "a max-size room message arrives", 20, |i, e| {
        i == 1 && matches!(e, Event::Message { room, data, .. } if *room == bob_room && data.len() == MAX_ROOM_MESSAGE)
    });
}

#[test]
fn the_author_channel_carries_only_signed_announcements_and_support_works() {
    let dir = tempfile::tempdir().unwrap();
    let key = iroh::SecretKey::generate();
    let author_key = *key.public().as_bytes();
    let author = author_node("Author", dir.path());
    let bob = node("Bob", dir.path());
    let carol = node("Carol", dir.path());
    let mut pump = Pump::new();
    let nodes = [&author, &bob, &carol];
    let boot = [author.node_id()];

    // With relays off, NodeIds alone are not enough to find someone; a real
    // install resolves the author's nodes through the relays' DNS.
    for n in [&bob, &carol] {
        n.add_address_hint(&author.node_ticket()).unwrap();
    }
    let a_room = author.author_join(&author_key, &boot).unwrap();
    assert_eq!(author.author_join(&author_key, &boot).unwrap(), a_room, "joining twice is one room");
    let b_room = bob.author_join(&author_key, &boot).unwrap();
    pump.until(&nodes, "bob reaches the author node", 20, || {
        author.room_peers(a_room).map(|p| p.contains(&bob.node_id())).unwrap_or(false)
    });

    let sign = |seq: u64, k: &iroh::SecretKey| {
        SignedAnnouncement::sign(
            k,
            &Announcement {
                seq,
                issued_at: 1_790_000_000 + seq as i64,
                title: format!("News {seq}"),
                body: "Patch notes.".into(),
            },
        )
        .unwrap()
        .to_string()
    };

    assert_eq!(author.author_announce(a_room, &sign(1, &key)).unwrap(), 1);
    let seen = pump.wait(&nodes, "bob gets the announcement", 20, |i, e| {
        i == 1 && matches!(e, Event::Announcement { .. })
    });
    let Event::Announcement { room, author: who, seq, title, .. } = seen else { unreachable!() };
    assert_eq!((room, who, seq, title.as_str()), (b_room, author_key, 1, "News 1"));

    // Nobody else can speak in it.
    let mallory = iroh::SecretKey::generate();
    assert!(bob.author_announce(b_room, &sign(2, &mallory)).is_err(), "not the author's key");
    assert!(bob.room_send(b_room, b"hello everyone").is_err(), "no chatter in the author channel");

    // A late joiner is handed the latest announcement by whoever it meets,
    // and a re-offer to someone who has it is not news again.
    let c_room = carol.author_join(&author_key, &boot).unwrap();
    let seen = pump.wait(&nodes, "carol catches up", 30, |i, e| {
        i == 2 && matches!(e, Event::Announcement { .. })
    });
    assert!(matches!(seen, Event::Announcement { room, seq: 1, .. } if room == c_room));
    assert!(pump.quiet(&nodes, 800, |i, e| i == 1 && matches!(e, Event::Announcement { .. })));
    assert_eq!(bob.announcements(&author_key, 10).unwrap().len(), 1);
    assert!(bob.announcements_json(&author_key, 10).unwrap().contains("News 1"));

    // Anyone relays: an announcement pasted into Bob's node reaches Carol.
    bob.author_announce(b_room, &sign(2, &key)).unwrap();
    pump.wait(&nodes, "carol gets #2 via anyone", 20, |i, e| {
        i == 2 && matches!(e, Event::Announcement { seq: 2, .. })
    });

    // Support: Bob writes to the author, the author answers.
    assert!(bob.support_send(&author.node_id(), "help").is_err(), "only to configured contacts");
    bob.set_support_contacts(&[author.node_id()]);
    let id = bob.support_send(&author.node_id(), "the relay is down").unwrap();
    let seen = pump.wait(&nodes, "author gets it", 20, |i, e| {
        i == 0 && matches!(e, Event::SupportMessage { .. })
    });
    let Event::SupportMessage { peer, id: got, text, .. } = seen else { unreachable!() };
    assert_eq!((peer, got, text.as_str()), (bob.node_id(), id, "the relay is down"));
    pump.wait(&nodes, "bob sees it delivered", 20, |i, e| {
        i == 1 && matches!(e, Event::SupportDelivered { id: got, .. } if *got == id)
    });
    author.support_reply(&bob.node_id(), "fixed").unwrap();
    pump.wait(&nodes, "bob gets the answer", 20, |i, e| {
        i == 1 && matches!(e, Event::SupportReply { text, .. } if text == "fixed")
    });

    // A reply to someone who does not list the author is refused by them, and
    // a player's node is not a support desk.
    let r = author.support_reply(&carol.node_id(), "hi").unwrap();
    pump.wait(&nodes, "carol refuses", 30, |i, e| {
        i == 0 && matches!(e, Event::SupportFailed { id, .. } if *id == r)
    });
    assert!(pump.quiet(&nodes, 300, |i, e| i == 2 && matches!(e, Event::SupportReply { .. })));
    assert!(bob.support_reply(&author.node_id(), "x").is_err());
    carol.set_support_contacts(&[bob.node_id()]);
    let s = carol.support_send(&bob.node_id(), "are you the author?").unwrap();
    pump.wait(&nodes, "bob is no support desk", 30, |i, e| {
        i == 2 && matches!(e, Event::SupportFailed { id, .. } if *id == s)
    });
}

#[test]
fn selftest_reports_a_working_direct_path_with_relays_off() {
    let dir = tempfile::tempdir().unwrap();
    let n = node("Solo", dir.path());
    let mut pump = Pump::new();
    let h = n.selftest().unwrap();
    let seen = pump.wait(&[&n], "selftest", 40, |_, e| {
        matches!(e, Event::SelfTest { handle, .. } if *handle == h)
    });
    let Event::SelfTest { report, .. } = seen else { unreachable!() };
    let hex: String = n.node_id().iter().map(|b| format!("{b:02x}")).collect();
    assert!(report.contains(&format!("\"node_id\":\"{hex}\"")), "{report}");
    assert!(report.contains("\"direct\":{\"ok\":true"), "{report}");
    assert!(report.contains("\"detail\":\"relays are off\""), "{report}");
}
