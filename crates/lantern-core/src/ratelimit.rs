//! Per-peer token buckets.
//!
//! Every frame another node can make us process — a friend-link frame, a room
//! message, a stranger presenting an invite or a support message — spends a
//! token from that peer's bucket. An empty bucket means the frame is dropped
//! before it is decoded further, stored or shown. Buckets refill continuously,
//! so an ordinary conversation never notices.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::event::PeerId;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// `rate` tokens per second, at most `burst` saved up.
#[derive(Debug)]
pub struct Limiter {
    rate: f64,
    burst: f64,
    buckets: HashMap<PeerId, Bucket>,
}

/// Beyond this many tracked peers, full buckets (peers who have been quiet
/// long enough to refill) are forgotten; they would start full again anyway.
const MAX_TRACKED: usize = 4096;

impl Limiter {
    pub fn new(rate_per_sec: f64, burst: f64) -> Self {
        Limiter {
            rate: rate_per_sec,
            burst: burst.max(1.0),
            buckets: HashMap::new(),
        }
    }

    /// Spend one token for `peer` at `now`. `false`: drop what they sent.
    pub fn allow(&mut self, peer: &PeerId, now: Instant) -> bool {
        if self.buckets.len() >= MAX_TRACKED && !self.buckets.contains_key(peer) {
            self.forget_full(now);
        }
        let (rate, burst) = (self.rate, self.burst);
        let b = self.buckets.entry(*peer).or_insert(Bucket {
            tokens: burst,
            last: now,
        });
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * rate).min(burst);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn forget_full(&mut self, now: Instant) {
        let (rate, burst) = (self.rate, self.burst);
        self.buckets.retain(|_, b| {
            let refilled = b.tokens + now.saturating_duration_since(b.last).as_secs_f64() * rate;
            refilled < burst
        });
        if self.buckets.len() >= MAX_TRACKED {
            // Everyone is busy: forgetting an arbitrary one is still bounded.
            if let Some(k) = self.buckets.keys().next().copied() {
                self.buckets.remove(&k);
            }
        }
    }

    pub fn tracked(&self) -> usize {
        self.buckets.len()
    }
}

/// The limits one node applies, and the "you are being rate limited" notice
/// throttle so a flood cannot turn into a flood of notices.
#[derive(Debug)]
pub struct Limits {
    /// Frames on a friend link: presence, texts, acks, invites to channels.
    pub friend: Limiter,
    /// Messages in a gossip room, per author.
    pub room: Limiter,
    /// A node that is not a friend: invite redemptions and support messages.
    pub stranger: Limiter,
    warned: HashMap<PeerId, Instant>,
}

pub const NOTICE_EVERY: Duration = Duration::from_secs(30);

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // A heartbeat is one frame per interval; a person typing fast is
            // well under 2 texts a second. Bursts cover an outbox flush.
            friend: Limiter::new(8.0, 64.0),
            room: Limiter::new(5.0, 30.0),
            stranger: Limiter::new(0.2, 5.0),
            warned: HashMap::new(),
        }
    }
}

impl Limits {
    /// `true` when a notice about `peer` should be raised now.
    pub fn should_warn(&mut self, peer: &PeerId, now: Instant) -> bool {
        if self.warned.len() >= MAX_TRACKED {
            self.warned.retain(|_, t| now.saturating_duration_since(*t) < NOTICE_EVERY);
        }
        match self.warned.get(peer) {
            Some(t) if now.saturating_duration_since(*t) < NOTICE_EVERY => false,
            _ => {
                self.warned.insert(*peer, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_allowed_then_refused_then_refills() {
        let mut l = Limiter::new(2.0, 5.0);
        let peer = [1u8; 32];
        let t0 = Instant::now();
        for i in 0..5 {
            assert!(l.allow(&peer, t0), "burst token {i}");
        }
        assert!(!l.allow(&peer, t0), "the sixth in the same instant is refused");
        assert!(!l.allow(&peer, t0 + Duration::from_millis(400)), "0.8 tokens is not one");
        assert!(l.allow(&peer, t0 + Duration::from_millis(500)), "one token after half a second");
        assert!(l.allow(&peer, t0 + Duration::from_secs(60)));
        let mut n = 0;
        while l.allow(&peer, t0 + Duration::from_secs(60)) {
            n += 1;
        }
        assert_eq!(n, 4, "refill is capped at the burst");
    }

    #[test]
    fn peers_do_not_share_a_bucket() {
        let mut l = Limiter::new(1.0, 1.0);
        let t = Instant::now();
        assert!(l.allow(&[1u8; 32], t));
        assert!(!l.allow(&[1u8; 32], t));
        assert!(l.allow(&[2u8; 32], t), "someone else's flood is not my problem");
    }

    #[test]
    fn tracking_is_bounded() {
        let mut l = Limiter::new(1.0, 2.0);
        let t = Instant::now();
        for i in 0..(MAX_TRACKED as u32 + 100) {
            let mut p = [0u8; 32];
            p[..4].copy_from_slice(&i.to_le_bytes());
            l.allow(&p, t);
        }
        assert!(l.tracked() <= MAX_TRACKED);
    }

    #[test]
    fn notices_are_throttled_per_peer() {
        let mut limits = Limits::default();
        let t = Instant::now();
        assert!(limits.should_warn(&[1u8; 32], t));
        assert!(!limits.should_warn(&[1u8; 32], t + Duration::from_secs(5)));
        assert!(limits.should_warn(&[2u8; 32], t + Duration::from_secs(5)));
        assert!(limits.should_warn(&[1u8; 32], t + NOTICE_EVERY + Duration::from_secs(1)));
    }
}
