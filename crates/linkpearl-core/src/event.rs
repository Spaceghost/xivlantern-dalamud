//! Everything the async half tells the caller about.

use crate::{BlobHandle, ConnHandle, RoomHandle};

/// A peer's public key, as raw bytes. The C ABI hands these out verbatim.
pub type PeerId = [u8; 32];

/// What a peer said it is offering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    pub hash: [u8; 32],
    pub size: u64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub enum Event {
    /// The endpoint is bound and the node id is stable.
    Ready { node_id: PeerId },
    RoomJoined { room: RoomHandle },
    RoomLeft { room: RoomHandle },
    PeerJoined { room: RoomHandle, peer: PeerId },
    PeerLeft { room: RoomHandle, peer: PeerId },
    /// A peer replaced its presence payload.
    Presence {
        room: RoomHandle,
        peer: PeerId,
        data: Vec<u8>,
    },
    /// An application message broadcast to a room.
    Message {
        room: RoomHandle,
        peer: PeerId,
        data: Vec<u8>,
    },
    /// An application message over a direct connection.
    Direct {
        conn: ConnHandle,
        peer: PeerId,
        data: Vec<u8>,
    },
    Connected { conn: ConnHandle, peer: PeerId },
    Disconnected { conn: ConnHandle, peer: PeerId },
    /// A peer offered a blob to a room we are in. Nothing is downloaded until
    /// the caller asks for it.
    BlobOffer {
        room: RoomHandle,
        peer: PeerId,
        blob: BlobHandle,
        meta: BlobMeta,
    },
    BlobProgress {
        blob: BlobHandle,
        done: u64,
        total: u64,
    },
    /// Hashing or downloading finished. `detail` is the local path for a fetch
    /// and the hash text for an add.
    BlobDone {
        blob: BlobHandle,
        detail: String,
    },
    BlobFailed {
        blob: BlobHandle,
        reason: String,
    },
    /// A ticket the caller asked for asynchronously.
    Ticket { handle: u64, text: String },
    /// This node started or stopped publishing into a room. The UI is expected
    /// to show this: it is the "you are live" indicator.
    Publishing { room: RoomHandle, publishing: bool },
    Error { message: String },
    Log { message: String },
}
