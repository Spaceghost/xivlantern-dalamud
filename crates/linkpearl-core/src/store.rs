//! The SQLite side: identity, bookmarks and a note of what we have hashed.
//!
//! One connection behind a mutex, `PRAGMA user_version` migrations, every call
//! short. The identity row is the whole reason this file exists: a player's
//! node id has to survive a restart or every bookmark anyone made of them dies.

use std::path::Path;

use iroh::SecretKey;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{ticket::Scope, Error, Result};

pub const SCHEMA_VERSION: i64 = 1;

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

fn store_err(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}

fn now_ms() -> i64 {
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
        Ok(())
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
    fn schema_version_is_recorded() {
        let s = Store::open(None).unwrap();
        let v: i64 = s.db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }
}
