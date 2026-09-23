//! Call signalling over friend links — a spike (cargo feature `calls`).
//!
//! This is the in-game half of docs/DESIGN.md "Calls and video": ring, answer
//! and hang up, carrying the moq URL where the caller's media sidecar would
//! serve the call. No audio or video moves. Not in the C ABI.

use iroh::EndpointId;

use super::{friends::Wire, Node, Shared};
use crate::{event::Event, event::PeerId, Error, Result};

fn endpoint(peer: &PeerId) -> Result<EndpointId> {
    EndpointId::from_bytes(peer).map_err(|_| Error::Arg("not a node id"))
}

impl Node {
    /// Ring a linked friend. Returns the call id; `CallAnswered` follows.
    pub fn call_ring(&self, peer: &PeerId) -> Result<u64> {
        self.check_open()?;
        let id = endpoint(peer)?;
        if !self.shared.friends.read().unwrap().contains_key(&id) {
            return Err(Error::NotFound);
        }
        if !self.shared.links.lock().unwrap().contains_key(&id) {
            return Err(Error::State("that friend is not connected"));
        }
        let call = loop {
            let c = u64::from_le_bytes(super::friends::random::<8>());
            if c != 0 {
                break c;
            }
        };
        // Where this node's media sidecar would serve the call, in the URL
        // form moq-native accepts for iroh. Only a label until media exists.
        let media = format!(
            "iroh://{}/call/{call:016x}",
            data_encoding::HEXLOWER.encode(&self.node_id())
        );
        self.shared.calls.lock().unwrap().insert(call, id);
        self.send_call(id, Wire::CallRing { call, media })?;
        Ok(call)
    }

    /// Accept or decline a call from `CallIncoming`.
    pub fn call_answer(&self, call: u64, accept: bool) -> Result<()> {
        let peer = self.call_peer(call)?;
        if !accept {
            self.shared.calls.lock().unwrap().remove(&call);
        }
        self.send_call(peer, Wire::CallAnswer { call, accept })
    }

    pub fn call_hangup(&self, call: u64) -> Result<()> {
        let peer = self.call_peer(call)?;
        self.shared.calls.lock().unwrap().remove(&call);
        self.send_call(peer, Wire::CallHangup { call })?;
        self.shared.emit(Event::CallEnded {
            peer: *peer.as_bytes(),
            call,
        });
        Ok(())
    }

    fn call_peer(&self, call: u64) -> Result<EndpointId> {
        self.check_open()?;
        self.shared
            .calls
            .lock()
            .unwrap()
            .get(&call)
            .copied()
            .ok_or(Error::NotFound)
    }

    fn send_call(&self, peer: EndpointId, wire: Wire) -> Result<()> {
        self.push(super::Cmd::Friend(super::friends::FriendCmd::Send { peer, wire }))
    }
}

/// A call frame from a friend. Frames about a call that belongs to somebody
/// else are ignored.
pub(super) fn on_frame(shared: &Shared, peer: EndpointId, wire: Wire) {
    let peer_bytes = *peer.as_bytes();
    let mut calls = shared.calls.lock().unwrap();
    match wire {
        Wire::CallRing { call, media } => {
            if calls.contains_key(&call) {
                return;
            }
            calls.insert(call, peer);
            drop(calls);
            shared.emit(Event::CallIncoming {
                peer: peer_bytes,
                call,
                media,
            });
        }
        Wire::CallAnswer { call, accept } => {
            if calls.get(&call) != Some(&peer) {
                return;
            }
            if !accept {
                calls.remove(&call);
            }
            drop(calls);
            shared.emit(Event::CallAnswered {
                peer: peer_bytes,
                call,
                accepted: accept,
            });
        }
        Wire::CallHangup { call } => {
            if calls.get(&call) != Some(&peer) {
                return;
            }
            calls.remove(&call);
            drop(calls);
            shared.emit(Event::CallEnded {
                peer: peer_bytes,
                call,
            });
        }
        _ => {}
    }
}

/// The link to `peer` is gone: every call with them is over.
pub(super) fn link_lost(shared: &Shared, peer: EndpointId) {
    let ended: Vec<u64> = {
        let mut calls = shared.calls.lock().unwrap();
        let ended: Vec<u64> = calls
            .iter()
            .filter(|(_, p)| **p == peer)
            .map(|(c, _)| *c)
            .collect();
        for c in &ended {
            calls.remove(c);
        }
        ended
    };
    for call in ended {
        shared.emit(Event::CallEnded {
            peer: *peer.as_bytes(),
            call,
        });
    }
}
