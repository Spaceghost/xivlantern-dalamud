//! Friends end to end: in-process nodes over the loopback interface with
//! relays and n0 address lookup switched off, so nothing here needs a network
//! beyond `lo`.
//!
//! One long test walks the whole life of a friendship, because each step needs
//! the state the previous one built: invite, presence, text both ways, a group
//! channel, an offline text that waits for a restart, and unfriending.

use std::{
    collections::VecDeque,
    path::Path,
    str::FromStr,
    time::{Duration, Instant},
};

use linkpearl_core::{Config, Event, FriendInvite, Node, PeerId, RelayMode, Status};

const HEARTBEAT_MS: u64 = 200;

fn node(name: &str, dir: &Path) -> Node {
    let config = Config {
        db_path: Some(dir.join(format!("{name}.sqlite"))),
        app_id: "linkpearl-test/1".to_string(),
        relay: RelayMode::Disabled,
        event_queue_cap: 1024,
        heartbeat_ms: HEARTBEAT_MS,
        ..Config::default()
    };
    let n = Node::open(config).expect("node must open");
    n.set_display_name(name).expect("name");
    n
}

/// Every polled event goes into a per-node backlog. `wait` removes only the
/// first event its predicate matches and leaves everything else for later
/// waits, so an event that arrives before anyone asks for it is not lost.
struct Pump {
    log: Vec<(usize, Event)>,
    pending: Vec<VecDeque<Event>>,
}

impl Pump {
    fn new() -> Self {
        Pump {
            log: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Poll every node into its backlog; returns how many events were new.
    fn fill(&mut self, nodes: &[&Node]) -> Vec<usize> {
        self.pending.resize_with(nodes.len().max(self.pending.len()), VecDeque::new);
        let mut fresh = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let events = n.poll(64);
            fresh.push(events.len());
            self.log.extend(events.iter().cloned().map(|e| (i, e)));
            self.pending[i].extend(events);
        }
        fresh
    }

    /// Forget node `i`'s backlog (it was restarted).
    fn reset(&mut self, i: usize) {
        if let Some(q) = self.pending.get_mut(i) {
            q.clear();
        }
    }

    fn transcript(&self) -> String {
        self.log
            .iter()
            .map(|(i, e)| format!("  node{i}: {e:?}\n"))
            .collect()
    }

    fn wait<F>(&mut self, nodes: &[&Node], what: &str, secs: u64, mut pred: F) -> Event
    where
        F: FnMut(usize, &Event) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            self.fill(nodes);
            for i in 0..nodes.len() {
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
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Poll until `cond` holds, for state that may have changed before this
    /// call started looking.
    fn until(&mut self, nodes: &[&Node], what: &str, secs: u64, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !cond() {
            self.fill(nodes);
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}\ntranscript:\n{}", self.transcript());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// No *new* event matching `pred` for `ms`.
    fn quiet<F>(&mut self, nodes: &[&Node], ms: u64, mut pred: F) -> bool
    where
        F: FnMut(usize, &Event) -> bool,
    {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            let fresh = self.fill(nodes);
            for (i, n) in fresh.into_iter().enumerate() {
                let q = &self.pending[i];
                if q.iter().skip(q.len() - n).any(|e| pred(i, e)) {
                    return false;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

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
        .room_join(linkpearl_core::Scope::Custom, "", Some(&ticket))
        .expect("join from the channel invite");
    assert_eq!(bob.room_label(bob_room).unwrap(), "Static");
    pump.wait(&[&alice, &bob], "alice sees bob in the channel", 20, |i, e| {
        i == 0 && matches!(e, Event::PeerJoined { room: r, peer } if *r == room && *peer == b_id)
    });
    alice.room_send(room, b"pull in 5").unwrap();
    pump.wait(&[&alice, &bob], "bob hears the channel", 20, |i, e| {
        i == 1 && matches!(e, Event::Message { room: r, data, peer } if *r == bob_room && data == b"pull in 5" && *peer == a_id)
    });

    // --- offline text waits for the friend to come back ----------------------
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
    assert!(invite.starts_with("lpfriend"));
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
    assert!(carol.invite_accept("lpfriendnope").is_err());
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
