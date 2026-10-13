//! Bounded channel metadata delivery, independent of the lossy general queue.
use super::*;
use std::collections::VecDeque;

#[derive(Default)]
pub(crate) struct Delivery {
    active: HashSet<RoomHandle>,
    rooms: HashMap<RoomHandle, VecDeque<Event>>,
    ready: VecDeque<RoomHandle>,
}

impl Delivery {
    pub(crate) fn register(&mut self, room: RoomHandle) { self.active.insert(room); }
    /// At most 256 pending originals and one clear acknowledgement per joined
    /// handle. Full channels retain the newest arrivals, like local retention.
    /// Returns whether an older pending record was evicted.
    pub(crate) fn push(&mut self, event: Event) -> bool {
        let room = match &event {
            Event::ChannelRecord { room, .. } | Event::ChannelHistoryCleared { room } => *room,
            _ => unreachable!("only channel metadata uses this queue"),
        };
        if !self.active.contains(&room) { return false; }
        if !self.rooms.contains_key(&room) { self.ready.push_back(room); }
        let queue = self.rooms.entry(room).or_default();
        if matches!(event, Event::ChannelHistoryCleared { .. }) {
            queue.clear();
            queue.push_back(event);
            return false;
        }
        if let Event::ChannelRecord { encoded, .. } = &event {
            if queue.iter().any(|e| matches!(e, Event::ChannelRecord { encoded: old, .. } if old == encoded)) {
                return false;
            }
        }
        let records = queue.iter().filter(|e| matches!(e, Event::ChannelRecord { .. })).count();
        let evicted = records >= crate::channel_history::MAX_RECORDS;
        if evicted {
            let index = queue.iter().position(|e| matches!(e, Event::ChannelRecord { .. })).unwrap();
            queue.remove(index);
        }
        queue.push_back(event);
        evicted
    }

    pub(crate) fn pop(&mut self) -> Option<Event> {
        let room = self.ready.pop_front()?;
        let queue = self.rooms.get_mut(&room).unwrap();
        let event = queue.pop_front();
        if queue.is_empty() { self.rooms.remove(&room); }
        else { self.ready.push_back(room); }
        event
    }

    pub(crate) fn block_author(&mut self, author: &PeerId) {
        for queue in self.rooms.values_mut() {
            queue.retain(|event| !matches!(event, Event::ChannelRecord { peer, .. } if peer == author));
        }
        self.rooms.retain(|_, queue| !queue.is_empty());
        self.ready.retain(|room| self.rooms.contains_key(room));
    }

    pub(crate) fn forget(&mut self, room: RoomHandle) {
        self.active.remove(&room);
        self.rooms.remove(&room);
        self.ready.retain(|r| *r != room);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(room: u64, id: u16) -> Event {
        Event::ChannelRecord { room, peer: [0; 32], encoded: id.to_be_bytes().to_vec(), historical: true }
    }
    #[test]
    fn channels_are_fair_and_pending_originals_deduplicate() {
        let mut q = Delivery::default();
        q.register(1); q.register(2);
        q.push(line(1, 1)); q.push(line(1, 1)); q.push(line(1, 2)); q.push(line(2, 3));
        let handles: Vec<_> = std::iter::from_fn(|| q.pop()).map(|e| match e { Event::ChannelRecord { room, .. } => room, _ => panic!() }).collect();
        assert_eq!(handles, [1, 2, 1]);
    }
    #[test]
    fn pressure_keeps_recent_records_and_never_evicts_clear_ack() {
        let mut q = Delivery::default();
        q.register(1); q.register(2);
        q.push(line(1, 999)); q.push(Event::ChannelHistoryCleared { room: 1 });
        for id in 0..300 { q.push(line(1, id)); }
        assert!(matches!(q.pop(), Some(Event::ChannelHistoryCleared { room: 1 })));
        let records: Vec<_> = std::iter::from_fn(|| q.pop()).map(|e| match e { Event::ChannelRecord { encoded, .. } => u16::from_be_bytes(encoded.try_into().unwrap()), _ => panic!() }).collect();
        assert_eq!(records, (44..300).collect::<Vec<_>>());
        q.push(line(2, 1)); q.forget(2); q.push(line(2, 2));
        assert!(q.pop().is_none(), "late delivery recreated a left channel");
    }
}
