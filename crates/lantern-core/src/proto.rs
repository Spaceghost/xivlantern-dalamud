//! The wire format inside a room.
//!
//! Gossip tells you which neighbour handed you a message, not who wrote it, so
//! every frame is signed by its author and verified on arrival. An unsigned or
//! badly signed frame is dropped without reaching the caller — a plugin should
//! never have to think about spoofing.

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use chacha20poly1305::{aead::{Aead, Payload}, KeyInit, XChaCha20Poly1305, XNonce};

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
    /// A separately signed, room-bound history record. Append-only wire variant.
    HistoryRecord(Vec<u8>),
    HistoryRequest {
        topic: [u8; 32],
        nonce: [u8; 16],
        attempt: u8,
    },
    HistoryOffer {
        topic: [u8; 32],
        to: [u8; 32],
        nonce: [u8; 16],
        attempt: u8,
        hash: [u8; 32],
        size: u64,
        provider: iroh::EndpointAddr,
    },
    /// Only accepted inside a sealed envelope. Size is plaintext; the blob
    /// hash and encrypted_size name the separately encrypted file bytes.
    SealedBlobOffer {
        hash: [u8; 32],
        size: u64,
        encrypted_size: u64,
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    version: u16,
    from: PublicKey,
    body: Vec<u8>,
    sig: Vec<u8>,
}

const LIVE_HISTORY_MAGIC: &[u8; 8] = b"LTHLIVE1";
const SEALED_MAGIC: &[u8; 8] = b"LTSEAL01";
const SEALED_NONCE_LEN: usize = 24;

fn sealed_context(domain: &[u8; 8], topic: &[u8; 32], sender: PublicKey) -> [u8; 72] {
    let mut context = [0u8; 72];
    context[..8].copy_from_slice(domain);
    context[8..40].copy_from_slice(topic);
    context[40..].copy_from_slice(sender.as_bytes());
    context
}

/// Encrypt the entire frame, then sign the ciphertext. Plain-room decoders
/// reject this body. This codec does not enable sealed Node rooms by itself.
pub fn encode_sealed(
    secret: &SecretKey,
    ticket: &crate::ticket::SealedRoomTicket,
    frame: &Frame,
) -> crate::Result<Vec<u8>> {
    let plaintext = postcard::to_stdvec(frame).map_err(|_| crate::Error::Arg("frame"))?;
    seal_payload(secret, ticket, SEALED_MAGIC, &plaintext, crate::GOSSIP_MAX_FRAME - 256)
}

/// Internal shared codec: each content kind must use a distinct domain.
pub(crate) fn seal_payload(
    secret: &SecretKey,
    ticket: &crate::ticket::SealedRoomTicket,
    domain: &[u8; 8],
    plaintext: &[u8],
    max_plaintext: usize,
) -> crate::Result<Vec<u8>> {
    if plaintext.len() > max_plaintext {
        return Err(crate::Error::TooLarge);
    }
    let mut nonce = [0u8; SEALED_NONCE_LEN];
    getrandom::fill(&mut nonce)
        .map_err(|_| crate::Error::State("OS random source unavailable"))?;
    let context = sealed_context(domain, ticket.topic.as_bytes(), secret.public());
    let ciphertext = XChaCha20Poly1305::new(ticket.key().into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad: &context })
        .map_err(|_| crate::Error::Internal("sealed encryption failed".into()))?;
    let mut body = Vec::with_capacity(SEALED_MAGIC.len() + nonce.len() + ciphertext.len());
    body.extend_from_slice(domain);
    body.extend_from_slice(&nonce);
    body.extend_from_slice(&ciphertext);
    Ok(encode_body(secret, body))
}

/// Verify the author's signature before decryption, and require an exact frame.
/// A valid signature cannot relocate ciphertext to a different room or sender.
/// Replay suppression remains the room/history layer's responsibility.
pub fn decode_sealed(
    ticket: &crate::ticket::SealedRoomTicket,
    bytes: &[u8],
) -> Option<(PublicKey, Frame)> {
    let (from, plaintext) = open_payload(ticket, bytes, SEALED_MAGIC, crate::GOSSIP_MAX_FRAME - 256)?;
    let (frame, trailing): (Frame, &[u8]) = postcard::take_from_bytes(&plaintext).ok()?;
    if !trailing.is_empty() {
        return None;
    }
    Some((from, frame))
}

pub(crate) fn open_payload(
    ticket: &crate::ticket::SealedRoomTicket,
    bytes: &[u8],
    domain: &[u8; 8],
    max_plaintext: usize,
) -> Option<(PublicKey, Vec<u8>)> {
    if bytes.len() > max_plaintext.checked_add(256)? {
        return None;
    }
    let (env, trailing): (Envelope, &[u8]) = postcard::take_from_bytes(bytes).ok()?;
    if !trailing.is_empty() || env.version != PROTOCOL_VERSION || env.sig.len() != 64 {
        return None;
    }
    let sig = Signature::from_bytes(&env.sig.as_slice().try_into().ok()?);
    env.from.verify(&env.body, &sig).ok()?;
    let sealed = env.body.strip_prefix(domain)?;
    let nonce = sealed.get(..SEALED_NONCE_LEN)?;
    let ciphertext = sealed.get(SEALED_NONCE_LEN..)?;
    let context = sealed_context(domain, ticket.topic.as_bytes(), env.from);
    let plaintext = XChaCha20Poly1305::new(ticket.key().into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad: &context }).ok()?;
    if plaintext.len() > max_plaintext {
        return None;
    }
    Some((env.from, plaintext))
}

/// The legacy decoder reads App and ignores its signed trailing bytes. Keeping
/// the unique record inside the SAME signed body preserves plain text and avoids
/// gossip collapsing repeated text, without duplicating live frames.
pub(crate) fn encode_history(
    secret: &SecretKey,
    room: &[u8; 32],
    encoded: &[u8],
) -> Option<Vec<u8>> {
    let record = crate::channel_history::verify_sender(room, secret.public(), encoded)?;
    let mut body = postcard::to_stdvec(&Frame::App(record.text().as_bytes().to_vec())).ok()?;
    body.extend_from_slice(LIVE_HISTORY_MAGIC);
    body.extend_from_slice(encoded);
    Some(encode_body(secret, body))
}

/// Sign and encode a frame for broadcast.
pub fn encode(secret: &SecretKey, frame: &Frame) -> Vec<u8> {
    let body = postcard::to_stdvec(frame).expect("postcard::to_stdvec is infallible for Frame");
    encode_body(secret, body)
}

/// Signed trailing entropy prevents gossip collapsing a file reoffer or a
/// second withdrawal. Existing decoders consume the original frame and ignore
/// these trailing bytes, which remain covered by the envelope signature.
pub(crate) fn encode_file_offer(secret: &SecretKey, frame: &Frame) -> Option<Vec<u8>> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).ok()?;
    let mut body = postcard::to_stdvec(frame).ok()?;
    body.extend_from_slice(b"LTFNON01");
    body.extend_from_slice(&nonce);
    Some(encode_body(secret, body))
}

fn encode_body(secret: &SecretKey, body: Vec<u8>) -> Vec<u8> {
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
    let (frame, trailing): (Frame, &[u8]) = postcard::take_from_bytes(&env.body).ok()?;
    if let Frame::App(ref text) = frame {
        if trailing.starts_with(LIVE_HISTORY_MAGIC) {
            let encoded = &trailing[LIVE_HISTORY_MAGIC.len()..];
            let room: [u8; 32] = encoded.get(40..72)?.try_into().ok()?;
            let record = crate::channel_history::verify_sender(&room, env.from, encoded)?;
            if record.text().as_bytes() != text {
                return None;
            }
            return Some((env.from, Frame::HistoryRecord(encoded.to_vec())));
        }
    }
    Some((env.from, frame))
}

#[cfg(test)]
mod tests {
    #[test]
    fn sealed_frames_hide_payloads_and_reject_plain_packets() {
        let secret = SecretKey::generate();
        let ticket = crate::ticket::SealedRoomTicket::new("Friends", vec![]).unwrap();
        let frames = [
            Frame::App(b"unique private message".to_vec()),
            Frame::Presence(b"private presence".to_vec()),
            Frame::Hello { presence: b"private hello".to_vec() },
            Frame::BlobOffer { hash: [1; 32], size: 321, name: "private screenshot.png".into() },
            Frame::BlobUnoffer { hash: [1; 32] },
            Frame::SealedBlobOffer { hash: [2; 32], size: 123, encrypted_size: 256, name: "private.png".into() },
            Frame::HistoryRecord(b"private signed history".to_vec()),
            Frame::HistoryRequest { topic: *ticket.topic.as_bytes(), nonce: [2; 16], attempt: 1 },
            Frame::HistoryOffer { topic: *ticket.topic.as_bytes(), to: [3; 32], nonce: [2; 16],
                attempt: 1, hash: [4; 32], size: 456,
                provider: iroh::EndpointAddr::from(secret.public()) },
        ];
        for frame in frames {
            let packet = encode_sealed(&secret, &ticket, &frame).unwrap();
            assert_eq!(decode_sealed(&ticket, &packet), Some((secret.public(), frame.clone())));
            assert!(decode(&packet).is_none(), "plain decoder must refuse ciphertext");
            assert!(decode_sealed(&ticket, &encode(&secret, &frame)).is_none());
            let env: Envelope = postcard::from_bytes(&packet).unwrap();
            let plain = postcard::to_stdvec(&frame).unwrap();
            if plain.len() > 16 {
                assert!(!env.body.windows(plain.len()).any(|window| window == plain));
            }
        }
        let frame = Frame::App(vec![7; crate::MAX_ROOM_MESSAGE]);
        assert!(encode_sealed(&secret, &ticket, &frame).unwrap().len() <= crate::GOSSIP_MAX_FRAME);
        assert_eq!(encode_sealed(&secret, &ticket, &Frame::App(vec![0; crate::GOSSIP_MAX_FRAME])),
            Err(crate::Error::TooLarge));
    }

    #[test]
    fn sealed_packets_bind_topic_sender_and_key_and_authenticate_every_byte() {
        let secret = SecretKey::generate();
        let ticket = crate::ticket::SealedRoomTicket::new("Friends", vec![]).unwrap();
        let other = crate::ticket::SealedRoomTicket::new("Friends", vec![]).unwrap();
        let frame = Frame::App(b"secret".to_vec());
        let packet = encode_sealed(&secret, &ticket, &frame).unwrap();
        assert_ne!(packet, encode_sealed(&secret, &ticket, &frame).unwrap());
        assert!(decode_sealed(&other, &packet).is_none());
        let mut moved = ticket.clone();
        moved.topic = other.topic;
        assert!(decode_sealed(&moved, &packet).is_none(), "same key, wrong topic");
        let mut wrong_key = other;
        wrong_key.topic = ticket.topic;
        assert!(decode_sealed(&wrong_key, &packet).is_none(), "same topic, wrong key");
        let env: Envelope = postcard::from_bytes(&packet).unwrap();
        let impostor = SecretKey::generate();
        assert!(decode_sealed(&ticket, &encode_body(&impostor, env.body.clone())).is_none());
        for index in 0..env.body.len() {
            let mut changed = env.body.clone();
            changed[index] ^= 1;
            // Even a ticket holder who re-signs a changed ciphertext cannot
            // authenticate it without recomputing its AEAD tag.
            assert!(decode_sealed(&ticket, &encode_body(&secret, changed)).is_none());
        }
        for end in 0..packet.len() {
            assert!(decode_sealed(&ticket, &packet[..end]).is_none());
        }
        let mut extended = packet.clone();
        extended.push(0);
        assert!(decode_sealed(&ticket, &extended).is_none());
        let mut bad_signature: Envelope = postcard::from_bytes(&packet).unwrap();
        bad_signature.sig[0] ^= 1;
        assert!(decode_sealed(&ticket, &postcard::to_stdvec(&bad_signature).unwrap()).is_none());
    }

    #[test]
    fn repeated_file_packets_stay_distinct_and_keep_the_legacy_frame_shape() {
        let secret = SecretKey::generate();
        for frame in [Frame::BlobOffer { hash: [7; 32], size: 123, name: "clear.png".into() },
            Frame::BlobUnoffer { hash: [7; 32] }] {
            let first = encode_file_offer(&secret, &frame).unwrap();
            let second = encode_file_offer(&secret, &frame).unwrap();
            assert_ne!(first, second);
            assert_eq!(decode(&first).unwrap(), (secret.public(), frame.clone()));
            let envelope: Envelope = postcard::from_bytes(&first).unwrap();
            let legacy: Frame = postcard::from_bytes(&envelope.body).unwrap();
            assert_eq!(legacy, frame);
            let mut changed = envelope;
            *changed.body.last_mut().unwrap() ^= 1;
            assert!(decode(&postcard::to_stdvec(&changed).unwrap()).is_none(), "suffix remains signed");
        }
    }
    use super::*;

    #[test]
    fn history_appends_without_changing_existing_wire_indexes() {
        let frames = [
            Frame::App(vec![]),
            Frame::Presence(vec![]),
            Frame::BlobOffer {
                hash: [0; 32],
                size: 0,
                name: String::new(),
            },
            Frame::BlobUnoffer { hash: [0; 32] },
            Frame::Hello { presence: vec![] },
            Frame::HistoryRecord(vec![1, 2, 3]),
            Frame::HistoryRequest {
                topic: [8; 32],
                nonce: [1; 16],
                attempt: 0,
            },
            Frame::HistoryOffer {
                topic: [8; 32],
                to: [2; 32],
                nonce: [1; 16],
                attempt: 1,
                hash: [3; 32],
                size: 42,
                provider: iroh::EndpointAddr::from(SecretKey::generate().public()),
            },
        ];
        let key = SecretKey::generate();
        assert_eq!(postcard::to_stdvec(&Frame::SealedBlobOffer {
            hash: [0; 32], size: 0, encrypted_size: 130, name: "empty".into(),
        }).unwrap()[0], 8, "sealed files append after all existing variants");
        for (index, frame) in frames.iter().enumerate() {
            assert_eq!(postcard::to_stdvec(frame).unwrap()[0], index as u8);
            assert_eq!(decode(&encode(&key, frame)).unwrap().1, *frame);
        }
    }

    #[test]
    fn history_rejects_a_valid_record_forwarded_as_another_authors_live_message() {
        let author = SecretKey::generate();
        let forwarder = SecretKey::generate();
        let room = [7; 32];
        let raw = crate::channel_history::sign(&author, room, 1, "hello").unwrap();
        for key in [&author, &forwarder] {
            let (from, frame) = decode(&encode(key, &Frame::HistoryRecord(raw.clone()))).unwrap();
            let Frame::HistoryRecord(bytes) = frame else {
                panic!("wrong frame")
            };
            assert_eq!(
                crate::channel_history::verify_sender(&room, from, &bytes).is_some(),
                key.public() == author.public()
            );
            assert!(crate::channel_history::verify_sender(&[8; 32], from, &bytes).is_none());
        }
    }

    #[test]
    fn signed_history_tail_is_plain_app_to_the_legacy_decoder() {
        // Independent pre-history enum and the exact original decoding behavior.
        #[allow(dead_code)]
        #[derive(Deserialize)]
        enum LegacyFrame {
            App(Vec<u8>),
            Presence(Vec<u8>),
            BlobOffer {
                hash: [u8; 32],
                size: u64,
                name: String,
            },
            BlobUnoffer {
                hash: [u8; 32],
            },
            Hello {
                presence: Vec<u8>,
            },
        }
        let author = SecretKey::generate();
        let room = [9; 32];
        let records: Vec<_> = (0..2)
            .map(|_| crate::channel_history::sign(&author, room, 42, "same text").unwrap())
            .collect();
        let packets: Vec<_> = records
            .iter()
            .map(|raw| encode_history(&author, &room, raw).unwrap())
            .collect();
        assert_ne!(
            packets[0], packets[1],
            "gossip must not collapse repeated text"
        );
        for (raw, packet) in records.iter().zip(&packets) {
            assert!(packet.len() < crate::GOSSIP_MAX_FRAME);
            let env: Envelope = postcard::from_bytes(packet).unwrap();
            env.from
                .verify(
                    &env.body,
                    &Signature::from_bytes(&env.sig.clone().try_into().unwrap()),
                )
                .unwrap();
            let LegacyFrame::App(text) = postcard::from_bytes::<LegacyFrame>(&env.body).unwrap()
            else {
                panic!("not legacy App")
            };
            assert_eq!(text, b"same text");
            assert_eq!(
                decode(packet).unwrap(),
                (author.public(), Frame::HistoryRecord(raw.clone()))
            );
        }
        let mut body = postcard::to_stdvec(&Frame::App(b"different text".to_vec())).unwrap();
        body.extend_from_slice(LIVE_HISTORY_MAGIC);
        body.extend_from_slice(&records[0]);
        assert!(
            decode(&encode_body(&author, body.clone())).is_none(),
            "legacy and new text must agree"
        );
        let forwarder = SecretKey::generate();
        assert!(encode_history(&forwarder, &room, &records[0]).is_none());
        assert!(decode(&encode_body(&forwarder, body)).is_none());
        assert!(encode_history(&author, &[8; 32], &records[0]).is_none());
        let mut env: Envelope = postcard::from_bytes(&packets[0]).unwrap();
        *env.body.last_mut().unwrap() ^= 1;
        assert!(
            decode(&postcard::to_stdvec(&env).unwrap()).is_none(),
            "tail is covered by outer signature"
        );
    }

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
