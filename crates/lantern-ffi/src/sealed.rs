//! Optional ABI-2 sealed-channel exports. No existing layout or symbol changes.
use super::*;

/// # Safety
/// node must be live; label must be NULL or UTF-8 NUL-terminated; out writable.
#[no_mangle]
pub unsafe extern "C" fn lt_channel_create_sealed(node: *mut lt_node, label: *const c_char, out: *mut u64) -> lt_status {
    guard(|| {
        if out.is_null() { return LT_E_ARG; }
        *out = 0;
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        let label = match opt_str(label) { Ok(v) => v.unwrap_or(""), Err(e) => return e };
        match node.node.channel_create_sealed(label) {
            Ok(room) => { *out = room; LT_OK },
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// node must be live; out must point to a writable u64.
#[no_mangle]
pub unsafe extern "C" fn lt_channel_reseal(node: *mut lt_node, room: u64, out: *mut u64) -> lt_status {
    guard(|| {
        if out.is_null() { return LT_E_ARG; }
        *out = 0;
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        match node.node.channel_reseal(room) {
            Ok(next) => { *out = next; LT_OK },
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// node must be live; out must point to a writable u32 (0 or 1).
#[no_mangle]
pub unsafe extern "C" fn lt_room_is_sealed(node: *mut lt_node, room: u64, out: *mut u32) -> lt_status {
    guard(|| {
        if out.is_null() { return LT_E_ARG; }
        *out = 0;
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        match node.node.room_is_sealed(room) {
            Ok(value) => { *out = u32::from(value); LT_OK },
            Err(e) => status_of(&e),
        }
    })
}

/// # Safety
/// node must be live; data must have len readable bytes; name must be UTF-8
/// NUL-terminated or NULL; out must be writable. Encryption runs on a worker.
#[no_mangle]
pub unsafe extern "C" fn lt_blob_add_sealed_bytes(node: *mut lt_node, room: u64,
    data: *const u8, len: u32, name: *const c_char, out: *mut u64) -> lt_status {
    guard(|| {
        if out.is_null() { return LT_E_ARG; }
        *out = 0;
        if len > 16 * 1024 * 1024 { return LT_E_TOO_LARGE; }
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        let Some(bytes) = slice_of(data, len) else { return LT_E_ARG; };
        let name = match opt_str(name) { Ok(v) => v.unwrap_or(""), Err(e) => return e };
        match node.node.blob_add_sealed_bytes(room, bytes, name) {
            Ok(blob) => { *out = blob; LT_OK },
            Err(e) => status_of(&e),
        }
    })
}
