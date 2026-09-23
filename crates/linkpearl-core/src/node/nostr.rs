//! Node-side nostr jobs (cargo feature `nostr`). Each `nostr_*` job returns a
//! handle at once and runs on the node's runtime; results arrive as
//! `NostrDone`, `NostrInvite` and `NostrDevices` events. The key calls touch
//! SQLite and are not for the render thread.

use std::{collections::HashSet, sync::Arc, time::Duration};

use iroh::EndpointId;
use nostr_sdk::prelude::{Keys, PublicKey, Timestamp, ToBech32};

use super::{Node, Shared};
use crate::{
    event::{Event, PeerId},
    nostr::{self, DeviceClaim, Relays},
    Error, Result,
};

const RELAY_TIMEOUT: Duration = Duration::from_secs(10);

fn keys_from_store(shared: &Shared) -> Result<Option<Keys>> {
    let store = shared
        .store
        .lock()
        .map_err(|_| Error::Store("store poisoned".into()))?;
    Ok(store
        .profile_get("nostr_secret")?
        .and_then(|hex| Keys::parse(&hex).ok()))
}

fn keep_keys(shared: &Shared, keys: &Keys) -> Result<()> {
    let mut store = shared
        .store
        .lock()
        .map_err(|_| Error::Store("store poisoned".into()))?;
    store.profile_set("nostr_secret", &keys.secret_key().to_secret_hex())?;
    store.profile_set("nostr_pubkey", &keys.public_key().to_hex())?;
    shared.me.write().unwrap().nostr = Some(keys.public_key().to_bytes());
    Ok(())
}

impl Node {
    /// This user's nostr keys, generated on first use and kept in SQLite
    /// beside the device key. Once they exist, new friend invites carry the
    /// public key.
    pub fn nostr_keys(&self) -> Result<Keys> {
        self.check_open()?;
        if let Some(keys) = keys_from_store(&self.shared)? {
            self.shared.me.write().unwrap().nostr = Some(keys.public_key().to_bytes());
            return Ok(keys);
        }
        let keys = Keys::generate();
        keep_keys(&self.shared, &keys)?;
        Ok(keys)
    }

    /// Use an existing nostr identity (nsec or hex) instead of a generated
    /// one, e.g. to give a second device the same user key.
    pub fn nostr_import(&self, secret: &str) -> Result<PublicKey> {
        self.check_open()?;
        let keys = Keys::parse(secret.trim()).map_err(|_| Error::Arg("not a nostr secret key"))?;
        keep_keys(&self.shared, &keys)?;
        Ok(keys.public_key())
    }

    /// The user's npub, if a nostr identity exists.
    pub fn nostr_npub(&self) -> Option<String> {
        let key = self.shared.me.read().unwrap().nostr?;
        PublicKey::from_slice(&key).ok()?.to_bech32().ok()
    }

    fn nostr_job<F>(&self, job: F) -> Result<u64>
    where
        F: FnOnce(Arc<Shared>, u64) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>>,
    {
        self.check_open()?;
        let rt = self.rt.as_ref().ok_or(Error::State("node is closed"))?;
        let handle = self.shared.handle();
        let shared = self.shared.clone();
        let fut = job(shared.clone(), handle);
        rt.spawn(async move {
            let (ok, detail) = match fut.await {
                Ok(detail) => (true, detail),
                Err(e) => (false, e.to_string()),
            };
            shared.emit(Event::NostrDone { handle, ok, detail });
        });
        Ok(handle)
    }

    /// Publish (or refresh) the user's device list with this device in it,
    /// keeping every other device the current list already proves.
    pub fn nostr_publish_devices(&self, relays: &[String]) -> Result<u64> {
        let keys = self.nostr_keys()?;
        let relays = relays.to_vec();
        let secret = self.shared.secret.clone();
        self.nostr_job(move |_, _| {
            Box::pin(async move {
                let conn = Relays::connect(&relays, RELAY_TIMEOUT).await?;
                let mine = DeviceClaim::sign(&secret, &keys.public_key());
                let (mut claims, replaces) = existing_claims(&conn, &keys.public_key()).await;
                claims.retain(|c| c.node_id != mine.node_id);
                claims.push(mine);
                let event = nostr::device_list_event(&keys, &claims, replaces)?;
                let accepted = conn.publish(&event).await;
                conn.shutdown().await;
                Ok(format!("device list with {} device(s) on {} relay(s)", claims.len(), accepted?))
            })
        })
    }

    /// Look up `user`'s (npub or hex) verified device list. Friends it names
    /// are bound to that key; `NostrDevices` carries the list.
    pub fn nostr_fetch_devices(&self, relays: &[String], user: &str) -> Result<u64> {
        let user = nostr::parse_public_key(user)?;
        let relays = relays.to_vec();
        self.nostr_job(move |shared, handle| {
            Box::pin(async move {
                let conn = Relays::connect(&relays, RELAY_TIMEOUT).await?;
                let found = conn.device_list(&user, RELAY_TIMEOUT).await;
                conn.shutdown().await;
                let Some(devices) = found? else {
                    return Err(Error::NotFound);
                };
                let bound = bind_friends(&shared, &user, &devices);
                shared.emit(Event::NostrDevices {
                    handle,
                    user: user.to_bytes(),
                    devices: devices.clone(),
                });
                Ok(format!("{} device(s), {bound} of them friends", devices.len()))
            })
        })
    }

    /// Mint a friend invite and send it to `to` (npub or hex) as a NIP-17
    /// private message.
    pub fn nostr_send_invite(&self, relays: &[String], to: &str, ttl: Option<Duration>) -> Result<u64> {
        let keys = self.nostr_keys()?;
        let to = nostr::parse_public_key(to)?;
        let ticket = self.invite_create(ttl)?;
        let wrap = nostr::invite_dm(&keys, &to, &ticket)?;
        let relays = relays.to_vec();
        self.nostr_job(move |_, _| {
            Box::pin(async move {
                let conn = Relays::connect(&relays, RELAY_TIMEOUT).await?;
                let accepted = conn.publish(&wrap).await;
                conn.shutdown().await;
                Ok(format!("invite sent via {} relay(s)", accepted?))
            })
        })
    }

    /// Read invites gift-wrapped to this user. Each one that is not expired,
    /// not our own and not from a device that is already a friend raises
    /// `NostrInvite`; nothing is redeemed automatically.
    pub fn nostr_check_inbox(&self, relays: &[String]) -> Result<u64> {
        let keys = self.nostr_keys()?;
        let relays = relays.to_vec();
        let me = self.shared.endpoint.id();
        self.nostr_job(move |shared, _| {
            Box::pin(async move {
                let conn = Relays::connect(&relays, RELAY_TIMEOUT).await?;
                let found = conn.invites(&keys, RELAY_TIMEOUT).await;
                conn.shutdown().await;
                let now = crate::store::now_ms() / 1000;
                let mut seen = HashSet::new();
                let mut raised = 0;
                for (from, invite) in found? {
                    let known = shared.friends.read().unwrap().contains_key(&invite.addr.id);
                    if invite.addr.id == me || known || invite.is_expired(now) || !seen.insert(invite.secret) {
                        continue;
                    }
                    raised += 1;
                    shared.emit(Event::NostrInvite {
                        from: from.to_bytes(),
                        ticket: invite.to_string(),
                    });
                }
                Ok(format!("{raised} new invite(s)"))
            })
        })
    }
}

/// The claims in the newest list `user` has published, and its timestamp.
async fn existing_claims(conn: &Relays, user: &PublicKey) -> (Vec<DeviceClaim>, Option<Timestamp>) {
    let filter = nostr_sdk::prelude::Filter::new()
        .kind(nostr_sdk::prelude::Kind::ApplicationSpecificData)
        .author(*user)
        .identifier(nostr::DEVICE_LIST_D);
    let Ok(mut events) = conn.fetch(filter, RELAY_TIMEOUT).await else {
        return (Vec::new(), None);
    };
    events.sort_by_key(|e| std::cmp::Reverse(e.created_at));
    events
        .iter()
        .find_map(|e| {
            nostr::parse_device_claims(e)
                .ok()
                .filter(|(a, _)| a == user)
                .map(|(_, claims)| (claims, Some(e.created_at)))
        })
        .unwrap_or_default()
}

/// Bind every friend device the verified list names to `user`.
fn bind_friends(shared: &Shared, user: &PublicKey, devices: &[PeerId]) -> usize {
    let key = user.to_bytes();
    let mut bound = 0;
    for node in devices {
        let Ok(id) = EndpointId::from_bytes(node) else {
            continue;
        };
        let friend_id = {
            let Ok(mut store) = shared.store.lock() else {
                continue;
            };
            store.friend_bind_nostr(node, &key).ok().flatten()
        };
        if let Some(friend_id) = friend_id {
            if let Some(d) = shared.friends.write().unwrap().get_mut(&id) {
                d.friend_id = friend_id;
                d.nostr = Some(key);
                bound += 1;
            }
        }
    }
    bound
}
