//! The SQLite side: identity, bookmarks, a note of what we have hashed, and
//! (schema 2) the friend list, pending invites and 1:1 message history.
//!
//! One connection behind a mutex, `PRAGMA user_version` migrations, every call
//! short. The identity row is the whole reason this file exists: a player's
//! node id has to survive a restart or every bookmark anyone made of them dies.

use std::path::Path;

use iroh::SecretKey;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{ticket::Scope, Error, Result};

pub const SCHEMA_VERSION: i64 = 2;

pub struct Store {
    db: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub id: String,
    pub scope: Scope,
    pub title: String,
    pub ticket: String,
    pub updated_at: i64,
}

/// One stored friend device, joined with its friend row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub node_id: [u8; 32],
    pub friend_id: i64,
    pub name: String,
    /// Last known iroh `EndpointAddr`, postcard encoded by the node layer.
    pub addr: Option<Vec<u8>>,
    pub last_seen: Option<i64>,
    /// Set only from a verified nostr device list.
    pub nostr: Option<[u8; 32]>,
}

/// One line of 1:1 history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub peer: [u8; 32],
    pub outgoing: bool,
    pub id: u64,
    pub body: String,
    pub sent_at: i64,
    pub delivered_at: Option<i64>,
}

fn blob32(v: Vec<u8>) -> rusqlite::Result<[u8; 32]> {
    v.try_into().map_err(|v: Vec<u8>| {
        rusqlite::Error::FromSqlConversionFailure(
            v.len(),
            rusqlite::types::Type::Blob,
            "expected 32 bytes".into(),
        )
    })
}

fn store_err(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Store {
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let db = match path {
            Some(p) => {
                if let Some(dir) = p.parent() {
                    if !dir.as_os_str().is_empty() {
                        std::fs::create_dir_all(dir)
                            .map_err(|e| Error::Store(format!("create {}: {e}", dir.display())))?;
                    }
                }
                Connection::open(p).map_err(store_err)?
            }
            None => Connection::open_in_memory().map_err(store_err)?,
        };
        // WAL so a reader never blocks the frame thread behind a writer.
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=2000;",
        )
        .map_err(store_err)?;
        let mut store = Store { db };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(store_err)?;
        if version < 1 {
            self.db
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS identity (
                        id         INTEGER PRIMARY KEY CHECK (id = 1),
                        secret     BLOB NOT NULL,
                        created_at INTEGER NOT NULL
                     );
                     CREATE TABLE IF NOT EXISTS bookmarks (
                        id         TEXT PRIMARY KEY,
                        scope      INTEGER NOT NULL,
                        title      TEXT NOT NULL,
                        ticket     TEXT NOT NULL,
                        updated_at INTEGER NOT NULL
                     );
                     CREATE INDEX IF NOT EXISTS bookmarks_scope
                        ON bookmarks (scope, updated_at);
                     CREATE TABLE IF NOT EXISTS blobs (
                        hash     BLOB PRIMARY KEY,
                        name     TEXT NOT NULL,
                        size     INTEGER NOT NULL,
                        added_at INTEGER NOT NULL
                     );
                     PRAGMA user_version = 1;",
                )
                .map_err(store_err)?;
        }
        if version < 2 {
            self.db
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS profile (
                        key   TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                     );
                     CREATE TABLE IF NOT EXISTS friends (
                        id           INTEGER PRIMARY KEY,
                        name         TEXT NOT NULL,
                        nostr_pubkey BLOB UNIQUE,
                        added_at     INTEGER NOT NULL
                     );
                     CREATE TABLE IF NOT EXISTS friend_devices (
                        node_id   BLOB PRIMARY KEY,
                        friend_id INTEGER NOT NULL REFERENCES friends(id) ON DELETE CASCADE,
                        addr      BLOB,
                        added_at  INTEGER NOT NULL,
                        last_seen INTEGER
                     );
                     CREATE TABLE IF NOT EXISTS invites (
                        secret      BLOB PRIMARY KEY,
                        created_at  INTEGER NOT NULL,
                        expires_at  INTEGER NOT NULL,
                        redeemed_by BLOB,
                        redeemed_at INTEGER
                     );
                     CREATE TABLE IF NOT EXISTS messages (
                        peer         BLOB NOT NULL,
                        outgoing     INTEGER NOT NULL,
                        msg_id       INTEGER NOT NULL,
                        body         TEXT NOT NULL,
                        sent_at      INTEGER NOT NULL,
                        delivered_at INTEGER,
                        PRIMARY KEY (peer, outgoing, msg_id)
                     );
                     CREATE INDEX IF NOT EXISTS messages_outbox
                        ON messages (peer, outgoing, delivered_at);
                     PRAGMA user_version = 2;",
                )
                .map_err(store_err)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------- profile

    pub fn profile_get(&self, key: &str) -> Result<Option<String>> {
        self.db
            .query_row("SELECT value FROM profile WHERE key = ?1", params![key], |r| r.get(0))
            .optional()
            .map_err(store_err)
    }

    pub fn profile_set(&mut self, key: &str, value: &str) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO profile (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(store_err)?;
        Ok(())
    }

    // ------------------------------------------------------------- friends

    /// Add a device as a friend, or refresh it if it is already one. Returns
    /// the friend row id.
    pub fn friend_add(&mut self, node_id: &[u8; 32], name: &str, addr: Option<&[u8]>) -> Result<i64> {
        let tx = self.db.transaction().map_err(store_err)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT friend_id FROM friend_devices WHERE node_id = ?1",
                params![node_id.to_vec()],
                |r| r.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let now = now_ms();
        let friend_id = match existing {
            Some(id) => {
                tx.execute("UPDATE friends SET name = ?2 WHERE id = ?1", params![id, name])
                    .map_err(store_err)?;
                if let Some(addr) = addr {
                    tx.execute(
                        "UPDATE friend_devices SET addr = ?2 WHERE node_id = ?1",
                        params![node_id.to_vec(), addr],
                    )
                    .map_err(store_err)?;
                }
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO friends (name, added_at) VALUES (?1, ?2)",
                    params![name, now],
                )
                .map_err(store_err)?;
                let id = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO friend_devices (node_id, friend_id, addr, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![node_id.to_vec(), id, addr, now],
                )
                .map_err(store_err)?;
                id
            }
        };
        tx.commit().map_err(store_err)?;
        Ok(friend_id)
    }

    /// Refresh what a device told us about itself. `None` leaves a field alone.
    pub fn friend_touch(
        &mut self,
        node_id: &[u8; 32],
        name: Option<&str>,
        addr: Option<&[u8]>,
        last_seen: Option<i64>,
    ) -> Result<()> {
        let id = node_id.to_vec();
        if let Some(name) = name {
            self.db
                .execute(
                    "UPDATE friends SET name = ?2
                     WHERE id = (SELECT friend_id FROM friend_devices WHERE node_id = ?1)",
                    params![id, name],
                )
                .map_err(store_err)?;
        }
        if let Some(addr) = addr {
            self.db
                .execute(
                    "UPDATE friend_devices SET addr = ?2 WHERE node_id = ?1",
                    params![id, addr],
                )
                .map_err(store_err)?;
        }
        if let Some(t) = last_seen {
            self.db
                .execute(
                    "UPDATE friend_devices SET last_seen = ?2 WHERE node_id = ?1",
                    params![id, t],
                )
                .map_err(store_err)?;
        }
        Ok(())
    }

    /// Forget a device; a friend left with no devices is forgotten too.
    pub fn friend_remove_device(&mut self, node_id: &[u8; 32]) -> Result<bool> {
        let tx = self.db.transaction().map_err(store_err)?;
        let friend: Option<i64> = tx
            .query_row(
                "SELECT friend_id FROM friend_devices WHERE node_id = ?1",
                params![node_id.to_vec()],
                |r| r.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let Some(friend) = friend else {
            return Ok(false);
        };
        tx.execute(
            "DELETE FROM friend_devices WHERE node_id = ?1",
            params![node_id.to_vec()],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friends WHERE id = ?1
             AND NOT EXISTS (SELECT 1 FROM friend_devices WHERE friend_id = ?1)",
            params![friend],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(true)
    }

    pub fn friend_devices(&self) -> Result<Vec<DeviceRow>> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT d.node_id, d.friend_id, f.name, d.addr, d.last_seen, f.nostr_pubkey
                 FROM friend_devices d JOIN friends f ON f.id = d.friend_id
                 ORDER BY f.name COLLATE NOCASE, d.added_at",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DeviceRow {
                    node_id: blob32(r.get(0)?)?,
                    friend_id: r.get(1)?,
                    name: r.get(2)?,
                    addr: r.get(3)?,
                    last_seen: r.get(4)?,
                    nostr: r
                        .get::<_, Option<Vec<u8>>>(5)?
                        .and_then(|v| <[u8; 32]>::try_from(v).ok()),
                })
            })
            .map_err(store_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(store_err)
    }

    /// Record that a *verified* nostr device list names this device. If
    /// another friend row already has that key, the device moves under it (two
    /// invites from the same person's two devices become one friend). Returns
    /// the friend id the device ends up under, or `None` for a non-friend.
    pub fn friend_bind_nostr(&mut self, node_id: &[u8; 32], pubkey: &[u8; 32]) -> Result<Option<i64>> {
        let tx = self.db.transaction().map_err(store_err)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT friend_id FROM friend_devices WHERE node_id = ?1",
                params![node_id.to_vec()],
                |r| r.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let Some(current) = current else {
            return Ok(None);
        };
        let owner: Option<i64> = tx
            .query_row(
                "SELECT id FROM friends WHERE nostr_pubkey = ?1",
                params![pubkey.to_vec()],
                |r| r.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let target = match owner {
            Some(owner) if owner != current => {
                tx.execute(
                    "UPDATE friend_devices SET friend_id = ?2 WHERE node_id = ?1",
                    params![node_id.to_vec(), owner],
                )
                .map_err(store_err)?;
                tx.execute(
                    "DELETE FROM friends WHERE id = ?1
                     AND NOT EXISTS (SELECT 1 FROM friend_devices WHERE friend_id = ?1)",
                    params![current],
                )
                .map_err(store_err)?;
                owner
            }
            Some(owner) => owner,
            None => {
                tx.execute(
                    "UPDATE friends SET nostr_pubkey = ?2 WHERE id = ?1",
                    params![current, pubkey.to_vec()],
                )
                .map_err(store_err)?;
                current
            }
        };
        tx.commit().map_err(store_err)?;
        Ok(Some(target))
    }

    // ------------------------------------------------------------- invites

    pub fn invite_put(&mut self, secret: &[u8; 16], expires_at: i64) -> Result<()> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO invites (secret, created_at, expires_at) VALUES (?1, ?2, ?3)",
                params![secret.to_vec(), now_ms(), expires_at],
            )
            .map_err(store_err)?;
        Ok(())
    }

    pub fn invite_redeemed(&mut self, secret: &[u8; 16], by: &[u8; 32]) -> Result<()> {
        self.db
            .execute(
                "UPDATE invites SET redeemed_by = ?2, redeemed_at = ?3 WHERE secret = ?1",
                params![secret.to_vec(), by.to_vec(), now_ms()],
            )
            .map_err(store_err)?;
        Ok(())
    }

    /// Invites nobody has redeemed and that have not expired at `now_secs`
    /// (unix seconds; an expiry of 0 never expires).
    pub fn invites_open(&self, now_secs: i64) -> Result<Vec<([u8; 16], i64)>> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT secret, expires_at FROM invites
                 WHERE redeemed_by IS NULL AND (expires_at = 0 OR expires_at > ?1)",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_secs], |r| {
                let v: Vec<u8> = r.get(0)?;
                Ok((v, r.get::<_, i64>(1)?))
            })
            .map_err(store_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (v, exp) = row.map_err(store_err)?;
            if let Ok(secret) = <[u8; 16]>::try_from(v.as_slice()) {
                out.push((secret, exp));
            }
        }
        Ok(out)
    }

    // ------------------------------------------------------------ messages

    pub fn message_out(&mut self, peer: &[u8; 32], id: u64, sent_at: i64, body: &str) -> Result<()> {
        self.db
            .execute(
                "INSERT OR IGNORE INTO messages (peer, outgoing, msg_id, body, sent_at)
                 VALUES (?1, 1, ?2, ?3, ?4)",
                params![peer.to_vec(), id as i64, body, sent_at],
            )
            .map_err(store_err)?;
        Ok(())
    }

    /// Record an incoming message. `false` means we already had it: the sender
    /// retried because our ack was lost, and the UI must not show it twice.
    pub fn message_in(&mut self, peer: &[u8; 32], id: u64, sent_at: i64, body: &str) -> Result<bool> {
        let n = self
            .db
            .execute(
                "INSERT OR IGNORE INTO messages (peer, outgoing, msg_id, body, sent_at, delivered_at)
                 VALUES (?1, 0, ?2, ?3, ?4, ?5)",
                params![peer.to_vec(), id as i64, body, sent_at, now_ms()],
            )
            .map_err(store_err)?;
        Ok(n > 0)
    }

    /// Mark an outgoing message acknowledged. `false` when it already was.
    pub fn message_delivered(&mut self, peer: &[u8; 32], id: u64) -> Result<bool> {
        let n = self
            .db
            .execute(
                "UPDATE messages SET delivered_at = ?3
                 WHERE peer = ?1 AND outgoing = 1 AND msg_id = ?2 AND delivered_at IS NULL",
                params![peer.to_vec(), id as i64, now_ms()],
            )
            .map_err(store_err)?;
        Ok(n > 0)
    }

    /// Outgoing messages to `peer` still waiting for an ack, oldest first.
    pub fn outbox(&self, peer: &[u8; 32]) -> Result<Vec<MessageRow>> {
        self.messages(
            "SELECT peer, outgoing, msg_id, body, sent_at, delivered_at FROM messages
             WHERE peer = ?1 AND outgoing = 1 AND delivered_at IS NULL
             ORDER BY sent_at, rowid LIMIT ?2",
            peer,
            i64::MAX,
        )
    }

    /// The last `limit` messages with `peer`, oldest first.
    pub fn history(&self, peer: &[u8; 32], limit: u32) -> Result<Vec<MessageRow>> {
        let mut rows = self.messages(
            "SELECT peer, outgoing, msg_id, body, sent_at, delivered_at FROM messages
             WHERE peer = ?1 ORDER BY sent_at DESC, rowid DESC LIMIT ?2",
            peer,
            limit as i64,
        )?;
        rows.reverse();
        Ok(rows)
    }

    fn messages(&self, sql: &str, peer: &[u8; 32], limit: i64) -> Result<Vec<MessageRow>> {
        let mut stmt = self.db.prepare(sql).map_err(store_err)?;
        let rows = stmt
            .query_map(params![peer.to_vec(), limit], |r| {
                Ok(MessageRow {
                    peer: blob32(r.get(0)?)?,
                    outgoing: r.get::<_, i64>(1)? != 0,
                    id: r.get::<_, i64>(2)? as u64,
                    body: r.get(3)?,
                    sent_at: r.get(4)?,
                    delivered_at: r.get(5)?,
                })
            })
            .map_err(store_err)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(store_err)
    }

    /// The node's long-lived key, created on first run.
    pub fn identity(&mut self) -> Result<SecretKey> {
        let existing: Option<Vec<u8>> = self
            .db
            .query_row("SELECT secret FROM identity WHERE id = 1", [], |r| r.get(0))
            .optional()
            .map_err(store_err)?;
        if let Some(bytes) = existing {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Ok(SecretKey::from_bytes(&key));
            }
            // A corrupt row is not worth failing the whole plugin over; replace it.
            self.db
                .execute("DELETE FROM identity WHERE id = 1", [])
                .map_err(store_err)?;
        }
        let key = SecretKey::generate();
        self.db
            .execute(
                "INSERT INTO identity (id, secret, created_at) VALUES (1, ?1, ?2)",
                params![key.to_bytes().to_vec(), now_ms()],
            )
            .map_err(store_err)?;
        Ok(key)
    }

    pub fn bookmark_put(&mut self, id: &str, scope: Scope, title: &str, ticket: &str) -> Result<()> {
        if id.is_empty() {
            return Err(Error::Arg("bookmark id must not be empty"));
        }
        self.db
            .execute(
                "INSERT INTO bookmarks (id, scope, title, ticket, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    scope = excluded.scope,
                    title = excluded.title,
                    ticket = excluded.ticket,
                    updated_at = excluded.updated_at",
                params![id, scope as u32, title, ticket, now_ms()],
            )
            .map_err(store_err)?;
        Ok(())
    }

    pub fn bookmark_forget(&mut self, id: &str) -> Result<bool> {
        let n = self
            .db
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])
            .map_err(store_err)?;
        Ok(n > 0)
    }

    pub fn bookmarks(&self) -> Result<Vec<Bookmark>> {
        let mut stmt = self
            .db
            .prepare(
                "SELECT id, scope, title, ticket, updated_at
                 FROM bookmarks ORDER BY updated_at DESC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Bookmark {
                    id: r.get(0)?,
                    scope: Scope::from_u32(r.get::<_, u32>(1)?).unwrap_or(Scope::Custom),
                    title: r.get(2)?,
                    ticket: r.get(3)?,
                    updated_at: r.get(4)?,
                })
            })
            .map_err(store_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(store_err)?);
        }
        Ok(out)
    }

    pub fn blob_note(&mut self, hash: &[u8; 32], name: &str, size: u64) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO blobs (hash, name, size, added_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(hash) DO UPDATE SET name = excluded.name, size = excluded.size",
                params![hash.to_vec(), name, size as i64, now_ms()],
            )
            .map_err(store_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("linkpearl.sqlite");

        let first = Store::open(Some(&path)).unwrap().identity().unwrap();
        let second = Store::open(Some(&path)).unwrap().identity().unwrap();
        assert_eq!(
            first.public(),
            second.public(),
            "a player's node id must survive a restart"
        );
    }

    #[test]
    fn a_corrupt_identity_row_is_replaced_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linkpearl.sqlite");
        {
            let mut s = Store::open(Some(&path)).unwrap();
            s.identity().unwrap();
            s.db
                .execute("UPDATE identity SET secret = ?1 WHERE id = 1", params![vec![1u8, 2, 3]])
                .unwrap();
        }
        let mut s = Store::open(Some(&path)).unwrap();
        s.identity().expect("a short key must not brick the store");
    }

    #[test]
    fn bookmarks_upsert_and_list_newest_first() {
        let mut s = Store::open(None).unwrap();
        s.bookmark_put("a", Scope::Public, "Alpha", "lproomaaa").unwrap();
        s.bookmark_put("b", Scope::Fc, "Beta", "lproombbb").unwrap();
        s.bookmark_put("a", Scope::Public, "Alpha renamed", "lproomaaa")
            .unwrap();

        let list = s.bookmarks().unwrap();
        assert_eq!(list.len(), 2, "upsert must not duplicate");
        assert_eq!(list[0].id, "a");
        assert_eq!(list[0].title, "Alpha renamed");

        assert!(s.bookmark_forget("a").unwrap());
        assert!(!s.bookmark_forget("a").unwrap());
        assert_eq!(s.bookmarks().unwrap().len(), 1);
    }

    #[test]
    fn an_empty_bookmark_id_is_refused() {
        let mut s = Store::open(None).unwrap();
        assert!(s.bookmark_put("", Scope::Public, "x", "y").is_err());
    }

    #[test]
    fn friends_add_refresh_and_remove() {
        let mut s = Store::open(None).unwrap();
        let bob = [2u8; 32];
        let id = s.friend_add(&bob, "Bob", Some(b"addr1")).unwrap();
        assert_eq!(
            s.friend_add(&bob, "Bobby", None).unwrap(),
            id,
            "re-adding a device refreshes it instead of duplicating"
        );
        s.friend_touch(&bob, None, Some(b"addr2"), Some(42)).unwrap();
        let devices = s.friend_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Bobby");
        assert_eq!(devices[0].addr.as_deref(), Some(&b"addr2"[..]));
        assert_eq!(devices[0].last_seen, Some(42));

        assert!(s.friend_remove_device(&bob).unwrap());
        assert!(!s.friend_remove_device(&bob).unwrap());
        assert!(s.friend_devices().unwrap().is_empty());
        let friends: i64 = s.db.query_row("SELECT count(*) FROM friends", [], |r| r.get(0)).unwrap();
        assert_eq!(friends, 0, "a friend with no devices left is forgotten");
    }

    #[test]
    fn a_verified_nostr_key_merges_two_devices_into_one_friend() {
        let mut s = Store::open(None).unwrap();
        let (pc, deck, key) = ([1u8; 32], [2u8; 32], [9u8; 32]);
        let a = s.friend_add(&pc, "Alice", None).unwrap();
        let b = s.friend_add(&deck, "Alice (Deck)", None).unwrap();
        assert_ne!(a, b, "two invites, two friends, until nostr says otherwise");
        assert_eq!(s.friend_bind_nostr(&pc, &key).unwrap(), Some(a));
        assert_eq!(s.friend_bind_nostr(&deck, &key).unwrap(), Some(a), "moved under the key's friend");
        let devices = s.friend_devices().unwrap();
        assert!(devices.iter().all(|d| d.friend_id == a && d.nostr == Some(key)));
        let friends: i64 = s.db.query_row("SELECT count(*) FROM friends", [], |r| r.get(0)).unwrap();
        assert_eq!(friends, 1, "the empty friend row is gone");
        assert_eq!(s.friend_bind_nostr(&[7u8; 32], &key).unwrap(), None, "strangers are not bound");
    }

    #[test]
    fn friends_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.sqlite");
        Store::open(Some(&path)).unwrap().friend_add(&[9u8; 32], "Nine", None).unwrap();
        let devices = Store::open(Some(&path)).unwrap().friend_devices().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].node_id, [9u8; 32]);
    }

    #[test]
    fn invites_are_open_until_redeemed_or_expired() {
        let mut s = Store::open(None).unwrap();
        s.invite_put(&[1u8; 16], 0).unwrap();
        s.invite_put(&[2u8; 16], 100).unwrap();
        let open = s.invites_open(50).unwrap();
        assert_eq!(open.len(), 2);
        assert_eq!(s.invites_open(100).unwrap().len(), 1, "expiry is exclusive");
        s.invite_redeemed(&[1u8; 16], &[7u8; 32]).unwrap();
        assert!(s.invites_open(50).unwrap().iter().all(|(k, _)| *k != [1u8; 16]));
    }

    #[test]
    fn messages_dedupe_ack_and_drain_the_outbox() {
        let mut s = Store::open(None).unwrap();
        let peer = [5u8; 32];
        assert!(s.message_in(&peer, 7, 1, "hi").unwrap());
        assert!(!s.message_in(&peer, 7, 1, "hi").unwrap(), "a retry is not a new message");

        s.message_out(&peer, 7, 2, "same id, other direction").unwrap();
        s.message_out(&peer, u64::MAX, 3, "ids use the whole u64").unwrap();
        let outbox = s.outbox(&peer).unwrap();
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox[1].id, u64::MAX);

        assert!(s.message_delivered(&peer, 7).unwrap());
        assert!(!s.message_delivered(&peer, 7).unwrap(), "acks are idempotent");
        assert_eq!(s.outbox(&peer).unwrap().len(), 1);

        let history = s.history(&peer, 10).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].body, "hi", "history is oldest first");
        assert_eq!(s.history(&peer, 1).unwrap()[0].id, u64::MAX, "limit keeps the newest");
    }

    #[test]
    fn a_version_one_database_is_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v1.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch(
                "CREATE TABLE identity (id INTEGER PRIMARY KEY CHECK (id = 1), secret BLOB NOT NULL, created_at INTEGER NOT NULL);
                 CREATE TABLE bookmarks (id TEXT PRIMARY KEY, scope INTEGER NOT NULL, title TEXT NOT NULL, ticket TEXT NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE blobs (hash BLOB PRIMARY KEY, name TEXT NOT NULL, size INTEGER NOT NULL, added_at INTEGER NOT NULL);
                 INSERT INTO bookmarks VALUES ('kept', 4, 'Kept', 'lproomx', 1);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let mut s = Store::open(Some(&path)).unwrap();
        assert_eq!(s.bookmarks().unwrap()[0].id, "kept", "v1 data survives");
        s.friend_add(&[1u8; 32], "New", None).unwrap();
    }

    #[test]
    fn schema_version_is_recorded() {
        let s = Store::open(None).unwrap();
        let v: i64 = s.db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
