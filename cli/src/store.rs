//! The append-only event log, `.madelog/log.db`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use crate::chain::{self, Verification};
use crate::event::{canonical_json, Event, EventKind, Payload};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq        INTEGER PRIMARY KEY,
    id         TEXT NOT NULL UNIQUE,
    ts         TEXT NOT NULL,
    kind       TEXT NOT NULL,
    commit_sha TEXT,
    payload    TEXT NOT NULL,
    prev_hash  TEXT NOT NULL,
    hash       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_kind_idx       ON events(kind);
CREATE INDEX IF NOT EXISTS events_commit_sha_idx ON events(commit_sha);
CREATE INDEX IF NOT EXISTS events_ts_idx         ON events(ts);

-- The log is append-only. Tampering is still possible with a sqlite shell,
-- but it has to be deliberate, and `madelog verify` will see it.
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'madelog: events is append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'madelog: events is append-only'); END;
"#;

const COLUMNS: &str = "seq, id, ts, kind, commit_sha, payload, prev_hash, hash";

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    /// Open (creating if needed) the log at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        conn.execute_batch(SCHEMA)
            .with_context(|| format!("preparing {}", path.display()))?;
        Ok(Store {
            conn,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event, linking it to the current last event.
    pub fn append(
        &self,
        kind: EventKind,
        commit_sha: Option<&str>,
        payload: serde_json::Value,
    ) -> Result<Event> {
        self.append_at(crate::event::now_ts(), kind, commit_sha, payload)
    }

    /// Same as [`Store::append`] with an explicit timestamp.
    pub fn append_at(
        &self,
        ts: String,
        kind: EventKind,
        commit_sha: Option<&str>,
        payload: serde_json::Value,
    ) -> Result<Event> {
        let tx = self.conn.unchecked_transaction()?;
        let prev = self.last()?;
        let event = chain::next_event(
            prev.as_ref(),
            crate::event::new_id(),
            ts,
            kind,
            commit_sha.map(str::to_string),
            payload,
        );
        tx.execute(
            "INSERT INTO events (seq, id, ts, kind, commit_sha, payload, prev_hash, hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                event.seq,
                event.id,
                event.ts,
                event.kind.as_str(),
                event.commit_sha,
                canonical_json(&event.payload),
                event.prev_hash,
                event.hash,
            ],
        )
        .context("appending event")?;
        tx.commit()?;
        Ok(event)
    }

    /// Append a typed payload; the kind comes from the payload itself.
    pub fn append_payload(&self, commit_sha: Option<&str>, payload: &Payload) -> Result<Event> {
        self.append(payload.kind(), commit_sha, payload.to_value())
    }

    pub fn all(&self) -> Result<Vec<Event>> {
        let sql = format!("SELECT {COLUMNS} FROM events ORDER BY seq ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_event)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row??);
        }
        Ok(out)
    }

    pub fn last(&self) -> Result<Option<Event>> {
        let sql = format!("SELECT {COLUMNS} FROM events ORDER BY seq DESC LIMIT 1");
        let found = self
            .conn
            .query_row(&sql, [], row_to_event)
            .optional()?
            .transpose()?;
        Ok(found)
    }

    pub fn last_of_kind(&self, kind: EventKind) -> Result<Option<Event>> {
        let sql = format!("SELECT {COLUMNS} FROM events WHERE kind = ?1 ORDER BY seq DESC LIMIT 1");
        let found = self
            .conn
            .query_row(&sql, [kind.as_str()], row_to_event)
            .optional()?
            .transpose()?;
        Ok(found)
    }

    pub fn first_of_kind(&self, kind: EventKind) -> Result<Option<Event>> {
        let sql = format!("SELECT {COLUMNS} FROM events WHERE kind = ?1 ORDER BY seq ASC LIMIT 1");
        let found = self
            .conn
            .query_row(&sql, [kind.as_str()], row_to_event)
            .optional()?
            .transpose()?;
        Ok(found)
    }

    pub fn by_id(&self, id: &str) -> Result<Option<Event>> {
        let sql = format!("SELECT {COLUMNS} FROM events WHERE id = ?1");
        let found = self
            .conn
            .query_row(&sql, [id], row_to_event)
            .optional()?
            .transpose()?;
        Ok(found)
    }

    /// Has this commit already been recorded? The post-commit hook can run twice.
    pub fn has_commit(&self, sha: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'commit' AND commit_sha = ?1",
            [sha],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Does this commit already have AI sessions recorded against it?
    ///
    /// What makes `madelog capture --record` idempotent: a commit that has
    /// any session is left exactly as it is.
    pub fn has_ai_sessions(&self, sha: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'ai_session' AND commit_sha = ?1",
            [sha],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Every commit in the log, oldest first.
    pub fn recorded_commits(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT commit_sha FROM events
             WHERE kind = 'commit' AND commit_sha IS NOT NULL ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for sha in rows {
            let sha = sha?;
            if !out.contains(&sha) {
                out.push(sha);
            }
        }
        Ok(out)
    }

    pub fn verify(&self) -> Result<Verification> {
        Ok(chain::verify(&self.all()?))
    }

    pub fn root(&self) -> Result<String> {
        Ok(chain::root(&self.all()?))
    }

    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

type RowResult = rusqlite::Result<Result<Event>>;

fn row_to_event(row: &rusqlite::Row<'_>) -> RowResult {
    let seq: i64 = row.get(0)?;
    let id: String = row.get(1)?;
    let ts: String = row.get(2)?;
    let kind_raw: String = row.get(3)?;
    let commit_sha: Option<String> = row.get(4)?;
    let payload_raw: String = row.get(5)?;
    let prev_hash: String = row.get(6)?;
    let hash: String = row.get(7)?;

    Ok((|| {
        let kind: EventKind = kind_raw
            .parse()
            .with_context(|| format!("event {seq} has an unknown kind"))?;
        let payload: serde_json::Value = serde_json::from_str(&payload_raw)
            .with_context(|| format!("event {seq} has an unreadable payload"))?;
        Ok(Event {
            seq,
            id,
            ts,
            kind,
            commit_sha,
            payload,
            prev_hash,
            hash,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DecisionPayload, IntentPayload};

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Store::open(&dir.path().join("nested").join("log.db")).unwrap();
        (dir, store)
    }

    fn decision(text: &str) -> Payload {
        Payload::Decision(DecisionPayload {
            text: text.to_string(),
            private: false,
            amends: None,
        })
    }

    #[test]
    fn append_links_events_into_a_chain() {
        let (_d, store) = temp_store();
        let a = store.append_payload(None, &decision("one")).unwrap();
        let b = store
            .append_payload(Some(&"a".repeat(40)), &decision("two"))
            .unwrap();

        assert_eq!(a.seq, 1);
        assert_eq!(b.seq, 2);
        assert_eq!(b.prev_hash, a.hash);
        assert_eq!(b.commit_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert!(store.verify().unwrap().is_ok());
        assert_eq!(store.root().unwrap(), b.hash);
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn events_survive_a_round_trip_through_sqlite() {
        let (_d, store) = temp_store();
        let intent = Payload::Intent(IntentPayload {
            what: "a thing".into(),
            for_whom: "someone".into(),
            tools: vec!["claude-code".into()],
            record_ai_sessions: true,
        });
        let written = store.append_payload(None, &intent).unwrap();
        let read = store.all().unwrap().pop().unwrap();
        assert_eq!(written, read);
        assert_eq!(read.payload_as::<IntentPayload>().unwrap().what, "a thing");
    }

    #[test]
    fn lookups_by_id_kind_and_commit() {
        let (_d, store) = temp_store();
        let sha = "b".repeat(40);
        let a = store.append_payload(None, &decision("one")).unwrap();
        store
            .append(
                EventKind::Commit,
                Some(&sha),
                serde_json::json!({
                    "sha": sha, "message": "m", "author": "a",
                    "parents": [], "files": [], "secs_since_prev": null
                }),
            )
            .unwrap();

        assert_eq!(store.by_id(&a.id).unwrap().unwrap(), a);
        assert!(store.by_id("nope").unwrap().is_none());
        assert_eq!(
            store.last_of_kind(EventKind::Decision).unwrap().unwrap().id,
            a.id
        );
        assert!(store.first_of_kind(EventKind::Intent).unwrap().is_none());
        assert!(store.has_commit(&sha).unwrap());
        assert!(!store.has_commit(&"c".repeat(40)).unwrap());
        assert_eq!(store.recorded_commits().unwrap(), vec![sha.clone()]);
        assert!(!store.has_ai_sessions(&sha).unwrap());

        store
            .append(
                EventKind::AiSession,
                Some(&sha),
                serde_json::json!({
                    "tool": "claude-code", "model": null, "session_id": "s",
                    "prompt": null, "response_excerpt": null, "files": [], "accepted": false
                }),
            )
            .unwrap();
        assert!(store.has_ai_sessions(&sha).unwrap());
        assert!(!store.has_ai_sessions(&"c".repeat(40)).unwrap());
    }

    #[test]
    fn the_table_is_append_only() {
        let (_d, store) = temp_store();
        store.append_payload(None, &decision("one")).unwrap();
        assert!(store
            .conn
            .execute("UPDATE events SET ts = 'x' WHERE seq = 1", [])
            .is_err());
        assert!(store.conn.execute("DELETE FROM events", []).is_err());
    }

    #[test]
    fn payloads_are_stored_canonically() {
        let (_d, store) = temp_store();
        store
            .append(
                EventKind::Decision,
                None,
                serde_json::json!({"text": "z", "private": false}),
            )
            .unwrap();
        let raw: String = store
            .conn
            .query_row("SELECT payload FROM events WHERE seq = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw, r#"{"private":false,"text":"z"}"#);
    }
}
