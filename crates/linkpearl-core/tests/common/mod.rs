//! Shared by the integration tests: open a relay-less node, and pump events.
#![allow(dead_code)]

use std::{
    collections::VecDeque,
    path::Path,
    time::{Duration, Instant},
};

use linkpearl_core::{Config, Event, Node, RelayMode};

pub const HEARTBEAT_MS: u64 = 200;

/// A node on the loopback interface with relays and n0 address lookup off.
pub fn node(name: &str, dir: &Path) -> Node {
    let config = Config {
        db_path: Some(dir.join(format!("{name}.sqlite"))),
        app_id: "linkpearl-test/1".to_string(),
        relay: RelayMode::Disabled,
        event_queue_cap: 1024,
        heartbeat_ms: HEARTBEAT_MS,
        ..Config::default()
    };
    let n = Node::open(config).expect("node must open");
    n.set_display_name(name).expect("name");
    n
}

/// Every polled event goes into a per-node backlog. `wait` removes only the
/// first event its predicate matches and leaves everything else for later
/// waits, so an event that arrives before anyone asks for it is not lost.
pub struct Pump {
    log: Vec<(usize, Event)>,
    pending: Vec<VecDeque<Event>>,
}

impl Pump {
    pub fn new() -> Self {
        Pump {
            log: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Poll every node into its backlog; returns how many events were new.
    pub fn fill(&mut self, nodes: &[&Node]) -> Vec<usize> {
        self.pending.resize_with(nodes.len().max(self.pending.len()), VecDeque::new);
        let mut fresh = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let events = n.poll(64);
            fresh.push(events.len());
            self.log.extend(events.iter().cloned().map(|e| (i, e)));
            self.pending[i].extend(events);
        }
        fresh
    }

    /// Forget node `i`'s backlog (it was restarted).
    pub fn reset(&mut self, i: usize) {
        if let Some(q) = self.pending.get_mut(i) {
            q.clear();
        }
    }

    pub fn transcript(&self) -> String {
        self.log
            .iter()
            .map(|(i, e)| format!("  node{i}: {e:?}\n"))
            .collect()
    }

    pub fn wait<F>(&mut self, nodes: &[&Node], what: &str, secs: u64, mut pred: F) -> Event
    where
        F: FnMut(usize, &Event) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            self.fill(nodes);
            for i in 0..nodes.len() {
                if let Some(pos) = self.pending[i].iter().position(|e| pred(i, e)) {
                    return self.pending[i].remove(pos).unwrap();
                }
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for {what} after {secs}s\ntranscript:\n{}",
                    self.transcript()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Poll until `cond` holds, for state that may have changed before this
    /// call started looking.
    pub fn until(&mut self, nodes: &[&Node], what: &str, secs: u64, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while !cond() {
            self.fill(nodes);
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}\ntranscript:\n{}", self.transcript());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// No *new* event matching `pred` for `ms`.
    pub fn quiet<F>(&mut self, nodes: &[&Node], ms: u64, mut pred: F) -> bool
    where
        F: FnMut(usize, &Event) -> bool,
    {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            let fresh = self.fill(nodes);
            for (i, n) in fresh.into_iter().enumerate() {
                let q = &self.pending[i];
                if q.iter().skip(q.len() - n).any(|e| pred(i, e)) {
                    return false;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

