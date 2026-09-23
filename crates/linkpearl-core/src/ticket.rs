//! Tickets: the only thing a player ever has to copy or paste.
//!
//! iroh 1.x does not ship a ticket type any more, so the format is ours. It is
//! postcard over a small struct, base32-nopad, lowercased, with a short human
//! prefix so a ticket is recognisable in a chat log.

use std::{fmt, str::FromStr};

use iroh::EndpointAddr;
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};

use crate::{Error, PROTOCOL_VERSION};

/// What a room is for. Advisory: it is mixed into topic derivation and shown in
/// the UI, and it is what the public directory filters on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum Scope {
    Party = 0,
    Fc = 1,
    Zone = 2,
    World = 3,
    Public = 4,
    Custom = 5,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Party => "party",
            Scope::Fc => "fc",
            Scope::Zone => "zone",
            Scope::World => "world",
            Scope::Public => "public",
            Scope::Custom => "custom",
        }
    }

    /// The room's name in Media-over-QUIC path form: `party/<id>`, `fc/<id>`,
    /// and so on.
    ///
    /// Linkpearl carries metadata, not media. When a room later grows a video
    /// track it will live on a MoQ relay under exactly this prefix, and the
    /// same scope/key pair that derives the gossip topic here names the path
    /// there. Keeping the two in step is the whole point of this function: the
    /// directory stores one identifier and both transports agree on it.
    pub fn moq_path(self, key: &str) -> String {
        format!("{}/{}", self.as_str(), key)
    }

    pub fn from_u32(v: u32) -> Option<Scope> {
        Some(match v {
            0 => Scope::Party,
            1 => Scope::Fc,
            2 => Scope::Zone,
            3 => Scope::World,
            4 => Scope::Public,
            5 => Scope::Custom,
            _ => return None,
        })
    }
}

pub const TICKET_PREFIX: &str = "lproom";

/// Everything a joiner needs: which topic, and somebody already in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomTicket {
    pub version: u16,
    pub topic: TopicId,
    pub scope: Scope,
    /// A short label for the UI. Never trusted, never used for derivation.
    pub label: String,
    pub peers: Vec<EndpointAddr>,
}

impl RoomTicket {
    pub fn new(topic: TopicId, scope: Scope, label: impl Into<String>, peers: Vec<EndpointAddr>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            topic,
            scope,
            label: label.into(),
            peers,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard::to_stdvec is infallible for this type")
    }

    pub fn from_bytes(bytes: &[u8]) -> crate::Result<Self> {
        let t: RoomTicket = postcard::from_bytes(bytes).map_err(|_| Error::Ticket)?;
        if t.version != PROTOCOL_VERSION {
            return Err(Error::Ticket);
        }
        Ok(t)
    }
}

impl fmt::Display for RoomTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = data_encoding::BASE32_NOPAD.encode(&self.to_bytes());
        text.make_ascii_lowercase();
        write!(f, "{TICKET_PREFIX}{text}")
    }
}

impl FromStr for RoomTicket {
    type Err = Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        let s = s.trim();
        let body = s.strip_prefix(TICKET_PREFIX).ok_or(Error::Ticket)?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::Ticket)?;
        Self::from_bytes(&bytes)
    }
}

/// A ticket that names one node, for "connect straight to me".
pub const NODE_TICKET_PREFIX: &str = "lpnode";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTicket {
    pub version: u16,
    pub addr: EndpointAddr,
}

impl NodeTicket {
    pub fn new(addr: EndpointAddr) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            addr,
        }
    }
}

impl fmt::Display for NodeTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = postcard::to_stdvec(self).expect("infallible");
        let mut text = data_encoding::BASE32_NOPAD.encode(&bytes);
        text.make_ascii_lowercase();
        write!(f, "{NODE_TICKET_PREFIX}{text}")
    }
}

impl FromStr for NodeTicket {
    type Err = Error;

    fn from_str(s: &str) -> crate::Result<Self> {
        let s = s.trim();
        let body = s.strip_prefix(NODE_TICKET_PREFIX).ok_or(Error::Ticket)?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::Ticket)?;
        let t: NodeTicket = postcard::from_bytes(&bytes).map_err(|_| Error::Ticket)?;
        if t.version != PROTOCOL_VERSION {
            return Err(Error::Ticket);
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn addr() -> EndpointAddr {
        EndpointAddr::from(SecretKey::generate().public())
    }

    #[test]
    fn room_ticket_round_trips() {
        let t = RoomTicket::new(
            TopicId::from_bytes([7u8; 32]),
            Scope::Public,
            "Watch party",
            vec![addr()],
        );
        let text = t.to_string();
        assert!(text.starts_with(TICKET_PREFIX));
        assert!(
            text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "tickets must survive a chat box: {text}"
        );
        assert_eq!(RoomTicket::from_str(&text).unwrap(), t);
    }

    #[test]
    fn a_node_ticket_is_not_a_room_ticket() {
        let n = NodeTicket::new(addr());
        assert!(RoomTicket::from_str(&n.to_string()).is_err());
        assert!(NodeTicket::from_str(&n.to_string()).is_ok());
    }

    #[test]
    fn garbage_is_rejected_rather_than_panicking() {
        for s in ["", "lproom", "lproomzzzz", "not a ticket", "lpnode!!!!"] {
            assert!(RoomTicket::from_str(s).is_err());
            assert!(NodeTicket::from_str(s).is_err());
        }
    }

    #[test]
    fn scope_round_trips_through_the_c_abi_integer() {
        for s in [
            Scope::Party,
            Scope::Fc,
            Scope::Zone,
            Scope::World,
            Scope::Public,
            Scope::Custom,
        ] {
            assert_eq!(Scope::from_u32(s as u32), Some(s));
        }
        assert_eq!(Scope::from_u32(6), None);
    }

    #[test]
    fn moq_paths_match_the_relay_prefix_convention() {
        assert_eq!(Scope::Party.moq_path("abc"), "party/abc");
        assert_eq!(Scope::Fc.moq_path("9000"), "fc/9000");
        assert_eq!(Scope::Public.moq_path("watch-party"), "public/watch-party");
    }
}
