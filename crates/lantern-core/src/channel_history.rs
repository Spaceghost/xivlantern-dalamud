//! Signed catch-up records and bounded snapshot fetches.
//! Automatic offers/fetch on join and plugin history delivery are not yet wired.
//! Each record binds the room, author, random id, claimed sender time and text.
//! Sender clocks are not authoritative. Copies retained by other members cannot
//! be deleted remotely. Snapshot publishers cannot reattribute or edit a line.

pub(crate) mod network;

use crate::{Error, Result, MAX_TEXT};
use iroh::{PublicKey, SecretKey, Signature};
use std::collections::HashSet;

const RECORD_MAGIC: &[u8; 8] = b"LTHREC01";
const SNAPSHOT_MAGIC: &[u8; 8] = b"LTHSNAP1";
const DOMAIN: &[u8] = b"XivLantern channel history v1\0";
const HEADER: usize = 98;
const SIGNATURE: usize = 64;
pub(crate) const SNAPSHOT_HEADER: usize = 42;
pub const MAX_RECORD_BYTES: usize = HEADER + MAX_TEXT + SIGNATURE;
pub const MAX_RECORDS: usize = 256;
pub const MAX_SNAPSHOT_BYTES: usize = SNAPSHOT_HEADER + MAX_RECORDS * (2 + MAX_RECORD_BYTES);
/// Includes the encrypted snapshot's nonce, authentication tag and signature.
pub const MAX_SEALED_SNAPSHOT_BYTES: usize = MAX_SNAPSHOT_BYTES + 256;
const SEALED_SNAPSHOT_DOMAIN: &[u8; 8] = b"LTHSEAL1";

/// Serve ciphertext rather than the plaintext history blob. The provider signs
/// the encrypted snapshot; each original record retains its author's signature.
pub fn sealed_snapshot(
    provider: &SecretKey,
    ticket: &crate::ticket::SealedRoomTicket,
    records: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let plaintext = snapshot(*ticket.topic.as_bytes(), records)?;
    crate::proto::seal_payload(provider, ticket, SEALED_SNAPSHOT_DOMAIN, &plaintext, MAX_SNAPSHOT_BYTES)
}

/// The expected provider comes from the authenticated history offer. A channel
/// member cannot substitute another provider's snapshot signature.
pub fn read_sealed_snapshot(
    ticket: &crate::ticket::SealedRoomTicket,
    provider: PublicKey,
    bytes: &[u8],
) -> Option<Vec<HistoryRecord>> {
    let (from, plaintext) = crate::proto::open_payload(ticket, bytes, SEALED_SNAPSHOT_DOMAIN, MAX_SNAPSHOT_BYTES)?;
    if from != provider {
        return None;
    }
    read_snapshot(ticket.topic.as_bytes(), &plaintext)
}

#[derive(Debug, Clone)]
pub struct HistoryRecord {
    author: PublicKey,
    room: [u8; 32],
    id: [u8; 16],
    sent_at: i64,
    text: String,
    signature: [u8; 64],
    encoded: Vec<u8>,
}

impl HistoryRecord {
    pub fn author(&self) -> PublicKey {
        self.author
    }
    pub fn room(&self) -> &[u8; 32] {
        &self.room
    }
    pub fn id(&self) -> &[u8; 16] {
        &self.id
    }
    pub fn sent_at(&self) -> i64 {
        self.sent_at
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn signature(&self) -> &[u8; 64] {
        &self.signature
    }
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }
}

pub(crate) fn valid_text(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TEXT && !text.chars().any(|c| c.is_control() && c != '\n')
}

fn signed_bytes(body: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DOMAIN.len() + body.len());
    bytes.extend_from_slice(DOMAIN);
    bytes.extend_from_slice(body);
    bytes
}

/// Only caller-provided chat text and a claimed clock time; never game state.
pub fn sign(secret: &SecretKey, room: [u8; 32], sent_at: i64, text: &str) -> Result<Vec<u8>> {
    if !valid_text(text) || sent_at < 0 {
        return Err(Error::Arg("invalid history text or timestamp"));
    }
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|_| Error::State("OS random source unavailable"))?;
    let mut bytes = Vec::with_capacity(HEADER + text.len() + SIGNATURE);
    bytes.extend_from_slice(RECORD_MAGIC);
    bytes.extend_from_slice(secret.public().as_bytes());
    bytes.extend_from_slice(&room);
    bytes.extend_from_slice(&id);
    bytes.extend_from_slice(&sent_at.to_be_bytes());
    bytes.extend_from_slice(&(text.len() as u16).to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(&secret.sign(&signed_bytes(&bytes)).to_bytes());
    Ok(bytes)
}

/// Require the live envelope sender to be the original record author.
pub(crate) fn verify_sender(room: &[u8; 32], sender: PublicKey, bytes: &[u8]) -> Option<HistoryRecord> {
    let record = verify(room, bytes)?;
    (record.author() == sender).then_some(record)
}

/// Verify before allocating text or exposing it. No trailing bytes accepted.
pub fn verify(expected_room: &[u8; 32], bytes: &[u8]) -> Option<HistoryRecord> {
    if bytes.len() < HEADER + SIGNATURE
        || bytes.len() > MAX_RECORD_BYTES
        || &bytes[..8] != RECORD_MAGIC
    {
        return None;
    }
    let room: [u8; 32] = bytes[40..72].try_into().ok()?;
    if &room != expected_room {
        return None;
    }
    let text_len = u16::from_be_bytes(bytes[96..98].try_into().ok()?) as usize;
    if bytes.len() != HEADER + text_len + SIGNATURE {
        return None;
    }
    let sent_at = i64::from_be_bytes(bytes[88..96].try_into().ok()?);
    let text = std::str::from_utf8(&bytes[HEADER..HEADER + text_len]).ok()?;
    if sent_at < 0 || !valid_text(text) {
        return None;
    }
    let author = PublicKey::from_bytes(&bytes[8..40].try_into().ok()?).ok()?;
    let signature: [u8; 64] = bytes[HEADER + text_len..].try_into().ok()?;
    author
        .verify(
            &signed_bytes(&bytes[..HEADER + text_len]),
            &Signature::from_bytes(&signature),
        )
        .ok()?;
    Some(HistoryRecord {
        author,
        room,
        id: bytes[72..88].try_into().ok()?,
        sent_at,
        text: text.into(),
        signature,
        encoded: bytes.into(),
    })
}

fn sort(records: &mut [HistoryRecord]) {
    records.sort_by(|a, b| {
        a.sent_at
            .cmp(&b.sent_at)
            .then(a.author.as_bytes().cmp(b.author.as_bytes()))
            .then(a.id.cmp(&b.id))
            .then(a.signature.cmp(&b.signature))
    });
}

/// Any member can forward these original signed bytes without author keys.
pub fn snapshot(room: [u8; 32], records: &[Vec<u8>]) -> Result<Vec<u8>> {
    if records.len() > MAX_RECORDS {
        return Err(Error::TooLarge);
    }
    let mut seen = HashSet::new();
    let mut valid: Vec<_> = records
        .iter()
        .filter_map(|r| verify(&room, r))
        .filter(|r| seen.insert(r.signature))
        .collect();
    sort(&mut valid);
    let mut out = Vec::new();
    out.extend_from_slice(SNAPSHOT_MAGIC);
    out.extend_from_slice(&room);
    out.extend_from_slice(&(valid.len() as u16).to_be_bytes());
    for record in valid {
        out.extend_from_slice(&(record.encoded.len() as u16).to_be_bytes());
        out.extend_from_slice(&record.encoded);
    }
    Ok(out)
}

/// Bound all counts/lengths before allocation; junk entries are individually
/// dropped, while malformed framing rejects the snapshot. Reverify every line.
pub fn read_snapshot(room: &[u8; 32], bytes: &[u8]) -> Option<Vec<HistoryRecord>> {
    if bytes.len() < SNAPSHOT_HEADER
        || bytes.len() > MAX_SNAPSHOT_BYTES
        || &bytes[..8] != SNAPSHOT_MAGIC
        || &bytes[8..40] != room
    {
        return None;
    }
    let count = u16::from_be_bytes(bytes[40..42].try_into().ok()?) as usize;
    if count > MAX_RECORDS {
        return None;
    }
    let mut pos = SNAPSHOT_HEADER;
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for _ in 0..count {
        let length = u16::from_be_bytes(bytes.get(pos..pos + 2)?.try_into().ok()?) as usize;
        pos += 2;
        if length > MAX_RECORD_BYTES {
            return None;
        }
        let raw = bytes.get(pos..pos + length)?;
        pos += length;
        if let Some(record) = verify(room, raw) {
            if seen.insert(record.signature) {
                records.push(record);
            }
        }
    }
    if pos != bytes.len() {
        return None;
    }
    sort(&mut records);
    Some(records)
}

#[cfg(test)]
mod tests {
    #[test]
    fn sealed_snapshots_preserve_original_authors_and_reject_downgrades() {
        let provider = SecretKey::generate();
        let author = SecretKey::generate();
        let ticket = crate::ticket::SealedRoomTicket::new("Friends", vec![]).unwrap();
        let original = sign(&author, *ticket.topic.as_bytes(), 42, "private history line").unwrap();
        let encrypted = sealed_snapshot(&provider, &ticket, &[original.clone()]).unwrap();
        assert!(!encrypted.windows(b"private history line".len())
            .any(|window| window == b"private history line"));
        let decoded = read_sealed_snapshot(&ticket, provider.public(), &encrypted).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].author(), author.public());
        assert_eq!(decoded[0].encoded(), original);
        assert!(read_snapshot(ticket.topic.as_bytes(), &encrypted).is_none());
        let plain = snapshot(*ticket.topic.as_bytes(), &[original.clone()]).unwrap();
        assert!(read_sealed_snapshot(&ticket, provider.public(), &plain).is_none());
        assert!(crate::proto::decode_sealed(&ticket, &encrypted).is_none());
        let frame = crate::proto::encode_sealed(&provider, &ticket,
            &crate::proto::Frame::App(plain)).unwrap();
        assert!(read_sealed_snapshot(&ticket, provider.public(), &frame).is_none());
        assert!(read_sealed_snapshot(&ticket, author.public(), &encrypted).is_none());
        let resealed = crate::ticket::SealedRoomTicket::new("Friends", vec![]).unwrap();
        assert!(read_sealed_snapshot(&resealed, provider.public(), &encrypted).is_none());
        // Resealing cannot revoke the old ticket's access to old ciphertext.
        assert!(read_sealed_snapshot(&ticket, provider.public(), &encrypted).is_some());
        assert_ne!(encrypted, sealed_snapshot(&provider, &ticket, &[original]).unwrap());
        for end in 0..encrypted.len() {
            assert!(read_sealed_snapshot(&ticket, provider.public(), &encrypted[..end]).is_none());
        }
        for index in 0..encrypted.len() {
            let mut altered = encrypted.clone();
            altered[index] ^= 1;
            assert!(read_sealed_snapshot(&ticket, provider.public(), &altered).is_none());
        }
        let mut extra = encrypted;
        extra.push(0);
        assert!(read_sealed_snapshot(&ticket, provider.public(), &extra).is_none());
    }

    #[test]
    fn sealed_snapshot_maximum_and_inner_room_binding_are_enforced() {
        let provider = SecretKey::generate();
        let ticket = crate::ticket::SealedRoomTicket::new("Friends", vec![]).unwrap();
        let records: Vec<_> = (0..MAX_RECORDS)
            .map(|_| sign(&provider, *ticket.topic.as_bytes(), 42, &"a".repeat(MAX_TEXT)).unwrap())
            .collect();
        let encrypted = sealed_snapshot(&provider, &ticket, &records).unwrap();
        assert!(encrypted.len() <= MAX_SEALED_SNAPSHOT_BYTES);
        let decoded = read_sealed_snapshot(&ticket, provider.public(), &encrypted).unwrap();
        assert_eq!(decoded.len(), MAX_RECORDS);
        assert_eq!(decoded[0].text().len(), MAX_TEXT);
        assert!(sealed_snapshot(&provider, &ticket, &vec![vec![]; MAX_RECORDS + 1]).is_err());
        assert!(read_sealed_snapshot(&ticket, provider.public(),
            &vec![0; MAX_SEALED_SNAPSHOT_BYTES + 1]).is_none());
        let wrong_room = snapshot([9; 32], &[]).unwrap();
        let authenticated = crate::proto::seal_payload(&provider, &ticket,
            SEALED_SNAPSHOT_DOMAIN, &wrong_room, MAX_SNAPSHOT_BYTES).unwrap();
        assert!(read_sealed_snapshot(&ticket, provider.public(), &authenticated).is_none());
    }

    use super::*;

    #[test]
    fn signed_record_binds_author_room_time_id_and_text() {
        let key = SecretKey::generate();
        let raw = sign(&key, [3; 32], 42, "maps\nat 21:00").unwrap();
        let record = verify(&[3; 32], &raw).unwrap();
        assert_eq!(record.author(), key.public());
        assert_eq!(record.room(), &[3; 32]);
        assert_eq!(record.sent_at(), 42);
        assert_eq!(record.text(), "maps\nat 21:00");
        assert_eq!(record.encoded(), raw);
        assert!(verify(&[4; 32], &raw).is_none());
        for offset in [8, 40, 72, 88, 98, raw.len() - 1] {
            let mut changed = raw.clone();
            changed[offset] ^= 1;
            assert!(
                verify(&[3; 32], &changed).is_none(),
                "field offset {offset}"
            );
        }
    }

    #[test]
    fn repeated_text_has_distinct_signed_identity() {
        let key = SecretKey::generate();
        let a = verify(&[0; 32], &sign(&key, [0; 32], 42, "ready").unwrap()).unwrap();
        let b = verify(&[0; 32], &sign(&key, [0; 32], 42, "ready").unwrap()).unwrap();
        assert_ne!(a.id(), b.id());
        assert_ne!(a.signature(), b.signature());
    }

    #[test]
    fn invalid_text_time_lengths_and_framing_are_rejected() {
        let key = SecretKey::generate();
        for text in ["", "bad\0text", "escape\u{1b}", &"a".repeat(MAX_TEXT + 1)] {
            assert!(sign(&key, [0; 32], 42, text).is_err());
        }
        assert!(sign(&key, [0; 32], -1, "text").is_err());
        let raw = sign(&key, [0; 32], 42, &"a".repeat(MAX_TEXT)).unwrap();
        assert_eq!(raw.len(), MAX_RECORD_BYTES);
        assert!(verify(&[0; 32], &raw).is_some());
        for length in 0..raw.len() {
            assert!(verify(&[0; 32], &raw[..length]).is_none());
        }
        let mut extra = raw;
        extra.push(0);
        assert!(verify(&[0; 32], &extra).is_none());
    }

    #[test]
    fn snapshots_preserve_signatures_deduplicate_and_sort_claimed_times() {
        let key = SecretKey::generate();
        let late = sign(&key, [1; 32], 200, "later").unwrap();
        let early = sign(&key, [1; 32], 100, "earlier").unwrap();
        let wrong_room = sign(&key, [2; 32], 50, "other room").unwrap();
        let raw = snapshot(
            [1; 32],
            &[late.clone(), early.clone(), late, wrong_room, vec![0; 10]],
        )
        .unwrap();
        let records = read_snapshot(&[1; 32], &raw).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].text(), "earlier");
        assert_eq!(records[0].encoded(), early);
        assert_eq!(records[1].text(), "later");
        assert!(read_snapshot(&[2; 32], &raw).is_none());
    }

    #[test]
    fn inbound_junk_is_dropped_without_losing_valid_lines() {
        let key = SecretKey::generate();
        let valid = sign(&key, [1; 32], 42, "verified").unwrap();
        let mut bytes = SNAPSHOT_MAGIC.to_vec();
        bytes.extend_from_slice(&[1; 32]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        for item in [vec![0; 10], valid.clone(), valid] {
            bytes.extend_from_slice(&(item.len() as u16).to_be_bytes());
            bytes.extend_from_slice(&item);
        }
        assert_eq!(read_snapshot(&[1; 32], &bytes).unwrap().len(), 1);
    }

    #[test]
    fn snapshot_count_size_version_and_trailing_bytes_are_bounded() {
        assert!(snapshot([0; 32], &vec![vec![]; MAX_RECORDS + 1]).is_err());
        let mut bytes = snapshot([0; 32], &[]).unwrap();
        assert!(read_snapshot(&[0; 32], &bytes).unwrap().is_empty());
        bytes.push(0);
        assert!(read_snapshot(&[0; 32], &bytes).is_none());
        bytes.pop();
        bytes[7] ^= 1;
        assert!(read_snapshot(&[0; 32], &bytes).is_none());
        bytes[7] ^= 1;
        bytes[40..42].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(read_snapshot(&[0; 32], &bytes).is_none());
        assert!(read_snapshot(&[0; 32], &vec![0; MAX_SNAPSHOT_BYTES + 1]).is_none());
    }
}
