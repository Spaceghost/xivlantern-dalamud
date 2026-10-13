#![cfg(feature = "nostr")]
//! The friend slice driven only through the C ABI, the way the plugin will:
//! open two nodes with `lt_node_open`, befriend with `lt_invite_create` /
//! `lt_invite_accept`, and read everything back as `lt_event`s from `lt_poll`.
//! Relays are disabled, so both nodes meet over the loopback interface.

use std::{
    ffi::{c_char, CStr, CString},
    ptr,
    time::{Duration, Instant},
};

use lantern::*;

struct Node {
    p: *mut lt_node,
}

impl Drop for Node {
    fn drop(&mut self) {
        unsafe { lt_node_close(self.p) };
    }
}

#[derive(Debug, Clone)]
struct Ev {
    kind: u32,
    handle: u64,
    peer: [u8; 32],
    data: Vec<u8>,
}

impl Ev {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

fn open(dir: &std::path::Path, name: &str) -> Node {
    let db = CString::new(dir.join(format!("{name}.sqlite")).to_str().unwrap()).unwrap();
    let app = CString::new("lantern-abi-test/1").unwrap();
    let cfg = lt_config {
        abi_version: LT_ABI_VERSION,
        db_path: db.as_ptr(),
        blob_dir: ptr::null(),
        app_id: app.as_ptr(),
        relay_mode: 1, // LT_RELAY_DISABLED
        relay_url: ptr::null(),
        event_queue_cap: 0,
        accept_inbound: 1,
    };
    let mut p: *mut lt_node = ptr::null_mut();
    assert_eq!(unsafe { lt_node_open(&cfg, &mut p) }, LT_OK);
    let cname = CString::new(name).unwrap();
    assert_eq!(unsafe { lt_profile_set_name(p, cname.as_ptr()) }, LT_OK);
    Node { p }
}

fn id(n: &Node) -> [u8; 32] {
    let mut out = [0u8; 32];
    assert_eq!(unsafe { lt_node_id(n.p, out.as_mut_ptr()) }, LT_OK);
    out
}

fn poll(n: &Node) -> Vec<Ev> {
    let mut buf: Vec<lt_event> = (0..32)
        .map(|_| lt_event {
            kind: 0,
            data_len: 0,
            seq: 0,
            handle: 0,
            peer: [0; 32],
            data: ptr::null(),
        })
        .collect();
    let count = unsafe { lt_poll(n.p, buf.as_mut_ptr(), buf.len() as u32) };
    assert!(count >= 0, "poll failed: {count}");
    buf.iter()
        .take(count as usize)
        .map(|e| Ev {
            kind: e.kind,
            handle: e.handle,
            peer: e.peer,
            // Copy now: the arena is cleared by the next lt_poll.
            data: if e.data.is_null() {
                Vec::new()
            } else {
                unsafe { std::slice::from_raw_parts(e.data, e.data_len as usize) }.to_vec()
            },
        })
        .collect()
}

thread_local! {
    /// Events polled but not yet matched, per node pointer. `wait` removes
    /// only the event it matched, so nothing polled early is lost.
    static PENDING: std::cell::RefCell<std::collections::HashMap<usize, Vec<Ev>>> =
        Default::default();
}

fn wait(nodes: &[&Node], what: &str, mut pred: impl FnMut(usize, &Ev) -> bool) -> Ev {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        for (i, n) in nodes.iter().enumerate() {
            let fresh = poll(n);
            let hit = PENDING.with(|p| {
                let mut p = p.borrow_mut();
                let q = p.entry(n.p as usize).or_default();
                q.extend(fresh);
                let pos = q.iter().position(|e| pred(i, e))?;
                Some(q.remove(pos))
            });
            if let Some(e) = hit {
                return e;
            }
        }
        if Instant::now() > deadline {
            let kinds = PENDING.with(|p| p.borrow().values().flatten().map(|e| e.kind).collect::<Vec<_>>());
            panic!("timed out waiting for {what}; unmatched: {kinds:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn text_call(f: impl Fn(*mut c_char, usize, *mut usize) -> lt_status) -> String {
    let mut need = 0usize;
    assert_eq!(f(ptr::null_mut(), 0, &mut need), LT_E_BUFFER);
    let mut buf = vec![0u8; need + 1];
    assert_eq!(f(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut need), LT_OK);
    unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) }
        .to_str()
        .unwrap()
        .to_string()
}


fn done(nodes: &[&Node], who: usize, handle: u64) {
    let result = wait(nodes, "nostr completion", |i, e| i == who && e.handle == handle
        && matches!(e.kind, LT_EV_NOSTR_DONE | LT_EV_NOSTR_FAILED));
    assert_eq!(result.kind, LT_EV_NOSTR_DONE, "nostr job failed");
}

#[test]
fn offline_name_card_invite_preserves_sender_and_requires_explicit_acceptance() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let relay = rt.block_on(nostr_sdk::prelude::MockRelay::run()).unwrap();
    let relay_url = rt.block_on(relay.url()).to_string();
    assert!(relay_url.starts_with("ws://127.0.0.1:") || relay_url.starts_with("ws://localhost:"));
    let urls = relay_url.as_bytes();
    let dir = tempfile::tempdir().unwrap();
    let alice = open(dir.path(), "NameCardAlice");
    let alice_npub = text_call(|b, l, n| unsafe { lt_nostr_identity(alice.p, b, l, n) });
    let bob = open(dir.path(), "NameCardBob");
    let bob_id = id(&bob);
    let bob_npub = text_call(|b, l, n| unsafe { lt_nostr_identity(bob.p, b, l, n) });
    drop(bob); // recipient is offline when the invitation is stored on the relay
    let recipient = CString::new(bob_npub.clone()).unwrap();
    let mut sent = 0;
    assert_eq!(unsafe { lt_nostr_send_invite(alice.p, urls.as_ptr(), urls.len() as u32,
        recipient.as_ptr(), 86400, &mut sent) }, LT_OK);
    assert_ne!(sent, 0);
    done(&[&alice], 0, sent);

    let reopened = open(dir.path(), "NameCardBob");
    assert_eq!(id(&reopened), bob_id);
    assert_eq!(text_call(|b,l,n| unsafe { lt_nostr_identity(reopened.p,b,l,n) }), bob_npub);
    let nodes = [&alice, &reopened];
    assert_eq!(unsafe { lt_block(reopened.p, id(&alice).as_ptr()) }, LT_OK);
    let mut blocked_check = 0;
    assert_eq!(unsafe { lt_nostr_check_inbox(reopened.p, urls.as_ptr(), urls.len() as u32, &mut blocked_check) }, LT_OK);
    done(&nodes, 1, blocked_check);
    PENDING.with(|p| assert!(!p.borrow().get(&(reopened.p as usize)).into_iter().flatten().any(|e| e.kind == LT_EV_NOSTR_INVITE)));
    assert_eq!(unsafe { lt_unblock(reopened.p, id(&alice).as_ptr()) }, LT_OK);
    let mut checked = 0;
    assert_eq!(unsafe { lt_nostr_check_inbox(reopened.p, urls.as_ptr(), urls.len() as u32, &mut checked) }, LT_OK);
    let incoming = wait(&nodes, "typed pending invite", |i,e| i == 1 && e.kind == LT_EV_NOSTR_INVITE);
    done(&nodes, 1, checked);
    let from = nostr_sdk::prelude::PublicKey::parse(&alice_npub).unwrap().to_bytes();
    assert_eq!(incoming.peer, from);
    let ticket: lantern_core::FriendInvite = incoming.text().parse().unwrap();
    assert_eq!(ticket.nostr, Some(from));
    assert_eq!(ticket.addr.id.as_bytes(), &id(&alice));
    assert_eq!(text_call(|b,l,n| unsafe { lt_friend_list(reopened.p,b,l,n) }), "[]");
    assert_eq!(text_call(|b,l,n| unsafe { lt_friend_list(alice.p,b,l,n) }), "[]");
    // Reading the relay did not redeem anything. A separate explicit ABI call does.
    let bearer = CString::new(incoming.data).unwrap();
    let mut accepted = 0;
    assert_eq!(unsafe { lt_invite_accept(reopened.p, bearer.as_ptr(), &mut accepted) }, LT_OK);
    let added = wait(&nodes, "explicit acceptance", |i,e| i == 1 && e.kind == LT_EV_FRIEND_ADDED && e.handle == accepted);
    assert_eq!(added.peer, id(&alice));
    let mut again = 0;
    assert_eq!(unsafe { lt_nostr_check_inbox(reopened.p, urls.as_ptr(), urls.len() as u32, &mut again) }, LT_OK);
    done(&nodes, 1, again);
    PENDING.with(|p| assert!(!p.borrow().get(&(reopened.p as usize)).into_iter().flatten().any(|e| e.kind == LT_EV_NOSTR_INVITE)));
}
