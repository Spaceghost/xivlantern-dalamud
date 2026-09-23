//! Node configuration.

use std::path::PathBuf;

/// Where a node is allowed to look for help traversing NAT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayMode {
    /// The n0 public relays plus hole punching. What almost everyone wants.
    Default,
    /// No relay at all. Direct paths only, so a player behind a symmetric NAT
    /// will simply fail to connect. Nothing ever touches a third party.
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
}
