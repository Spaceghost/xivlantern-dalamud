//! Call signalling spike (feature `calls`): ring, answer, hang up, and the
//! call ending when the link drops. No media is involved.
#![cfg(feature = "calls")]

mod common;

use common::{node, Pump};
use lantern_core::Event;

#[test]
fn ring_answer_hang_up_and_drop() {
    let dir = tempfile::tempdir().unwrap();
    let alice = node("Alice", dir.path());
    let bob = node("Bob", dir.path());
    let mut pump = Pump::new();
    let nodes = [&alice, &bob];
    let (a_id, b_id) = (alice.node_id(), bob.node_id());

    assert!(alice.call_ring(&b_id).is_err(), "only friends");
    let invite = alice.invite_create(None).unwrap();
    bob.invite_accept(&invite).unwrap();
    pump.until(&nodes, "linked", 20, || {
        alice.friends().first().is_some_and(|f| f.linked) && bob.friends().first().is_some_and(|f| f.linked)
    });

    // Ring, accept, hang up from the callee's side.
    let call = alice.call_ring(&b_id).unwrap();
    let seen = pump.wait(&nodes, "bob rings", 20, |i, e| {
        i == 1 && matches!(e, Event::CallIncoming { call: c, .. } if *c == call)
    });
    let Event::CallIncoming { peer, media, .. } = seen else { unreachable!() };
    assert_eq!(peer, a_id);
    assert!(media.starts_with("iroh://") && media.ends_with(&format!("/call/{call:016x}")), "{media}");
    bob.call_answer(call, true).unwrap();
    pump.wait(&nodes, "alice hears yes", 20, |i, e| {
        i == 0 && matches!(e, Event::CallAnswered { call: c, accepted: true, .. } if *c == call)
    });
    bob.call_hangup(call).unwrap();
    pump.wait(&nodes, "alice sees it end", 20, |i, e| {
        i == 0 && matches!(e, Event::CallEnded { call: c, peer } if *c == call && *peer == b_id)
    });
    assert!(alice.call_hangup(call).is_err(), "an ended call is gone");

    // Declined.
    let call = alice.call_ring(&b_id).unwrap();
    pump.wait(&nodes, "ring 2", 20, |i, e| i == 1 && matches!(e, Event::CallIncoming { .. }));
    bob.call_answer(call, false).unwrap();
    pump.wait(&nodes, "declined", 20, |i, e| {
        i == 0 && matches!(e, Event::CallAnswered { call: c, accepted: false, .. } if *c == call)
    });

    // The link drops mid-call: both ends are told.
    let call = alice.call_ring(&b_id).unwrap();
    pump.wait(&nodes, "ring 3", 20, |i, e| i == 1 && matches!(e, Event::CallIncoming { .. }));
    bob.close();
    pump.wait(&nodes, "alice's call ends with the link", 20, |i, e| {
        i == 0 && matches!(e, Event::CallEnded { call: c, .. } if *c == call)
    });
}
