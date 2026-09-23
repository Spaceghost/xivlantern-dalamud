//! Optional nostr plumbing (cargo feature `nostr`).
//!
//! nostr is never required: two players with a ticket, or on one LAN, never
//! touch it. What it adds:
//!
//! * a **device list** — a NIP-78 application-data event (kind 30078,
//!   `d` = [`DEVICE_LIST_D`]) signed by the user's nostr key, listing their
//!   iroh NodeIds. Each entry also carries the *device's* ed25519 signature
//!   over [`device_claim_message`], so a list proves both that the user claims
//!   the device and that the device agreed. A list cannot adopt somebody
//!   else's NodeId.
//! * **invites over relays** — an `ltfriend` ticket sent as a NIP-17 private
//!   direct message (kind 14 rumor, sealed, gift-wrapped as kind 1059), marked
//!   with a `["lantern", "invite/1"]` tag on the rumor.
//!
//! The pure functions here build and check events and need no network. The
//! [`Relays`] helper is a thin wrapper over `nostr_sdk::Client` for the node's
//! background jobs.

use std::time::Duration;

use iroh::{PublicKey as DeviceKey, SecretKey as DeviceSecret, Signature};
use nostr_sdk::prelude::{
    Client, Event as NostrEvent, EventBuilder, FinalizeEvent, Filter, Keys, Kind,
    PrivateDirectMessageBuilder, PublicKey, Tag, Timestamp, UnwrappedGift,
};
use serde::{Deserialize, Serialize};

use crate::{event::PeerId, ticket::FriendInvite, Error, Result};

pub use nostr_sdk::prelude::{Keys as NostrKeys, PublicKey as NostrPublicKey};

/// The `d` tag of the device list event.
pub const DEVICE_LIST_D: &str = "xivlantern/devices";
/// Marks a NIP-17 rumor as carrying a Lantern friend invite.
pub const INVITE_TAG: (&str, &str) = ("lantern", "invite/1");
const CLAIM_CONTEXT: &[u8] = b"lantern-device-claim/1";

fn nerr(e: impl std::fmt::Display) -> Error {
    Error::Internal(format!("nostr: {e}"))
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Net(format!("nostr: {e}"))
}

/// What a device signs to agree to be listed under a user key.
pub fn device_claim_message(user: &PublicKey, node: &PeerId) -> Vec<u8> {
    let mut msg = Vec::with_capacity(CLAIM_CONTEXT.len() + 64);
    msg.extend_from_slice(CLAIM_CONTEXT);
    msg.extend_from_slice(&user.to_bytes());
    msg.extend_from_slice(node);
    msg
}

/// A device's signed consent to appear in `user`'s device list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceClaim {
    pub node_id: PeerId,
    pub sig: [u8; 64],
}

impl DeviceClaim {
    pub fn sign(device: &DeviceSecret, user: &PublicKey) -> DeviceClaim {
        let node_id = *device.public().as_bytes();
        let sig = device.sign(&device_claim_message(user, &node_id));
        DeviceClaim {
            node_id,
            sig: sig.to_bytes(),
        }
    }

    pub fn verify(&self, user: &PublicKey) -> bool {
        let Ok(key) = DeviceKey::from_bytes(&self.node_id) else {
            return false;
        };
        key.verify(
            &device_claim_message(user, &self.node_id),
            &Signature::from_bytes(&self.sig),
        )
        .is_ok()
    }
}

#[derive(Serialize, Deserialize)]
struct DeviceListContent {
    v: u16,
    devices: Vec<DeviceEntry>,
}

#[derive(Serialize, Deserialize)]
struct DeviceEntry {
    id: String,
    sig: String,
}

/// The user's device list, signed by the user key. `replaces` is the
/// `created_at` of the list this one supersedes: relays keep only the newest
/// replaceable event and refuse one that is not newer, so two devices
/// publishing within the same second must still move the clock forward.
pub fn device_list_event(
    user: &Keys,
    claims: &[DeviceClaim],
    replaces: Option<Timestamp>,
) -> Result<NostrEvent> {
    let content = DeviceListContent {
        v: 1,
        devices: claims
            .iter()
            .map(|c| DeviceEntry {
                id: data_encoding::HEXLOWER.encode(&c.node_id),
                sig: data_encoding::HEXLOWER.encode(&c.sig),
            })
            .collect(),
    };
    let json = serde_json::to_string(&content).map_err(nerr)?;
    let now = Timestamp::now();
    let created_at = match replaces {
        Some(prev) if prev >= now => prev + 1u64,
        _ => now,
    };
    EventBuilder::new(Kind::ApplicationSpecificData, json)
        .tag(Tag::identifier(DEVICE_LIST_D))
        .custom_created_at(created_at)
        .finalize(user)
        .map_err(nerr)
}

/// Check a device list and return the author and the devices whose claims
/// verify. Entries with a bad or foreign claim are dropped, not trusted.
pub fn parse_device_list(event: &NostrEvent) -> Result<(PublicKey, Vec<PeerId>)> {
    let (author, claims) = parse_device_claims(event)?;
    Ok((author, claims.into_iter().map(|c| c.node_id).collect()))
}

/// [`parse_device_list`], keeping the verified claims so a device can
/// republish the list with itself added without dropping the others.
pub fn parse_device_claims(event: &NostrEvent) -> Result<(PublicKey, Vec<DeviceClaim>)> {
    event.verify().map_err(|_| Error::Arg("device list signature does not verify"))?;
    if event.kind != Kind::ApplicationSpecificData
        || event.tags.identifier().as_deref() != Some(DEVICE_LIST_D)
    {
        return Err(Error::Arg("not a lantern device list"));
    }
    let content: DeviceListContent =
        serde_json::from_str(&event.content).map_err(|_| Error::Arg("device list content"))?;
    if content.v != 1 {
        return Err(Error::Arg("unknown device list version"));
    }
    let mut claims: Vec<DeviceClaim> = Vec::new();
    for entry in content.devices {
        let (Ok(id), Ok(sig)) = (
            data_encoding::HEXLOWER.decode(entry.id.as_bytes()),
            data_encoding::HEXLOWER.decode(entry.sig.as_bytes()),
        ) else {
            continue;
        };
        let (Ok(node_id), Ok(sig)) = (<[u8; 32]>::try_from(id), <[u8; 64]>::try_from(sig)) else {
            continue;
        };
        let claim = DeviceClaim { node_id, sig };
        if claim.verify(&event.pubkey) && !claims.iter().any(|c| c.node_id == node_id) {
            claims.push(claim);
        }
    }
    Ok((event.pubkey, claims))
}

/// Wrap an invite ticket as a NIP-17 private message to `to`.
pub fn invite_dm(from: &Keys, to: &PublicKey, ticket: &str) -> Result<NostrEvent> {
    let tag = Tag::custom(INVITE_TAG.0, [INVITE_TAG.1]);
    PrivateDirectMessageBuilder::new(*to, ticket)
        .rumor_extra_tags([tag])
        .finalize(from)
        .map_err(nerr)
}

/// Open a gift wrap addressed to `me`. `Some` only for a Lantern invite
/// that parses and does not claim a nostr key other than the (verified)
/// sender's.
pub fn open_invite_dm(me: &Keys, gift_wrap: &NostrEvent) -> Option<(PublicKey, FriendInvite)> {
    let unwrapped = UnwrappedGift::from_gift_wrap(me, gift_wrap).ok()?;
    let rumor = unwrapped.rumor;
    if rumor.kind != Kind::PrivateDirectMessage || rumor.pubkey != unwrapped.sender {
        return None;
    }
    let marked = rumor
        .tags
        .iter()
        .any(|t| t.kind() == INVITE_TAG.0 && t.content() == Some(INVITE_TAG.1));
    if !marked {
        return None;
    }
    let invite: FriendInvite = rumor.content.trim().parse().ok()?;
    if invite.nostr.is_some_and(|k| k != unwrapped.sender.to_bytes()) {
        return None;
    }
    Some((unwrapped.sender, invite))
}

/// Parse an npub, nprofile or hex public key.
pub fn parse_public_key(text: &str) -> Result<PublicKey> {
    PublicKey::parse(text.trim()).map_err(|_| Error::Arg("not a nostr public key"))
}

/// A connection to a set of relays for one job.
pub struct Relays {
    client: Client,
}

impl Relays {
    pub async fn connect(urls: &[String], timeout: Duration) -> Result<Relays> {
        if urls.is_empty() {
            return Err(Error::Arg("no nostr relays configured"));
        }
        let client = Client::new();
        for url in urls {
            client.add_relay(url.as_str()).await.map_err(net)?;
        }
        client.connect().and_wait(timeout).await;
        Ok(Relays { client })
    }

    /// Publish; `Ok` when at least one relay accepted it.
    pub async fn publish(&self, event: &NostrEvent) -> Result<usize> {
        let out = self.client.send_event(event).await.map_err(net)?;
        if out.success.is_empty() {
            return Err(net(format!("no relay accepted the event: {:?}", out.failed)));
        }
        Ok(out.success.len())
    }

    pub async fn fetch(&self, filter: Filter, timeout: Duration) -> Result<Vec<NostrEvent>> {
        let events = self
            .client
            .fetch_events(filter)
            .timeout(timeout)
            .await
            .map_err(net)?;
        Ok(events.into_iter().collect())
    }

    /// The newest verifying device list `user` published.
    pub async fn device_list(&self, user: &PublicKey, timeout: Duration) -> Result<Option<Vec<PeerId>>> {
        let filter = Filter::new()
            .kind(Kind::ApplicationSpecificData)
            .author(*user)
            .identifier(DEVICE_LIST_D);
        let mut events = self.fetch(filter, timeout).await?;
        events.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        Ok(events
            .iter()
            .filter_map(|e| parse_device_list(e).ok())
            .find(|(author, _)| author == user)
            .map(|(_, devices)| devices))
    }

    /// Every Lantern invite gift-wrapped to `me`.
    pub async fn invites(&self, me: &Keys, timeout: Duration) -> Result<Vec<(PublicKey, FriendInvite)>> {
        let filter = Filter::new().kind(Kind::GiftWrap).pubkey(me.public_key());
        let events = self.fetch(filter, timeout).await?;
        Ok(events.iter().filter_map(|e| open_invite_dm(me, e)).collect())
    }

    pub async fn shutdown(self) {
        self.client.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PROTOCOL_VERSION;

    fn invite(nostr: Option<[u8; 32]>) -> FriendInvite {
        FriendInvite {
            version: PROTOCOL_VERSION,
            addr: iroh::EndpointAddr::from(DeviceSecret::generate().public()),
            secret: [4u8; 16],
            name: "Alice".into(),
            expires_at: 0,
            nostr,
        }
    }

    #[test]
    fn a_device_claim_binds_one_device_to_one_user() {
        let user = Keys::generate();
        let other = Keys::generate();
        let device = DeviceSecret::generate();
        let claim = DeviceClaim::sign(&device, &user.public_key());
        assert!(claim.verify(&user.public_key()));
        assert!(!claim.verify(&other.public_key()), "a claim is for one user only");
        let mut forged = claim.clone();
        forged.node_id = *DeviceSecret::generate().public().as_bytes();
        assert!(!forged.verify(&user.public_key()), "a claim is for one device only");
    }

    #[test]
    fn device_lists_round_trip_and_drop_bad_claims() {
        let user = Keys::generate();
        let mine = DeviceSecret::generate();
        let good = DeviceClaim::sign(&mine, &user.public_key());
        // A NodeId somebody else owns, "claimed" with a signature over the
        // wrong user: it must not be listed.
        let victim = DeviceSecret::generate();
        let stolen = DeviceClaim::sign(&victim, &Keys::generate().public_key());
        let event = device_list_event(&user, &[good.clone(), stolen], None).unwrap();
        assert_eq!(event.kind, Kind::ApplicationSpecificData);
        let (author, devices) = parse_device_list(&event).unwrap();
        assert_eq!(author, user.public_key());
        assert_eq!(devices, vec![good.node_id]);
    }

    #[test]
    fn a_replacement_list_is_always_newer() {
        let user = Keys::generate();
        let first = device_list_event(&user, &[], None).unwrap();
        let second = device_list_event(&user, &[], Some(first.created_at)).unwrap();
        assert!(second.created_at > first.created_at);
    }

    #[test]
    fn other_events_are_not_device_lists() {
        let user = Keys::generate();
        let note = EventBuilder::new(Kind::TextNote, "hi").finalize(&user).unwrap();
        assert!(parse_device_list(&note).is_err());
        let wrong_d = EventBuilder::new(Kind::ApplicationSpecificData, "{\"v\":1,\"devices\":[]}")
            .tag(Tag::identifier("someone-else/devices"))
            .finalize(&user)
            .unwrap();
        assert!(parse_device_list(&wrong_d).is_err());
    }

    #[test]
    fn invites_travel_as_gift_wrapped_dms() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let carol = Keys::generate();
        let ticket = invite(Some(alice.public_key().to_bytes()));
        let wrap = invite_dm(&alice, &bob.public_key(), &ticket.to_string()).unwrap();
        assert_eq!(wrap.kind, Kind::GiftWrap);
        assert_ne!(wrap.pubkey, alice.public_key(), "the wrapper is ephemeral, not the sender");
        assert!(!wrap.content.contains("ltfriend"), "the ticket is encrypted");

        let (from, got) = open_invite_dm(&bob, &wrap).expect("bob can open it");
        assert_eq!(from, alice.public_key());
        assert_eq!(got, ticket);
        assert!(open_invite_dm(&carol, &wrap).is_none(), "nobody else can");
    }

    #[test]
    fn an_invite_claiming_someone_elses_key_is_dropped() {
        let mallory = Keys::generate();
        let bob = Keys::generate();
        let alice = Keys::generate();
        let ticket = invite(Some(alice.public_key().to_bytes()));
        let wrap = invite_dm(&mallory, &bob.public_key(), &ticket.to_string()).unwrap();
        assert!(open_invite_dm(&bob, &wrap).is_none());
    }

    #[test]
    fn an_ordinary_dm_is_not_an_invite() {
        let alice = Keys::generate();
        let bob = Keys::generate();
        let dm = PrivateDirectMessageBuilder::new(bob.public_key(), invite(None).to_string())
            .finalize(&alice)
            .unwrap();
        assert!(open_invite_dm(&bob, &dm).is_none(), "untagged DMs are left alone");
    }

    #[test]
    fn public_keys_parse_from_npub_and_hex() {
        use nostr_sdk::prelude::ToBech32;
        let k = Keys::generate().public_key();
        assert_eq!(parse_public_key(&k.to_bech32().unwrap()).unwrap(), k);
        assert_eq!(parse_public_key(&k.to_hex()).unwrap(), k);
        assert!(parse_public_key("npub1nope").is_err());
    }
}
