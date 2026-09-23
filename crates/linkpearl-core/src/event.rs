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

/// A friend's presence status. `Invisible` is what a friend who hid themselves
/// looks like, and what this node sends while hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum Status {
    Invisible = 0,
    #[default]
    Online = 1,
    Away = 2,
    Busy = 3,
}

impl Status {
    pub fn from_u32(v: u32) -> Option<Status> {
        Some(match v {
            0 => Status::Invisible,
            1 => Status::Online,
            2 => Status::Away,
            3 => Status::Busy,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Invisible => "invisible",
            Status::Online => "online",
            Status::Away => "away",
            Status::Busy => "busy",
        }
    }
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

    // ------------------------------------------------------------- friends
    /// A device became a friend: either we redeemed its invite (`handle` is
    /// what `invite_accept` returned) or it redeemed ours (`handle` is 0).
    FriendAdded {
        handle: u64,
        peer: PeerId,
        name: String,
    },
    /// Removed by us or by them. Either way the link is gone.
    FriendRemoved { peer: PeerId },
    /// An `invite_accept` did not work out.
    InviteFailed { handle: u64, reason: String },
    /// A friend came online or changed status or note.
    FriendOnline {
        peer: PeerId,
        status: Status,
        note: String,
    },
    /// A friend went offline, went invisible, or stopped answering.
    FriendOffline { peer: PeerId },
    /// A 1:1 text. Raised once per message even if the sender retried.
    FriendText {
        peer: PeerId,
        id: u64,
        sent_at: i64,
        text: String,
    },
    /// The friend acknowledged a text we sent.
    FriendDelivered { peer: PeerId, id: u64 },
    /// A friend invited us to a channel. Nothing is joined until the caller
    /// passes the ticket to `room_join`.
    ChannelInvite { peer: PeerId, ticket: String },

    // --------------------------------------------- nostr (feature `nostr`)
    /// A nostr job (`nostr_*` on `Node`) finished.
    NostrDone {
        handle: u64,
        ok: bool,
        detail: String,
    },
    /// A friend invite arrived as a NIP-17 message. `from` is the sender's
    /// (verified) nostr key; nothing is redeemed until the caller passes the
    /// ticket to `invite_accept`.
    NostrInvite { from: [u8; 32], ticket: String },
    /// A user's verified device list.
    NostrDevices {
        handle: u64,
        user: [u8; 32],
        devices: Vec<PeerId>,
    },

    // ---------------------------------------- calls (feature `calls`, spike)
    /// A friend is calling. `media` is the moq URL their media sidecar will
    /// serve (`iroh://<node>/call/<id>`); nothing carries media yet.
    CallIncoming { peer: PeerId, call: u64, media: String },
    /// The callee answered a call we placed.
    CallAnswered {
        peer: PeerId,
        call: u64,
        accepted: bool,
    },
    /// Hung up by either side, or the friend link dropped.
    CallEnded { peer: PeerId, call: u64 },
}
