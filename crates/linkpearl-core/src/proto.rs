//! The wire format inside a room.
//!
//! Gossip tells you which neighbour handed you a message, not who wrote it, so
//! every frame is signed by its author and verified on arrival. An unsigned or
//! badly signed frame is dropped without reaching the caller — a plugin should
//! never have to think about spoofing.

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

use crate::PROTOCOL_VERSION;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frame {
    /// Opaque application bytes (chat, position pings, whatever the mod wants).
    App(Vec<u8>),
    /// This peer's current presence payload.
    Presence(Vec<u8>),
    /// "I am serving this, ask me for it." Nothing is pushed.
    BlobOffer {
        hash: [u8; 32],
        size: u64,
        name: String,
    },
    /// "I stopped serving that."
    BlobUnoffer { hash: [u8; 32] },
    /// Sent to a new neighbour so it learns who is already here.
    Hello { presence: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    version: u16,
    from: PublicKey,
    body: Vec<u8>,
    sig: Vec<u8>,
}

/// Sign and encode a frame for broadcast.
pub fn encode(secret: &SecretKey, frame: &Frame) -> Vec<u8> {
    let body = postcard::to_stdvec(frame).expect("postcard::to_stdvec is infallible for Frame");
    let sig = secret.sign(&body);
    let env = Envelope {
        version: PROTOCOL_VERSION,
        from: secret.public(),
        body,
        sig: sig.to_bytes().to_vec(),
    };
    postcard::to_stdvec(&env).expect("postcard::to_stdvec is infallible for Envelope")
}

/// Decode and verify. `None` means "drop this, quietly".
pub fn decode(bytes: &[u8]) -> Option<(PublicKey, Frame)> {
    let env: Envelope = postcard::from_bytes(bytes).ok()?;
    if env.version != PROTOCOL_VERSION {
        return None;
    }
    if env.sig.len() != 64 {
        return None;
    }
    let mut sig_bytes = [0u8; 64];
    sig_bytes.copy_from_slice(&env.sig);
    let sig = Signature::from_bytes(&sig_bytes);
    env.from.verify(&env.body, &sig).ok()?;
    let frame: Frame = postcard::from_bytes(&env.body).ok()?;
    Some((env.from, frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_frames_round_trip() {
        let sk = SecretKey::generate();
        let frame = Frame::App(b"hello linkshell".to_vec());
        let bytes = encode(&sk, &frame);
        let (from, got) = decode(&bytes).expect("our own frame must verify");
        assert_eq!(from, sk.public());
        assert_eq!(got, frame);
    }

    #[test]
    fn a_tampered_body_is_dropped() {
        let sk = SecretKey::generate();
        let mut bytes = encode(&sk, &Frame::App(b"pay me 1 gil".to_vec()));
        // Flip a byte somewhere in the middle: the signature no longer covers it.
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        assert!(decode(&bytes).is_none());
    }

    #[test]
    fn a_frame_reattributed_to_someone_else_is_dropped() {
        let author = SecretKey::generate();
        let victim = SecretKey::generate();
        let bytes = encode(&author, &Frame::App(b"not mine".to_vec()));
        let mut env: Envelope = postcard::from_bytes(&bytes).unwrap();
        env.from = victim.public();
        let forged = postcard::to_stdvec(&env).unwrap();
        assert!(decode(&forged).is_none());
    }

    #[test]
    fn garbage_is_dropped_rather_than_panicking() {
        assert!(decode(&[]).is_none());
        assert!(decode(&[0xff; 64]).is_none());
    }

    #[test]
    fn blob_offers_carry_their_metadata() {
        let sk = SecretKey::generate();
        let frame = Frame::BlobOffer {
            hash: [3u8; 32],
            size: 12345,
            name: "screenshot.png".into(),
        };
        let (_, got) = decode(&encode(&sk, &frame)).unwrap();
        assert_eq!(got, frame);
    }
}
