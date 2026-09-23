//! Node configuration.

use std::path::PathBuf;

/// Where a node is allowed to look for help traversing NAT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayMode {
    /// The n0 public relays plus hole punching. What almost everyone wants.
    Default,
    /// No relay at all. Direct paths only, so a player behind a symmetric NAT
    /// will simply fail to connect. Nothing ever touches a third party: this
    /// also turns off n0's DNS/pkarr address lookup.
    Disabled,
    /// A relay the player (or the FC) runs.
    Custom(String),
}

impl Default for RelayMode {
    fn default() -> Self {
        RelayMode::Default
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// SQLite file holding the identity key and bookmarks. `None` => in memory,
    /// which means a fresh identity every run (tests use this).
    pub db_path: Option<PathBuf>,
    /// Directory for the iroh-blobs store. Defaults to `<db_path>.blobs`, or a
    /// memory store when `db_path` is `None`.
    pub blob_dir: Option<PathBuf>,
    /// Namespace for room derivation and for the direct-message ALPN. Two nodes
    /// with different app ids never meet, even with the same room key.
    pub app_id: String,
    pub relay: RelayMode,
    /// Event queue capacity. Overflow is counted, not blocked on.
    pub event_queue_cap: usize,
    /// Accept inbound connections at all. Off means the player is invisible to
    /// strangers; joined rooms still work outbound.
    pub accept_inbound: bool,
    /// How often friend presence is sent, and the unit of the offline timeout
    /// (three missed heartbeats) and of the redial backoff.
    pub heartbeat_ms: u64,
    /// Bind the endpoint's IPv4 socket to this UDP port instead of a random
    /// one, so a stored friend address stays valid across restarts on a LAN
    /// with no relay. `None` lets iroh choose.
    pub bind_port: Option<u16>,
    /// Author mode: accept support messages from nodes that are not friends
    /// (rate limited). Only the mod author's own node turns this on; a player's
    /// never does.
    pub accept_support: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: None,
            blob_dir: None,
            app_id: "ffxiv-linkpearl/1".to_string(),
            relay: RelayMode::Default,
            event_queue_cap: 4096,
            accept_inbound: true,
            heartbeat_ms: 15_000,
            bind_port: None,
            accept_support: false,
        }
    }
}

impl Config {
    pub fn in_memory(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_string(),
            ..Default::default()
        }
    }

    /// The ALPN used for direct (non-gossip, non-blob) messages.
    pub fn direct_alpn(&self) -> Vec<u8> {
        format!("{}/direct/1", self.app_id).into_bytes()
    }

    /// The ALPN friend links speak: invites, presence, 1:1 text.
    pub fn friend_alpn(&self) -> Vec<u8> {
        format!("{}/friend/1", self.app_id).into_bytes()
    }

    /// The ALPN `selftest` dials this node on; it only accepts and waits.
    pub fn selftest_alpn(&self) -> Vec<u8> {
        format!("{}/selftest/1", self.app_id).into_bytes()
    }
}
