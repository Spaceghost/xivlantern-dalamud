//! The nostr feature against a relay running inside this test process
//! (`nostr_sdk`'s `MockRelay`, bound to the loopback interface). No real relay
//! is contacted: the only URL the nodes are given is the mock's.
#![cfg(feature = "nostr")]

mod common;

use common::{node, Pump};
use lantern_core::Event;
use nostr_sdk::prelude::MockRelay;

fn done(pump: &mut Pump, nodes: &[&lantern_core::Node], who: usize, handle: u64, what: &str) -> String {
    match pump.wait(nodes, what, 30, |i, e| {
        i == who && matches!(e, Event::NostrDone { handle: h, .. } if *h == handle)
    }) {
        Event::NostrDone { ok: true, detail, .. } => detail,
        Event::NostrDone { detail, .. } => panic!("{what} failed: {detail}"),
        _ => unreachable!(),
    }
}

#[test]
fn device_lists_and_invites_through_a_local_relay() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay = rt.block_on(MockRelay::run()).expect("mock relay");
    let url = rt.block_on(relay.url()).to_string();
    assert!(
        url.starts_with("ws://127.0.0.1:") || url.starts_with("ws://localhost:"),
        "the relay must be local: {url}"
    );
    let relays = vec![url];

    let dir = tempfile::tempdir().unwrap();
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    let nodes = [&alice, &bob];

    // --- Alice publishes her device list ----------------------------------
    let alice_keys = alice.nostr_keys().unwrap();
    assert_eq!(alice.nostr_keys().unwrap().public_key(), alice_keys.public_key(), "keys persist");
    let alice_npub = alice.nostr_npub().unwrap();
    assert!(alice_npub.starts_with("npub1"));
    let h = alice.nostr_publish_devices(&relays).unwrap();
    let detail = done(&mut pump, &nodes, 0, h, "publish device list");
    assert!(detail.contains("1 device"), "{detail}");

    // --- Bob looks her up by npub -----------------------------------------
    let h = bob.nostr_fetch_devices(&relays, &alice_npub).unwrap();
    let seen = pump.wait(&nodes, "bob gets alice's devices", 30, |i, e| {
        i == 1 && matches!(e, Event::NostrDevices { handle, .. } if *handle == h)
    });
    let Event::NostrDevices { user, devices, .. } = seen else { unreachable!() };
    assert_eq!(user, alice_keys.public_key().to_bytes());
    assert_eq!(devices, vec![alice.node_id()]);
    done(&mut pump, &nodes, 1, h, "fetch devices");

    // --- Alice invites Bob over the relay; Bob redeems it over iroh --------
    bob.nostr_keys().unwrap();
    let h = alice
        .nostr_send_invite(&relays, &bob.nostr_npub().unwrap(), None)
        .unwrap();
    done(&mut pump, &nodes, 0, h, "send invite");

    let h = bob.nostr_check_inbox(&relays).unwrap();
    let seen = pump.wait(&nodes, "bob finds the invite", 30, |i, e| {
        i == 1 && matches!(e, Event::NostrInvite { .. })
    });
    let Event::NostrInvite { from, ticket } = seen else { unreachable!() };
    assert_eq!(from, alice_keys.public_key().to_bytes(), "the sender is verified");
    let detail = done(&mut pump, &nodes, 1, h, "check inbox");
    assert_eq!(detail, "1 new invite(s)");
    let invite: lantern_core::FriendInvite = ticket.parse().unwrap();
    assert_eq!(invite.nostr, Some(from), "invites carry the inviter's nostr key");

    let handle = bob.invite_accept(&ticket).unwrap();
    pump.wait(&nodes, "bob befriends alice", 30, |i, e| {
        if let Event::InviteFailed { reason, .. } = e {
            panic!("{reason}");
        }
        i == 1 && matches!(e, Event::FriendAdded { handle: h, .. } if *h == handle)
    });
    assert_eq!(bob.friends()[0].nostr, None, "a key in a ticket is only a hint");

    // A verified device list is what binds the friend to the key.
    let h = bob.nostr_fetch_devices(&relays, &alice_npub).unwrap();
    let detail = done(&mut pump, &nodes, 1, h, "bind alice");
    assert!(detail.contains("1 of them friends"), "{detail}");
    assert_eq!(bob.friends()[0].nostr, Some(alice_keys.public_key().to_bytes()));

    // An invite from a friend is not raised again.
    let h = bob.nostr_check_inbox(&relays).unwrap();
    assert_eq!(done(&mut pump, &nodes, 1, h, "recheck inbox"), "0 new invite(s)");

    // --- a second device joins the same user ------------------------------
    let deck = node("AliceDeck", dir.path());
    let imported = deck
        .nostr_import(&alice_keys.secret_key().to_secret_hex())
        .unwrap();
    assert_eq!(imported, alice_keys.public_key());
    let h = deck.nostr_publish_devices(&relays).unwrap();
    let detail = done(&mut pump, &[&alice, &bob, &deck], 2, h, "deck publishes");
    assert!(detail.contains("2 device"), "the list keeps the first device: {detail}");

    let h = bob.nostr_fetch_devices(&relays, &alice_npub).unwrap();
    let seen = pump.wait(&nodes, "bob sees both devices", 30, |i, e| {
        i == 1 && matches!(e, Event::NostrDevices { handle, .. } if *handle == h)
    });
    let Event::NostrDevices { devices, .. } = seen else { unreachable!() };
    assert_eq!(devices.len(), 2);
    assert!(devices.contains(&alice.node_id()) && devices.contains(&deck.node_id()));

    // Nobody else's key finds anything.
    let stranger = nostr_sdk::prelude::Keys::generate().public_key().to_hex();
    let h = bob.nostr_fetch_devices(&relays, &stranger).unwrap();
    match pump.wait(&nodes, "unknown user", 30, |i, e| {
        i == 1 && matches!(e, Event::NostrDone { handle, .. } if *handle == h)
    }) {
        Event::NostrDone { ok, .. } => assert!(!ok, "no device list for a stranger"),
        _ => unreachable!(),
    }

    drop(relay);
}

#[test]
fn nostr_jobs_without_relays_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let n = node("Solo", dir.path());
    let mut pump = Pump::new();
    let h = n.nostr_publish_devices(&[]).unwrap();
    match pump.wait(&[&n], "refusal", 10, |_, e| matches!(e, Event::NostrDone { handle, .. } if *handle == h)) {
        Event::NostrDone { ok, detail, .. } => {
            assert!(!ok);
            assert!(detail.contains("no nostr relays"), "{detail}");
        }
        _ => unreachable!(),
    }
    assert!(n.nostr_fetch_devices(&["ws://127.0.0.1:1".into()], "not-a-key").is_err());
}

#[test]
fn fetched_event_collection_refuses_a_relay_batch_over_the_local_limit() {
    use lantern_core::nostr::{Relays, MAX_INBOX_EVENTS};
    use nostr_sdk::prelude::{EventBuilder, Filter, FinalizeEvent, Keys, Kind};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let relay = MockRelay::run().await.unwrap();
        let url = relay.url().await.to_string();
        assert!(url.starts_with("ws://127.0.0.1:") || url.starts_with("ws://localhost:"));
        let conn = Relays::connect(&[url], std::time::Duration::from_secs(10)).await.unwrap();
        let author = Keys::generate();
        for i in 0..=MAX_INBOX_EVENTS {
            let event = EventBuilder::new(Kind::TextNote, format!("synthetic bounded fetch {i}"))
                .finalize(&author).unwrap();
            relay.add_event(event).await.unwrap();
        }
        // No server filter limit: the caller's SDK buffer cap must still apply.
        let result = conn.fetch(Filter::new().kind(Kind::TextNote), std::time::Duration::from_secs(10)).await;
        conn.shutdown().await;
        assert!(result.is_err(), "129 unique events must not be returned to the caller");
    });
}

#[test]
fn relay_events_over_size_or_tag_limits_do_not_reach_the_fetch_result() {
    use lantern_core::nostr::{Relays, MAX_RELAY_EVENT_BYTES};
    use nostr_sdk::prelude::{EventBuilder, Filter, FinalizeEvent, Keys, Kind, Tag};
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let relay = MockRelay::run().await.unwrap();
        let url = relay.url().await.to_string();
        assert!(url.starts_with("ws://127.0.0.1:") || url.starts_with("ws://localhost:"));
        let author = Keys::generate();
        let oversized = EventBuilder::new(Kind::TextNote, "x".repeat(MAX_RELAY_EVENT_BYTES as usize + 10))
            .finalize(&author).unwrap();
        relay.add_event(oversized).await.unwrap();
        let mut tagged = EventBuilder::new(Kind::TextNote, "synthetic excessive tags");
        for i in 0..65 { tagged = tagged.tag(Tag::custom("t", [i.to_string()])); }
        relay.add_event(tagged.finalize(&author).unwrap()).await.unwrap();
        let conn = Relays::connect(&[url], std::time::Duration::from_secs(10)).await.unwrap();
        let found = conn.fetch(Filter::new().kind(Kind::TextNote), std::time::Duration::from_secs(10)).await;
        conn.shutdown().await;
        assert!(found.unwrap().is_empty(), "oversized and excessive-tag events must be rejected");
    });
}
