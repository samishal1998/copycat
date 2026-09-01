//! Persistent history: SQLite for metadata, sealed blobs for payloads.
//!
//! Representations are serialized together and encrypted as one blob, so the
//! database never learns that a clip was an image of a password rather than a
//! password. Media types stay in the clear because the daemon has to filter on
//! them without decrypting, and a media type is not the secret.

pub mod crypto;

use anyhow::{Context, Result};
use copycat_core::{
    ClipEvent, ClipId, ClipPayload, ClipSource, ClipSummary, ContentHash, PREVIEW_CHARS,
};
use rusqlite::{Connection, OptionalExtension, params};

pub use crypto::{KeyStorage, PayloadCipher};

/// Bump when the schema changes, and add a migration below.
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug)]
pub struct Store {
    conn: Connection,
    cipher: PayloadCipher,
}

impl Store {
    pub fn open(path: &std::path::Path, cipher: PayloadCipher) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening {}", path.display()))?;
        Self::from_connection(conn, cipher)
    }

    #[cfg(test)]
    pub fn in_memory(cipher: PayloadCipher) -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, cipher)
    }

    fn from_connection(conn: Connection, cipher: PayloadCipher) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let store = Store { conn, cipher };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version    INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );",
        )?;

        let current: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0))?;

        if current > SCHEMA_VERSION {
            anyhow::bail!(
                "the history database is at schema version {current}, newer than this build's {SCHEMA_VERSION}"
            );
        }
        if current >= SCHEMA_VERSION {
            return Ok(());
        }

        if current < 1 {
            self.conn.execute_batch(
                "CREATE TABLE clip_events (
                     id           INTEGER PRIMARY KEY,
                     captured_at  INTEGER NOT NULL,
                     content_hash TEXT    NOT NULL,
                     media_types  TEXT    NOT NULL,
                     byte_len     INTEGER NOT NULL,
                     pinned       INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX idx_clip_events_captured_at ON clip_events(captured_at DESC);

                 -- One sealed blob per clip: every representation together, so
                 -- the row shape leaks nothing about the contents.
                 CREATE TABLE clip_payloads (
                     clip_id    INTEGER PRIMARY KEY
                                REFERENCES clip_events(id) ON DELETE CASCADE,
                     nonce      BLOB NOT NULL,
                     ciphertext BLOB NOT NULL
                 );",
            )?;
        }

        self.conn.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![SCHEMA_VERSION, now_ms()],
        )?;
        Ok(())
    }

    pub fn insert(&self, event: &ClipEvent) -> Result<()> {
        let plaintext = serde_json::to_vec(&event.payload)?;
        let (nonce, ciphertext) = self.cipher.seal(&plaintext)?;

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO clip_events
                 (id, captured_at, content_hash, media_types, byte_len, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id.0 as i64,
                event.captured_at,
                event.content_hash.to_hex(),
                event.payload.media_types().join(","),
                event.payload.byte_len() as i64,
                event.pinned as i64,
            ],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO clip_payloads (clip_id, nonce, ciphertext) VALUES (?1, ?2, ?3)",
            params![event.id.0 as i64, nonce, ciphertext],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The newest `limit` events, returned oldest first so they can be replayed
    /// into hot history in order.
    pub fn recent(&self, limit: usize) -> Result<Vec<ClipEvent>> {
        let mut statement = self.conn.prepare(
            "SELECT e.id, e.captured_at, e.content_hash, e.pinned, p.nonce, p.ciphertext
               FROM clip_events e
               JOIN clip_payloads p ON p.clip_id = e.id
              ORDER BY e.id DESC
              LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (id, captured_at, hash, pinned, nonce, ciphertext) = row?;
            let payload: ClipPayload = match self
                .cipher
                .unseal(&nonce, &ciphertext)
                .and_then(|bytes| Ok(serde_json::from_slice(&bytes)?))
            {
                Ok(payload) => payload,
                Err(error) => {
                    // A row we cannot decrypt is almost always a key that
                    // changed underneath us. Skip it and keep going: refusing to
                    // start would make the situation unrecoverable from the CLI.
                    tracing::warn!(clip_id = id, error = %error, "skipping unreadable clip");
                    continue;
                }
            };
            events.push(ClipEvent {
                id: ClipId(id as u64),
                captured_at,
                source: ClipSource::External,
                content_hash: ContentHash::from_hex(&hash).unwrap_or(payload.content_hash()),
                payload,
                pinned: pinned != 0,
            });
        }
        events.reverse();
        Ok(events)
    }

    /// Summaries of persisted clips older than `before`, newest first.
    ///
    /// Lets `history list` see past the hot window without loading everything:
    /// hot history answers the common case, and this fills the tail.
    pub fn older_than(&self, before: Option<ClipId>, limit: usize) -> Result<Vec<ClipSummary>> {
        let cutoff = before.map(|id| id.0 as i64).unwrap_or(i64::MAX);
        let mut statement = self.conn.prepare(
            "SELECT e.id, e.captured_at, e.content_hash, e.pinned, p.nonce, p.ciphertext
               FROM clip_events e
               JOIN clip_payloads p ON p.clip_id = e.id
              WHERE e.id < ?1
              ORDER BY e.id DESC
              LIMIT ?2",
        )?;
        let mut rows = statement.query(params![cutoff, limit as i64])?;

        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let nonce: Vec<u8> = row.get(4)?;
            let ciphertext: Vec<u8> = row.get(5)?;
            let Ok(plaintext) = self.cipher.unseal(&nonce, &ciphertext) else { continue };
            let Ok(payload) = serde_json::from_slice::<ClipPayload>(&plaintext) else { continue };
            let hash: String = row.get(2)?;
            out.push(ClipSummary {
                id: ClipId(row.get::<_, i64>(0)? as u64),
                captured_at: row.get(1)?,
                content_hash: ContentHash::from_hex(&hash).unwrap_or(payload.content_hash()),
                media_types: payload.media_types(),
                byte_len: payload.byte_len(),
                preview: payload.preview(PREVIEW_CHARS),
                pinned: row.get::<_, i64>(3)? != 0,
                duplicate_run: 1,
            });
        }
        Ok(out)
    }

    pub fn get(&self, id: ClipId) -> Result<Option<ClipEvent>> {
        let row = self
            .conn
            .query_row(
                "SELECT e.captured_at, e.content_hash, e.pinned, p.nonce, p.ciphertext
                   FROM clip_events e
                   JOIN clip_payloads p ON p.clip_id = e.id
                  WHERE e.id = ?1",
                params![id.0 as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()?;

        let Some((captured_at, hash, pinned, nonce, ciphertext)) = row else {
            return Ok(None);
        };
        let payload: ClipPayload = serde_json::from_slice(&self.cipher.unseal(&nonce, &ciphertext)?)?;
        Ok(Some(ClipEvent {
            id,
            captured_at,
            source: ClipSource::External,
            content_hash: ContentHash::from_hex(&hash).unwrap_or(payload.content_hash()),
            payload,
            pinned: pinned != 0,
        }))
    }

    /// Search persisted text, newest first, decrypting at most `scan_limit`
    /// payloads (R18). The flag says whether the scan stopped early.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        scan_limit: usize,
    ) -> Result<(Vec<ClipSummary>, bool)> {
        let needle = query.to_lowercase();
        let mut statement = self.conn.prepare(
            "SELECT e.id, e.captured_at, e.content_hash, e.pinned, p.nonce, p.ciphertext
               FROM clip_events e
               JOIN clip_payloads p ON p.clip_id = e.id
              ORDER BY e.id DESC
              LIMIT ?1",
        )?;
        let mut rows = statement.query(params![scan_limit as i64])?;

        let mut hits = Vec::new();
        let mut scanned = 0usize;
        while let Some(row) = rows.next()? {
            scanned += 1;
            let id: i64 = row.get(0)?;
            let nonce: Vec<u8> = row.get(4)?;
            let ciphertext: Vec<u8> = row.get(5)?;
            let Ok(plaintext) = self.cipher.unseal(&nonce, &ciphertext) else {
                continue;
            };
            let Ok(payload) = serde_json::from_slice::<ClipPayload>(&plaintext) else {
                continue;
            };
            let Some(text) = payload.as_text() else { continue };
            if !text.to_lowercase().contains(&needle) {
                continue;
            }
            let hash: String = row.get(2)?;
            hits.push(ClipSummary {
                id: ClipId(id as u64),
                captured_at: row.get(1)?,
                content_hash: ContentHash::from_hex(&hash).unwrap_or(payload.content_hash()),
                media_types: payload.media_types(),
                byte_len: payload.byte_len(),
                preview: payload.preview(PREVIEW_CHARS),
                pinned: row.get::<_, i64>(3)? != 0,
                duplicate_run: 1,
            });
            if hits.len() >= limit {
                break;
            }
        }

        let truncated = scanned >= scan_limit && hits.len() < limit;
        Ok((hits, truncated))
    }

    pub fn delete(&self, id: ClipId) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM clip_events WHERE id = ?1", params![id.0 as i64])?
            > 0)
    }

    pub fn clear(&self, keep_pinned: bool) -> Result<usize> {
        let removed = if keep_pinned {
            self.conn.execute("DELETE FROM clip_events WHERE pinned = 0", [])?
        } else {
            self.conn.execute("DELETE FROM clip_events", [])?
        };
        Ok(removed)
    }

    pub fn set_pinned(&self, id: ClipId, pinned: bool) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE clip_events SET pinned = ?2 WHERE id = ?1",
            params![id.0 as i64, pinned as i64],
        )? > 0)
    }

    /// Drop unpinned history older than the retention period.
    ///
    /// Pinned items are exempt: a retention policy is about forgetting what
    /// accumulated, not about discarding what the user deliberately kept.
    pub fn prune(&self, retention_days: u32, now: i64) -> Result<usize> {
        if retention_days == 0 {
            return Ok(0);
        }
        let cutoff = now - (retention_days as i64) * 86_400_000;
        Ok(self.conn.execute(
            "DELETE FROM clip_events WHERE pinned = 0 AND captured_at < ?1",
            params![cutoff],
        )?)
    }

    pub fn count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row("SELECT COUNT(*) FROM clip_events", [], |r| r.get(0))?;
        Ok(count as usize)
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::in_memory(PayloadCipher::from_key([3u8; 32])).unwrap()
    }

    fn event(id: u64, text: &str, at: i64) -> ClipEvent {
        let payload = ClipPayload::text(text);
        ClipEvent {
            id: ClipId(id),
            captured_at: at,
            source: ClipSource::External,
            content_hash: payload.content_hash(),
            payload,
            pinned: false,
        }
    }

    #[test]
    fn events_round_trip_through_encryption() {
        let store = store();
        store.insert(&event(1, "postgres://localhost", 100)).unwrap();

        let restored = store.get(ClipId(1)).unwrap().unwrap();
        assert_eq!(restored.payload.as_text(), Some("postgres://localhost"));
        assert_eq!(restored.captured_at, 100);
        assert_eq!(restored.content_hash, event(1, "postgres://localhost", 100).content_hash);
    }

    #[test]
    fn payload_text_is_not_stored_in_the_clear() {
        // The point of ADR-007: grepping the database must not find the value.
        let store = store();
        store.insert(&event(1, "hunter2-the-password", 100)).unwrap();

        let blob: Vec<u8> = store
            .conn
            .query_row("SELECT ciphertext FROM clip_payloads", [], |r| r.get(0))
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&blob).contains("hunter2"),
            "payload text must not be readable in the stored blob"
        );
    }

    #[test]
    fn recent_returns_oldest_first_for_replay() {
        let store = store();
        for (id, text) in [(1, "A"), (2, "B"), (3, "C")] {
            store.insert(&event(id, text, id as i64)).unwrap();
        }
        let events = store.recent(2).unwrap();
        let texts: Vec<_> = events.iter().map(|e| e.payload.as_text().unwrap()).collect();
        assert_eq!(texts, ["B", "C"], "newest two, in replay order");
    }

    #[test]
    fn deleting_an_event_takes_its_payload_with_it() {
        let store = store();
        store.insert(&event(1, "A", 1)).unwrap();
        assert!(store.delete(ClipId(1)).unwrap());

        let orphans: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM clip_payloads", [], |r| r.get(0))
            .unwrap();
        assert_eq!(orphans, 0, "the cascade must not leave the ciphertext behind");
    }

    #[test]
    fn clear_can_spare_pinned_items() {
        let store = store();
        store.insert(&event(1, "A", 1)).unwrap();
        store.insert(&event(2, "B", 2)).unwrap();
        store.set_pinned(ClipId(2), true).unwrap();

        assert_eq!(store.clear(true).unwrap(), 1);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn retention_spares_pinned_items() {
        let store = store();
        let now = 90 * 86_400_000 + 1_000;
        store.insert(&event(1, "old", 0)).unwrap();
        store.insert(&event(2, "old but kept", 0)).unwrap();
        store.insert(&event(3, "recent", now)).unwrap();
        store.set_pinned(ClipId(2), true).unwrap();

        assert_eq!(store.prune(90, now).unwrap(), 1);
        assert!(store.get(ClipId(2)).unwrap().is_some());
        assert!(store.get(ClipId(3)).unwrap().is_some());
    }

    #[test]
    fn retention_of_zero_days_keeps_everything() {
        let store = store();
        store.insert(&event(1, "A", 0)).unwrap();
        assert_eq!(store.prune(0, now_ms()).unwrap(), 0);
    }

    #[test]
    fn search_reports_truncation_when_it_hits_the_scan_bound() {
        // R18: the caller must be able to tell "no matches" from "stopped
        // looking".
        let store = store();
        for id in 1..=10 {
            store.insert(&event(id, &format!("entry {id}"), id as i64)).unwrap();
        }

        let (hits, truncated) = store.search("entry", 100, 3).unwrap();
        assert_eq!(hits.len(), 3);
        assert!(truncated);

        let (hits, truncated) = store.search("entry", 100, 100).unwrap();
        assert_eq!(hits.len(), 10);
        assert!(!truncated);
    }

    #[test]
    fn older_than_pages_backwards_from_a_cursor() {
        let store = store();
        for id in 1..=5 {
            store.insert(&event(id, &format!("clip {id}"), id as i64)).unwrap();
        }
        let page = store.older_than(Some(ClipId(4)), 2).unwrap();
        let ids: Vec<_> = page.iter().map(|c| c.id.0).collect();
        assert_eq!(ids, [3, 2], "newest first, strictly older than the cursor");

        let head = store.older_than(None, 2).unwrap();
        assert_eq!(head.iter().map(|c| c.id.0).collect::<Vec<_>>(), [5, 4]);
    }

    #[test]
    fn search_is_newest_first() {
        let store = store();
        store.insert(&event(1, "match one", 1)).unwrap();
        store.insert(&event(2, "match two", 2)).unwrap();
        let (hits, _) = store.search("match", 10, 100).unwrap();
        assert_eq!(hits[0].id, ClipId(2));
    }

    #[test]
    fn an_undecryptable_row_is_skipped_rather_than_fatal() {
        // A key that changed must not make the daemon unstartable.
        let store = store();
        store.insert(&event(1, "readable", 1)).unwrap();
        store
            .conn
            .execute("UPDATE clip_payloads SET ciphertext = X'00010203' WHERE clip_id = 1", [])
            .unwrap();

        assert!(store.recent(10).unwrap().is_empty());
    }

    #[test]
    fn a_newer_schema_is_refused_rather_than_written_to() {
        let cipher = PayloadCipher::from_key([3u8; 32]);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);
             INSERT INTO schema_migrations VALUES (99, 0);",
        )
        .unwrap();

        let error = Store::from_connection(conn, cipher).unwrap_err().to_string();
        assert!(error.contains("99"), "{error}");
    }

    #[test]
    fn migrations_are_idempotent() {
        let store = store();
        store.migrate().unwrap();
        store.migrate().unwrap();
        let count: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
