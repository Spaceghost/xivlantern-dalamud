//! Signed channel metadata and local clearing through the actual C ABI:
//! use a temporary identity/database, with relays disabled and no live accounts.

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
            let kinds = PENDING.with(|p| format!("{:?}", p.borrow()));
            panic!("timed out waiting for {what}; unmatched: {kinds}");
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

#[test]
fn signed_records_saved_history_and_clear_have_distinct_abi_events() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), "history");
    wait(&[&node], "ready", |_, e| e.kind == LT_EV_READY);
    let label = CString::new("metadata").unwrap();
    let mut room = 0;
    assert_eq!(unsafe { lt_channel_create(node.p, label.as_ptr(), &mut room) }, LT_OK);
    wait(&[&node], "joined", |_, e| e.kind == LT_EV_ROOM_JOINED && e.handle == room);
    let ticket = text_call(|b, l, n| unsafe { lt_room_ticket(node.p, room, b, l, n) });
    let ticket: lantern_core::ticket::RoomTicket = ticket.parse().unwrap();
    let peer = id(&node);
    assert_eq!(unsafe { lt_channel_record_events_enable(node.p, 2) }, LT_E_ARG);
    assert_eq!(unsafe { lt_channel_record_events_enable(node.p, 1) }, LT_OK);
    assert_eq!(unsafe { lt_room_history_enable(node.p, room, 2) }, LT_E_ARG);
    assert_eq!(unsafe { lt_room_history_enable(node.p, 0, 1) }, LT_E_NOT_FOUND);
    assert_eq!(unsafe { lt_room_history_clear(node.p, 0) }, LT_E_NOT_FOUND);
    assert_eq!(unsafe { lt_room_send_with_history(node.p, room, ptr::null(), 0) }, LT_E_ARG);
    assert_eq!(unsafe { lt_room_send_with_history(node.p, room, [0xff].as_ptr(), 1) }, LT_E_ARG);
    assert_eq!(unsafe { lt_room_send_with_history(node.p, room, b"bad\0".as_ptr(), 4) }, LT_E_ARG);
    let oversized = vec![b'a'; 2049];
    assert_eq!(unsafe { lt_room_send_with_history(node.p, room, oversized.as_ptr(), 2049) }, LT_E_TOO_LARGE);
    assert_eq!(unsafe { lt_room_history_enable(node.p, room, 1) }, LT_OK);
    let text = b"signed original";
    assert_eq!(unsafe { lt_room_send_with_history(node.p, room, text.as_ptr(), text.len() as u32) }, LT_OK);
    let live = wait(&[&node], "live signed event", |_, e| e.kind == LT_EV_CHANNEL_RECORD);
    assert_eq!(live.handle, room);
    assert_eq!(live.peer, peer);
    let verified = lantern_core::channel_history::verify(ticket.topic.as_bytes(), &live.data).unwrap();
    assert_eq!(verified.author().as_bytes(), &peer);
    assert_eq!(verified.text(), "signed original");
    assert_eq!(unsafe { lt_room_history_enable(node.p, room, 1) }, LT_OK);
    let history = wait(&[&node], "saved signed event", |_, e| e.kind == LT_EV_CHANNEL_HISTORY);
    assert_eq!(history.handle, room);
    assert_eq!(history.peer, peer);
    assert_eq!(history.data, live.data);
    assert_eq!(unsafe { lt_room_history_clear(node.p, room) }, LT_OK);
    let cleared = wait(&[&node], "clear acknowledgement", |_, e| e.kind == LT_EV_CHANNEL_HISTORY_CLEARED);
    assert_eq!(cleared.handle, room);
    assert!(cleared.data.is_empty());
    assert_eq!(unsafe { lt_room_history_enable(node.p, room, 1) }, LT_OK);
    let text = b"after clear";
    assert_eq!(unsafe { lt_room_send_with_history(node.p, room, text.as_ptr(), text.len() as u32) }, LT_OK);
    let next = wait(&[&node], "new live signed event", |_, e| e.kind == LT_EV_CHANNEL_RECORD);
    assert_ne!(next.data, live.data);
    assert_eq!(unsafe { lt_room_history_enable(node.p, room, 1) }, LT_OK);
    let loaded = wait(&[&node], "new saved signed event", |_, e| e.kind == LT_EV_CHANNEL_HISTORY);
    assert_eq!(loaded.data, next.data);
    PENDING.with(|p| assert!(!p.borrow()[&(node.p as usize)].iter().any(|e| e.kind == LT_EV_MESSAGE || e.kind == LT_EV_CHANNEL_HISTORY)));
}

#[test]
fn optional_sealed_exports_preserve_tickets_and_decrypt_files_through_abi() {
    let dir = tempfile::tempdir().unwrap();
    let a = open(dir.path(), "sealed-a");
    let b = open(dir.path(), "sealed-b");
    wait(&[&a, &b], "sealed ABI ready", |i, e| i == 0 && e.kind == LT_EV_READY);
    wait(&[&a, &b], "sealed ABI peer ready", |i, e| i == 1 && e.kind == LT_EV_READY);
    let label = CString::new("private").unwrap();
    let mut room_a = 99;
    assert_eq!(unsafe { lt_channel_create_sealed(a.p, label.as_ptr(), ptr::null_mut()) }, LT_E_ARG);
    assert_eq!(unsafe { lt_channel_create_sealed(ptr::null_mut(), label.as_ptr(), &mut room_a) }, LT_E_ARG);
    assert_eq!(room_a, 0);
    assert_eq!(unsafe { lt_channel_create_sealed(a.p, label.as_ptr(), &mut room_a) }, LT_OK);
    wait(&[&a, &b], "sealed ABI creator joined", |i, e| i == 0 && e.kind == LT_EV_ROOM_JOINED && e.handle == room_a);
    let mut sealed = 99;
    assert_eq!(unsafe { lt_room_is_sealed(a.p, 0, &mut sealed) }, LT_E_NOT_FOUND);
    assert_eq!(sealed, 0);
    assert_eq!(unsafe { lt_room_is_sealed(a.p, room_a, &mut sealed) }, LT_OK);
    assert_eq!(sealed, 1);
    let ticket = text_call(|buf, len, out| unsafe { lt_room_ticket(a.p, room_a, buf, len, out) });
    assert!(ticket.starts_with("ltsealed"));
    let parsed: lantern_core::ticket::SealedRoomTicket = ticket.parse().unwrap();
    let c_ticket = CString::new(ticket).unwrap();
    let mut room_b = 0;
    assert_eq!(unsafe { lt_room_join(b.p, 0, ptr::null(), c_ticket.as_ptr(), &mut room_b) }, LT_E_ARG);
    assert_eq!(unsafe { lt_room_join(b.p, 5, ptr::null(), c_ticket.as_ptr(), &mut room_b) }, LT_OK);
    wait(&[&a, &b], "sealed ABI peer connected", |i, e| i == 1 && e.kind == LT_EV_PEER_JOINED && e.handle == room_b);
    let name = CString::new("private.bin").unwrap();
    let bytes = vec![31u8; 65537];
    let mut blob = 99;
    assert_eq!(unsafe { lt_blob_add_sealed_bytes(a.p, room_a, ptr::null(), 16 * 1024 * 1024 + 1, name.as_ptr(), &mut blob) }, LT_E_TOO_LARGE);
    assert_eq!(blob, 0);
    assert_eq!(unsafe { lt_blob_add_sealed_bytes(a.p, room_a, ptr::null(), 1, name.as_ptr(), &mut blob) }, LT_E_ARG);
    assert_eq!(unsafe { lt_blob_add_sealed_bytes(a.p, room_a, bytes.as_ptr(), bytes.len() as u32, name.as_ptr(), &mut blob) }, LT_OK);
    wait(&[&a, &b], "sealed ABI encrypted import", |i, e| i == 0 && e.kind == LT_EV_BLOB_DONE && e.handle == blob);
    assert_eq!(unsafe { lt_blob_offer(a.p, room_a, blob) }, LT_OK);
    let offered = wait(&[&a, &b], "sealed ABI file offer", |i, e| i == 1 && e.kind == LT_EV_BLOB_OFFER);
    let metadata: serde_json::Value = serde_json::from_slice(&offered.data).unwrap();
    assert_eq!(metadata["size"].as_u64(), Some(bytes.len() as u64));
    assert_eq!(metadata["room"].as_u64(), Some(room_b));
    let path = dir.path().join("sealed-abi.download");
    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { lt_blob_fetch(b.p, offered.handle, c_path.as_ptr()) }, LT_E_STATE);
    assert!(!path.exists());
    assert_eq!(unsafe { lt_blob_fetch_bounded(b.p, offered.handle, c_path.as_ptr()) }, LT_OK);
    wait(&[&a, &b], "sealed ABI decrypted save", |i, e| i == 1 && e.kind == LT_EV_BLOB_DONE && e.handle == offered.handle);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(unsafe { lt_blob_unoffer(a.p, room_a, blob) }, LT_OK);
    wait(&[&a, &b], "sealed ABI withdrawal", |i, e| i == 1 && e.kind == LT_EV_BLOB_WITHDRAWN && e.handle == offered.handle);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut next = 0;
    assert_eq!(unsafe { lt_channel_reseal(a.p, room_a, &mut next) }, LT_OK);
    let next_ticket = text_call(|buf, len, out| unsafe { lt_room_ticket(a.p, next, buf, len, out) });
    let next_ticket: lantern_core::ticket::SealedRoomTicket = next_ticket.parse().unwrap();
    assert_ne!(parsed.topic, next_ticket.topic);
    assert_eq!(unsafe { lt_blob_offer(a.p, next, blob) }, LT_E_STATE);
}
