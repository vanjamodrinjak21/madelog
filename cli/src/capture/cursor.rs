//! Cursor: metadata only, and honest about it.
//!
//! Cursor 3.20 keeps conversation bodies encrypted or server-side. The
//! `composerData:<uuid>` rows on disk are shells: empty `conversationMap`,
//! empty `codeBlockData`, empty `text`. What is readable is the header of
//! each composer session — when it ran, which model, how many lines it
//! claims to have added — and which workspace folder it belonged to.
//!
//! So a Cursor session on a madelog page says that it happened, and nothing
//! it cannot back up. No prompt, no writes, no attributed lines.
//!
//! ```text
//! <User>/workspaceStorage/<hash>/workspace.json   {"folder": "file:///…"}
//! <User>/globalStorage/state.vscdb                composerHeaders(...)
//! ```
//!
//! The database is live, so it is copied — together with its `-wal` and
//! `-shm` companions, or the copy loses whatever has not been checkpointed —
//! into a temporary directory and opened read-only. Cursor's own file is
//! never opened for writing and never locked.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{
    from_epoch_millis, is_within, overlaps, Adapter, CapturedSession, Detection, CURSOR_NOTE,
};

/// Overrides Cursor's `User` directory, so tests never read a real one.
pub const HOME_ENV: &str = "MADELOG_CURSOR_HOME";

pub struct Cursor;

impl Adapter for Cursor {
    fn name(&self) -> &str {
        "cursor"
    }

    fn detect(&self) -> Detection {
        match global_db() {
            Some(db) if db.is_file() => Detection::found(CURSOR_NOTE),
            _ => Detection::missing("no Cursor storage found, so nothing to read"),
        }
    }

    fn sessions(
        &self,
        repo_root: &Path,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<CapturedSession> {
        headers_for(repo_root)
            .into_iter()
            .filter(|s| overlaps(s.started, s.ended, since, until))
            .collect()
    }

    fn touching(&self, repo_root: &Path, _since: DateTime<Utc>, _until: DateTime<Utc>) -> usize {
        headers_for(repo_root).len()
    }
}

/// Cursor's `User` directory, or whatever [`HOME_ENV`] names.
pub fn user_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        if !dir.is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    let home = dirs::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User")
    } else if cfg!(windows) {
        dirs::config_dir()?.join("Cursor").join("User")
    } else {
        home.join(".config").join("Cursor").join("User")
    })
}

pub fn global_db() -> Option<PathBuf> {
    Some(user_dir()?.join("globalStorage").join("state.vscdb"))
}

pub fn workspace_storage() -> Option<PathBuf> {
    Some(user_dir()?.join("workspaceStorage"))
}

/// The `workspaceStorage` directory names whose `workspace.json` points at
/// this repository.
fn workspace_ids(repo_root: &Path) -> Vec<String> {
    let Some(dir) = workspace_storage() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(path.join("workspace.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        let Some(folder) = value.get("folder").and_then(Value::as_str) else {
            continue;
        };
        if let Some(folder) = file_url_to_path(folder) {
            if is_within(repo_root, &folder) {
                if let Some(name) = path.file_name() {
                    out.push(name.to_string_lossy().into_owned());
                }
            }
        }
    }
    out.sort();
    out
}

/// `file:///Users/v/CODING%20PROJECTS/x` → `/Users/v/CODING PROJECTS/x`.
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file://host/path` is not something Cursor writes, but an empty
    // authority leaves a leading `/` that is part of the path.
    let path = percent_decode(rest);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every composer session recorded against one of this repository's
/// workspaces.
fn headers_for(repo_root: &Path) -> Vec<CapturedSession> {
    let ids = workspace_ids(repo_root);
    if ids.is_empty() {
        return Vec::new();
    }
    let Some(db) = global_db() else {
        return Vec::new();
    };
    let Some(copy) = CopiedDb::of(&db) else {
        return Vec::new();
    };
    let mut out = read_headers(&copy.path(), &ids);
    out.sort_by(|a, b| {
        a.started
            .cmp(&b.started)
            .then(a.session_id.cmp(&b.session_id))
    });
    out
}

fn read_headers(db: &Path, workspace_ids: &[String]) -> Vec<CapturedSession> {
    let Ok(conn) = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT composerId, workspaceId, createdAt, lastUpdatedAt, value \
         FROM composerHeaders ORDER BY createdAt ASC",
    ) else {
        // No such table: an older Cursor, or a database we do not know.
        return Vec::new();
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0).unwrap_or_default(),
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, i64>(2).unwrap_or_default(),
            row.get::<_, i64>(3).unwrap_or_default(),
            row.get::<_, String>(4).unwrap_or_default(),
        ))
    });
    let Ok(rows) = rows else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for row in rows.flatten() {
        let (composer_id, workspace_id, created, updated, raw) = row;
        if composer_id.is_empty() || !workspace_ids.contains(&workspace_id) {
            continue;
        }
        let Some(started) = from_epoch_millis(created) else {
            continue;
        };
        let ended = from_epoch_millis(updated).unwrap_or(started).max(started);
        let value: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
        out.push(CapturedSession {
            tool: "cursor".to_string(),
            session_id: composer_id,
            model: model_of(&value),
            started,
            ended,
            prompt: None,
            prompt_count: 0,
            title: value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            writes: Vec::new(),
            // Cursor cannot say what it wrote, so an empty list here means
            // "no record", not "nothing".
            writes_recorded: false,
            lines_added_hint: value.get("totalLinesAdded").and_then(Value::as_u64),
        });
    }
    out
}

fn model_of(value: &Value) -> Option<String> {
    value
        .get("modelConfig")
        .and_then(|m| m.get("modelName"))
        .or_else(|| value.get("modelName"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// A copy of a live SQLite database, with its write-ahead log, deleted when
/// it goes out of scope.
struct CopiedDb {
    dir: tempfile::TempDir,
    name: String,
}

impl CopiedDb {
    fn of(source: &Path) -> Option<CopiedDb> {
        if !source.is_file() {
            return None;
        }
        let dir = tempfile::TempDir::new().ok()?;
        let name = source.file_name()?.to_string_lossy().into_owned();
        std::fs::copy(source, dir.path().join(&name)).ok()?;
        // Without these the copy is whatever was last checkpointed, which on
        // Cursor is most of a megabyte behind.
        for suffix in ["-wal", "-shm"] {
            let companion = source.with_file_name(format!("{name}{suffix}"));
            if companion.is_file() {
                let _ = std::fs::copy(&companion, dir.path().join(format!("{name}{suffix}")));
            }
        }
        Some(CopiedDb { dir, name })
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_url_decodes_to_a_path() {
        assert_eq!(
            file_url_to_path("file:///Users/v/CODING%20PROJECTS/x"),
            Some(PathBuf::from("/Users/v/CODING PROJECTS/x"))
        );
        assert_eq!(
            file_url_to_path("file:///plain/path"),
            Some(PathBuf::from("/plain/path"))
        );
        assert_eq!(file_url_to_path("/not/a/url"), None);
        assert_eq!(file_url_to_path("file://"), None);
    }

    #[test]
    fn a_broken_escape_is_left_as_it_stands() {
        assert_eq!(percent_decode("a%zzb"), "a%zzb");
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
    }

    #[test]
    fn a_model_is_read_from_either_shape() {
        let nested = serde_json::json!({"modelConfig": {"modelName": "claude-4.5"}});
        assert_eq!(model_of(&nested).as_deref(), Some("claude-4.5"));
        let flat = serde_json::json!({"modelName": "gpt-5"});
        assert_eq!(model_of(&flat).as_deref(), Some("gpt-5"));
        assert_eq!(model_of(&serde_json::json!({})), None);
        assert_eq!(model_of(&serde_json::json!({"modelName": " "})), None);
    }

    #[test]
    fn a_database_without_the_table_reads_as_no_sessions() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("state.vscdb");
        Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE other (a TEXT)")
            .unwrap();
        assert!(read_headers(&db, &["ws".to_string()]).is_empty());
        assert!(read_headers(&dir.path().join("gone.vscdb"), &[]).is_empty());
    }

    #[test]
    fn a_copy_carries_the_write_ahead_log() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("state.vscdb");
        std::fs::write(&db, "main").unwrap();
        std::fs::write(dir.path().join("state.vscdb-wal"), "wal").unwrap();
        let copy = CopiedDb::of(&db).unwrap();
        assert_eq!(std::fs::read_to_string(copy.path()).unwrap(), "main");
        assert_eq!(
            std::fs::read_to_string(copy.path().with_file_name("state.vscdb-wal")).unwrap(),
            "wal"
        );
        assert!(CopiedDb::of(&dir.path().join("nope.vscdb")).is_none());
    }
}
