//! Reverify cached provider routing claims using the original signed offer.
//! Remember only offers accepted by the live nonce/provider checks after a
//! successful authenticated snapshot fetch. This helper is not that admission
//! check: old request nonces intentionally remain usable as routing evidence.
use super::*;

pub(super) const MAX_HINT_BYTES: usize = 4096;
const MAX_ROUTES: usize = 8;

#[cfg(test)]
pub(super) fn verified_address(
    room: &[u8; 32], recipient: &PeerId, original: &[u8],
) -> Option<EndpointAddr> {
    verified_address_sealed(room, recipient, original, None)
}

pub(super) fn verified_address_sealed(
    room: &[u8; 32], recipient: &PeerId, original: &[u8], sealed: Option<&SealedRoomTicket>,
) -> Option<EndpointAddr> {
    if sealed.is_some_and(|t| t.topic.as_bytes() != room) { return None; }
    if original.is_empty() || original.len() > MAX_HINT_BYTES { return None; }
    let (author, frame) = match sealed {
        Some(ticket) => proto::decode_sealed(ticket, original)?,
        None => proto::decode(original)?,
    };
    let maximum = if sealed.is_some() { crate::channel_history::MAX_SEALED_SNAPSHOT_BYTES } else { crate::channel_history::MAX_SNAPSHOT_BYTES };
    let Frame::HistoryOffer { topic, to, attempt, size, provider, .. } = frame else { return None; };
    if &topic != room || &to != recipient || provider.id != author || attempt > 7
        || size < crate::channel_history::SNAPSHOT_HEADER as u64
        || size > maximum as u64
        || provider.addrs.len() > MAX_ROUTES
        || provider.addrs.iter().any(|addr| !addr.is_ip() && !addr.is_relay())
    { return None; }
    Some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer(secret: &SecretKey, room: [u8; 32], to: PeerId, addr: EndpointAddr, size: u64, attempt: u8) -> Vec<u8> {
        proto::encode(secret, &Frame::HistoryOffer {
            topic: room, to, nonce: [7; 16], attempt, hash: [9; 32], size, provider: addr,
        })
    }
    #[test]
    fn original_offer_binds_address_owner_channel_and_recipient() {
        let sender = SecretKey::generate();
        let recipient = *SecretKey::generate().public().as_bytes();
        let addr = EndpointAddr::new(sender.public()).with_ip_addr("127.0.0.1:4242".parse().unwrap());
        let original = offer(&sender, [1; 32], recipient, addr.clone(), 42, 0);
        assert_eq!(verified_address(&[1; 32], &recipient, &original), Some(addr));
        assert!(verified_address(&[2; 32], &recipient, &original).is_none());
        assert!(verified_address(&[1; 32], &[0; 32], &original).is_none());
        let forged_owner = EndpointAddr::new(SecretKey::generate().public());
        assert!(verified_address(&[1; 32], &recipient, &offer(&sender, [1; 32], recipient, forged_owner, 42, 0)).is_none());
        let mut forged = original;
        *forged.last_mut().unwrap() ^= 1;
        assert!(verified_address(&[1; 32], &recipient, &forged).is_none());
    }
    #[test]
    fn corrupt_wrong_kind_and_oversized_hints_are_not_routing_inputs() {
        let sender = SecretKey::generate();
        let recipient = *SecretKey::generate().public().as_bytes();
        assert!(verified_address(&[1; 32], &recipient, &[]).is_none());
        assert!(verified_address(&[1; 32], &recipient, &vec![0; MAX_HINT_BYTES + 1]).is_none());
        assert!(verified_address(&[1; 32], &recipient, &proto::encode(&sender, &Frame::App(b"not a route".to_vec()))).is_none());
        let mut addr = EndpointAddr::new(sender.public());
        for port in 4200..4209 { addr = addr.with_ip_addr(format!("127.0.0.1:{port}").parse().unwrap()); }
        assert!(verified_address(&[1; 32], &recipient, &offer(&sender, [1; 32], recipient, addr, 42, 0)).is_none());
        for (size, attempt) in [(41, 0), (crate::channel_history::MAX_SNAPSHOT_BYTES as u64 + 1, 0), (42, 8)] {
            assert!(verified_address(&[1; 32], &recipient, &offer(&sender, [1; 32], recipient, EndpointAddr::new(sender.public()), size, attempt)).is_none());
        }
    }
}
