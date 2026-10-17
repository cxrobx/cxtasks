//! Writing post-images into this database.
//!
//! # Never `INSERT OR REPLACE`
//!
//! It is the obvious way to write a row you already have, and here it is
//! catastrophic. REPLACE resolves a conflict by DELETING the existing row and
//! inserting a new one — and `task_comments`, `task_tags`, `task_deps`,
//! `task_attachments` and `task_runs` all reference `tasks(id)` with
//! `ON DELETE CASCADE`, with `PRAGMA foreign_keys = ON` set everywhere. So
//! "update this task" would take every comment, tag, dependency, attachment and
//! run belonging to it, and the insert that followed would look completely
//! successful.
//!
//! `ON CONFLICT(pk) DO UPDATE` keeps the row's identity, so nothing cascades.
//!
//! # Why the update carries a WHERE clause
//!
//! `DO UPDATE` counts as a change even when every value is identical, and the
//! pull applies the whole database on every reconcile. Without a difference
//! test, each pull would report thousands of changes and bump nothing —
//! which matters because the UI resets in-progress edits on a task's
//! `updated_at`: gotcha #24. A sync round trip that touched every row would
//! discard whatever the user was typing, at an interval they cannot see, and
//! the symptom is "typing in Notes loses characters".

use rusqlite::{types::Value as SqlValue, Connection};
use serde_json::Value;

use crate::error::AppError;
use crate::sync::snapshot::{Snapshot, TableSnapshot};
use crate::sync::tables;

/// What an apply actually did. Zero `changed` means nothing on screen should
/// move.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ApplyStats {
    pub inserted_or_updated: usize,
    pub deleted: usize,
}

impl ApplyStats {
    pub fn changed(&self) -> bool {
        self.inserted_or_updated > 0 || self.deleted > 0
    }
}

/// A table's primary key columns, in key order.
///
/// Read from the database rather than hard-coded, so a migration that changes a
/// key does not leave a stale list here writing rows to the wrong place.
fn primary_key(conn: &Connection, table: &str) -> Result<Vec<String>, AppError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut cols: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(pk, _)| *pk > 0)
        .collect();
    cols.sort_by_key(|(pk, _)| *pk);
    if cols.is_empty() {
        return Err(AppError::General(format!(
            "{table} has no primary key, so a post-image has no identity to be written to"
        )));
    }
    Ok(cols.into_iter().map(|(_, name)| name).collect())
}

fn to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| n.as_f64().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null),
        Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// Hold foreign keys until COMMIT for the duration of this transaction.
///
/// Rows arrive in an arbitrary order WITHIN a table, and `projects.parent_id`
/// references `projects(id)` — so a nested project can land before the area it
/// belongs to and fail a constraint that the completed batch satisfies
/// perfectly. Sorting rows topologically would work for that one case and break
/// again on the next self-reference somebody adds.
///
/// Deferring is not weakening: every constraint is still checked, at COMMIT,
/// against the finished state. It is the difference between "is this true yet"
/// and "is this true".
///
/// Applies only inside a transaction, and resets when that transaction ends.
fn defer_foreign_keys(conn: &Connection) -> Result<(), AppError> {
    conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    Ok(())
}

/// [`upsert`], in a transaction of its own.
///
/// For a caller applying one intent at a time: a partially-applied batch is
/// worse than a rejected one, because the sender is told it failed and the
/// receiver keeps half of it.
pub fn upsert_atomic(
    conn: &mut Connection,
    table: &str,
    data: &TableSnapshot,
) -> Result<usize, AppError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    defer_foreign_keys(&tx)?;
    let n = upsert(&tx, table, data)?;
    tx.commit()?;
    Ok(n)
}

/// Write post-images into `table`, updating only rows that actually differ.
pub fn upsert(conn: &Connection, table: &str, data: &TableSnapshot) -> Result<usize, AppError> {
    if data.rows.is_empty() {
        return Ok(0);
    }
    if tables::class_of(table) != Some(tables::TableClass::Synced) {
        return Err(AppError::Validation(format!(
            "'{table}' is not a synced table — refusing to write post-images into it"
        )));
    }
    let pk = primary_key(conn, table)?;
    let cols = &data.columns;
    let placeholders = (1..=cols.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
    let quoted = cols.join(", ");

    // Non-key columns get assigned; key columns are what we matched on.
    let assignments = cols
        .iter()
        .filter(|c| !pk.contains(c))
        .map(|c| format!("{c} = excluded.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    // `IS NOT`, not `<>`: NULL <> NULL is NULL, so a `<>` test would treat every
    // nullable column as "unchanged" and silently skip real updates.
    let differs = cols
        .iter()
        .filter(|c| !pk.contains(c))
        .map(|c| format!("{table}.{c} IS NOT excluded.{c}"))
        .collect::<Vec<_>>()
        .join(" OR ");

    let sql = if assignments.is_empty() {
        // A table that is nothing but its key — `task_tags`. There is no update
        // to make; presence IS the fact.
        format!("INSERT INTO {table} ({quoted}) VALUES ({placeholders}) ON CONFLICT DO NOTHING")
    } else {
        format!(
            "INSERT INTO {table} ({quoted}) VALUES ({placeholders})
             ON CONFLICT({keys}) DO UPDATE SET {assignments} WHERE {differs}",
            keys = pk.join(", ")
        )
    };

    let mut stmt = conn.prepare(&sql)?;
    let mut changed = 0usize;
    for row in &data.rows {
        if row.len() != cols.len() {
            return Err(AppError::Validation(format!(
                "{table}: a row has {} values for {} columns",
                row.len(),
                cols.len()
            )));
        }
        let values: Vec<SqlValue> = row.iter().map(to_sql).collect();
        changed += stmt.execute(rusqlite::params_from_iter(values.iter()))?;
    }
    Ok(changed)
}

/// Make this database match `snap`, whole-row.
///
/// Upserts run parents-first and deletes run children-first, both following the
/// declared table order. That ordering is what keeps foreign keys satisfied at
/// every intermediate step rather than only at the end.
pub fn apply_snapshot(conn: &mut Connection, snap: &Snapshot) -> Result<ApplyStats, AppError> {
    if snap.schema_version != crate::db::schema::CURRENT_VERSION {
        return Err(AppError::Validation(format!(
            "snapshot is schema v{} and this build is v{} — refusing to apply it. \
             Columns would silently not travel, which whole-row sync cannot detect on its own.",
            snap.schema_version,
            crate::db::schema::CURRENT_VERSION
        )));
    }
    let mut stats = ApplyStats::default();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    defer_foreign_keys(&tx)?;

    let order: Vec<&str> = tables::synced_tables().collect();
    for table in &order {
        if let Some(data) = snap.tables.get(*table) {
            stats.inserted_or_updated += upsert(&tx, table, data)?;
        }
    }
    for table in order.iter().rev() {
        if let Some(data) = snap.tables.get(*table) {
            stats.deleted += delete_absent(&tx, table, data)?;
        }
    }

    // The allowlist, applied last so a settings change lands after the rows it
    // might describe.
    for (key, value) in &snap.meta {
        if tables::meta_key_travels(key) {
            crate::db::meta::set(&tx, key, value)?;
        }
    }

    tx.commit()?;
    Ok(stats)
}

/// Remove local rows the snapshot does not have.
///
/// This is what makes a pull converge rather than only ever growing. It is also
/// why a PARTIAL snapshot must never reach here: absence would read as deletion
/// and take the whole database with it.
fn delete_absent(conn: &Connection, table: &str, data: &TableSnapshot) -> Result<usize, AppError> {
    let pk = primary_key(conn, table)?;
    let idx: Vec<usize> = pk
        .iter()
        .map(|k| {
            data.columns.iter().position(|c| c == k).ok_or_else(|| {
                AppError::Validation(format!("{table}: snapshot omits key column '{k}'"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let incoming: std::collections::HashSet<String> = data
        .rows
        .iter()
        .map(|r| idx.iter().map(|i| r[*i].to_string()).collect::<Vec<_>>().join("\u{1f}"))
        .collect();

    let select = format!("SELECT {} FROM {table}", pk.join(", "));
    let mut stmt = conn.prepare(&select)?;
    let local: Vec<Vec<Value>> = stmt
        .query_map([], |row| {
            (0..pk.len())
                .map(|i| {
                    Ok(match row.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => Value::from(n),
                        rusqlite::types::ValueRef::Real(f) => Value::from(f),
                        rusqlite::types::ValueRef::Text(t) => {
                            Value::from(String::from_utf8_lossy(t).to_string())
                        }
                        rusqlite::types::ValueRef::Blob(_) => Value::Null,
                    })
                })
                .collect::<rusqlite::Result<Vec<Value>>>()
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let where_pk = pk.iter().enumerate().map(|(i, k)| format!("{k} = ?{}", i + 1)).collect::<Vec<_>>().join(" AND ");
    let mut del = conn.prepare(&format!("DELETE FROM {table} WHERE {where_pk}"))?;
    let mut removed = 0usize;
    for key in local {
        let joined = key.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\u{1f}");
        if !incoming.contains(&joined) {
            let values: Vec<SqlValue> = key.iter().map(to_sql).collect();
            removed += del.execute(rusqlite::params_from_iter(values.iter()))?;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::test_conn;
    use crate::db::tasks::{self, NewTask};
    use crate::sync::snapshot;

    fn seeded() -> Connection {
        let conn = test_conn();
        let t = tasks::create(&conn, &NewTask { title: "one".into(), ..Default::default() }).unwrap();
        crate::db::tags::attach(&conn, &t.id, "urgent").unwrap();
        crate::db::comments::add(&conn, &t.id, "me", "a note").unwrap();
        conn
    }

    /// **The cascade hazard, asserted directly.**
    ///
    /// Writing a task's post-image must not touch its comments or tags.
    /// `INSERT OR REPLACE` would delete the task row to resolve the conflict
    /// and take all of them with it, and the insert that followed would look
    /// entirely successful.
    #[test]
    fn upserting_a_task_does_not_cascade_away_its_children() {
        let conn = seeded();
        let comments_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_comments", [], |r| r.get(0))
            .unwrap();
        let tags_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_tags", [], |r| r.get(0))
            .unwrap();
        assert!(comments_before > 0 && tags_before > 0, "the fixture must have children");

        let snap = snapshot::take(&conn).unwrap();
        upsert(&conn, "tasks", &snap.tables["tasks"]).unwrap();

        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM task_comments", [], |r| r.get(0)).unwrap(),
            comments_before,
            "the comment was cascade-deleted — this is INSERT OR REPLACE behaviour"
        );
        assert_eq!(
            conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM task_tags", [], |r| r.get(0)).unwrap(),
            tags_before
        );
    }

    /// **A nested project may arrive before the area it belongs to.**
    ///
    /// `projects.parent_id` references `projects(id)`, and rows travel in
    /// whatever order the table yields them — so the constraint is satisfied by
    /// the finished batch and violated part-way through it. Found by pushing a
    /// real database, where three of nine projects are nested.
    #[test]
    fn a_child_row_may_arrive_before_its_parent() {
        let source = test_conn();
        crate::db::projects::create(&source, "Area", crate::db::projects::ProjectKind::Area, None).unwrap();
        crate::db::projects::create(&source, "Nested", crate::db::projects::ProjectKind::Project, Some("Area")).unwrap();
        let snap = snapshot::take(&source).unwrap();

        // Reverse the rows, so the child is written first.
        let mut reversed = snap.clone();
        reversed.tables.get_mut("projects").unwrap().rows.reverse();

        let mut target = test_conn();
        apply_snapshot(&mut target, &reversed).expect("a child before its parent must still apply");

        let n: i64 = target
            .query_row("SELECT COUNT(*) FROM projects WHERE parent_id IS NOT NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "the nesting must survive");
    }

    /// **An identical post-image changes nothing** — the gotcha-#24 guard.
    ///
    /// A pull applies the whole database. If every row counted as changed, the
    /// UI would reset in-progress edits on every reconcile, and the symptom
    /// would be characters disappearing while typing in Notes.
    #[test]
    fn re_applying_the_same_snapshot_reports_no_change() {
        let mut conn = seeded();
        let snap = snapshot::take(&conn).unwrap();

        let first = apply_snapshot(&mut conn, &snap).unwrap();
        assert!(!first.changed(), "applying a snapshot to its own source changed {first:?}");

        let again = apply_snapshot(&mut conn, &snap).unwrap();
        assert!(!again.changed());
    }

    /// A real difference IS reported and applied.
    #[test]
    fn a_changed_row_is_written_and_counted() {
        let mut conn = seeded();
        let mut snap = snapshot::take(&conn).unwrap();

        let cols = &snap.tables["tasks"].columns;
        let title = cols.iter().position(|c| c == "title").unwrap();
        snap.tables.get_mut("tasks").unwrap().rows[0][title] = Value::from("renamed elsewhere");

        let stats = apply_snapshot(&mut conn, &snap).unwrap();
        assert_eq!(stats.inserted_or_updated, 1);
        let got: String = conn
            .query_row("SELECT title FROM tasks LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, "renamed elsewhere");
    }

    /// A row the snapshot no longer has is removed, which is what makes a pull
    /// converge instead of only ever growing.
    #[test]
    fn a_row_absent_from_the_snapshot_is_deleted_locally() {
        let mut conn = seeded();
        let snap = snapshot::take(&conn).unwrap();

        tasks::create(&conn, &NewTask { title: "only here".into(), ..Default::default() }).unwrap();
        assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap(), 2);

        let stats = apply_snapshot(&mut conn, &snap).unwrap();
        assert_eq!(stats.deleted, 1);
        assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap(), 1);
    }

    /// `reminded_at` is not in a snapshot, so applying one must leave this
    /// host's value alone rather than clearing it.
    #[test]
    fn applying_a_snapshot_does_not_disturb_reminded_at() {
        let mut conn = seeded();
        conn.execute("UPDATE tasks SET reminded_at = '2026-01-01 00:00:00'", []).unwrap();
        let snap = snapshot::take(&conn).unwrap();
        conn.execute("UPDATE tasks SET title = 'local edit'", []).unwrap();

        apply_snapshot(&mut conn, &snap).unwrap();

        let kept: Option<String> = conn
            .query_row("SELECT reminded_at FROM tasks LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            kept.as_deref(),
            Some("2026-01-01 00:00:00"),
            "a pull must not re-arm or disarm this machine's reminder"
        );
    }

    /// A snapshot from a different schema is refused rather than applied.
    #[test]
    fn a_snapshot_from_another_schema_version_is_refused() {
        let mut conn = seeded();
        let mut snap = snapshot::take(&conn).unwrap();
        snap.schema_version = crate::db::schema::CURRENT_VERSION - 1;

        let err = apply_snapshot(&mut conn, &snap).unwrap_err().to_string();
        assert!(err.contains("refusing to apply"), "{err}");
    }

    /// Post-images may only be written into tables that actually sync.
    #[test]
    fn writing_post_images_into_an_unsynced_table_is_refused() {
        let conn = seeded();
        let data = TableSnapshot { columns: vec!["key".into(), "value".into()], rows: vec![vec![Value::from("x"), Value::from("y")]] };
        let err = upsert(&conn, "app_meta", &data).unwrap_err().to_string();
        assert!(err.contains("not a synced table"), "{err}");
    }
}
