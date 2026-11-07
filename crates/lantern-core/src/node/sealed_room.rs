//! Worker-only sealing admission and bounded recent-packet replay suppression.
use super::*;
use std::collections::VecDeque;

pub(super) fn register(store: &mut Store, topic: &TopicId, sealed: Option<&SealedRoomTicket>) -> Result<()> {
    let name = format!("sealed-room:{}", data_encoding::HEXLOWER.encode(topic.as_bytes()));
    let expected = sealed.map(|ticket| {
        // A marker, not the key or a usable ticket. Kept even after room leave.
        blake3::derive_key("XivLantern sealed channel key marker v1", ticket.key())
    }).map(|hash| data_encoding::HEXLOWER.encode(&hash));
    match (store.profile_get(&name)?, expected) {
        (Some(saved), Some(expected)) if saved == expected => Ok(()),
        (Some(_), _) => Err(Error::State("channel is sealed; a matching sealed ticket is required")),
        (None, Some(expected)) => store.profile_set(&name, &expected),
        (None, None) => Ok(()),
    }
}

/// Gossip already suppresses duplicates. Keep an additional bounded window
/// after authentication; this is not persistent replay protection after restart.
#[derive(Default)]
pub(super) struct Replay {
    queue: VecDeque<[u8; 32]>,
    seen: HashSet<[u8; 32]>,
}
impl Replay {
    pub(super) fn admit(&mut self, packet: &[u8]) -> bool {
        let hash = *blake3::hash(packet).as_bytes();
        if !self.seen.insert(hash) { return false; }
        self.queue.push_back(hash);
        if self.queue.len() > 2048 {
            self.seen.remove(&self.queue.pop_front().unwrap());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sealed_marker_survives_restart_and_refuses_plain_or_wrong_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.sqlite");
        let ticket = SealedRoomTicket::new("Friends", vec![]).unwrap();
        {
            let mut store = Store::open(Some(&path)).unwrap();
            register(&mut store, &ticket.topic, Some(&ticket)).unwrap();
        }
        let mut store = Store::open(Some(&path)).unwrap();
        register(&mut store, &ticket.topic, Some(&ticket)).unwrap();
        assert!(register(&mut store, &ticket.topic, None).is_err());
        let mut wrong = SealedRoomTicket::new("wrong", vec![]).unwrap();
        wrong.topic = ticket.topic;
        assert!(register(&mut store, &ticket.topic, Some(&wrong)).is_err());
        assert!(register(&mut store, &TopicId::from_bytes([3; 32]), None).is_ok());
    }
    #[test]
    fn replay_window_is_bounded_and_duplicates_do_not_shift_it() {
        let mut replay = Replay::default();
        assert!(replay.admit(b"first"));
        for _ in 0..2049 { assert!(!replay.admit(b"first")); }
        for i in 0u32..2048 { assert!(replay.admit(&i.to_be_bytes())); }
        assert_eq!(replay.seen.len(), 2048);
        assert_eq!(replay.queue.len(), 2048);
        assert!(!replay.admit(&2047u32.to_be_bytes()));
    }
}
