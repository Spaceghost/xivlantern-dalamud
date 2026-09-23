//! The friend slice driven only through the C ABI, the way the plugin will:
//! open two nodes with `lp_node_open`, befriend with `lp_invite_create` /
//! `lp_invite_accept`, and read everything back as `lp_event`s from `lp_poll`.
//! Relays are disabled, so both nodes meet over the loopback interface.

use std::{
    ffi::{c_char, CStr, CString},
    ptr,
    time::{Duration, Instant},
};

use linkpearl::*;

struct Node {
    p: *mut lp_node,
}

impl Drop for Node {
    fn drop(&mut self) {
        unsafe { lp_node_close(self.p) };
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
    let app = CString::new("linkpearl-abi-test/1").unwrap();
    let cfg = lp_config {
        abi_version: LP_ABI_VERSION,
        db_path: db.as_ptr(),
        blob_dir: ptr::null(),
        app_id: app.as_ptr(),
        relay_mode: 1, // LP_RELAY_DISABLED
        relay_url: ptr::null(),
        event_queue_cap: 0,
        accept_inbound: 1,
    };
    let mut p: *mut lp_node = ptr::null_mut();
    assert_eq!(unsafe { lp_node_open(&cfg, &mut p) }, LP_OK);
    let cname = CString::new(name).unwrap();
    assert_eq!(unsafe { lp_profile_set_name(p, cname.as_ptr()) }, LP_OK);
    Node { p }
}

fn id(n: &Node) -> [u8; 32] {
    let mut out = [0u8; 32];
    assert_eq!(unsafe { lp_node_id(n.p, out.as_mut_ptr()) }, LP_OK);
    out
}

fn poll(n: &Node) -> Vec<Ev> {
    let mut buf: Vec<lp_event> = (0..32)
        .map(|_| lp_event {
            kind: 0,
            data_len: 0,
            seq: 0,
            handle: 0,
            peer: [0; 32],
            data: ptr::null(),
        })
        .collect();
    let count = unsafe { lp_poll(n.p, buf.as_mut_ptr(), buf.len() as u32) };
    assert!(count >= 0, "poll failed: {count}");
    buf.iter()
        .take(count as usize)
        .map(|e| Ev {
            kind: e.kind,
            handle: e.handle,
            peer: e.peer,
            // Copy now: the arena is cleared by the next lp_poll.
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
            let kinds = PENDING.with(|p| format!("{:?}", p.borrow()));
            panic!("timed out waiting for {what}; unmatched: {kinds}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn text_call(f: impl Fn(*mut c_char, usize, *mut usize) -> lp_status) -> String {
    let mut need = 0usize;
    assert_eq!(f(ptr::null_mut(), 0, &mut need), LP_E_BUFFER);
    let mut buf = vec![0u8; need + 1];
    assert_eq!(f(buf.as_mut_ptr() as *mut c_char, buf.len(), &mut need), LP_OK);
    unsafe { CStr::from_ptr(buf.as_ptr() as *const c_char) }
        .to_str()
        .unwrap()
        .to_string()
}

#[test]
fn befriend_and_chat_through_the_c_abi() {
    let dir = tempfile::tempdir().unwrap();
    let alice = open(dir.path(), "Alice");
    let bob = open(dir.path(), "Bob");
    let (a_id, b_id) = (id(&alice), id(&bob));

    // A too-small buffer reports the size and mints nothing.
    let invite = text_call(|b, l, n| unsafe { lp_invite_create(alice.p, 0, b, l, n) });
    assert!(invite.starts_with("lpfriend"), "{invite}");

    let ticket = CString::new(invite).unwrap();
    let mut handle = 0u64;
    assert_eq!(unsafe { lp_invite_accept(bob.p, ticket.as_ptr(), &mut handle) }, LP_OK);
    assert_ne!(handle, 0);

    let mut added = [false, false];
    wait(&[&alice, &bob], "friend added on both sides", |i, e| {
        assert_ne!(e.kind, LP_EV_INVITE_FAILED, "{}", e.text());
        if e.kind == LP_EV_FRIEND_ADDED {
            if i == 0 {
                assert_eq!((e.handle, e.peer, e.text().as_str()), (0, b_id, "Bob"));
                added[0] = true;
            } else {
                assert_eq!((e.handle, e.peer, e.text().as_str()), (handle, a_id, "Alice"));
                added[1] = true;
            }
        }
        added[0] && added[1]
    });
    wait(&[&alice, &bob], "alice sees bob online", |i, e| {
        i == 0 && e.kind == LP_EV_FRIEND_ONLINE && e.peer == b_id && e.handle == LP_PRESENCE_ONLINE as u64
    });

    let list = text_call(|b, l, n| unsafe { lp_friend_list(alice.p, b, l, n) });
    assert!(list.contains("\"name\":\"Bob\""), "{list}");
    assert!(list.contains("\"online\":true"), "{list}");

    // --- text --------------------------------------------------------------
    let msg = "hello over the C ABI";
    let mut msg_id = 0u64;
    assert_eq!(
        unsafe { lp_friend_send(bob.p, a_id.as_ptr(), msg.as_ptr(), msg.len() as u32, &mut msg_id) },
        LP_OK
    );
    let got = wait(&[&alice, &bob], "alice gets the text", |i, e| {
        i == 0 && e.kind == LP_EV_FRIEND_TEXT
    });
    assert_eq!((got.peer, got.handle, got.text().as_str()), (b_id, msg_id, msg));
    wait(&[&alice, &bob], "bob sees delivered", |i, e| {
        i == 1 && e.kind == LP_EV_FRIEND_DELIVERED && e.handle == msg_id
    });
    let history = text_call(|b, l, n| unsafe { lp_friend_history(alice.p, b_id.as_ptr(), 10, b, l, n) });
    assert!(history.contains(msg), "{history}");

    let bad = [0xffu8, 0xfe];
    assert_eq!(
        unsafe { lp_friend_send(bob.p, a_id.as_ptr(), bad.as_ptr(), 2, ptr::null_mut()) },
        LP_E_ARG,
        "texts must be UTF-8"
    );
    assert_eq!(
        unsafe { lp_friend_send(bob.p, [9u8; 32].as_ptr(), msg.as_ptr(), 1, ptr::null_mut()) },
        LP_E_NOT_FOUND,
        "only to friends"
    );

    // --- presence ----------------------------------------------------------
    let note = CString::new("brb").unwrap();
    assert_eq!(unsafe { lp_presence_status_set(alice.p, LP_PRESENCE_AWAY, note.as_ptr()) }, LP_OK);
    wait(&[&alice, &bob], "bob sees alice away", |i, e| {
        i == 1 && e.kind == LP_EV_FRIEND_ONLINE && e.handle == LP_PRESENCE_AWAY as u64 && e.text() == "brb"
    });
    assert_eq!(unsafe { lp_presence_status_set(alice.p, 99, ptr::null()) }, LP_E_ARG);

    // --- channel -----------------------------------------------------------
    let label = CString::new("Static").unwrap();
    let mut room = 0u64;
    assert_eq!(unsafe { lp_channel_create(alice.p, label.as_ptr(), &mut room) }, LP_OK);
    assert_eq!(unsafe { lp_channel_invite(alice.p, b_id.as_ptr(), room) }, LP_OK);
    let inv = wait(&[&alice, &bob], "bob gets the channel invite", |i, e| {
        i == 1 && e.kind == LP_EV_CHANNEL_INVITE
    });
    let ticket = CString::new(inv.data).unwrap();
    let mut bob_room = 0u64;
    assert_eq!(
        unsafe { lp_room_join(bob.p, 5, ptr::null(), ticket.as_ptr(), &mut bob_room) },
        LP_OK
    );
    wait(&[&alice, &bob], "alice sees bob join", |i, e| {
        i == 0 && e.kind == LP_EV_PEER_JOINED && e.handle == room && e.peer == b_id
    });
    let line = b"pull in 5";
    assert_eq!(unsafe { lp_room_send(alice.p, room, line.as_ptr(), line.len() as u32) }, LP_OK);
    wait(&[&alice, &bob], "bob hears the channel", |i, e| {
        i == 1 && e.kind == LP_EV_MESSAGE && e.handle == bob_room && e.data == line
    });

    // --- unfriend ----------------------------------------------------------
    assert_eq!(unsafe { lp_friend_remove(bob.p, a_id.as_ptr()) }, LP_OK);
    wait(&[&alice, &bob], "alice is told", |i, e| {
        i == 0 && e.kind == LP_EV_FRIEND_REMOVED && e.peer == b_id
    });
    assert_eq!(unsafe { lp_friend_remove(bob.p, a_id.as_ptr()) }, LP_E_NOT_FOUND);
}

#[test]
fn a_bad_invite_is_refused_synchronously() {
    let dir = tempfile::tempdir().unwrap();
    let n = open(dir.path(), "Solo");
    let mut h = 7u64;
    let bad = CString::new("lpfriendnotaticket").unwrap();
    assert_eq!(unsafe { lp_invite_accept(n.p, bad.as_ptr(), &mut h) }, LP_E_TICKET);
    assert_eq!(h, 0, "no handle for a refused call");
    assert_eq!(unsafe { lp_invite_accept(n.p, ptr::null(), &mut h) }, LP_E_ARG);
}
