//! Linkpearl: a friend list, presence, 1:1 text, group channels, rooms and file
//! transfer over iroh, shaped so a game plugin can drive it from its frame
//! thread. See `docs/DESIGN.md` for what is built and what is only designed.
//!
//! The crate is deliberately split in two halves:
//!
//! * an **async half** that owns a tokio runtime, the iroh endpoint, gossip and
//!   blobs. Nothing in the async half is ever awaited by a caller.
//! * a **synchronous handle** ([`Node`]) whose every method either reads an
//!   atomic/lock snapshot or pushes a command onto an unbounded queue and
//!   returns. Results come back as [`Event`]s, drained with [`Node::poll`].
//!
//! [`Node::open`] and [`Node::close`] are the only blocking calls: they start
//! and stop the runtime, open SQLite and bind the endpoint. Everything else is
//! safe to call once per frame.

pub mod config;
pub mod event;
pub mod proto;
pub mod store;
pub mod ticket;
pub mod topic;

mod node;

pub use config::{Config, RelayMode};
pub use event::{BlobMeta, Event, PeerId, Status};
pub use node::{FriendInfo, HistoryEntry, Node, PreparedInvite};
pub use ticket::{FriendInvite, RoomTicket, Scope};

/// Largest single room or direct message. Anything bigger belongs in a blob.
pub const MAX_MESSAGE: usize = 61440;

/// The ABI/protocol generation. Bumped only on a wire-incompatible change.
pub const PROTOCOL_VERSION: u16 = 1;

/// Errors a synchronous call can return. Anything that can only be discovered
/// asynchronously arrives as [`Event::Error`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A caller argument was unusable.
    Arg(&'static str),
    /// The node is closed, or the operation needs a state it is not in.
    State(&'static str),
    /// Unknown room, connection or blob handle.
    NotFound,
    /// The command queue is full; retry on a later frame.
    Full,
    /// A ticket did not parse.
    Ticket,
    /// SQLite or blob store failure.
    Store(String),
    /// Endpoint bind or transport failure.
    Net(String),
    /// Payload exceeded [`MAX_MESSAGE`].
    TooLarge,
    /// Anything we did not classify.
    Internal(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Arg(m) => write!(f, "bad argument: {m}"),
            Error::State(m) => write!(f, "bad state: {m}"),
            Error::NotFound => write!(f, "unknown handle"),
            Error::Full => write!(f, "queue full"),
            Error::Ticket => write!(f, "ticket did not parse"),
            Error::Store(m) => write!(f, "store: {m}"),
            Error::Net(m) => write!(f, "network: {m}"),
            Error::TooLarge => write!(f, "payload too large"),
            Error::Internal(m) => write!(f, "internal: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Opaque handle types. Zero is never valid.
pub type RoomHandle = u64;
pub type ConnHandle = u64;
pub type BlobHandle = u64;

/// Build string reported through the C ABI, so a plugin can log exactly what it
/// loaded.
pub fn build_info() -> &'static str {
    concat!(
        "linkpearl ",
        env!("CARGO_PKG_VERSION"),
        " (iroh 1.2, iroh-gossip 0.101, iroh-blobs 0.103)"
    )
}
