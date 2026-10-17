//! The whole database, as post-images.
//!
//! # Why a whole-table snapshot and not a delta
//!
//! At ~2.7 MB the whole thing gzips to a few hundred KB over a tailnet, and a
//! delta is not worth its correctness risk. Three concrete reasons, each of
//! which produces silent divergence rather than an error:
//!
//! * a watermark cannot see a HARD DELETE — the row is simply gone, so a
//!   `WHERE updated_at > ?` returns nothing and the receiver keeps it forever;
//! * `datetime('now')` is second-resolution, so a `>` cursor skips any row
//!   written in the same second as the cursor;
//! * `tags`, `task_tags`, `task_deps` and `project_repos` have **no
//!   `updated_at` at all** — there is nothing to compare.
//!
//! The `since` parameter exists anyway and is always answered "full". Designing
//! the shape now costs nothing; retrofitting a cursor onto a deployed protocol
//! costs a version bump on both ends.

use std::collections::BTreeMap;

use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::sync::tables;

/// One table's rows, column-oriented header plus positional rows.
///
/// Positional rather than a map per row, because the row count is in the
/// thousands and repeating every column name per row triples the payload for
/// nothing. The header is what makes it readable at the other end.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableSnapshot {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Everything a peer needs to become identical to this database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// The sender's schema version. A receiver on a different one must refuse
    /// rather than apply — columns silently not travelling is the failure mode
    /// whole-row sync cannot detect on its own.
    pub schema_version: i32,
    /// Who took it. Not a trust boundary — the token is — but it is what makes
    /// "paired with the wrong machine" diagnosable.
    pub host: String,
    pub taken_at: String,
    /// Always `false` in v1. Present so the field exists on the wire before
    /// anything depends on it.
    pub partial: bool,
    pub tables: BTreeMap<String, TableSnapshot>,
    /// The `app_meta` allowlist, extracted rather than sent as a table.
    pub meta: BTreeMap<String, String>,
}

/// Read one table, minus any host-local columns.
fn read_table(conn: &Connection, table: &str) -> Result<TableSnapshot, AppError> {
    read_table_where(conn, table, None, &[])
}

/// The same read, optionally narrowed.
///
/// Shared with the sweep rather than copied, because the column-skipping and
/// the JSON encoding are the parts that must not diverge: a sweep that emitted
/// `reminded_at` would push a host-local column the snapshot is careful to
/// withhold, and nothing downstream would notice.
///
/// `where_sql` is built from the caller's own string constants — never from
/// anything a user typed — and the values it compares against are bound.
pub(crate) fn read_table_where(
    conn: &Connection,
    table: &str,
    where_sql: Option<&str>,
    params: &[&dyn rusqlite::ToSql],
) -> Result<TableSnapshot, AppError> {
    let skip: Vec<&str> = tables::skipped_columns(table).collect();

    // Column names come from the STATEMENT rather than from a hand-written
    // list, so a migration that adds a column starts syncing it with no edit
    // here. That is the half of "classify at the table level" that keeps
    // working after the classification is written.
    let clause = where_sql.map(|w| format!(" WHERE {w}")).unwrap_or_default();
    let mut stmt = conn.prepare(&format!("SELECT * FROM {table}{clause}"))?;
    let all: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
    let keep: Vec<usize> = (0..all.len()).filter(|i| !skip.contains(&all[*i].as_str())).collect();
    let columns: Vec<String> = keep.iter().map(|i| all[*i].clone()).collect();

    let mut rows = Vec::new();
    let mut cursor = stmt.query(params)?;
    while let Some(row) = cursor.next()? {
        let mut out = Vec::with_capacity(keep.len());
        for &i in &keep {
            out.push(to_json(row.get_ref(i)?, table, &all[i])?);
        }
        rows.push(out);
    }
    Ok(TableSnapshot { columns, rows })
}

/// A SQLite value as JSON.
///
/// A BLOB is a deliberate hard error rather than a base64 fallback. No synced
/// table has one today, the schema fingerprint means one cannot appear
/// unnoticed, and a silent lossy encoding is exactly the kind of thing that
/// works in testing and corrupts an attachment in production. If a BLOB column
/// is ever wanted, decide its encoding on purpose.
fn to_json(value: ValueRef<'_>, table: &str, column: &str) -> Result<Value, AppError> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).to_string()),
        ValueRef::Blob(_) => {
            return Err(AppError::General(format!(
                "{table}.{column} is a BLOB and the snapshot has no encoding for one — \
                 decide it deliberately rather than letting it travel lossily"
            )))
        }
    })
}

/// Take a full snapshot of everything classified [`Synced`].
///
/// [`Synced`]: tables::TableClass::Synced
pub fn take(conn: &Connection) -> Result<Snapshot, AppError> {
    let mut out = BTreeMap::new();
    for table in tables::synced_tables() {
        out.insert(table.to_string(), read_table(conn, table)?);
    }

    // `app_meta` is not emitted as a table at all — the allowlist is applied
    // here, so a caller cannot reach the raw rows through the snapshot even by
    // accident.
    let mut meta = BTreeMap::new();
    let mut stmt = conn.prepare("SELECT key, value FROM app_meta")?;
    let mut cursor = stmt.query([])?;
    while let Some(row) = cursor.next()? {
        let key: String = row.get(0)?;
        if tables::meta_key_travels(&key) {
            meta.insert(key, row.get::<_, String>(1)?);
        }
    }

    Ok(Snapshot {
        schema_version: crate::db::schema::CURRENT_VERSION,
        host: crate::host::host_id(),
        taken_at: crate::recurrence::to_iso(chrono::Utc::now()),
        partial: false,
        tables: out,
        meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};

    fn seeded() -> Connection {
        let conn = test_conn();
        tasks::create(&conn, &NewTask { title: "one".into(), ..Default::default() }).unwrap();
        tasks::create(&conn, &NewTask { title: "two".into(), ..Default::default() }).unwrap();
        conn
    }

    #[test]
    fn a_snapshot_carries_every_synced_table() {
        let snap = take(&seeded()).unwrap();
        for table in tables::synced_tables() {
            assert!(snap.tables.contains_key(table), "{table} missing from the snapshot");
        }
        assert_eq!(snap.tables["tasks"].rows.len(), 2);
        assert_eq!(snap.schema_version, crate::db::schema::CURRENT_VERSION);
        assert!(!snap.partial, "v1 always answers full");
    }

    /// The excluded tables are excluded, not merely empty.
    #[test]
    fn host_local_and_internal_tables_never_appear() {
        let conn = seeded();
        conn.execute(
            "INSERT INTO sync_outbox (cmd, args_json, host) VALUES ('x', '{}', 'h')",
            [],
        )
        .unwrap();
        let snap = take(&conn).unwrap();
        for table in ["run_events", "app_meta", "tasks_fts", "sync_outbox", "short_id_leases"] {
            assert!(!snap.tables.contains_key(table), "{table} must not be in a snapshot");
        }
    }

    /// **The refusal the plan names by name.**
    ///
    /// Each of these does a specific, silent kind of damage on arrival:
    /// `schema_version` clobbers a receiver on a different build,
    /// `next_short_id` rewinds a cursor whose numbers are already in commit
    /// messages, and `sync_*` makes a peer think it is paired with itself.
    #[test]
    fn the_snapshot_refuses_to_emit_the_dangerous_meta_keys() {
        let conn = seeded();
        for (k, v) in [
            ("sync_enabled", "1"),
            ("sync_peer_id", "some-other-host"),
            ("sync_seeded_from", "whoever"),
            ("role", "canonical"),
            ("bg_budget_reset_at", "2026-08-20T00:00:00Z"),
        ] {
            conn.execute("INSERT OR REPLACE INTO app_meta (key, value) VALUES (?1, ?2)", [k, v])
                .unwrap();
        }
        let snap = take(&conn).unwrap();

        for key in ["schema_version", "next_short_id", "role", "bg_budget_reset_at"] {
            assert!(!snap.meta.contains_key(key), "{key} escaped into the snapshot");
        }
        assert!(
            !snap.meta.keys().any(|k| k.starts_with("sync_")),
            "a sync_* key escaped: {:?}",
            snap.meta.keys().collect::<Vec<_>>()
        );
    }

    /// A deliberately chosen setting DOES travel, or the test above passes by
    /// emitting nothing at all.
    #[test]
    fn a_real_setting_still_travels() {
        let conn = seeded();
        conn.execute(
            "INSERT OR REPLACE INTO app_meta (key, value) VALUES ('bg_daily_token_cap', '12345')",
            [],
        )
        .unwrap();
        let snap = take(&conn).unwrap();
        assert_eq!(snap.meta.get("bg_daily_token_cap").map(String::as_str), Some("12345"));
    }

    /// `reminded_at` stays behind, and every other task column travels.
    #[test]
    fn reminded_at_is_the_only_task_column_withheld() {
        let conn = seeded();
        let snap = take(&conn).unwrap();
        let cols = &snap.tables["tasks"].columns;
        assert!(!cols.iter().any(|c| c == "reminded_at"), "reminded_at must not travel");

        // Every other column on the real table is present, so the exclusion
        // cannot quietly widen.
        let mut stmt = conn.prepare("SELECT * FROM tasks LIMIT 0").unwrap();
        let all: Vec<String> = stmt.column_names().into_iter().map(str::to_string).collect();
        let expected: Vec<&String> = all.iter().filter(|c| *c != "reminded_at").collect();
        assert_eq!(cols.len(), expected.len(), "columns went missing: {cols:?}");
    }

    /// The payload round-trips through JSON unchanged.
    #[test]
    fn a_snapshot_survives_a_json_round_trip() {
        let snap = take(&seeded()).unwrap();
        let encoded = serde_json::to_vec(&snap).unwrap();
        let back: Snapshot = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(back.tables["tasks"], snap.tables["tasks"]);
        assert_eq!(back.schema_version, snap.schema_version);
    }

    /// NULLs survive as NULL rather than becoming empty strings.
    ///
    /// They are load-bearing everywhere: `due_at IS NULL` is "no date", and a
    /// row that arrives with `""` there fails `datetime()` comparison silently.
    #[test]
    fn nulls_stay_null() {
        let conn = seeded();
        let snap = take(&conn).unwrap();
        let cols = &snap.tables["tasks"].columns;
        let due = cols.iter().position(|c| c == "due_at").unwrap();
        assert!(
            snap.tables["tasks"].rows.iter().all(|r| r[due].is_null()),
            "an undated task must carry NULL, not \"\""
        );
    }
}
