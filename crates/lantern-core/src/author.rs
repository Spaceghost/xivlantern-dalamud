//! The author channel: signed announcements from a mod's author.
//!
//! The author holds an ed25519 **author key** on their own machine. It is not
//! a node key and it never ships: a plugin embeds only the public half (and the
//! NodeIds of the author's always-on nodes, for bootstrapping). An
//! announcement is signed offline with `lantern author sign` and pasted into
//! a node that is in the channel; from there it travels as an ordinary room
//! message, and every member re-offers the latest ones to new neighbours. Who
//! relays it does not matter: a member shows an announcement only if the
//! author key's signature over it verifies, so nobody else can speak as the
//! author, and anything in the channel that is not a verified announcement is
//! dropped unseen.
//!
//! The channel's topic is derived from the author key, so every install of the
//! same mod finds the same channel with no directory.

use std::{fmt, path::Path, str::FromStr};

use iroh::{PublicKey, SecretKey, Signature};
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};

use crate::{ticket::Scope, Error, Result, PROTOCOL_VERSION};

pub const ANNOUNCE_PREFIX: &str = "ltannounce";
const SIGN_CONTEXT: &[u8] = b"lantern-announcement/1";
pub const MAX_TITLE: usize = 120;
pub const MAX_BODY: usize = 1500;

/// What the author says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announcement {
    /// Increases with every announcement; a member keeps each (author, seq)
    /// once, so re-offers never notify twice.
    pub seq: u64,
    /// Unix seconds when it was signed.
    pub issued_at: i64,
    pub title: String,
    pub body: String,
}

/// An announcement with the author's signature, in the form that travels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedAnnouncement {
    pub version: u16,
    pub author: [u8; 32],
    /// postcard of [`Announcement`]; the signature covers exactly these bytes.
    pub body: Vec<u8>,
    pub sig: Vec<u8>,
}

fn message(body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGN_CONTEXT.len() + body.len());
    m.extend_from_slice(SIGN_CONTEXT);
    m.extend_from_slice(body);
    m
}

impl SignedAnnouncement {
    pub fn sign(key: &SecretKey, a: &Announcement) -> Result<SignedAnnouncement> {
        if a.title.trim().is_empty() {
            return Err(Error::Arg("an announcement needs a title"));
        }
        if a.title.len() > MAX_TITLE || a.body.len() > MAX_BODY {
            return Err(Error::TooLarge);
        }
        let body = postcard::to_stdvec(a).expect("postcard::to_stdvec is infallible for Announcement");
        let sig = key.sign(&message(&body));
        Ok(SignedAnnouncement {
            version: PROTOCOL_VERSION,
            author: *key.public().as_bytes(),
            body,
            sig: sig.to_bytes().to_vec(),
        })
    }

    /// The announcement, only if it is signed by `author` and well formed.
    pub fn verify(&self, author: &[u8; 32]) -> Option<Announcement> {
        if self.version != PROTOCOL_VERSION || &self.author != author {
            return None;
        }
        let key = PublicKey::from_bytes(author).ok()?;
        let sig: [u8; 64] = self.sig.as_slice().try_into().ok()?;
        key.verify(&message(&self.body), &Signature::from_bytes(&sig)).ok()?;
        let a: Announcement = postcard::from_bytes(&self.body).ok()?;
        (a.title.len() <= MAX_TITLE && a.body.len() <= MAX_BODY && !a.title.trim().is_empty()).then_some(a)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard::to_stdvec is infallible")
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<SignedAnnouncement> {
        postcard::from_bytes(bytes).ok()
    }
}

impl fmt::Display for SignedAnnouncement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut text = data_encoding::BASE32_NOPAD.encode(&self.to_bytes());
        text.make_ascii_lowercase();
        write!(f, "{ANNOUNCE_PREFIX}{text}")
    }
}

impl FromStr for SignedAnnouncement {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let body = s.trim().strip_prefix(ANNOUNCE_PREFIX).ok_or(Error::Ticket)?;
        let bytes = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| Error::Ticket)?;
        SignedAnnouncement::from_bytes(&bytes).ok_or(Error::Ticket)
    }
}

/// The gossip topic of `author`'s channel for this app.
pub fn topic(app_id: &str, author: &[u8; 32]) -> TopicId {
    let key = format!("author/{}", data_encoding::HEXLOWER.encode(author));
    crate::topic::derive(app_id, Scope::Public, &key)
}

/// Parse an author public key given as 64 hex characters.
pub fn parse_author(hex: &str) -> Result<[u8; 32]> {
    let bytes = data_encoding::HEXLOWER_PERMISSIVE
        .decode(hex.trim().as_bytes())
        .map_err(|_| Error::Arg("author key is not hex"))?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Arg("author key is not 32 bytes"))?;
    PublicKey::from_bytes(&key).map_err(|_| Error::Arg("not an ed25519 public key"))?;
    Ok(key)
}

/// Generate an author key into `path` (refusing to overwrite), readable only
/// by its owner on Unix. Returns the public key to embed in the plugin.
pub fn keygen(path: &Path) -> Result<[u8; 32]> {
    use std::io::Write;
    let key = SecretKey::generate();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| Error::Store(format!("{}: {e}", dir.display())))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| Error::Store(format!("{}: {e}", path.display())))?;
    writeln!(f, "{}", data_encoding::HEXLOWER.encode(&key.to_bytes()))
        .map_err(|e| Error::Store(e.to_string()))?;
    Ok(*key.public().as_bytes())
}

pub fn load_key(path: &Path) -> Result<SecretKey> {
    let text = std::fs::read_to_string(path).map_err(|e| Error::Store(format!("{}: {e}", path.display())))?;
    let bytes = data_encoding::HEXLOWER_PERMISSIVE
        .decode(text.trim().as_bytes())
        .map_err(|_| Error::Arg("author key file is not hex"))?;
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| Error::Arg("author key file is not 32 bytes"))?;
    Ok(SecretKey::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ann(seq: u64) -> Announcement {
        Announcement {
            seq,
            issued_at: 1_790_000_000,
            title: "Maintenance".into(),
            body: "Relay upgrade tonight.".into(),
        }
    }

    #[test]
    fn a_signed_announcement_verifies_only_for_its_author() {
        let key = SecretKey::generate();
        let other = SecretKey::generate();
        let s = SignedAnnouncement::sign(&key, &ann(1)).unwrap();
        assert_eq!(s.verify(key.public().as_bytes()), Some(ann(1)));
        assert_eq!(s.verify(other.public().as_bytes()), None);

        let text = s.to_string();
        assert!(text.starts_with(ANNOUNCE_PREFIX));
        assert_eq!(SignedAnnouncement::from_str(&text).unwrap(), s);
    }

    #[test]
    fn tampering_and_impersonation_fail() {
        let key = SecretKey::generate();
        let mut s = SignedAnnouncement::sign(&key, &ann(1)).unwrap();
        let mut body = s.body.clone();
        let last = body.len() - 1;
        body[last] ^= 1;
        s.body = body;
        assert_eq!(s.verify(key.public().as_bytes()), None, "a changed body");

        // Somebody signs with their own key but claims to be the author.
        let mallory = SecretKey::generate();
        let mut forged = SignedAnnouncement::sign(&mallory, &ann(2)).unwrap();
        forged.author = *key.public().as_bytes();
        assert_eq!(forged.verify(key.public().as_bytes()), None);
    }

    #[test]
    fn limits_are_enforced_on_both_ends() {
        let key = SecretKey::generate();
        let mut big = ann(1);
        big.body = "x".repeat(MAX_BODY + 1);
        assert!(SignedAnnouncement::sign(&key, &big).is_err());
        let mut untitled = ann(1);
        untitled.title = "  ".into();
        assert!(SignedAnnouncement::sign(&key, &untitled).is_err());
        assert!(SignedAnnouncement::from_str("ltannouncezz").is_err());
        assert!(SignedAnnouncement::from_str("ltfriendabc").is_err());
    }

    #[test]
    fn the_topic_is_per_author_and_per_app() {
        let a = *SecretKey::generate().public().as_bytes();
        let b = *SecretKey::generate().public().as_bytes();
        assert_eq!(topic("app/1", &a), topic("app/1", &a));
        assert_ne!(topic("app/1", &a), topic("app/1", &b));
        assert_ne!(topic("app/1", &a), topic("app/2", &a));
    }

    #[test]
    fn keygen_writes_a_private_file_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys").join("author.key");
        let public = keygen(&path).unwrap();
        assert_eq!(*load_key(&path).unwrap().public().as_bytes(), public);
        assert!(keygen(&path).is_err(), "an existing key is never replaced");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(parse_author(&data_encoding::HEXLOWER.encode(&public)).unwrap(), public);
        assert!(parse_author("abcd").is_err());
    }
}
