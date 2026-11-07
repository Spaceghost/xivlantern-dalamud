//! Bounded encrypted file bytes; separate domain from chat and history.
use crate::{ticket::SealedRoomTicket, Error, Result};
use iroh::{PublicKey, SecretKey};

const DOMAIN: &[u8; 8] = b"LTFSEAL1";
pub(crate) const MAX_CIPHERTEXT_BYTES: u64 = crate::bounded_blob::MAX_FILE_BYTES + 256;

pub(crate) fn encode(secret: &SecretKey, ticket: &SealedRoomTicket, bytes: &[u8]) -> Result<Vec<u8>> {
    crate::proto::seal_payload(secret, ticket, DOMAIN, bytes, crate::bounded_blob::MAX_FILE_BYTES as usize)
}

pub(crate) fn decode(ticket: &SealedRoomTicket, provider: PublicKey, bytes: &[u8], size: u64) -> Result<Vec<u8>> {
    if size > crate::bounded_blob::MAX_FILE_BYTES { return Err(Error::TooLarge); }
    let (from, plaintext) = crate::proto::open_payload(ticket, bytes, DOMAIN,
        crate::bounded_blob::MAX_FILE_BYTES as usize)
        .ok_or(Error::State("sealed file authentication failed"))?;
    if from != provider || plaintext.len() as u64 != size {
        return Err(Error::State("sealed file provider or size does not match offer"));
    }
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_ciphertext_binds_provider_room_key_size_and_content_kind() {
        let provider = SecretKey::generate();
        let ticket = SealedRoomTicket::new("Files", vec![]).unwrap();
        let original = b"private screenshot bytes";
        let encrypted = encode(&provider, &ticket, original).unwrap();
        assert_eq!(decode(&ticket, provider.public(), &encrypted, original.len() as u64).unwrap(), original);
        assert!(decode(&ticket, SecretKey::generate().public(), &encrypted, original.len() as u64).is_err());
        assert!(decode(&ticket, provider.public(), &encrypted, original.len() as u64 + 1).is_err());
        assert!(decode(&ticket, provider.public(), original, original.len() as u64).is_err());
        assert!(crate::proto::decode_sealed(&ticket, &encrypted).is_none());
        assert!(crate::channel_history::read_sealed_snapshot(&ticket, provider.public(), &encrypted).is_none());
        let mut other = SealedRoomTicket::new("Other", vec![]).unwrap();
        other.topic = ticket.topic;
        assert!(decode(&other, provider.public(), &encrypted, original.len() as u64).is_err());
        for i in 0..encrypted.len() {
            let mut bad = encrypted.clone(); bad[i] ^= 1;
            assert!(decode(&ticket, provider.public(), &bad, original.len() as u64).is_err());
        }
        for len in 0..encrypted.len() {
            assert!(decode(&ticket, provider.public(), &encrypted[..len], original.len() as u64).is_err());
        }
        let maximum = vec![7; crate::bounded_blob::MAX_FILE_BYTES as usize];
        let ciphertext = encode(&provider, &ticket, &maximum).unwrap();
        assert!(ciphertext.len() as u64 <= MAX_CIPHERTEXT_BYTES);
        assert_eq!(decode(&ticket, provider.public(), &ciphertext, maximum.len() as u64).unwrap(), maximum);
        assert_eq!(encode(&provider, &ticket, &vec![0; maximum.len() + 1]), Err(Error::TooLarge));
    }
}
