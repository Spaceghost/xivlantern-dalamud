//! Optional name-card bridge. Identity and invite preparation touch SQLite and
//! cryptography, so these entry points must be called on a background thread.
use super::*;
use std::time::Duration;

unsafe fn relay_list(data: *const u8, len: u32) -> Result<Vec<String>, lt_status> {
    if data.is_null() || len == 0 || len > 16384 { return Err(LT_E_ARG); }
    let text = std::str::from_utf8(std::slice::from_raw_parts(data, len as usize))
        .map_err(|_| LT_E_ARG)?;
    let mut urls = Vec::new();
    for url in text.split('\n') {
        if url.is_empty() || url.len() > 2048 || url.chars().any(char::is_whitespace)
            || url.chars().any(char::is_control)
            || !(url.starts_with("wss://") || url.starts_with("ws://")) {
            return Err(LT_E_ARG);
        }
        if !urls.iter().any(|old| old == url) { urls.push(url.to_owned()); }
        if urls.len() > 8 { return Err(LT_E_ARG); }
    }
    Ok(urls)
}

/// Create/load this node's nostr identity and copy its public npub. No network.
/// # Safety
/// Valid node and writable output buffer/length; background thread only.
#[no_mangle]
pub unsafe extern "C" fn lt_nostr_identity(
    node: *mut lt_node, buf: *mut c_char, cap: usize, out_len: *mut usize,
) -> lt_status {
    guard(|| {
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        if let Err(e) = node.node.nostr_keys() { return status_of(&e); }
        match node.node.nostr_npub() {
            Some(npub) => write_text(&npub, buf, cap, out_len),
            None => LT_E_INTERNAL,
        }
    })
}

/// Queue a NIP-17 invite. Completion arrives as NOSTR_DONE/NOSTR_FAILED.
/// # Safety
/// Valid node, NUL-terminated recipient, relay bytes and writable out_handle.
/// Background thread only. No automatic acceptance.
#[no_mangle]
pub unsafe extern "C" fn lt_nostr_send_invite(
    node: *mut lt_node, relays: *const u8, relays_len: u32,
    recipient: *const c_char, ttl_secs: u32, out_handle: *mut u64,
) -> lt_status {
    guard(|| {
        if out_handle.is_null() { return LT_E_ARG; }
        *out_handle = 0;
        if ttl_secs == 0 || ttl_secs > 7 * 86400 { return LT_E_ARG; }
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        let relays = match relay_list(relays, relays_len) { Ok(r) => r, Err(e) => return e };
        let recipient = match req_str(recipient) {
            Ok(r) if !r.is_empty() && r.len() <= 1024 => r,
            _ => return LT_E_ARG,
        };
        match node.node.nostr_send_invite(&relays, recipient, Some(Duration::from_secs(ttl_secs as u64))) {
            Ok(h) => { *out_handle = h; LT_OK },
            Err(e) => status_of(&e),
        }
    })
}

/// Queue an inbox check. Verified pending invitations arrive as NOSTR_INVITE.
/// # Safety
/// Valid node, relay bytes and writable out_handle; background thread only.
#[no_mangle]
pub unsafe extern "C" fn lt_nostr_check_inbox(
    node: *mut lt_node, relays: *const u8, relays_len: u32, out_handle: *mut u64,
) -> lt_status {
    guard(|| {
        if out_handle.is_null() { return LT_E_ARG; }
        *out_handle = 0;
        let node = match node_ref(node) { Ok(n) => n, Err(e) => return e };
        let relays = match relay_list(relays, relays_len) { Ok(r) => r, Err(e) => return e };
        match node.node.nostr_check_inbox(&relays) {
            Ok(h) => { *out_handle = h; LT_OK },
            Err(e) => status_of(&e),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relay_inputs_are_bounded_and_reject_ambiguous_whitespace() {
        unsafe {
            assert!(relay_list(ptr::null(), 4).is_err());
            assert!(relay_list(b"x".as_ptr(), 16385).is_err());
            for bad in ["", "https://example.invalid", "wss://example.invalid\n", " wss://example.invalid", "wss://example.invalid\r", "ws://example.invalid\0"] {
                assert!(relay_list(bad.as_ptr(), bad.len() as u32).is_err(), "{bad:?}");
            }
            let good = "ws://127.0.0.1:1\nwss://example.invalid\nws://127.0.0.1:1";
            assert_eq!(relay_list(good.as_ptr(), good.len() as u32).unwrap().len(), 2);
            let too_many = (0..9).map(|i| format!("wss://{i}.invalid")).collect::<Vec<_>>().join("\n");
            assert!(relay_list(too_many.as_ptr(), too_many.len() as u32).is_err());
            let long = format!("wss://{}", "x".repeat(2048));
            assert!(relay_list(long.as_ptr(), long.len() as u32).is_err());
            let malformed = [0xff];
            assert!(relay_list(malformed.as_ptr(), 1).is_err());
        }
    }
}
